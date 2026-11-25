//! Fixed payload for one escrow-backed fungible asset transfer.

use crate::{hash, intercell::Identity};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetTransfer {
    pub asset: Identity,
    pub recipient: Identity,
    pub amount: u128,
}

impl AssetTransfer {
    pub const SIZE: usize = 80;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[..32].copy_from_slice(&self.asset);
        bytes[32..64].copy_from_slice(&self.recipient);
        bytes[64..].copy_from_slice(&self.amount.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes: &[u8; Self::SIZE] = bytes.try_into().ok()?;
        let asset = bytes[..32].try_into().ok()?;
        let recipient = bytes[32..64].try_into().ok()?;
        let amount = u128::from_le_bytes(bytes[64..].try_into().ok()?);
        if asset == [0; 32] || recipient == [0; 32] || amount == 0 {
            return None;
        }
        Some(Self {
            asset,
            recipient,
            amount,
        })
    }
}

/// Scope an outbound nonce to the consumed custody OutPoint.
pub fn export_nonce(custody_input: &[u8; 36]) -> u64 {
    let digest = hash(b"Stem UDT export nonce\0", &[custody_input]);
    u64::from_le_bytes(digest[..8].try_into().expect("nonce prefix"))
}

pub const STEM_ESCROW_FIELDS: usize = 7;
pub const CKB_ESCROW_FIELDS: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_transfer_encoding_is_exact_and_positive() {
        let transfer = AssetTransfer {
            asset: [1; 32],
            recipient: [2; 32],
            amount: u128::MAX,
        };
        let bytes = transfer.encode();
        assert_eq!(AssetTransfer::decode(&bytes), Some(transfer));
        assert!(AssetTransfer::decode(&bytes[..79]).is_none());
        assert!(
            AssetTransfer::decode(
                &AssetTransfer {
                    amount: 0,
                    ..transfer
                }
                .encode()
            )
            .is_none()
        );
        assert!(
            AssetTransfer::decode(
                &AssetTransfer {
                    recipient: [0; 32],
                    ..transfer
                }
                .encode()
            )
            .is_none()
        );
    }
}
