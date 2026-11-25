use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{HeaderView, OutPoint, TransactionAndWitnessProof};
use ckb_types::{
    H256,
    core::ScriptHashType,
    packed::{Script, Transaction, WitnessArgs},
    prelude::*,
};
use clap::Args;
use serde::Serialize;
use serde_json::json;
use stem_bridge_wire::intercell::{Delivery, MailboxPolicy, MailboxState, Message};
use stem_stake::intercell::consume;

use crate::{
    bridge_cli::{parse_hash, print_json},
    l1::Rpc,
};

#[derive(Args)]
pub struct Recover {
    #[arg(long)]
    l1_rpc: String,
    #[arg(long, value_parser = parse_hash)]
    tx_hash: H256,
    #[arg(long)]
    output_index: u32,
    /// Complete Type Script hash of the expected mailbox.
    #[arg(long, value_parser = parse_hash)]
    mailbox: H256,
    /// Maximum committed transaction reads, including unrelated inputs.
    #[arg(long, default_value_t = 2048, value_parser = clap::value_parser!(u32).range(1..=65536))]
    max_transactions: u32,
}

#[derive(Serialize)]
struct Recovered {
    mailbox: H256,
    head: OutPoint,
    l1_height: u64,
    l1_block: H256,
    expected_root: H256,
    consumed: Vec<H256>,
    transitions: u32,
    transaction_reads: u32,
}

struct History<'a> {
    node: &'a Rpc,
    remaining: u32,
}

impl History<'_> {
    fn read(&mut self, hash: &H256) -> Result<(Transaction, u64, H256)> {
        ensure!(
            self.remaining > 0,
            "Intercell recovery transaction budget exhausted"
        );
        self.remaining -= 1;
        let (raw, height, block) = self.node.committed(hash)?;
        ensure!(
            self.node.block_hash(height)? == block,
            "Intercell history changed during recovery"
        );
        Ok((Transaction::from_slice(raw.as_bytes())?, height, block))
    }
}

fn state(tx: &Transaction, index: usize) -> Result<ckb_types::bytes::Bytes> {
    Ok(tx
        .raw()
        .outputs_data()
        .get(index)
        .context("Intercell mailbox output data unavailable")?
        .raw_data())
}

impl Recover {
    pub(super) fn run(self) -> Result<()> {
        let node = Rpc::new(&self.l1_rpc)?;
        let head = OutPoint {
            tx_hash: self.tx_hash,
            index: self.output_index.into(),
        };
        let live = node.live_cell(&head)?;
        let script: Script = live
            .output
            .type_
            .context("missing Intercell mailbox type")?
            .into();
        let mailbox = script.calc_script_hash();
        ensure!(
            mailbox.as_slice() == self.mailbox.as_bytes()
                && ScriptHashType::try_from(script.hash_type())? == ScriptHashType::Data2,
            "Intercell mailbox Type Script mismatch"
        );
        let policy = MailboxPolicy::decode(&script.args().raw_data())
            .context("invalid Intercell mailbox policy")?;
        ensure!(
            node.block_hash(0)?.as_bytes() == &policy.ckb_genesis,
            "Intercell mailbox CKB genesis mismatch"
        );

        let mut history = History {
            node: &node,
            remaining: self.max_transactions,
        };
        let (mut tx, l1_height, l1_block) = history.read(&head.tx_hash)?;
        let mut block = l1_block.clone();
        let mut index = self.output_index as usize;
        let live_data = live
            .data
            .context("missing live Intercell mailbox data")?
            .content;
        let expected_root = MailboxState::decode(live_data.as_bytes())
            .context("invalid live Intercell mailbox state")?
            .consumed;
        ensure!(
            state(&tx, index)?.as_ref() == live_data.as_bytes(),
            "live Intercell data differs from committed transaction"
        );

        let mut consumed = Vec::new();
        let mut transitions = 0;
        loop {
            let raw = tx.raw();
            let output = raw
                .outputs()
                .get(index)
                .context("missing Intercell mailbox output")?;
            ensure!(
                output.type_().to_opt().as_ref() == Some(&script)
                    && output.lock().calc_script_hash().as_slice() == policy.owner,
                "Intercell mailbox type or owner changed"
            );
            let data = state(&tx, index)?;
            let current = MailboxState::decode(&data).context("invalid Intercell mailbox state")?;
            let mut predecessor = None;
            for (input_index, input) in raw.inputs().into_iter().enumerate() {
                let out_point: OutPoint = input.previous_output().into();
                let (previous_tx, _, previous_block) = history.read(&out_point.tx_hash)?;
                let output_index = out_point.index.value() as usize;
                let spent = previous_tx
                    .raw()
                    .outputs()
                    .get(output_index)
                    .context("missing committed Intercell transaction input")?;
                if spent.type_().to_opt().as_ref() == Some(&script) {
                    predecessor = Some((input_index, previous_tx, previous_block, output_index));
                    break;
                }
            }
            let Some((input_index, previous_tx, previous_block, output_index)) = predecessor else {
                let first = raw
                    .inputs()
                    .get(0)
                    .context("missing Intercell mailbox creation input")?;
                let mut hash = ckb_hash::new_blake2b();
                hash.update(first.as_slice());
                hash.update(&(index as u64).to_le_bytes());
                let mut type_id = [0; 32];
                hash.finalize(&mut type_id);
                ensure!(
                    type_id == policy.type_id
                        && raw
                            .header_deps()
                            .get(0)
                            .is_some_and(|h| h.as_slice() == policy.ckb_genesis)
                        && current == MailboxState::default(),
                    "invalid Intercell mailbox creation"
                );
                break;
            };

            let previous_data = state(&previous_tx, output_index)?;
            let previous = MailboxState::decode(&previous_data)
                .context("invalid preceding Intercell mailbox state")?;
            if current.outgoing.is_empty() {
                let header = node
                    .call::<HeaderView>("get_header", json!([block]))?
                    .into();
                let hash: H256 = raw.calc_tx_hash().unpack();
                let proof: TransactionAndWitnessProof =
                    node.call("get_transaction_and_witness_proof", json!([[hash], block]))?;
                stem::l1_proof::verify_transaction(
                    &block.pack(),
                    &header,
                    &tx.clone().into_view(),
                    &proof,
                )
                .context("Intercell recovery requires the committed CKB delivery witness")?;
                let witness = tx
                    .witnesses()
                    .get(input_index)
                    .context("missing historical Intercell delivery witness")?;
                let witness = WitnessArgs::from_slice(&witness.raw_data())?;
                let evidence = witness
                    .input_type()
                    .to_opt()
                    .context("missing historical Intercell input_type")?
                    .raw_data();
                let delivery =
                    Delivery::decode(&evidence).context("invalid historical Intercell witness")?;
                let message = Message::decode(delivery.record)
                    .context("invalid historical Intercell delivery")?;
                let id = message.id();
                ensure!(
                    message.source == policy.stem_network
                        && message.destination == policy.ckb_genesis
                        && mailbox.as_slice() == message.receiver
                        && current.next_nonce == previous.next_nonce
                        && consume(&message.receiver, &id, &previous.consumed, delivery.replay)?
                            == current.consumed,
                    "historical Intercell delivery does not match the replay root"
                );
                consumed.push(H256::from(id));
            } else {
                let message = Message::decode(current.outgoing)
                    .context("invalid historical Intercell send")?;
                ensure!(
                    message.source == policy.ckb_genesis
                        && message.destination == policy.stem_network
                        && mailbox.as_slice() == message.sender
                        && message.nonce == previous.next_nonce
                        && current.consumed == previous.consumed,
                    "historical Intercell send changed the replay root or skipped a nonce"
                );
            }
            transitions += 1;
            tx = previous_tx;
            block = previous_block;
            index = output_index;
        }
        node.live_cell(&head)
            .context("Intercell head changed during recovery")?;
        ensure!(
            node.block_hash(l1_height)? == l1_block,
            "Intercell recovery head was reorganized"
        );
        consumed.reverse();
        print_json(&Recovered {
            mailbox: self.mailbox,
            head,
            l1_height,
            l1_block,
            expected_root: H256::from(expected_root),
            consumed,
            transitions,
            transaction_reads: self.max_transactions - history.remaining,
        })
    }
}
