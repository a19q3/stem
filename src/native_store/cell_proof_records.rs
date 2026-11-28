//! Fetch Cell records in small batches and bind each unique record to its proof.

use std::collections::HashMap;

use anyhow::{Context, Result, ensure};
use ckb_db::internal::prelude::*;
use ckb_types::packed::OutPoint;
use stem_ledger::{
    LiveCell,
    patricia::{Key, Namespace, Proof},
};

use super::{STATE_RECORD, StateReadFault, Store, state_key};

const RECORD_READ_WIDTH: usize = 8;
const MIN_MULTI_GET: usize = 4;

struct Query<'a> {
    point: &'a OutPoint,
    id: &'a Key,
    expected: Option<Key>,
}

impl Store {
    pub(super) fn bind_cell_proof_records(
        &self,
        points: &[OutPoint],
        keys: &[Key],
        proofs: Vec<(Option<Key>, Proof)>,
    ) -> Result<Vec<(Option<LiveCell>, Proof)>> {
        let mut index = HashMap::with_capacity(points.len());
        let mut queries: Vec<Query<'_>> = Vec::with_capacity(points.len());
        let mut order = Vec::with_capacity(points.len());
        for ((point, id), (expected, proof)) in points.iter().zip(keys).zip(proofs) {
            let position = *index.entry(point).or_insert_with(|| {
                let position = queries.len();
                queries.push(Query {
                    point,
                    id,
                    expected,
                });
                position
            });
            ensure!(
                queries[position].expected == expected,
                "duplicate Cell proof has a different authenticated value"
            );
            order.push((position, proof));
        }
        let mut records = vec![None; queries.len()];
        let mut misses = Vec::with_capacity(RECORD_READ_WIDTH);
        for (queries, records) in queries
            .chunks(RECORD_READ_WIDTH)
            .zip(records.chunks_mut(RECORD_READ_WIDTH))
        {
            for (position, query) in queries.iter().enumerate() {
                if let Some(record) = self.live_cell_in_memory(query.point)? {
                    self.verify_live_cell_for_proof(query.point, query.expected, record.as_ref())?;
                    records[position] = record;
                } else {
                    misses.push(position);
                }
            }
            let keys = misses
                .iter()
                .map(|&position| state_key(STATE_RECORD, Namespace::Cells, queries[position].id));
            let values = if misses.len() < MIN_MULTI_GET {
                keys.map(|key| self.db.get_opt(key, &self.read_options))
                    .collect()
            } else {
                self.db.multi_get_opt(keys, &self.read_options)
            };
            ensure!(values.len() == misses.len(), "wrong number of Cell records");
            for (position, bytes) in misses.drain(..).zip(values) {
                let query = &queries[position];
                let record = bytes
                    .context(StateReadFault)?
                    .map(|bytes| Self::decode_live_cell_value(query.point, bytes))
                    .transpose()?;
                self.verify_live_cell_for_proof(query.point, query.expected, record.as_ref())?;
                self.remember_committed_live_cell(query.point, record.as_ref());
                records[position] = record;
            }
        }
        Ok(order
            .into_iter()
            .map(|(position, proof)| (records[position].clone(), proof))
            .collect())
    }
}
