//! Canonical Live Cell records and their Patricia proof binding.

use anyhow::{Context, Result};
use ckb_types::{packed::OutPoint, prelude::Entity};

use crate::{
    Cell, LiveCell, Origin,
    patricia::{Key, Patricia, Proof},
};

pub fn encode_cell(point: &OutPoint, record: &LiveCell) -> Vec<u8> {
    let mut bytes =
        Vec::with_capacity(OutPoint::TOTAL_SIZE + Origin::LEN + record.cell.as_slice().len());
    bytes.extend_from_slice(point.as_slice());
    bytes.extend_from_slice(&record.origin.encode());
    bytes.extend_from_slice(record.cell.as_slice());
    bytes
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
    tree.value_hash(&encode_cell(point, record))
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
