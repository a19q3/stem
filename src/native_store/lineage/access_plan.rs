//! Cell accesses required before materializing a Lineage command.

use std::collections::BTreeMap;

use anyhow::Result;
use ckb_types::packed::{Byte32, OutPoint};
use stem_ledger::{Transaction, lineage::Head};

#[derive(Default)]
struct Access {
    read: usize,
    write: usize,
}

/// Earliest accepted prefix length for each candidate within the window.
pub(in crate::native_store) fn plan(
    transactions: &[Transaction],
    network: &Byte32,
    mut eligible: impl FnMut(&Transaction) -> bool,
    mut load_head: impl FnMut(&Byte32) -> Result<Option<Head>>,
) -> Vec<usize> {
    let mut positions = Vec::with_capacity(transactions.len());
    let mut heads: BTreeMap<Byte32, Vec<OutPoint>> = BTreeMap::new();
    let mut accesses: BTreeMap<OutPoint, Access> = BTreeMap::new();
    'planning: for (index, tx) in transactions.iter().enumerate() {
        let Some(command) = tx.lineage_command() else {
            break;
        };
        if tx.lineage_batch().is_some()
            || tx
                .lineage_intent()
                .is_some_and(|intent| intent.batch.is_some())
            || !eligible(tx)
        {
            break;
        }
        let mut reads = Vec::with_capacity(tx.cell_deps().len());
        for dep in tx.cell_deps() {
            // A Dep Group can read a member which is also written in the
            // proposed prefix. Its expansion is left to ordered staging.
            if dep.dep_type().as_slice() != [0] {
                break 'planning;
            }
            reads.push(dep.out_point());
        }
        let id = tx.id(network);
        let mut writes = vec![
            tx.input(0)
                .expect("validated Lineage fee input")
                .previous_output,
            OutPoint::new(id.clone(), 0),
        ];
        for (output, target) in command.targets.iter().enumerate() {
            let target_id = target.reference.id();
            let points = match heads.entry(target_id.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let Ok(Some(head)) = load_head(target_id) else {
                        // Resolve missing or damaged state at its ordered
                        // position so earlier block errors retain priority.
                        break 'planning;
                    };
                    entry.insert(vec![head.out_point])
                }
            };
            // A prior command can leave the old Head live through fee-only
            // rollback. All its possible successors must remain in the set.
            writes.extend(points.iter().cloned());
            let successor = OutPoint::new(id.clone(), output as u32 + 1);
            writes.push(successor.clone());
            points.push(successor);
        }

        let read_after_write = reads
            .iter()
            .filter_map(|point| accesses.get(point).map(|access| access.write))
            .max()
            .unwrap_or_default();
        let conflicting_write = writes
            .iter()
            .filter_map(|point| {
                accesses
                    .get(point)
                    .map(|access| access.read.max(access.write))
            })
            .max()
            .unwrap_or_default();
        positions.push(read_after_write.max(conflicting_write));
        for point in reads {
            accesses.entry(point).or_default().read = index + 1;
        }
        for point in writes {
            accesses.entry(point).or_default().write = index + 1;
        }
    }
    positions
}
