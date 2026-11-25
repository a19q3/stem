//! A guardian-certified CKB anchor change with no custody transition.

use anyhow::{Result, bail, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::Entity};
use stem_stake::Authority;

use super::{Certificate, L1Anchor, Operation, byte32, hash32};
use crate::{
    hash,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct L1AnchorUpdate {
    pub l1_genesis: Byte32,
    pub authority: Byte32,
    pub previous_anchor: L1Anchor,
    pub anchor: L1Anchor,
}

impl L1AnchorUpdate {
    pub fn encode(&self) -> Bytes {
        Operation::L1AnchorUpdate(self.clone()).encode().as_bytes()
    }

    pub fn decode(raw: &[u8]) -> Result<Self> {
        let Operation::L1AnchorUpdate(update) =
            Operation::decode(wire::ProtocolOperation::from_slice(raw)?)?
        else {
            bail!("expected an L1 anchor update");
        };
        update.validate()?;
        Ok(update)
    }

    pub fn digest(&self, network: &Byte32) -> Byte32 {
        hash(b"Stem CKB anchor update\0", network, &[&self.encode()])
    }

    pub(super) fn payload(&self) -> Bytes {
        wire::L1AnchorUpdate::new_builder()
            .l1_genesis(byte32(&self.l1_genesis))
            .authority(byte32(&self.authority))
            .previous_anchor(self.previous_anchor.encode())
            .anchor(self.anchor.encode())
            .build()
            .as_bytes()
    }

    pub(super) fn decode_payload(raw: &[u8]) -> Result<Self> {
        let wire = wire::L1AnchorUpdate::from_slice(raw)?;
        Ok(Self {
            l1_genesis: hash32(wire.l1_genesis()),
            authority: hash32(wire.authority()),
            previous_anchor: L1Anchor::decode(wire.previous_anchor()),
            anchor: L1Anchor::decode(wire.anchor()),
        })
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            [
                &self.l1_genesis,
                &self.authority,
                &self.previous_anchor.hash,
                &self.anchor.hash
            ]
            .into_iter()
            .all(|id| *id != Byte32::default()),
            "invalid L1 anchor update identity"
        );
        ensure!(
            self.anchor.height > self.previous_anchor.height
                && self.anchor.hash != self.previous_anchor.hash,
            "L1 anchor update must advance"
        );
        Ok(())
    }

    pub fn verify(
        &self,
        network: &Byte32,
        l1_genesis: &Byte32,
        current: &L1Anchor,
        authority: &Authority,
        proof: &Certificate,
    ) -> Result<()> {
        self.validate()?;
        ensure!(
            &self.l1_genesis == l1_genesis && self.authority.as_slice() == authority.generation(),
            "L1 anchor update names a different chain or authority"
        );
        ensure!(
            &self.previous_anchor == current,
            "L1 anchor update does not extend the accepted anchor"
        );
        ensure!(
            proof.body().is_empty(),
            "unexpected L1 anchor certificate body"
        );
        authority.verify(
            self.digest(network).as_slice(),
            &proof.attestations().collect::<Vec<_>>(),
        )?;
        Ok(())
    }
}
