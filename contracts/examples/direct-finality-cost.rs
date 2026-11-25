//! RISC-V measurement of the direct finality verifier, without CKB transaction plumbing.
#![no_main]
#![no_std]

use alloc::vec::Vec;
use ckb_std::{default_alloc, entry, env::argv};
use stem_stake::finality::{self, Trust};

entry!(main);
default_alloc!();

fn main() -> i8 {
    if verify().is_some() { 0 } else { 1 }
}

fn verify() -> Option<()> {
    let [committee, app, proof, expected_tip] = argv() else {
        return None;
    };
    let trust = Trust {
        network: [5; 32],
        chain_id: "stem-direct-cost",
        initial_committee: bytes(committee.to_bytes())?.try_into().ok()?,
        genesis_app_hash: bytes(app.to_bytes())?.try_into().ok()?,
    };
    let proof = bytes(proof.to_bytes())?;
    let result = finality::verify(&trust, None, &proof).ok()?;
    (result.encode() == bytes(expected_tip.to_bytes())?).then_some(())
}

fn bytes(hex: &[u8]) -> Option<Vec<u8>> {
    let (pairs, remainder) = hex.as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let nibble = |byte| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    pairs
        .iter()
        .map(|&[a, b]| Some(nibble(a)? << 4 | nibble(b)?))
        .collect()
}
