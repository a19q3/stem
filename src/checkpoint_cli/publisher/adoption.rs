use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{JsonBytes, Uint64};
use ckb_types::{
    H256,
    packed::{self, CellInput, WitnessArgs},
    prelude::*,
};
use prost::Message;
use serde_json::{Value, json};
use stem_stake::{jolteon::CommitteeChange, settlement::ckb as jolteon};

use super::super::next_jolteon_roster;
use super::{Confirmed, Publisher, State, out_point};

pub(super) const MAX_ADOPTED_UPDATES: usize = 64;

impl Publisher {
    pub(super) fn adopt(&self, state: &mut State, hashes: &[H256]) -> Result<Value> {
        ensure!(
            (1..=MAX_ADOPTED_UPDATES).contains(&hashes.len()),
            "adopt between 1 and {MAX_ADOPTED_UPDATES} checkpoint transactions"
        );
        self.validate_anchor(state.confirmed.as_ref())?;
        let pending_wallet_tx = state
            .pending
            .as_ref()
            .filter(|pending| !pending.uses_fund)
            .map(|pending| {
                packed::Transaction::from_slice(pending.signed.as_bytes())
                    .map(|tx| H256::from(tx.into_view().hash()))
            })
            .transpose()?;
        let tip_height: Uint64 = self.node.call("get_tip_block_number", json!([]))?;
        let mut adopted = 0;
        let mut wallet_change = self.retained_wallet(state.confirmed.as_ref())?;
        let mut latest = None;
        for (index, hash) in hashes.iter().enumerate() {
            let previous = latest.as_ref().or(state.confirmed.as_ref());
            if index == 0 && previous.is_some_and(|entry| &entry.transaction == hash) {
                continue;
            }
            let (raw, height, block) = self.node.committed(hash)?;
            ensure!(
                tip_height
                    .value()
                    .checked_sub(height)
                    .is_some_and(|depth| depth >= self.config.confirmations)
                    && self.node.block_hash(height)? == block,
                "checkpoint is not sufficiently confirmed on the canonical CKB chain"
            );
            ensure!(
                previous.is_none_or(|entry| entry.l1_height <= height),
                "checkpoint inclusion height regressed"
            );
            let tx = packed::Transaction::from_slice(raw.as_bytes())?.into_view();
            let mut group = tx.outputs().into_iter().enumerate().filter(|(_, output)| {
                output.type_().to_opt().as_ref() == Some(&self.checkpoint_type)
            });
            let (output_index, output) = group
                .next()
                .context("transaction lacks the pinned checkpoint Type Script")?;
            ensure!(
                group.next().is_none() && output.lock() == self.checkpoint_lock,
                "invalid pinned checkpoint output"
            );
            let expected_input =
                previous.map_or_else(|| self.config.seed.clone(), Confirmed::point);
            let expected_input = packed::OutPoint::from(expected_input);
            let witness_index = if previous.is_some() {
                tx.inputs()
                    .into_iter()
                    .position(|input| input.previous_output() == expected_input)
                    .context("checkpoint does not spend the preceding adopted OutPoint")?
            } else {
                ensure!(
                    output_index == 0
                        && tx
                            .inputs()
                            .get(0)
                            .is_some_and(|input| input == CellInput::new(expected_input, 0)),
                    "checkpoint does not bootstrap from the pinned seed"
                );
                0
            };
            let witness = tx
                .witnesses()
                .get(witness_index)
                .context("missing checkpoint witness")?;
            let witness = WitnessArgs::from_slice(&witness.raw_data())?;
            let evidence = witness
                .input_type()
                .to_opt()
                .context("missing checkpoint evidence")?
                .raw_data();
            let old_tip = previous
                .map(|entry| jolteon::Tip::decode(entry.tip.as_bytes()))
                .transpose()?;
            let tip = jolteon::verify(&self.policy, old_tip.as_ref(), &evidence)?;
            let output_data = tx
                .outputs_data()
                .get(output_index)
                .context("missing checkpoint output data")?
                .raw_data();
            ensure!(
                output_data.as_ref() == tip.encode(),
                "checkpoint output differs from its proof"
            );
            let advance = jolteon::Advance::decode(evidence.as_ref())?;
            let roster = next_jolteon_roster(advance)?;
            ensure!(
                jolteon::roster_id(&roster) == tip.roster,
                "adopted roster differs from the checkpoint output"
            );
            if pending_wallet_tx.as_ref() == Some(hash) {
                let change = tx
                    .outputs()
                    .get(1)
                    .context("missing journaled wallet change")?;
                let data = tx
                    .outputs_data()
                    .get(1)
                    .context("missing wallet change data")?
                    .raw_data();
                ensure!(
                    self.is_fee_output(&change, &data),
                    "invalid journaled wallet change"
                );
                wallet_change = Some(out_point(hash, 1));
            }
            latest = Some(Confirmed {
                transaction: hash.clone(),
                output_index: u32::try_from(output_index)?,
                l1_height: height,
                l1_block: block,
                tip: JsonBytes::from_bytes(output_data),
                roster: JsonBytes::from_vec(roster),
                wallet_change: wallet_change.clone(),
            });
            adopted += 1;
        }
        let entry = latest
            .as_ref()
            .or(state.confirmed.as_ref())
            .context("no confirmed checkpoint to adopt")?;
        let current: Uint64 = self.node.call("get_tip_block_number", json!([]))?;
        ensure!(
            current
                .value()
                .checked_sub(entry.l1_height)
                .is_some_and(|depth| depth >= self.config.confirmations),
            "checkpoint confirmation depth changed during adoption"
        );
        self.validate_anchor(state.confirmed.as_ref())?;
        self.validate_anchor(Some(entry))?;
        let result = json!({
            "status": "adopted", "updates": adopted, "transaction": entry.transaction,
            "checkpoint": entry.point(), "l1_height": entry.l1_height, "l1_block": entry.l1_block,
            "stem_height": jolteon::Tip::decode(entry.tip.as_bytes())?.height(),
            "next_epoch": CommitteeChange::decode(entry.roster.as_bytes())?.epoch
        });
        if let Some(last) = latest {
            state.confirmed = Some(last);
            // This sequence either includes the pending transaction or spends
            // its checkpoint input. The wallet follows its own confirmed change.
            state.pending = None;
            self.journal.write_record(state)?;
        }
        Ok(result)
    }
}
