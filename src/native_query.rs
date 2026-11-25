//! Read-only queries shared by the Stem consensus entry points.

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::Entity,
};
use stem_ledger::{proof::Tree, record};

use crate::native_store::Store;

pub(crate) fn query(
    store: &Store,
    path: &str,
    data: &[u8],
    height: u64,
    prove: bool,
) -> Result<(u64, Bytes)> {
    let tip = store.tip().context.height;
    let requested = if height == 0 { tip } else { height };
    let value = match path {
        "/stem/lineage" if prove => {
            ensure!(
                requested == tip,
                "Lineage proof requires the current height"
            );
            let id = Byte32::from_slice(data)?;
            let lineage = store.prove_current_lineage(&id)?;
            let (head, cell, cell_path) = if let Some((head, cell, path)) = lineage.head {
                let encoded = record::encode_cell(&head.out_point, &cell);
                (Some(head.encode()), Some(encoded), Some(path.encode()?))
            } else {
                (None, None, None)
            };
            let hex = |bytes: &[u8]| format!("0x{}", hex::encode(bytes));
            serde_json::to_vec(&serde_json::json!({
                "record": head.as_deref().map(hex),
                "proof": hex(&lineage.system_path.encode()?),
                "cell": cell.as_deref().map(hex),
                "cell_proof": cell_path.as_deref().map(hex),
            }))?
            .into()
        }
        "/stem/keeper" if prove => {
            ensure!(requested == tip, "Keeper proof requires the current height");
            let height = u64::from_le_bytes(data.try_into()?);
            ensure!(height > 0, "invalid Keeper height");
            let (queue, proof) = store.prove_keeper_queue(height)?;
            let hex = |bytes: &[u8]| format!("0x{}", hex::encode(bytes));
            serde_json::to_vec(&serde_json::json!({
                "execute_at": height,
                "record": queue.as_deref().map(hex),
                "proof": hex(&proof.encode()?),
            }))?
            .into()
        }
        "/stem/header" if !prove => store
            .header(requested)?
            .context("Header outside local retention window")?
            .encode(),
        "/stem/stake-retirement" if prove => {
            ensure!(
                requested == tip,
                "Stake retirement queries require the current height"
            );
            let validator = data.try_into()?;
            let (height, proof) = store.prove_stake_retirement(validator)?;
            serde_json::to_vec(&serde_json::json!({
                "effective_height": height,
                "proof": format!("0x{}", hex::encode(proof.encode()?)),
            }))?
            .into()
        }
        "/stem/stake-epoch-end" if prove => {
            ensure!(
                requested == tip,
                "Stake epoch proof requires the current height"
            );
            let epoch = u64::from_le_bytes(data.try_into()?);
            let (end_height, proof) = store.prove_stake_epoch_end(epoch)?;
            serde_json::to_vec(&serde_json::json!({
                "end_height": end_height, "proof": format!("0x{}", hex::encode(proof.encode()?)),
            }))?
            .into()
        }
        "/stem/stake-registry-application" if prove && data.len() == 40 => {
            ensure!(
                requested == tip,
                "Registry application proof requires the current height"
            );
            let registry = data[..32].try_into()?;
            let sequence = u64::from_le_bytes(data[32..].try_into()?);
            let (record, proof) = store.prove_registry_application(registry, sequence)?;
            serde_json::to_vec(&serde_json::json!({
                "sequence": sequence,
                "previous_sequence": record.map(|item| item.previous_sequence),
                "effective_height": record.map(|item| item.effective_height),
                "l1_height": record.map(|item| item.l1_anchor.height),
                "l1_hash": record.map(|item| format!("0x{}", hex::encode(item.l1_anchor.hash))),
                "proof": format!("0x{}", hex::encode(proof.encode()?)),
            }))?
            .into()
        }
        "/stem/stake-reward-epoch" if prove && data.len() == 8 => {
            ensure!(
                requested == tip,
                "reward epoch proof requires the current height"
            );
            let epoch = u64::from_le_bytes(data.try_into()?);
            let (record, proof) = store.prove_reward_epoch(epoch)?;
            serde_json::to_vec(&serde_json::json!({
                "record": record.map(|item| format!("0x{}", hex::encode(item.encode()))),
                "budget": record.map(|item| ckb_jsonrpc_types::Uint64::from(item.budget)),
                "proof": format!("0x{}", hex::encode(proof.encode()?)),
            }))?
            .into()
        }
        "/stem/stake-snapshot" if prove && data.is_empty() => {
            ensure!(
                requested == tip,
                "Stake Snapshot queries require the current height"
            );
            let (snapshot, proof) = store.prove_stake_snapshot()?;
            serde_json::to_vec(&serde_json::json!({
                "snapshot": snapshot.map(hex::encode),
                "nodes": proof.nodes.iter().map(hex::encode).collect::<Vec<_>>(),
            }))?
            .into()
        }
        "/stem/extinguished-fees" if prove && data.is_empty() => {
            ensure!(
                requested == tip,
                "fee proof queries require the current height"
            );
            let (amount, proof) = store.prove_extinguished_fees()?;
            serde_json::to_vec(&serde_json::json!({
                "extinguished_shannons": amount.map(ckb_jsonrpc_types::Uint64::from),
                "proof": format!("0x{}", hex::encode(proof.encode()?)),
            }))?
            .into()
        }
        "/stem/fee-funds" if prove && data.is_empty() => {
            use stem_ledger::protocol::FeeFund;

            ensure!(
                requested == tip,
                "fee fund proofs require the current height"
            );
            let policy = store.fee_fund_policy().context("fee funds are disabled")?;
            let (fees, fee_proof) = store.prove_extinguished_fees()?;
            let (payouts, payout_proof) = store.prove_fee_payouts()?;
            let accounts = FeeFund::ALL
                .iter()
                .map(|&fund| {
                    use ckb_jsonrpc_types::Uint64;

                    let destination = &policy.funds[fund as usize];
                    let payout = payouts.fund(fund);
                    let allocated =
                        FeeFund::allocation(fees.unwrap_or_default(), destination.basis_points)?;
                    let available = allocated
                        .checked_sub(payout.queued)
                        .context("fee fund is overallocated")?;
                    Ok(serde_json::json!({
                        "id": fund as u8,
                        "name": fund.name(),
                        "basis_points": destination.basis_points,
                        "recipient": ckb_jsonrpc_types::Script::from(destination.recipient.clone()),
                        "allocated_shannons": Uint64::from(allocated),
                        "available_shannons": Uint64::from(available),
                        "queued_shannons": Uint64::from(payout.queued),
                        "paid_shannons": Uint64::from(payout.paid),
                    }))
                })
                .collect::<Result<Vec<_>>>()?;
            serde_json::to_vec(&serde_json::json!({
                "lane": format!("0x{}", hex::encode(policy.lane.as_slice())),
                "accounts": accounts,
                "extinguished_shannons": fees.map(ckb_jsonrpc_types::Uint64::from),
                "fee_proof": format!("0x{}", hex::encode(fee_proof.encode()?)),
                "policy_record": format!("0x{}", hex::encode(policy.encode())),
                "policy_proof": format!("0x{}", hex::encode(store.prove_fee_fund_policy()?.encode()?)),
                "payout_record": format!("0x{}", hex::encode(payouts.encode())),
                "payout_proof": format!("0x{}", hex::encode(payout_proof.encode()?)),
            }))?.into()
        }
        "/stem/bridge-backing" if prove && data.is_empty() => {
            ensure!(
                requested == tip,
                "bridge backing proof requires the current height"
            );
            let (backing, path) = store.prove_bridge_backing()?;
            serde_json::to_vec(&serde_json::json!({
                "record": format!("0x{}", hex::encode(backing.encode())),
                "proof": format!("0x{}", hex::encode(path.encode()?)),
                "liquid_shannons": ckb_jsonrpc_types::Uint64::from(backing.liquid()),
                "circulating_shannons": ckb_jsonrpc_types::Uint64::from(backing.circulating()),
                "pending_shannons": ckb_jsonrpc_types::Uint64::from(backing.pending()),
                "unallocated_shannons": ckb_jsonrpc_types::Uint64::from(backing.unallocated()),
            }))?
            .into()
        }
        "/stem/bridge-withdrawal" if prove => {
            ensure!(
                requested == tip,
                "bridge withdrawal proof requires the current height"
            );
            ensure!(data.len() == 40, "expected lane and withdrawal nonce");
            let lane = Byte32::from_slice(&data[..32])?;
            let nonce = u64::from_le_bytes(data[32..].try_into()?);
            let (record, proof) = store.prove_bridge_withdrawal(&lane, nonce)?;
            serde_json::to_vec(&serde_json::json!({
                "record": record.map(|record| format!("0x{}", hex::encode(record))),
                "proof": format!("0x{}", hex::encode(proof.encode()?)),
            }))?
            .into()
        }
        "/stem/bridge-lane" if prove => {
            ensure!(
                requested == tip,
                "bridge lane proof requires the current height"
            );
            let id = Byte32::from_slice(data)?;
            let ((lane, next_request), proof) = store.prove_bridge_lane(&id)?;
            serde_json::to_vec(&serde_json::json!({
                "record": format!("0x{}", hex::encode(lane.encode(next_request))),
                "proof": format!("0x{}", hex::encode(proof.encode()?)),
            }))?
            .into()
        }
        "/stem/cell" => {
            ensure!(
                requested == tip,
                "Live Cell queries require the current height"
            );
            let point = OutPoint::from_slice(data)?;
            let (cell, proof) = if prove {
                let (cell, proof) = store.prove_cell(&point)?;
                (cell, Some(format!("0x{}", hex::encode(proof.encode()?))))
            } else {
                (store.live_cell(&point)?, None)
            };
            let cell = cell
                .as_ref()
                .map(|cell| hex::encode(record::encode_cell(&point, cell)));
            serde_json::to_vec(&serde_json::json!({"cell": cell, "proof": proof}))?.into()
        }
        "/stem/transactions" if !prove && data.is_empty() => {
            let (transactions, _) = store
                .execution(requested)?
                .context("execution outside local retention window")?;
            let ids: Vec<_> = transactions
                .iter()
                .map(|tx| hex::encode(tx.id(store.network()).as_slice()))
                .collect();
            serde_json::to_vec(&ids)?.into()
        }
        "/stem/execution" => {
            let index = u32::from_le_bytes(data.try_into()?);
            let (transactions, receipts) = store
                .execution(requested)?
                .context("execution outside local retention window")?;
            let tx = transactions
                .get(index as usize)
                .context("transaction index out of range")?;
            let receipt = receipts
                .get(index as usize)
                .context("receipt index out of range")?;
            let proof = if prove {
                let tx =
                    Tree::transactions(store.network().clone(), &transactions)?.prove(index)?;
                let receipt = Tree::receipts(store.network().clone(), &receipts)?.prove(index)?;
                Some(format!(
                    "0x{}",
                    hex::encode(tx.encode_execution_pair(&receipt)?)
                ))
            } else {
                None
            };
            serde_json::to_vec(&serde_json::json!({
                "transaction": hex::encode(tx.as_slice()), "receipt": hex::encode(receipt.encode()),
                "proof": proof,
            }))?
            .into()
        }
        _ => anyhow::bail!("unsupported native query"),
    };
    Ok((requested, value))
}
