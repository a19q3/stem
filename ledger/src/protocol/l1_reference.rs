//! Explicit read-only CKB outputs certified by the pinned L1 observation authority.

use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, bail, ensure};
use ckb_types::{
    bytes::Bytes,
    core::ScriptHashType,
    packed::{Byte32, CellOutput, OutPoint},
    prelude::Entity,
};
use lru::LruCache;
use stem_stake::Authority;

use super::{L1Anchor, byte32, bytes, hash32, out_point};
use crate::{
    Transaction, hash,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

pub const MAX_REFERENCES: usize = 4;
const MAX_OUTPUT_DATA: usize = 16 * 1024;
const MAX_OUTPUT: usize = 4096;
const SIGNATURE_CYCLES: u64 = 4_000_000;
const VERIFIED_SET_LIMIT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum L1ReferenceKind {
    Historical = 0,
    LiveAtAnchor = 1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct L1Reference {
    pub kind: L1ReferenceKind,
    pub genesis: Byte32,
    pub out_point: OutPoint,
    pub creation: L1Anchor,
    pub anchor: L1Anchor,
    pub output: CellOutput,
    pub data: Bytes,
}

impl L1Reference {
    pub fn encode(&self) -> Bytes {
        wire::L1Reference::new_builder()
            .kind((self.kind as u8).into())
            .genesis(byte32(&self.genesis))
            .out_point(out_point(&self.out_point))
            .creation(self.creation.encode())
            .anchor(self.anchor.encode())
            .output(bytes(self.output.as_bytes()))
            .data(bytes(self.data.clone()))
            .build()
            .as_bytes()
    }

    pub fn decode(body: &[u8]) -> Result<Self> {
        Self::from_entity(wire::L1Reference::from_slice(body)?)
    }

    fn from_entity(raw: wire::L1Reference) -> Result<Self> {
        let kind = match u8::from(raw.kind()) {
            0 => L1ReferenceKind::Historical,
            1 => L1ReferenceKind::LiveAtAnchor,
            _ => bail!("unknown L1 reference kind"),
        };
        let output = raw.output().raw_data();
        let data = raw.data().raw_data();
        ensure!(
            output.len() <= MAX_OUTPUT && data.len() <= MAX_OUTPUT_DATA,
            "L1 reference cell is too large"
        );
        let output = CellOutput::from_slice(&output)?;
        ScriptHashType::try_from(output.lock().hash_type())?;
        if let Some(script) = output.type_().to_opt() {
            ScriptHashType::try_from(script.hash_type())?;
        }
        let reference = Self {
            kind,
            genesis: hash32(raw.genesis()),
            out_point: OutPoint::new_unchecked(raw.out_point().as_bytes()),
            creation: L1Anchor::decode(raw.creation()),
            anchor: L1Anchor::decode(raw.anchor()),
            output,
            data,
        };
        ensure!(
            !reference.out_point.is_null()
                && reference.genesis != Byte32::default()
                && reference.creation.hash != Byte32::default()
                && reference.anchor.hash != Byte32::default()
                && reference.creation.height <= reference.anchor.height
                && (reference.creation.height != reference.anchor.height
                    || reference.creation.hash == reference.anchor.hash),
            "invalid CKB creation or observation anchor"
        );
        Ok(reference)
    }
}

/// Build the signed body from records already sorted by their serialized OutPoint.
pub fn encode_reference_set(records: &[impl AsRef<[u8]>]) -> Result<Bytes> {
    ensure!(
        (1..=MAX_REFERENCES).contains(&records.len()),
        "invalid L1 reference count"
    );
    let entries = records
        .iter()
        .map(|raw| Ok(wire::L1Reference::from_slice(raw.as_ref())?))
        .collect::<Result<Vec<_>>>()?;
    let set = wire::L1ReferenceSet::new_builder().set(entries).build();
    validate_set(set.clone())?;
    Ok(set.as_bytes())
}

pub(crate) fn decode_reference_set(body: &[u8]) -> Result<Vec<(L1Reference, Bytes)>> {
    validate_set(wire::L1ReferenceSet::from_slice(body)?)
}

fn validate_set(set: wire::L1ReferenceSet) -> Result<Vec<(L1Reference, Bytes)>> {
    ensure!(
        (1..=MAX_REFERENCES).contains(&set.len()),
        "invalid L1 reference count"
    );
    let mut records = Vec::<(L1Reference, Bytes)>::with_capacity(set.len());
    for entry in set {
        let body = entry.as_bytes();
        let reference = L1Reference::from_entity(entry)?;
        if let Some((last, _)) = records.last() {
            ensure!(
                last.out_point.as_slice() < reference.out_point.as_slice(),
                "L1 references must be ordered by unique OutPoint"
            );
            ensure!(
                last.genesis == reference.genesis && last.anchor == reference.anchor,
                "L1 reference set must use one CKB genesis and anchor"
            );
        }
        records.push((reference, body));
    }
    Ok(records)
}

pub fn l1_reference_digest(network: &Byte32, generation: &[u8; 32], body: &[u8]) -> Byte32 {
    hash(
        b"Stem CKB reference set observation\0",
        network,
        &[generation, body],
    )
}

pub struct VerifiedL1References {
    transaction: Byte32,
    records: Arc<[L1ReferenceRecord]>,
}

/// One immutable observation authenticated by the reference-set certificate.
#[derive(Debug)]
pub struct L1ReferenceRecord {
    raw: Bytes,
    observation: L1Reference,
    type_hash: OnceLock<Option<Byte32>>,
    live_typed_data: OnceLock<Option<Bytes>>,
}

impl L1ReferenceRecord {
    pub fn as_slice(&self) -> &[u8] {
        &self.raw
    }

    pub fn observation(&self) -> &L1Reference {
        &self.observation
    }

    pub fn type_hash(&self) -> Option<&Byte32> {
        self.type_hash
            .get_or_init(|| {
                self.observation
                    .output
                    .type_()
                    .to_opt()
                    .map(|ty| ty.calc_script_hash())
            })
            .as_ref()
    }

    /// Syscall 2130's typed projection, cached separately from the raw record.
    pub fn live_typed_data(&self) -> Option<&[u8]> {
        self.live_typed_data
            .get_or_init(|| {
                if self.observation.kind != L1ReferenceKind::LiveAtAnchor {
                    return None;
                }
                let type_hash = self.type_hash()?;
                let mut data = Vec::with_capacity(32 + self.observation.data.len());
                data.extend_from_slice(type_hash.as_slice());
                data.extend_from_slice(&self.observation.data);
                Some(data.into())
            })
            .as_deref()
    }
}

impl VerifiedL1References {
    /// Fixed protocol charge, applied before Ed25519 verification and independently of VM caching.
    pub fn cycles(tx: &Transaction) -> u64 {
        tx.l1_reference_certificate().map_or(0, |certificate| {
            certificate.attestations().len() as u64 * SIGNATURE_CYCLES
        })
    }

    pub(crate) fn authorize(
        self,
        tx: &Transaction,
        network: &Byte32,
    ) -> Result<Arc<[L1ReferenceRecord]>> {
        ensure!(
            self.transaction == tx.commitment(network),
            "L1 reference authorization differs from transaction"
        );
        Ok(self.records)
    }
}

/// Shares successful quorum checks only within a single immutable bridge trust domain.
pub struct L1ReferenceVerifier {
    network: Byte32,
    genesis: Byte32,
    authority: Authority,
    verified: Mutex<LruCache<Vec<u8>, Arc<[L1ReferenceRecord]>>>,
}

impl L1ReferenceVerifier {
    pub fn new(network: Byte32, genesis: Byte32, authority: Authority) -> Self {
        Self {
            network,
            genesis,
            authority,
            verified: Mutex::new(LruCache::new(VERIFIED_SET_LIMIT)),
        }
    }

    pub fn authority(&self) -> &Authority {
        &self.authority
    }

    pub fn verify(
        &self,
        tx: &Transaction,
        anchor: &L1Anchor,
        live_fresh: bool,
    ) -> Result<VerifiedL1References> {
        let certificate = tx
            .l1_reference_certificate()
            .context("missing L1 reference certificate")?;
        // Keep only the exact certificate bytes, not the backing transaction buffer.
        let key = certificate.encoded.as_slice();
        let cached = self
            .verified
            .lock()
            .expect("L1 reference cache lock")
            .get(key)
            .cloned();
        let records = if let Some(records) = cached {
            self.check_context(&records, anchor, live_fresh)?;
            records
        } else {
            let body = certificate.body();
            let records: Arc<[L1ReferenceRecord]> = decode_reference_set(&body)?
                .into_iter()
                .map(|(observation, raw)| L1ReferenceRecord {
                    raw,
                    observation,
                    type_hash: OnceLock::new(),
                    live_typed_data: OnceLock::new(),
                })
                .collect();
            self.check_context(&records, anchor, live_fresh)?;
            self.authority.verify(
                l1_reference_digest(&self.network, &self.authority.generation(), &body).as_slice(),
                &certificate.attestations().collect::<Vec<_>>(),
            )?;
            self.verified
                .lock()
                .expect("L1 reference cache lock")
                .put(key.to_vec(), Arc::clone(&records));
            records
        };
        Ok(VerifiedL1References {
            transaction: tx.commitment(&self.network),
            records,
        })
    }

    fn check_context(
        &self,
        records: &[L1ReferenceRecord],
        anchor: &L1Anchor,
        live_fresh: bool,
    ) -> Result<()> {
        for entry in records {
            let reference = entry.observation();
            ensure!(
                reference.genesis == self.genesis && &reference.anchor == anchor,
                "L1 reference does not use the committed CKB anchor"
            );
            ensure!(
                reference.kind != L1ReferenceKind::LiveAtAnchor || live_fresh,
                "live L1 reference observation is stale"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ckb_types::{
        packed::Script,
        prelude::{Builder, Pack},
    };
    use ed25519_dalek::{Signer, SigningKey};
    use stem_stake::{Attestation, Member};

    use super::*;
    use crate::{CellInput, protocol::Certificate};

    fn anchor(height: u64) -> L1Anchor {
        L1Anchor {
            height,
            hash: [height as u8; 32].pack(),
        }
    }

    fn record(id: u8, kind: L1ReferenceKind) -> L1Reference {
        L1Reference {
            kind,
            genesis: [1; 32].pack(),
            out_point: OutPoint::new([id; 32].pack(), 0),
            creation: anchor(2),
            anchor: anchor(3),
            output: CellOutput::default(),
            data: Bytes::from(vec![id]),
        }
    }

    #[test]
    fn reference_sets_require_one_anchor_and_unique_canonical_points() {
        let first = record(1, L1ReferenceKind::Historical).encode();
        let mut second = record(2, L1ReferenceKind::LiveAtAnchor);
        let empty: [Bytes; 0] = [];
        assert!(encode_reference_set(&empty).is_err());
        assert!(encode_reference_set(&[first.clone(), first.clone()]).is_err());
        assert!(encode_reference_set(&[second.encode(), first.clone()]).is_err());
        assert!(encode_reference_set(&[first.clone(), second.encode()]).is_ok());
        second.anchor = anchor(4);
        assert!(encode_reference_set(&[first.clone(), second.encode()]).is_err());
        second.anchor = anchor(3);
        second.genesis = [2; 32].pack();
        assert!(encode_reference_set(&[first, second.encode()]).is_err());
        let too_many = (1..=5)
            .map(|id| record(id, L1ReferenceKind::Historical).encode())
            .collect::<Vec<_>>();
        assert!(encode_reference_set(&too_many).is_err());
    }

    #[test]
    fn one_quorum_binds_every_record_and_does_not_share_live_freshness() -> Result<()> {
        let first = record(1, L1ReferenceKind::Historical);
        let mut second = record(2, L1ReferenceKind::LiveAtAnchor);
        let ty = Script::default();
        second.output = CellOutput::new_builder()
            .type_(Some(ty.clone()).pack())
            .build();
        let records = [first.encode(), second.encode()];
        let network = [8; 32].pack();
        let generation = [6; 32];
        let key = SigningKey::from_bytes(&[7; 32]);
        let member = Member {
            key: key.verifying_key().to_bytes(),
            power: 1,
        };
        let authority = Authority::new(generation, &[member])?;
        let verifier =
            L1ReferenceVerifier::new(network.clone(), first.genesis.clone(), authority.clone());
        let body = encode_reference_set(&records)?;
        let digest = l1_reference_digest(&network, &generation, &body);
        let vote = Attestation {
            guardian: 0,
            signature: key.sign(digest.as_slice()).to_bytes(),
        };
        let unsigned = Transaction::new(
            vec![],
            vec![],
            vec![CellInput::new(first.out_point.clone(), None)],
            vec![],
            vec![],
        )?;
        let attach =
            |body| unsigned.with_l1_reference_certificate(Certificate::new(body, vec![vote])?);
        let tx = attach(body)?;
        let verify = |tx: &Transaction, fresh| verifier.verify(tx, &first.anchor, fresh);
        assert_eq!(VerifiedL1References::cycles(&tx), SIGNATURE_CYCLES);
        let resolved = verify(&tx, true)?.authorize(&tx, &network)?;
        for (entry, bytes) in resolved.iter().zip(&records) {
            assert_eq!(entry.as_slice(), bytes.as_ref());
        }
        assert_eq!(resolved.len(), records.len());
        assert!(resolved[0].live_typed_data().is_none());
        let typed = resolved[1].live_typed_data().unwrap();
        assert_eq!(&typed[..32], ty.calc_script_hash().as_slice());
        assert_eq!(&typed[32..], second.data.as_ref());
        let cert = tx.l1_reference_certificate().unwrap();
        let reuse = unsigned
            .with_valid_before_height(10)?
            .with_l1_reference_certificate(cert.clone())?;
        assert!(verify(&tx, true)?.authorize(&reuse, &network).is_err());
        assert!(Arc::ptr_eq(
            &verify(&reuse, true)?.authorize(&reuse, &network)?,
            &resolved
        ));
        assert!(verify(&tx, false).is_err());
        assert!(verifier.verify(&tx, &anchor(4), true).is_err());
        let mut invalid = vote;
        invalid.signature[0] ^= 1;
        let invalid = unsigned
            .with_l1_reference_certificate(Certificate::new(cert.body(), vec![invalid])?)?;
        assert!(verify(&invalid, true).is_err());
        assert!(verify(&attach(encode_reference_set(&records[..1])?)?, true).is_err());
        let mut changed = second;
        changed.data = Bytes::from_static(b"modified");
        assert!(
            verify(
                &attach(encode_reference_set(&[
                    records[0].clone(),
                    changed.encode()
                ])?)?,
                true
            )
            .is_err()
        );
        let wrong_generation = Authority::new([9; 32], &[member])?;
        let other_signer = SigningKey::from_bytes(&[4; 32]);
        let other_guardian = Authority::new(
            generation,
            &[Member {
                key: other_signer.verifying_key().to_bytes(),
                power: 1,
            }],
        )?;
        for (domain, guardian) in [
            ([9; 32].pack(), authority),
            (network.clone(), wrong_generation),
            (network.clone(), other_guardian),
        ] {
            let verifier = L1ReferenceVerifier::new(domain, first.genesis.clone(), guardian);
            assert!(verifier.verify(&tx, &first.anchor, true).is_err());
        }
        Ok(())
    }
}
