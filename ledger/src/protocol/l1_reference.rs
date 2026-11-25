//! Explicit read-only CKB outputs certified by the pinned L1 observation authority.

use anyhow::{Result, bail, ensure};
use ckb_types::{
    bytes::Bytes,
    core::ScriptHashType,
    packed::{Byte32, CellOutput, OutPoint},
    prelude::Entity,
};
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
        let raw = wire::L1Reference::from_slice(body)?;
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

pub fn l1_reference_digest(network: &Byte32, generation: &[u8; 32], body: &[u8]) -> Byte32 {
    hash(
        b"Stem CKB reference observation\0",
        network,
        &[generation, body],
    )
}

pub struct VerifiedL1References {
    transaction: Byte32,
    records: Vec<Bytes>,
}

impl VerifiedL1References {
    /// Fixed protocol charge, applied before Ed25519 verification and independently of VM caching.
    pub fn cycles(tx: &Transaction) -> u64 {
        tx.l1_references()
            .map(|cert| cert.attestations().len() as u64 * SIGNATURE_CYCLES)
            .sum()
    }

    pub fn verify(
        tx: &Transaction,
        network: &Byte32,
        genesis: &Byte32,
        anchor: &L1Anchor,
        live_fresh: bool,
        authority: &Authority,
    ) -> Result<Self> {
        let mut records = Vec::with_capacity(tx.l1_references().len());
        for certificate in tx.l1_references() {
            let body = certificate.body();
            let reference = L1Reference::decode(&body)?;
            ensure!(
                &reference.genesis == genesis && &reference.anchor == anchor,
                "L1 reference does not use the committed CKB anchor"
            );
            ensure!(
                reference.kind != L1ReferenceKind::LiveAtAnchor || live_fresh,
                "live L1 reference observation is stale"
            );
            authority.verify(
                l1_reference_digest(network, &authority.generation(), &body).as_slice(),
                &certificate.attestations().collect::<Vec<_>>(),
            )?;
            records.push(body);
        }
        Ok(Self {
            transaction: tx.commitment(network),
            records,
        })
    }

    pub(crate) fn authorize(self, tx: &Transaction, network: &Byte32) -> Result<Vec<Bytes>> {
        ensure!(
            self.transaction == tx.commitment(network),
            "L1 reference authorization differs from transaction"
        );
        Ok(self.records)
    }
}
