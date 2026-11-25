use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{CellWithStatus, JsonBytes, Status, Uint32, Uint64};
use ckb_types::{H256, packed, prelude::*};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Confirmed, Publisher, State, adoption::MAX_ADOPTED_UPDATES};

#[derive(Deserialize)]
struct IndexerTip {
    block_hash: H256,
    block_number: Uint64,
}

#[derive(Deserialize)]
struct IndexedTransaction {
    tx_hash: H256,
    block_number: Uint64,
    tx_index: Uint32,
}

#[derive(Deserialize)]
struct IndexedPage {
    objects: Vec<IndexedTransaction>,
    last_cursor: JsonBytes,
}

impl Publisher {
    pub(super) fn follow(&self, state: &mut State) -> Result<Option<Value>> {
        let previous = state.confirmed.as_ref();
        let tracked = previous.map_or_else(|| self.config.seed.clone(), Confirmed::point);
        let cell: CellWithStatus = self
            .node
            .call("get_live_cell", json!([tracked, false, false]))?;
        if cell.status == "live" {
            return Ok(None);
        }

        if let Some(pending) = &state.pending {
            let tx = packed::Transaction::from_slice(pending.signed.as_bytes())?.into_view();
            if self.transaction_status(&tx.hash().into(), true)?.status == Status::Committed {
                // Keep the locally written transaction's original confirmation
                // result, including its funding information.
                return Ok(None);
            }
        }

        let tip: Uint64 = self.node.call("get_tip_block_number", json!([]))?;
        let mature = tip.value().checked_sub(self.config.confirmations);
        let indexed: Option<IndexerTip> = self
            .node
            .call("get_indexer_tip", json!([]))
            .context("auto_follow requires the CKB Indexer RPC")?;
        let waiting = || {
            Some(json!({
                "status": "awaiting_successor", "checkpoint": tracked,
                "confirmed_l1_height": mature,
                "indexed_l1_height": indexed.as_ref().map(|tip| tip.block_number.value()),
            }))
        };
        let (Some(mature), Some(indexed)) = (mature, indexed.as_ref()) else {
            return Ok(waiting());
        };
        let indexed_height = indexed.block_number.value();
        ensure!(
            self.node.block_hash(indexed_height)? == indexed.block_hash,
            "CKB Indexer tip differs from the canonical chain"
        );
        let hashes = self.discover_successors(previous, mature.min(indexed_height))?;
        ensure!(
            self.node.block_hash(indexed_height)? == indexed.block_hash,
            "CKB Indexer tip was reorganized during discovery"
        );
        if hashes.is_empty() {
            return Ok(waiting());
        }
        Ok(Some(self.adopt(state, &hashes)?))
    }

    fn discover_successors(&self, previous: Option<&Confirmed>, through: u64) -> Result<Vec<H256>> {
        let (start, passed) = if let Some(previous) = previous {
            let status = self.transaction_status(&previous.transaction, true)?;
            ensure!(
                status.status == Status::Committed
                    && status.block_hash.as_ref() == Some(&previous.l1_block)
                    && status.block_number.map(|height| height.value()) == Some(previous.l1_height),
                "checkpoint position disagrees with the pinned L1 anchor"
            );
            let index = status
                .tx_index
                .context("missing checkpoint transaction index")?;
            (
                previous.l1_height,
                Some((previous.l1_height, index.value())),
            )
        } else {
            let (_, height, block) = self.node.committed(&self.config.seed.tx_hash)?;
            ensure!(
                self.node.block_hash(height)? == block,
                "checkpoint seed is outside the canonical L1 chain"
            );
            (height, None)
        };
        if start > through {
            return Ok(vec![]);
        }
        let end = through.checked_add(1).context("L1 height overflow")?;
        let script = ckb_jsonrpc_types::Script::from(self.checkpoint_type.clone());
        let query = json!({
            "script": script, "script_type": "type", "script_search_mode": "exact",
            "group_by_transaction": true, "filter": {"block_range": [Uint64::from(start), Uint64::from(end)]},
        });
        let mut hashes = Vec::new();
        let mut cursor: Option<JsonBytes> = None;
        let mut last_position = None;
        // One grouped entry per transaction, regardless of the number of Cell matches.
        let page_size = MAX_ADOPTED_UPDATES;
        while hashes.len() < MAX_ADOPTED_UPDATES {
            let page: IndexedPage = self.node.call(
                "get_transactions",
                json!([query, "asc", Uint32::from(page_size as u32), cursor]),
            )?;
            let count = page.objects.len();
            ensure!(
                count <= page_size,
                "CKB Indexer exceeded the requested page size"
            );
            for entry in page.objects {
                let position = (entry.block_number.value(), entry.tx_index.value());
                ensure!(
                    (start..end).contains(&position.0)
                        && last_position.is_none_or(|last| last < position),
                    "CKB Indexer returned an unordered checkpoint page"
                );
                last_position = Some(position);
                if passed.is_none_or(|position_to_skip| position > position_to_skip) {
                    hashes.push(entry.tx_hash);
                    if hashes.len() == MAX_ADOPTED_UPDATES {
                        break;
                    }
                }
            }
            if count < page_size || hashes.len() == MAX_ADOPTED_UPDATES {
                break;
            }
            ensure!(
                cursor.as_ref() != Some(&page.last_cursor)
                    && !page.last_cursor.as_bytes().is_empty(),
                "CKB Indexer cursor did not advance"
            );
            cursor = Some(page.last_cursor);
        }
        Ok(hashes)
    }
}
