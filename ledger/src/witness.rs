use crate::telegraph::prelude::{Builder as WireBuilder, Entity as WireEntity};
use anyhow::{Result, ensure};
use ckb_types::{bytes::Bytes, prelude::*};

use crate::wire;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupWitness {
    pub group_index: u16,
    pub proof_data: Bytes,
    pub seal: Bytes,
}

impl GroupWitness {
    pub(crate) fn encode(&self) -> wire::GroupWitness {
        wire::GroupWitness::new_builder()
            .group_index(wire::Uint16::new_unchecked(Bytes::copy_from_slice(
                &self.group_index.to_le_bytes(),
            )))
            .proof_data(wire::Bytes::new_unchecked(
                self.proof_data.pack().as_bytes(),
            ))
            .seal(wire::Bytes::new_unchecked(self.seal.pack().as_bytes()))
            .build()
    }

    pub fn encode_bytes(&self) -> Bytes {
        self.encode().as_bytes()
    }

    pub(crate) fn decode(witness: wire::GroupWitness) -> Self {
        Self {
            group_index: u16::from_le_bytes(
                witness.group_index().as_slice().try_into().expect("Uint16"),
            ),
            proof_data: witness.proof_data().raw_data(),
            seal: witness.seal().raw_data(),
        }
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.proof_data.is_empty() || !self.seal.is_empty(),
            "empty Group Witness must be omitted"
        );
        Ok(())
    }

    pub(crate) fn bytes(&self) -> Option<usize> {
        self.proof_data.len().checked_add(self.seal.len())
    }
}
