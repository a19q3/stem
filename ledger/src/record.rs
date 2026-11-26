//! Canonical Live Cell records and their Patricia proof binding.

use anyhow::{Context, Result};
use ckb_types::{packed::OutPoint, prelude::Entity};

use crate::{
    Cell, LiveCell, Origin,
    patricia::{Key, Patricia, Proof},
};

fn cell_parts<'a>(
    point: &'a OutPoint,
    origin: &'a [u8; Origin::LEN],
    cell: &'a Cell,
) -> [&'a [u8]; 3] {
    [point.as_slice(), origin, cell.as_slice()]
}

pub fn encode_cell(point: &OutPoint, record: &LiveCell) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode_cell_into(point, record, &mut bytes);
    bytes
}

/// Replace a scratch buffer with the canonical Live Cell record.
pub fn encode_cell_into(point: &OutPoint, record: &LiveCell, bytes: &mut Vec<u8>) {
    let origin = record.origin.encode();
    let parts = cell_parts(point, &origin, &record.cell);
    bytes.clear();
    bytes.reserve(parts.iter().map(|part| part.len()).sum());
    for part in parts {
        bytes.extend_from_slice(part);
    }
}

pub fn decode_cell(bytes: &[u8]) -> Result<(OutPoint, LiveCell)> {
    let (point, rest) = bytes
        .split_at_checked(OutPoint::TOTAL_SIZE)
        .context("truncated Cell OutPoint")?;
    let (origin, cell) = rest
        .split_at_checked(Origin::LEN)
        .context("truncated Cell origin")?;
    Ok((
        OutPoint::from_slice(point)?,
        LiveCell {
            cell: Cell::from_slice(cell)?,
            origin: Origin::decode(origin)?,
        },
    ))
}

pub fn cell_key(tree: &Patricia, point: &OutPoint) -> Key {
    tree.record_key(point.as_slice())
}

pub fn cell_value(tree: &Patricia, point: &OutPoint, record: &LiveCell) -> Key {
    tree.value_hash_parts(&cell_parts(point, &record.origin.encode(), &record.cell))
}

pub fn verify_cell(
    tree: &Patricia,
    root: &Key,
    point: &OutPoint,
    record: Option<&LiveCell>,
    proof: &Proof,
) -> bool {
    let value = record.map(|record| cell_value(tree, point, record));
    tree.verify(root, &cell_key(tree, point), value.as_ref(), proof)
}

#[cfg(test)]
mod tests {
    use ckb_types::{
        bytes::Bytes,
        core::ScriptHashType,
        packed::{Byte32, Script},
        prelude::*,
    };

    use super::*;
    use crate::patricia::Namespace;

    #[test]
    fn streamed_cell_value_matches_serialized_records() -> Result<()> {
        let tree = Patricia::new(Byte32::from_slice(&[9; 32])?, Namespace::Cells);
        let point = OutPoint::new([5; 32].pack(), 12);
        let lock = Script::new_builder()
            .code_hash([7; 32])
            .hash_type(ScriptHashType::Data)
            .build();
        let origin = Origin {
            height: 5,
            timestamp_ms: 2_048,
            context_id: [3; 32].pack(),
        };
        let mut scratch = Vec::new();
        for size in [0, 127, 128, 129, 16_384, 1] {
            let cell = Cell::new(
                1_000_000_000_000,
                lock.clone(),
                vec![],
                Bytes::from(vec![4; size]),
            )?;
            let record = LiveCell {
                cell,
                origin: origin.clone(),
            };
            // Verify the original fixed field order and exact record bytes.
            let canonical = [point.as_slice(), &origin.encode(), record.cell.as_slice()].concat();
            encode_cell_into(&point, &record, &mut scratch);
            assert_eq!(scratch, canonical);
            assert_eq!(
                cell_value(&tree, &point, &record),
                tree.value_hash(&canonical)
            );
            let (reopened_point, reopened) = decode_cell(&scratch)?;
            assert_eq!(reopened_point, point);
            assert_eq!(reopened.origin, origin);
            assert_eq!(reopened.cell.as_slice(), record.cell.as_slice());
        }
        Ok(())
    }
}
