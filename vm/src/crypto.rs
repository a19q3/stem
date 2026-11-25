use std::sync::OnceLock;

use ckb_script::runtime::SUCCESS;
use ckb_vm::{
    Error, Register, SupportMachine, Syscalls,
    memory::Memory,
    registers::{A0, A1, A2, A7},
};
use secp256k1::{
    Message, Secp256k1, VerifyOnly,
    ecdsa::{RecoverableSignature, RecoveryId},
};

pub const SECP256K1_RECOVER: u64 = 2121;
pub const SECP256K1_RECOVERY_CYCLES: u64 = 1_900_000;
const INVALID_SIGNATURE: u8 = 4;

pub(crate) struct Recover;

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
        let digest = Message::from_digest(digest.as_ref().try_into().expect("32-byte digest"));
        let status = match recover(&signature, &digest) {
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
    static CONTEXT: OnceLock<Secp256k1<VerifyOnly>> = OnceLock::new();
    let key = CONTEXT
        .get_or_init(Secp256k1::verification_only)
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
