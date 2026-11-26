//! The limited, immutable Spore data accepted by object settlement.

use crate::{
    hash,
    intercell::{Identity, MAX_PAYLOAD},
};

/// Published CKB Spore V2 mainnet program (0.2.2-beta.1).
pub const CKB_SPORE_CODE: Identity = [
    0x4a, 0x4d, 0xce, 0x1d, 0xf3, 0xdf, 0xff, 0xf7, 0xf8, 0xb2, 0xcd, 0x7d, 0xff, 0x73, 0x03, 0xdf,
    0x3b, 0x61, 0x50, 0xc9, 0x78, 0x8c, 0xb7, 0x5d, 0xcf, 0x67, 0x47, 0x24, 0x71, 0x32, 0xb9, 0xf5,
];

pub const TRANSFER_HEADER: usize = 64;
pub const MAX_SPORE_DATA: usize = MAX_PAYLOAD - TRANSFER_HEADER;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SporeTransfer<'a> {
    pub original_type: Identity,
    pub recipient: Identity,
    pub data: &'a [u8],
}

impl<'a> SporeTransfer<'a> {
    pub fn decode(payload: &'a [u8]) -> Option<Self> {
        let (header, data) = payload.split_at_checked(TRANSFER_HEADER)?;
        let original_type = header[..32].try_into().ok()?;
        let recipient = header[32..].try_into().ok()?;
        if original_type == [0; 32] || recipient == [0; 32] || !plain_spore(data) {
            return None;
        }
        Some(Self {
            original_type,
            recipient,
            data,
        })
    }
}

pub fn return_nonce(input: &[u8; 36]) -> u64 {
    let digest = hash(b"Stem Spore return nonce\0", &[input]);
    u64::from_le_bytes(digest[..8].try_into().expect("nonce prefix"))
}

pub fn plain_spore(data: &[u8]) -> bool {
    plain_spore_fields(data).is_some()
}

pub(crate) fn plain_spore_fields(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let (header, _) = data.split_at_checked(16)?;
    let (words, _) = header.as_chunks::<4>();
    let [total, mime, content, cluster] = words else {
        return None;
    };
    let content = u32::from_le_bytes(*content) as usize;
    if data.len() > MAX_SPORE_DATA
        || u32::from_le_bytes(*total) as usize != data.len()
        || u32::from_le_bytes(*mime) != 16
        || u32::from_le_bytes(*cluster) as usize != data.len()
    {
        return None;
    }
    let (mime, content) = data[16..].split_at_checked(content.checked_sub(16)?)?;
    let mime = molecule_bytes(mime)?;
    let content = molecule_bytes(content)?;
    (matches!(mime, b"text/plain" | b"application/octet-stream") && !content.is_empty())
        .then_some((mime, content))
}

fn molecule_bytes(raw: &[u8]) -> Option<&[u8]> {
    let (len, bytes) = raw.split_at_checked(4)?;
    (u32::from_le_bytes(len.try_into().ok()?) as usize == bytes.len()).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_noncanonical_molecule_and_cluster_data() {
        let mut data = [
            &[36, 0, 0, 0, 16, 0, 0, 0, 30, 0, 0, 0, 36, 0, 0, 0][..],
            &[10, 0, 0, 0],
            b"text/plain",
            &[2, 0, 0, 0],
            b"hi",
        ]
        .concat();
        assert!(plain_spore(&data));
        data[30] = 1;
        assert!(!plain_spore(&data));
        data[30] = 2;
        data[12] = 35;
        assert!(!plain_spore(&data));
        data[12] = 36;
        data[20] = b'!';
        assert!(!plain_spore(&data));
    }
}
