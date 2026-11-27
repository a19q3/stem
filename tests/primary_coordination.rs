//! Fixed-route Case A experiment. Each chain uses the native execution proof.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_hash::blake2b_256;
use ckb_types::{packed::Byte32, prelude::*};
use ed25519_dalek::{Signer, SigningKey};
use prost::Message as _;
use stem_bridge_wire::intercell::{Identity, Message, record_key};
use stem_ledger::{
    block::{Header, HeaderContext, StateCommitment},
    history::History,
    light::{FinalityVerifier, verify_intercell, verify_system_record},
    patricia::{Namespace, Patricia, Proof},
};
use stem_stake::{
    Member,
    jolteon::{Committee, FinalityProof, Point},
};

struct Head {
    network: Byte32,
    verifier: FinalityVerifier,
    current: Header,
}

impl Head {
    fn advance(&mut self, certificate: &[u8]) -> Result<()> {
        let next = self.verifier.verify(certificate, false)?;
        let header = next.header();
        ensure!(
            self.current.context.height.checked_add(1) == Some(header.context.height)
                && header.context.parent == self.current.id(&self.network)
                && header.context.timestamp_ms > self.current.context.timestamp_ms,
            "checkpoint does not extend the accepted head"
        );
        self.current = header.clone();
        Ok(())
    }

    fn require_record(&self, key: &[u8], value: &[u8], proof: &Proof) -> Result<()> {
        ensure!(
            verify_system_record(&self.network, &self.current, key, Some(value), proof),
            "record is outside the accepted checkpoint"
        );
        Ok(())
    }
}

struct Route {
    source: Identity,
    primary: Identity,
    destination: Identity,
    sender: Identity,
    receiver: Identity,
}

struct Request {
    id: Identity,
    bytes_hash: Identity,
    amount: u64,
    cancel_after: u64,
    destination_expiry: u64,
}

impl Request {
    fn authenticate(route: &Route, head: &Head, raw: &[u8], path: &Proof) -> Result<Self> {
        let message = Message::decode(raw).context("invalid Intercell envelope")?;
        ensure!(
            head.network.as_slice() == route.source
                && message.source == route.source
                && message.destination == route.destination
                && message.sender == route.sender
                && message.receiver == route.receiver,
            "message outside the pinned route"
        );
        let id = message.id();
        ensure!(
            verify_intercell(&head.network, &head.current, false, &id, Some(raw), path),
            "message outside the authenticated Outbox"
        );
        let (fields, rest) = message.payload.as_chunks::<8>();
        let [amount, cancel_after, destination_expiry] = fields else {
            anyhow::bail!("invalid workflow payload length");
        };
        ensure!(rest.is_empty(), "invalid workflow payload length");
        let amount = u64::from_le_bytes(*amount);
        ensure!(amount > 0, "empty reservation");
        Ok(Self {
            id,
            bytes_hash: blake2b_256(raw),
            amount,
            cancel_after: u64::from_le_bytes(*cancel_after),
            destination_expiry: u64::from_le_bytes(*destination_expiry),
        })
    }

    fn decision_key(&self) -> Vec<u8> {
        [b"primary/decision/".as_slice(), self.id.as_slice()].concat()
    }

    fn outcome_key(&self) -> Vec<u8> {
        [b"primary/outcome/".as_slice(), self.id.as_slice()].concat()
    }

    fn decision(&self, decision: Decision) -> [u8; 33] {
        let mut raw = [0; 33];
        raw[0] = decision as u8;
        raw[1..].copy_from_slice(&self.bytes_hash);
        raw
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Decision {
    Deliver = 0,
    Cancel = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Outcome {
    bytes_hash: Identity,
    executed: bool,
    height: u64,
}

impl Outcome {
    fn encode(self) -> [u8; 41] {
        let mut raw = [0; 41];
        raw[0] = u8::from(self.executed);
        raw[1..9].copy_from_slice(&self.height.to_le_bytes());
        raw[9..].copy_from_slice(&self.bytes_hash);
        raw
    }
}

#[derive(Default)]
struct Destination {
    outcomes: BTreeMap<Identity, Outcome>,
    delivered: u64,
}

impl Destination {
    fn apply(
        &mut self,
        route: &Route,
        request: &Request,
        primary: &Head,
        decision: Decision,
        proof: &Proof,
        height: u64,
    ) -> Result<Outcome> {
        ensure!(primary.network.as_slice() == route.primary, "wrong Primary");
        primary.require_record(&request.decision_key(), &request.decision(decision), proof)?;
        if let Some(outcome) = self.outcomes.get(&request.id) {
            ensure!(outcome.bytes_hash == request.bytes_hash, "changed request");
            return Ok(*outcome);
        }
        ensure!(
            (decision == Decision::Cancel) == (height >= request.destination_expiry),
            "decision is ineligible at the destination height"
        );
        let outcome = Outcome { bytes_hash: request.bytes_hash, executed: decision == Decision::Deliver, height };
        if outcome.executed {
            self.delivered = self.delivered.checked_add(request.amount).context("amount overflow")?;
        }
        self.outcomes.insert(request.id, outcome);
        Ok(outcome)
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Phase {
    Reserved,
    CancelRequested,
    Settled(Outcome),
    Refunded(Outcome),
}

struct Workflow {
    phase: Phase,
    request: Request,
}

impl Workflow {
    fn open(request: Request) -> Self {
        Self { request, phase: Phase::Reserved }
    }

    fn cancel(&mut self, height: u64) -> Result<()> {
        ensure!(self.phase == Phase::Reserved && height >= self.request.cancel_after, "cancellation is ineligible on Primary");
        self.phase = Phase::CancelRequested;
        Ok(())
    }

    fn decision(&self) -> Result<[u8; 33]> {
        match self.phase {
            Phase::Reserved => Ok(self.request.decision(Decision::Deliver)),
            Phase::CancelRequested => Ok(self.request.decision(Decision::Cancel)),
            _ => anyhow::bail!("workflow already finished"),
        }
    }

    fn finish(&mut self, route: &Route, head: &Head, outcome: Outcome, path: &Proof) -> Result<()> {
        ensure!(head.network.as_slice() == route.destination, "wrong destination");
        head.require_record(&self.request.outcome_key(), &outcome.encode(), path)?;
        ensure!(
            self.request.bytes_hash == outcome.bytes_hash
                && outcome.height <= head.current.context.height
                && outcome.executed == (outcome.height < self.request.destination_expiry),
            "outcome does not match the reserved workflow"
        );
        match self.phase {
            Phase::Settled(prior) | Phase::Refunded(prior) => {
                ensure!(outcome == prior, "conflicting terminal outcome");
            }
            Phase::Reserved | Phase::CancelRequested if outcome.executed => {
                self.phase = Phase::Settled(outcome);
            }
            Phase::CancelRequested => self.phase = Phase::Refunded(outcome),
            Phase::Reserved => anyhow::bail!("refund requires a prior cancellation request"),
        }
        Ok(())
    }
}

/// Produces synthetic state, signed by a real independent weighted committee.
struct Fixture {
    head: Head,
    committee: Committee,
    keys: Vec<SigningKey>,
    history: History,
    headers: Vec<Header>,
}

impl Fixture {
    fn new(network: Identity, seed: u8) -> Result<Self> {
        let mut keys: Vec<_> = (seed..seed + 4).map(|key| SigningKey::from_bytes(&[key; 32])).collect();
        keys.sort_by_key(|key| key.verifying_key().to_bytes());
        let members: Vec<_> =
            keys.iter().zip([1, 2, 1, 3]).map(|(key, power)| Member { key: key.verifying_key().to_bytes(), power }).collect();
        let network: Byte32 = network.pack();
        let history = History::empty(network.clone());
        let genesis = Header {
            context: HeaderContext { height: 0, timestamp_ms: 0, parent: Byte32::default(), transaction_root: Byte32::default() },
            state: StateCommitment {
                cell_root: Byte32::default(),
                system_root: Patricia::new(network.clone(), Namespace::System).empty_root().pack(),
                history_root: history.root(),
            },
            receipt_root: Byte32::default(),
        };
        let genesis_id = genesis.id(&network);
        let committee = Committee::new(network.unpack(), genesis_id.unpack(), 0, &members)?;
        let verifier = FinalityVerifier::new(network.clone(), genesis_id, &members)?;
        let head = Head { network, verifier, current: genesis.clone() };
        Ok(Self { head, committee, keys, history, headers: vec![genesis] })
    }

    fn append(&mut self, root: Byte32) -> Result<()> {
        let parent = self.headers.last().context("missing genesis")?;
        let context = HeaderContext::after(&self.head.network, parent, parent.context.timestamp_ms + 1, Byte32::default())?;
        self.headers.push(Header::next(
            &self.head.network,
            parent,
            &mut self.history,
            context,
            Byte32::default(),
            root,
            Byte32::default(),
        )?);
        Ok(())
    }

    fn quorum(&self, header: &Header) -> Result<Vec<u8>> {
        let point = Point {
            view: header.context.height,
            parent_view: header.context.height - 1,
            header: header.id(&self.head.network).unpack(),
        };
        let votes = self
            .keys
            .iter()
            .enumerate()
            .map(|(index, key)| (index as u16, key.sign(&self.committee.vote_digest(point)).to_bytes()))
            .collect();
        Ok(self.committee.assemble_quorum(point, &votes)?.encode())
    }

    fn certificate(&self, height: u64) -> Result<Vec<u8>> {
        let parent = &self.headers[height as usize];
        let child = &self.headers[(height + 1) as usize];
        Ok(FinalityProof {
            parent_header: parent.encode().to_vec(),
            parent_qc: self.quorum(parent)?,
            child_header: child.encode().to_vec(),
            child_qc: self.quorum(child)?,
        }
        .encode_to_vec())
    }

    fn commit_record(&mut self, height: u64, key: &[u8], value: &[u8]) -> Result<Proof> {
        // An earlier QC has fixed the current fixture tip. Fill the remaining
        // heights on that same parent chain before building the requested record.
        ensure!(height >= self.headers.len() as u64, "record height was already proposed");
        let tree = Patricia::new(self.head.network.clone(), Namespace::System);
        let mut edit = tree.edit(tree.empty_root(), |_| Ok(None::<Vec<u8>>));
        let key = tree.record_key(key);
        edit.set(key, Some(tree.value_hash(value)))?;
        let proof = edit.prove(&key)?.1;
        while self.headers.len() < height as usize {
            self.append(self.headers.last().unwrap().state.system_root.clone())?;
        }
        self.append(edit.root().pack())?;
        self.append(edit.root().pack())?;
        self.advance_head(height)?;
        Ok(proof)
    }

    fn advance_head(&mut self, height: u64) -> Result<()> {
        while self.head.current.context.height < height {
            let certificate = self.certificate(self.head.current.context.height + 1)?;
            self.head.advance(&certificate)?;
        }
        Ok(())
    }

    fn extend_unchanged(&mut self, height: u64) -> Result<()> {
        while self.headers.len() <= height as usize + 1 {
            self.append(self.headers.last().unwrap().state.system_root.clone())?;
        }
        self.advance_head(height)
    }
}

fn make_route() -> Route {
    Route { source: [10; 32], primary: [20; 32], destination: [30; 32], sender: [40; 32], receiver: [50; 32] }
}

fn reserved(route: &Route) -> Result<(Fixture, Workflow, Vec<u8>, Proof)> {
    let mut source = Fixture::new(route.source, 1)?;
    let payload = [600u64, 5, 4].into_iter().flat_map(u64::to_le_bytes).collect::<Vec<_>>();
    let message = Message {
        source: route.source,
        destination: route.destination,
        sender: route.sender,
        receiver: route.receiver,
        nonce: 0,
        payload: &payload,
    };
    let raw = [message.header().as_slice(), &payload].concat();
    let proof = source.commit_record(1, &record_key(false, &message.id()), &raw)?;
    let request = Request::authenticate(route, &source.head, &raw, &proof)?;
    Ok((source, Workflow::open(request), raw, proof))
}

#[test]
fn pre_expiry_execution_settles_even_if_its_receipt_arrives_after_cancel() -> Result<()> {
    let route = make_route();
    let (_, mut workflow, _, _) = reserved(&route)?;
    let mut primary = Fixture::new(route.primary, 11)?;
    let mut remote = Fixture::new(route.destination, 21)?;
    let mut destination = Destination::default();
    let key = workflow.request.decision_key();
    let relay = primary.commit_record(1, &key, &workflow.decision()?)?;
    let outcome = destination.apply(&route, &workflow.request, &primary.head, Decision::Deliver, &relay, 3)?;
    assert!(outcome.executed);
    let receipt = remote.commit_record(3, &workflow.request.outcome_key(), &outcome.encode())?;
    assert!(workflow.cancel(4).is_err());
    workflow.cancel(5)?;
    assert_eq!(workflow.phase, Phase::CancelRequested);
    let cancel = primary.commit_record(5, &key, &workflow.decision()?)?;
    assert_eq!(destination.apply(&route, &workflow.request, &primary.head, Decision::Cancel, &cancel, 8)?, outcome);
    assert!(destination.apply(&route, &workflow.request, &primary.head, Decision::Deliver, &relay, 8).is_err());
    remote.extend_unchanged(7)?;
    let cancelled = Outcome { executed: false, height: 4, ..outcome };
    assert!(workflow.finish(&route, &remote.head, cancelled, &receipt).is_err());
    assert_eq!(workflow.phase, Phase::CancelRequested);
    let mut recovered = Fixture::new(route.destination, 21)?;
    for height in 1..=7 {
        recovered.head.advance(&remote.certificate(height)?)?;
    }
    assert_eq!(recovered.head.current, remote.head.current);
    workflow.finish(&route, &recovered.head, outcome, &receipt)?;
    workflow.finish(&route, &remote.head, outcome, &receipt)?;
    assert_eq!(workflow.phase, Phase::Settled(outcome));
    assert_eq!(destination.delivered, 600);
    Ok(())
}

#[test]
fn cancellation_needs_destination_evidence_and_rules_out_late_execution() -> Result<()> {
    let route = make_route();
    let (_, mut workflow, _, _) = reserved(&route)?;
    let mut primary = Fixture::new(route.primary, 11)?;
    let mut remote = Fixture::new(route.destination, 21)?;
    let mut destination = Destination::default();
    let key = workflow.request.decision_key();
    let relay = primary.commit_record(1, &key, &workflow.decision()?)?;
    let mut delayed_relay = Fixture::new(route.primary, 11)?;
    delayed_relay.head.advance(&primary.certificate(1)?)?;
    assert!(destination.apply(&route, &workflow.request, &primary.head, Decision::Deliver, &relay, 4).is_err());
    assert_eq!(destination.delivered, 0);
    workflow.cancel(5)?;
    let cancel = primary.commit_record(5, &key, &workflow.decision()?)?;
    assert!(destination.apply(&route, &workflow.request, &primary.head, Decision::Cancel, &cancel, 3).is_err());
    let outcome = destination.apply(&route, &workflow.request, &primary.head, Decision::Cancel, &cancel, 4)?;
    assert!(!outcome.executed);
    let absent = Proof { nodes: vec![] };
    assert!(verify_system_record(&remote.head.network, &remote.head.current, &workflow.request.outcome_key(), None, &absent,));
    assert!(workflow.finish(&route, &remote.head, outcome, &absent).is_err());
    assert_eq!(workflow.phase, Phase::CancelRequested);
    let receipt = remote.commit_record(4, &workflow.request.outcome_key(), &outcome.encode())?;
    assert!(workflow.finish(&route, &primary.head, outcome, &receipt).is_err());
    workflow.finish(&route, &remote.head, outcome, &receipt)?;
    workflow.finish(&route, &remote.head, outcome, &receipt)?;
    assert_eq!(workflow.phase, Phase::Refunded(outcome));
    assert_eq!(destination.apply(&route, &workflow.request, &delayed_relay.head, Decision::Deliver, &relay, 5)?, outcome);
    assert_eq!(destination.apply(&route, &workflow.request, &primary.head, Decision::Cancel, &cancel, 5)?, outcome);
    assert_eq!(destination.delivered, 0);
    Ok(())
}

#[test]
fn source_requires_the_pinned_committee_ancestry_endpoint_and_exact_bytes() -> Result<()> {
    let route = make_route();
    let (source, workflow, raw, path) = reserved(&route)?;
    let mut impostor = Fixture::new(route.source, 51)?;
    let other_path = impostor.commit_record(1, &record_key(false, &workflow.request.id), &raw)?;
    assert_eq!(path, other_path);
    let mut verifier = Fixture::new(route.source, 1)?;
    assert!(verifier.head.advance(&impostor.certificate(1)?).is_err());
    assert!(verifier.head.advance(&source.certificate(1)?).is_ok());
    assert!(verifier.head.advance(&source.certificate(1)?).is_err());
    let mut skipped = Fixture::new(route.source, 1)?;
    skipped.commit_record(2, &record_key(false, &workflow.request.id), &raw)?;
    let mut start = Fixture::new(route.source, 1)?;
    assert!(start.head.advance(&skipped.certificate(2)?).is_err());
    let mut different_payload = raw.clone();
    *different_payload.last_mut().unwrap() ^= 1;
    assert_eq!(Message::decode(&different_payload).unwrap().id(), workflow.request.id);
    assert!(Request::authenticate(&route, &source.head, &different_payload, &path).is_err());
    let wrong_route = Route { receiver: [91; 32], ..route };
    assert!(Request::authenticate(&wrong_route, &source.head, &raw, &path).is_err());
    assert_eq!(verifier.head.current, source.head.current);
    Ok(())
}
