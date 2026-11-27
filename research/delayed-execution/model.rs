//! Fixed-committee model for a later ordering certificate binding earlier execution.

use super::*;
use std::collections::{BTreeMap, VecDeque};

use ckb_types::packed::Byte32;
use ed25519_dalek::{Signer, SigningKey};
use prost::Message;
use stem_ledger::block::Header;
use stem_stake::{
    Hash32, Member,
    history::Frontier,
    jolteon::{Committee, CommitteeChange, FinalityProof, Point, Quorum, roster_id},
    settlement::ckb::{self, Advance, Policy},
};

const DELAY: u64 = 3;

fn hash(domain: &[u8], fields: &[&[u8]]) -> Hash32 {
    let mut hash = ckb_hash::new_blake2b();
    hash.update(domain);
    for field in fields {
        hash.update(field);
    }
    let mut result = [0; 32];
    hash.finalize(&mut result);
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Execution {
    height: u64,
    ordered: Hash32,
    header: Hash32,
}

#[derive(Clone, Debug)]
struct Ordered {
    height: u64,
    parent: Hash32,
    time: u64,
    payload: Hash32,
    executed: Execution,
}

impl Ordered {
    fn id(&self, domain: &Hash32) -> Hash32 {
        hash(
            b"Stem experimental delayed ordering block\0",
            &[
                domain,
                &self.height.to_le_bytes(),
                &self.parent,
                &self.time.to_le_bytes(),
                &self.payload,
                &self.executed.height.to_le_bytes(),
                &self.executed.ordered,
                &self.executed.header,
            ],
        )
    }
}

struct Cursor {
    network: Byte32,
    domain: Hash32,
    ordered: Hash32,
    ordered_height: u64,
    ordered_time: u64,
    execution: Execution,
    in_flight: VecDeque<Ordered>,
    committee: Committee,
}

impl Cursor {
    fn new(network: Byte32, genesis: &Header, members: &[Member]) -> Result<Self> {
        let header: Hash32 = genesis.id(&network).unpack();
        let domain = hash(
            b"Stem experimental delayed ordering network\0",
            &[network.as_slice(), &header, &DELAY.to_le_bytes()],
        );
        let ordered = hash(b"Stem experimental delayed ordering genesis\0", &[&domain]);
        Ok(Self {
            network,
            domain,
            ordered,
            ordered_height: 0,
            ordered_time: genesis.context.timestamp_ms,
            execution: Execution {
                height: 0,
                ordered,
                header,
            },
            in_flight: VecDeque::new(),
            committee: Committee::new(domain, ordered, 0, members)?,
        })
    }

    fn advance(
        &mut self,
        block: &Ordered,
        parent_qc: &Quorum,
        child: &Ordered,
        child_qc: &Quorum,
        executed: Option<&Header>,
    ) -> Result<Option<Execution>> {
        let id = block.id(&self.domain);
        anyhow::ensure!(
            parent_qc.point.header == id
                && child_qc.point.header == child.id(&self.domain)
                && child_qc.point.directly_certifies(parent_qc.point)
                && Some(block.height) == self.ordered_height.checked_add(1)
                && block.parent == self.ordered
                && block.time > self.ordered_time
                && Some(child.height) == block.height.checked_add(1)
                && child.parent == id
                && child.time > block.time,
            "ordering proof is not a certified contiguous pair",
        );
        self.committee.verify_quorum(parent_qc)?;
        self.committee.verify_quorum(child_qc)?;

        let finalized = if block.height <= DELAY {
            anyhow::ensure!(
                executed.is_none() && block.executed == self.execution,
                "invalid startup execution marker"
            );
            None
        } else {
            let earlier = self
                .in_flight
                .front()
                .context("missing earlier ordered payload")?;
            let header = executed.context("ordered prefix reached its execution deadline")?;
            let execution = Execution {
                height: earlier.height,
                ordered: earlier.id(&self.domain),
                header: header.id(&self.network).unpack(),
            };
            anyhow::ensure!(
                block.height - DELAY == earlier.height
                    && header.context.height == earlier.height
                    && header.context.timestamp_ms == earlier.time
                    && header.context.parent.as_slice() == self.execution.header
                    && header.context.transaction_root.as_slice() == earlier.payload
                    && block.executed == execution,
                "execution differs from the certified ordered prefix",
            );
            Some(execution)
        };

        if let Some(execution) = finalized {
            self.execution = execution;
            self.in_flight.pop_front();
        }
        self.in_flight.push_back(block.clone());
        self.ordered = id;
        self.ordered_height = block.height;
        self.ordered_time = block.time;
        Ok(finalized)
    }
}

fn qc(committee: &Committee, keys: &[SigningKey], view: u64, id: Hash32) -> Result<Quorum> {
    let point = Point {
        view,
        parent_view: view - 1,
        header: id,
    };
    let votes: BTreeMap<_, _> = keys
        .iter()
        .enumerate()
        .take(3)
        .map(|(i, key)| (i as u16, key.sign(&committee.vote_digest(point)).to_bytes()))
        .collect();
    Ok(committee.assemble_quorum(point, &votes)?)
}

#[test]
fn ordered_prefix_requires_later_execution_commitment_before_settlement() -> Result<()> {
    let fixture = Fixture::new()?;
    let home = tempfile::tempdir()?;
    let mut store = Store::open(home.path(), &fixture.spec, Retention::Archive)?;
    let genesis = store.tip().clone();
    let network = store.network().clone();

    let mut keys: Vec<_> = (1..=4)
        .map(|byte| SigningKey::from_bytes(&[byte; 32]))
        .collect();
    keys.sort_by_key(|key| key.verifying_key().to_bytes());
    let members: Vec<_> = keys
        .iter()
        .map(|key| Member {
            key: key.verifying_key().to_bytes(),
            power: 1,
        })
        .collect();
    let mut cursor = Cursor::new(network.clone(), &genesis, &members)?;
    let mut blocks = Vec::<Ordered>::new();
    let mut certificates = Vec::new();
    let mut headers = vec![genesis.clone()];
    let mut transactions = Vec::new();
    let mut receipts = Vec::new();
    let mut input = fixture.spend.clone();

    let conflict_a = fixture.signed(input.clone(), None, super::FUND - FEE, None)?;
    let conflict_b = fixture.signed(input.clone(), None, super::FUND - 2 * FEE, None)?;
    store.check(&conflict_a)?;
    store.check(&conflict_b)?;
    assert!(
        store
            .prepare(&[conflict_a, conflict_b], super::TIME + 1, false)
            .is_err()
    );
    assert_eq!(store.tip(), &genesis);

    for height in 1..=7 {
        let tx = fixture.signed(input, None, super::FUND - height * FEE, None)?;
        input = OutPoint::new(tx.id(&network), 0);
        let time = super::TIME + height;
        let block = Ordered {
            height,
            parent: blocks
                .last()
                .map_or(cursor.ordered, |parent| parent.id(&cursor.domain)),
            time,
            payload: Tree::transactions(network.clone(), std::slice::from_ref(&tx))?
                .root()
                .unpack(),
            executed: if height <= DELAY {
                cursor.execution
            } else {
                let earlier = &blocks[(height - DELAY - 1) as usize];
                Execution {
                    height: earlier.height,
                    ordered: earlier.id(&cursor.domain),
                    header: headers[earlier.height as usize].id(&network).unpack(),
                }
            },
        };
        certificates.push(qc(
            &cursor.committee,
            &keys,
            height,
            block.id(&cursor.domain),
        )?);
        blocks.push(block);
        let prepared = store.prepare(std::slice::from_ref(&tx), time, false)?;
        receipts.push(prepared.receipts()[0].clone());
        transactions.push(tx);
        store.commit(prepared)?;
        headers.push(store.tip().clone());
    }

    for i in 0..3 {
        assert!(
            cursor
                .advance(
                    &blocks[i],
                    &certificates[i],
                    &blocks[i + 1],
                    &certificates[i + 1],
                    None
                )?
                .is_none()
        );
    }
    assert_eq!(cursor.execution.height, 0);
    assert_eq!(cursor.in_flight.len(), 3);
    assert!(
        cursor
            .advance(
                &blocks[3],
                &certificates[3],
                &blocks[4],
                &certificates[4],
                None
            )
            .is_err()
    );

    let mut changed = headers[1].clone();
    changed.state.system_root = [123; 32].pack();
    assert!(
        cursor
            .advance(
                &blocks[3],
                &certificates[3],
                &blocks[4],
                &certificates[4],
                Some(&changed)
            )
            .is_err()
    );
    let mut changed_order = blocks[3].clone();
    changed_order.executed.ordered = blocks[1].id(&cursor.domain);
    let changed_certificate = qc(
        &cursor.committee,
        &keys,
        4,
        changed_order.id(&cursor.domain),
    )?;
    let mut changed_child = blocks[4].clone();
    changed_child.parent = changed_order.id(&cursor.domain);
    let changed_child_certificate = qc(
        &cursor.committee,
        &keys,
        5,
        changed_child.id(&cursor.domain),
    )?;
    assert!(
        cursor
            .advance(
                &changed_order,
                &changed_certificate,
                &changed_child,
                &changed_child_certificate,
                Some(&headers[1])
            )
            .is_err()
    );
    assert_eq!(cursor.ordered_height, 3);

    for i in 3..6 {
        let executed = cursor
            .advance(
                &blocks[i],
                &certificates[i],
                &blocks[i + 1],
                &certificates[i + 1],
                Some(&headers[i - 2]),
            )?
            .unwrap();
        assert_eq!(executed.height, (i - 2) as u64);
        assert_eq!(cursor.in_flight.len(), 3);
        let tx = &transactions[i - 3];
        let receipt = &receipts[i - 3];
        let tx_proof = Tree::transactions(network.clone(), std::slice::from_ref(tx))?.prove(0)?;
        let receipt_proof =
            Tree::receipts(network.clone(), std::slice::from_ref(receipt))?.prove(0)?;
        assert!(tx_proof.verify_execution(
            &network,
            &headers[i - 2].execution(),
            tx,
            &receipt_proof,
            receipt
        ));
    }

    let genesis_id: Hash32 = genesis.id(&network).unpack();
    let production = Committee::new(network.unpack(), genesis_id, 0, &members)?;
    assert!(production.verify_quorum(&certificates[0]).is_err());
    let production_parent = qc(&production, &keys, 1, headers[1].id(&network).unpack())?;
    let production_child = qc(&production, &keys, 2, headers[2].id(&network).unpack())?;
    let roster = CommitteeChange {
        epoch: 0,
        activation_height: 1,
        members,
    }
    .encode();
    let policy = Policy {
        network: network.unpack(),
        genesis: genesis_id,
        ckb_genesis: [71; 32],
        initial_roster: roster_id(&roster),
    };
    let witness = |parent: &Quorum, child: &Quorum| {
        Advance {
            roster: roster.clone(),
            genesis_header: genesis.encode().to_vec(),
            previous_frontier: Frontier::empty(network.unpack()).encode(),
            suffix: Vec::new(),
            proof: FinalityProof {
                parent_header: headers[1].encode().to_vec(),
                parent_qc: parent.encode(),
                child_header: headers[2].encode().to_vec(),
                child_qc: child.encode(),
            }
            .encode_to_vec(),
            terminal: false,
        }
        .encode_to_vec()
    };
    assert!(ckb::verify(&policy, None, &witness(&certificates[0], &certificates[1])).is_err());
    assert_eq!(
        ckb::verify(
            &policy,
            None,
            &witness(&production_parent, &production_child)
        )?
        .height(),
        1
    );
    Ok(())
}
