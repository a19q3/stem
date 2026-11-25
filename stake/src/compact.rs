//! Bounded signer bitmap shared by compact weighted certificate verifiers.

use crate::{Error, MAX_RECORDS, Result};

pub struct SignerBitmap<'a> {
    bytes: &'a [u8],
}

impl<'a> SignerBitmap<'a> {
    /// Bit zero of the first byte selects the first canonical committee member.
    pub fn new(member_count: usize, bytes: &'a [u8]) -> Result<Self> {
        if !(1..=MAX_RECORDS).contains(&member_count) || bytes.len() != member_count.div_ceil(8) {
            return Err(Error::Encoding);
        }
        let trailing = member_count % 8;
        if trailing != 0 && bytes[bytes.len() - 1] >> trailing != 0 {
            return Err(Error::Encoding);
        }
        Ok(Self { bytes })
    }

    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.bytes.iter().enumerate().flat_map(|(byte, value)| {
            (0..8).filter_map(move |bit| (value & (1 << bit) != 0).then_some(byte * 8 + bit))
        })
    }

    pub fn signature_count(&self) -> usize {
        self.bytes
            .iter()
            .map(|byte| byte.count_ones() as usize)
            .sum()
    }
}

impl crate::Authority {
    pub fn verify_compact(
        &self,
        message: &[u8],
        bitmap: &[u8],
        signatures: &[[u8; 64]],
    ) -> Result<()> {
        use ed25519_dalek::Signature;
        let bitmap = SignerBitmap::new(self.guardians.len(), bitmap)?;
        if bitmap.signature_count() != signatures.len() {
            return Err(Error::Encoding);
        }
        let mut signed = 0u64;
        for (index, signature) in bitmap.indices().zip(signatures) {
            let (key, power) = &self.guardians[index];
            key.verify_strict(message, &Signature::from_bytes(signature))
                .map_err(|_| Error::Signature)?;
            signed = signed.checked_add(*power).ok_or(Error::Overflow)?;
        }
        self.require_quorum(signed)
    }
}
