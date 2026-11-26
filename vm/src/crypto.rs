use std::{cell::RefCell, sync::OnceLock};

use ckb_script::runtime::SUCCESS;
use ckb_vm::{
    Error, Register, SupportMachine, Syscalls,
    memory::Memory,
    registers::{A0, A1, A2, A3, A7},
};
use lru::LruCache;
use secp256k1::{
    Message, Secp256k1, VerifyOnly,
    ecdsa::{RecoverableSignature, RecoveryId},
};

pub const SECP256K1_RECOVER: u64 = 2121;
pub const SECP256K1_RECOVERY_CYCLES: u64 = 1_900_000;
const INVALID_SIGNATURE: u8 = 4;

const RECOVERY_CACHE_ENTRIES: usize = 512;
type RecoveryKey = ([u8; 65], [u8; 32]);

thread_local! {
    static RECOVERED: RefCell<LruCache<RecoveryKey, [u8; 33]>> =
        RefCell::new(LruCache::new(RECOVERY_CACHE_ENTRIES));
}

pub const CRYPTO_V1: u64 = 2131;
pub const CRYPTO_BASE_CYCLES: u64 = 25_000;
pub const HASH_CYCLES_PER_BYTE: u64 = 128;
const SECP256K1_VERIFY_CYCLES: u64 = 2_200_000;
const ED25519_VERIFY_CYCLES: u64 = 3_200_000;
pub const MAX_BATCH: u64 = 64;
pub const MAX_HASH_BYTES: u64 = 1 << 20;

pub(crate) struct Recover;
pub(crate) struct V1;

impl<M: SupportMachine> Syscalls<M> for Recover {
    fn initialize(&mut self, _: &mut M) -> Result<(), Error> {
        Ok(())
    }

    fn ecall(&mut self, machine: &mut M) -> Result<bool, Error> {
        if machine.registers()[A7].to_u64() != SECP256K1_RECOVER {
            return Ok(false);
        }
        let signature = machine.registers()[A0].to_u64();
        let digest = machine.registers()[A1].to_u64();
        let destination = machine.registers()[A2].to_u64();
        machine.add_cycles(SECP256K1_RECOVERY_CYCLES)?;
        let signature = machine.memory_mut().load_bytes(signature, 65)?;
        let digest = machine.memory_mut().load_bytes(digest, 32)?;
        let signature = signature.as_ref().try_into().expect("65-byte signature");
        let digest = digest.as_ref().try_into().expect("32-byte digest");
        let status = match recover_cached(signature, digest) {
            Some(key) => {
                machine.memory_mut().store_bytes(destination, &key)?;
                SUCCESS
            }
            None => INVALID_SIGNATURE,
        };
        machine.set_register(A0, M::REG::from_u8(status));
        Ok(true)
    }
}

impl<M: SupportMachine> Syscalls<M> for V1 {
    fn initialize(&mut self, _: &mut M) -> Result<(), Error> {
        Ok(())
    }

    fn ecall(&mut self, machine: &mut M) -> Result<bool, Error> {
        if machine.registers()[A7].to_u64() != CRYPTO_V1 {
            return Ok(false);
        }
        crypto_v1(machine)
    }
}

/// A0 selects the operation, A1/A2 describe the source span and A3 is
/// the output pointer for the fixed 32-byte hash. Batch input holds complete
/// key/digest/signature records. Charge every record before reading memory.
fn crypto_v1<M: SupportMachine>(machine: &mut M) -> Result<bool, Error> {
    let operation = machine.registers()[A0].to_u64();
    let source = machine.registers()[A1].to_u64();
    let bytes = machine.registers()[A2].to_u64();
    let destination = machine.registers()[A3].to_u64();
    machine.add_cycles(CRYPTO_BASE_CYCLES)?;
    let status = match operation {
        0 if bytes <= MAX_HASH_BYTES => {
            machine.add_cycles(bytes * HASH_CYCLES_PER_BYTE)?;
            let input = machine.memory_mut().load_bytes(source, bytes)?;
            let hash = ckb_hash::blake2b_256(&input);
            machine.memory_mut().store_bytes(destination, &hash)?;
            SUCCESS
        }
        1 | 2 => {
            let width = if operation == 1 { 129 } else { 128 };
            let count = bytes / width;
            if !(1..=MAX_BATCH).contains(&count) || !bytes.is_multiple_of(width) {
                INVALID_SIGNATURE
            } else {
                let cost = if operation == 1 {
                    SECP256K1_VERIFY_CYCLES
                } else {
                    ED25519_VERIFY_CYCLES
                };
                machine.add_cycles(count * cost)?;
                let input = machine.memory_mut().load_bytes(source, bytes)?;
                let valid = if operation == 1 {
                    input.chunks_exact(129).all(verify_secp256k1)
                } else {
                    input.chunks_exact(128).all(verify_ed25519)
                };
                if valid { SUCCESS } else { INVALID_SIGNATURE }
            }
        }
        _ => INVALID_SIGNATURE,
    };
    machine.set_register(A0, M::REG::from_u8(status));
    Ok(true)
}

fn verify_secp256k1(record: &[u8]) -> bool {
    let Ok(key) = secp256k1::PublicKey::from_slice(&record[..33]) else {
        return false;
    };
    let digest = Message::from_digest(record[33..65].try_into().expect("32-byte digest"));
    let Ok(sig) = secp256k1::ecdsa::Signature::from_compact(&record[65..]) else {
        return false;
    };
    // libsecp256k1 verification accepts only a low-S signature.
    verification_context()
        .verify_ecdsa(&digest, &sig, &key)
        .is_ok()
}

fn verify_ed25519(record: &[u8]) -> bool {
    let Ok(key) =
        ed25519_dalek::VerifyingKey::from_bytes(record[..32].try_into().expect("32-byte key"))
    else {
        return false;
    };
    let sig =
        ed25519_dalek::Signature::from_bytes(record[64..].try_into().expect("64-byte signature"));
    key.verify_strict(&record[32..64], &sig).is_ok()
}

fn verification_context() -> &'static Secp256k1<VerifyOnly> {
    static CONTEXT: OnceLock<Secp256k1<VerifyOnly>> = OnceLock::new();
    CONTEXT.get_or_init(Secp256k1::verification_only)
}

/// Local reuse of successful, pure recoveries. Current-cell and group
/// authorization still run in the caller, and the syscall always charges.
pub(super) fn recover_cached(seal: [u8; 65], digest: [u8; 32]) -> Option<[u8; 33]> {
    let key = (seal, digest);
    RECOVERED.with_borrow_mut(|cache| {
        if let Some(public_key) = cache.get(&key) {
            return Some(*public_key);
        }
        let public_key = recover(&seal, &Message::from_digest(digest))?;
        cache.put(key, public_key);
        Some(public_key)
    })
}

fn recover(seal: &[u8], digest: &Message) -> Option<[u8; 33]> {
    let (id, compact) = seal.split_last()?;
    let recovery_id = RecoveryId::try_from(i32::from(*id)).ok()?;
    let signature = RecoverableSignature::from_compact(compact, recovery_id).ok()?;
    let original = signature.to_standard();
    let mut normalized = original;
    normalized.normalize_s();
    if original != normalized {
        return None;
    }
    let key = verification_context()
        .recover_ecdsa(digest, &signature)
        .ok()?;
    Some(key.serialize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{RecoveryId as ReferenceId, Signature, SigningKey, VerifyingKey};

    fn reference(seal: &[u8; 65], digest: &[u8; 32]) -> Option<[u8; 33]> {
        let signature = Signature::from_slice(&seal[..64]).ok()?;
        let recovery_id = ReferenceId::from_byte(seal[64])?;
        if signature.normalize_s().is_some() {
            return None;
        }
        let key = VerifyingKey::recover_from_prehash(digest, &signature, recovery_id).ok()?;
        key.to_encoded_point(true).as_bytes().try_into().ok()
    }

    fn sign(key: &SigningKey, digest: &[u8; 32]) -> [u8; 65] {
        let (signature, id) = key.sign_prehash_recoverable(digest).unwrap();
        let mut seal = [0; 65];
        seal[..64].copy_from_slice(&signature.to_bytes());
        seal[64] = id.to_byte();
        seal
    }

    #[test]
    fn cached_recovery_keys_cover_digest_compact_seal_and_recovery_id() {
        let key = SigningKey::from_bytes((&[14; 32]).into()).unwrap();
        let digest = [9; 32];
        let seal = sign(&key, &digest);
        let expected = reference(&seal, &digest).unwrap();
        assert_eq!(recover_cached(seal, digest), Some(expected));
        assert_eq!(recover_cached(seal, digest), Some(expected));

        let changed_digest = [21; 32];
        assert_ne!(recover_cached(seal, changed_digest), Some(expected));
        assert_eq!(
            recover_cached(seal, changed_digest),
            reference(&seal, &changed_digest)
        );

        let mut changed_seal = seal;
        changed_seal[32..64].fill(0);
        assert_eq!(recover_cached(changed_seal, digest), None);

        let mut changed_id = seal;
        changed_id[64] ^= 1;
        assert_ne!(recover_cached(changed_id, digest), Some(expected));
        assert_eq!(
            recover_cached(changed_id, digest),
            reference(&changed_id, &digest)
        );
    }

    #[test]
    #[ignore = "controlled native recovery cache microbenchmark"]
    fn measure_signature_recovery_reuse() {
        use std::time::Instant;

        const SAMPLES: usize = 1_000;
        let key = SigningKey::from_bytes((&[19; 32]).into()).unwrap();
        let mut cases = Vec::with_capacity(SAMPLES);
        for number in 0..SAMPLES {
            let mut digest = [0; 32];
            digest[..8].copy_from_slice(&(number as u64).to_le_bytes());
            cases.push((sign(&key, &digest), digest));
        }
        let mut durations = [
            Vec::with_capacity(SAMPLES),
            Vec::with_capacity(SAMPLES),
            Vec::with_capacity(SAMPLES),
        ];
        for (seal, digest) in cases {
            let message = Message::from_digest(digest);
            let start = Instant::now();
            let reference = recover(&seal, &message);
            durations[0].push(start.elapsed().as_nanos());
            let start = Instant::now();
            let miss = recover_cached(seal, digest);
            durations[1].push(start.elapsed().as_nanos());
            let start = Instant::now();
            let hit = recover_cached(seal, digest);
            durations[2].push(start.elapsed().as_nanos());
            assert_eq!(hit, reference);
            assert_eq!(miss, reference);
        }
        let percentiles = durations.map(|mut durations| {
            durations.sort_unstable();
            [50, 95, 99].map(|p| durations[(SAMPLES * p).div_ceil(100) - 1])
        });
        println!(
            "recovery samples={SAMPLES} uncached_ns={:?} miss_ns={:?} hit_ns={:?} fixed_cycles={SECP256K1_RECOVERY_CYCLES}",
            percentiles[0], percentiles[1], percentiles[2],
        );
    }

    #[test]
    #[ignore = "calibrate crypto ABI against uncached native recovery on a pinned CPU"]
    fn measure_crypto_v1() {
        use ed25519_dalek::Signer;
        use std::{hint::black_box, time::Instant};
        let key = SigningKey::from_bytes((&[17; 32]).into()).unwrap();
        let ed = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        let mut cases = Vec::new();
        for i in 0..128u16 {
            let digest = ckb_hash::blake2b_256(i.to_le_bytes());
            let seal = sign(&key, &digest);
            let secp = [
                key.verifying_key().to_encoded_point(true).as_bytes(),
                &digest,
                &seal[..64],
            ]
            .concat();
            let ed_record = [
                &ed.verifying_key().to_bytes()[..],
                &digest,
                &ed.sign(&digest).to_bytes(),
            ]
            .concat();
            cases.push((Message::from_digest(digest), seal, secp, ed_record));
        }
        let sizes = [0, 32, 256, 4096, 65_536, 1_048_576];
        let data = vec![3u8; sizes[5]];
        for trial in 0..18 {
            // Rotate the three signature loops to distribute transient host effects.
            for index in 0..3 {
                let op = (index + trial) % 3;
                let start = Instant::now();
                for (digest, seal, secp, ed) in black_box(&cases) {
                    assert!(match op {
                        0 => recover(black_box(seal), digest).is_some(),
                        1 => verify_secp256k1(black_box(secp)),
                        _ => verify_ed25519(black_box(ed)),
                    });
                }
                println!(
                    "CAL {trial} {} 1 {} {}",
                    ["recover", "secp", "ed"][op],
                    cases.len(),
                    start.elapsed().as_nanos()
                );
            }
            for size in sizes {
                let repeats = (2_097_152 / size.max(1)).clamp(8, 2048);
                let start = Instant::now();
                for _ in 0..repeats {
                    black_box(ckb_hash::blake2b_256(black_box(&data[..size])));
                }
                println!(
                    "CAL {trial} hash {size} {repeats} {}",
                    start.elapsed().as_nanos()
                );
            }
        }
    }

    #[test]
    fn agrees_with_the_riscv_lock_reference() {
        let key = SigningKey::from_bytes((&[19; 32]).into()).unwrap();
        for digest in [[0; 32], [19; 32], [255; 32]] {
            let (signature, id) = key.sign_prehash_recoverable(&digest).unwrap();
            let mut valid = [0; 65];
            valid[..64].copy_from_slice(&signature.to_bytes());
            valid[64] = id.to_byte();
            let message = Message::from_digest(digest);
            assert_eq!(recover(&valid, &message), reference(&valid, &digest));
            assert!(recover(&valid, &message).is_some());
            for id in [0, 1, 2, 3, 4, 255] {
                let mut seal = valid;
                seal[64] = id;
                assert_eq!(recover(&seal, &message), reference(&seal, &digest));
            }
            for scalar in [[0; 32], [255; 32], [128; 32]] {
                let mut seal = valid;
                seal[32..64].copy_from_slice(&scalar);
                assert!(recover(&seal, &message).is_none());
                assert!(reference(&seal, &digest).is_none());
                seal = valid;
                seal[..32].copy_from_slice(&scalar);
                assert_eq!(recover(&seal, &message), reference(&seal, &digest));
            }
        }
    }
}
