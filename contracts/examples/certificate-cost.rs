//! RISC-V cycle fixture. The committee and its registrations are trusted inputs.
#![no_main]
#![no_std]

use alloc::vec::Vec;
use bls12_381::{
    G1Affine, G1Projective, G2Affine, G2Prepared, G2Projective,
    hash_to_curve::{ExpandMsgXmd, HashToCurve},
    multi_miller_loop,
};
use ckb_std::{default_alloc, entry, env::argv};
use stem_stake::{Authority, MAX_RECORDS, Member, compact::SignerBitmap};

entry!(main);
default_alloc!();

const BLS_VOTE_DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

fn main() -> i8 {
    if verify() == Some(()) { 0 } else { 1 }
}

fn verify() -> Option<()> {
    let [mode, message, committee, bitmap, signatures] = argv() else {
        return None;
    };
    let message = decode(message.to_bytes())?;
    if message.len() != 32 {
        return None;
    }
    let committee = decode(committee.to_bytes())?;
    let bitmap = decode(bitmap.to_bytes())?;
    let signatures = decode(signatures.to_bytes())?;
    match mode.to_bytes() {
        b"ed" => verify_ed(&message, &committee, &bitmap, &signatures),
        b"bls" => verify_bls(&message, &committee, &bitmap, &signatures),
        _ => None,
    }
}

fn verify_ed(message: &[u8], committee: &[u8], bitmap: &[u8], signatures: &[u8]) -> Option<()> {
    let (committee, committee_rest) = committee.as_chunks::<40>();
    let (signatures, signature_rest) = signatures.as_chunks::<64>();
    if !committee_rest.is_empty() || !signature_rest.is_empty() {
        return None;
    }
    let members: Vec<_> = committee
        .iter()
        .map(|entry| Member {
            key: entry[..32].try_into().unwrap(),
            power: u64::from_le_bytes(entry[32..].try_into().unwrap()),
        })
        .collect();
    Authority::new([1; 32], &members)
        .ok()?
        .verify_compact(message, bitmap, signatures)
        .ok()
}

fn verify_bls(message: &[u8], committee: &[u8], bitmap: &[u8], signatures: &[u8]) -> Option<()> {
    let (members, rest) = committee.as_chunks::<56>();
    if !rest.is_empty() {
        return None;
    }
    let bitmap = SignerBitmap::new(members.len(), bitmap).ok()?;
    let power = |member: &[u8; 56]| u64::from_le_bytes(member[48..].try_into().unwrap());
    let total = members
        .iter()
        .try_fold(0u64, |sum, member| sum.checked_add(power(member)))?;
    let mut selected = 0u64;
    let mut public = G1Projective::identity();
    for index in bitmap.indices() {
        let member = &members[index];
        let key =
            Option::<G1Affine>::from(G1Affine::from_compressed(member[..48].try_into().ok()?))?;
        if bool::from(key.is_identity()) {
            return None;
        }
        public += key;
        selected = selected.checked_add(power(member))?;
    }
    if 3 * u128::from(selected) <= 2 * u128::from(total) || bool::from(public.is_identity()) {
        return None;
    }
    let signature =
        Option::<G2Affine>::from(G2Affine::from_compressed(signatures.try_into().ok()?))?;
    if bool::from(signature.is_identity()) {
        return None;
    }
    let point = <G2Projective as HashToCurve<ExpandMsgXmd<sha2::Sha256>>>::hash_to_curve(
        message,
        BLS_VOTE_DST,
    );
    let message = G2Prepared::from(G2Affine::from(point));
    let signature = G2Prepared::from(signature);
    let public = G1Affine::from(public);
    let negative = -G1Affine::generator();
    let result = multi_miller_loop(&[(&public, &message), (&negative, &signature)]);
    (result.final_exponentiation() == bls12_381::Gt::identity()).then_some(())
}

fn decode(hex: &[u8]) -> Option<Vec<u8>> {
    let (pairs, rest) = hex.as_chunks::<2>();
    if !rest.is_empty() || pairs.len() > MAX_RECORDS * 64 {
        return None;
    }
    let digit = |digit| match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    };
    pairs
        .iter()
        .map(|pair| Some((digit(pair[0])? << 4) | digit(pair[1])?))
        .collect()
}
