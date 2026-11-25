//! Measures native two-chain proofs inside CKB-VM.
#![no_main]
#![no_std]

use alloc::vec::Vec;
use ckb_std::{default_alloc, entry, env::argv};
use stem_stake::{
    Member,
    jolteon::{Committee, MAX_MEMBERS},
};

entry!(main);
default_alloc!();

fn main() -> i8 {
    if verify().is_some() { 0 } else { 1 }
}

fn verify() -> Option<()> {
    let [network, genesis, members, proof, expected_header] = argv() else {
        return None;
    };
    let network = unhex(network.to_bytes())?.try_into().ok()?;
    let genesis = unhex(genesis.to_bytes())?.try_into().ok()?;
    let members = unhex(members.to_bytes())?;
    let (records, remainder) = members.as_chunks::<40>();
    if !remainder.is_empty() || !(1..=MAX_MEMBERS).contains(&records.len()) {
        return None;
    }
    let members = records
        .iter()
        .map(|raw| Member {
            key: raw[..32].try_into().unwrap(),
            power: u64::from_le_bytes(raw[32..].try_into().unwrap()),
        })
        .collect::<Vec<_>>();
    let committee = Committee::new(network, genesis, 0, &members).ok()?;
    let finalized = committee.verify_finality(&unhex(proof.to_bytes())?).ok()?;
    (finalized.as_bytes().as_slice() == unhex(expected_header.to_bytes())?).then_some(())
}

fn unhex(raw: &[u8]) -> Option<Vec<u8>> {
    let (pairs, remainder) = raw.as_chunks::<2>();
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
