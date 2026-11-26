//! Canonical, immutable Spore data accepted by object settlement.

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
/// The native transaction envelope is at most 512 KiB, including all Cell metadata.
pub const MAX_CELL_SPORE_DATA: usize = 512 * 1024;
pub const CONTENT_COMMITMENT_TAG: &[u8; 4] = b"SPH1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferContent<'a> {
    Inline(&'a [u8]),
    Committed { length: usize, digest: &'a Identity },
}

impl TransferContent<'_> {
    pub fn matches(self, bytes: &[u8], hash: impl FnOnce() -> Option<Identity>) -> bool {
        match self {
            Self::Inline(raw) => raw == bytes,
            Self::Committed { length, digest } => {
                length == bytes.len() && hash().as_ref() == Some(digest)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellTransfer<'a> {
    pub original_type: Identity,
    pub recipient: Identity,
    pub content: TransferContent<'a>,
}

impl<'a> CellTransfer<'a> {
    /// Build a mailbox payload from the complete Molecule bytes of the actual Spore Cell.
    pub fn encode(
        original_type: &Identity,
        recipient: &Identity,
        data: &[u8],
        out: &'a mut [u8; MAX_PAYLOAD],
    ) -> Option<&'a [u8]> {
        if original_type == &[0; 32] || recipient == &[0; 32] {
            return None;
        }
        SporeData::from_cell(data)?;
        out[..32].copy_from_slice(original_type);
        out[32..TRANSFER_HEADER].copy_from_slice(recipient);
        let body = &mut out[TRANSFER_HEADER..];
        let size = if data.len() <= MAX_SPORE_DATA {
            body[..data.len()].copy_from_slice(data);
            data.len()
        } else {
            body[..4].copy_from_slice(CONTENT_COMMITMENT_TAG);
            body[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes());
            body[8..40].copy_from_slice(&hash(b"", &[data]));
            40
        };
        Some(&out[..TRANSFER_HEADER + size])
    }

    pub fn decode(payload: &'a [u8]) -> Option<Self> {
        let (header, body) = payload.split_at_checked(TRANSFER_HEADER)?;
        let fields = header.as_chunks::<32>().0;
        let [original_type, recipient] = fields else {
            return None;
        };
        if fields.contains(&[0; 32]) {
            return None;
        }
        let content = if let Some(rest) = body.strip_prefix(CONTENT_COMMITMENT_TAG) {
            let (size, hash) = rest.split_at_checked(4)?;
            let length = u32::from_le_bytes(size.try_into().ok()?) as usize;
            if !(MAX_SPORE_DATA + 1..=MAX_CELL_SPORE_DATA).contains(&length) {
                return None;
            }
            TransferContent::Committed {
                length,
                digest: hash.try_into().ok()?,
            }
        } else {
            SporeData::decode(body)?;
            TransferContent::Inline(body)
        };
        Some(Self {
            original_type: *original_type,
            recipient: *recipient,
            content,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SporeTransfer<'a> {
    pub original_type: Identity,
    pub recipient: Identity,
    pub data: &'a [u8],
    pub cluster_id: Option<&'a Identity>,
}

impl<'a> SporeTransfer<'a> {
    pub fn decode(payload: &'a [u8]) -> Option<Self> {
        Self::read(payload, false)
    }

    /// Stem-issued objects use the plain content profile.
    pub fn decode_plain(payload: &'a [u8]) -> Option<Self> {
        Self::read(payload, true)
    }

    fn read(payload: &'a [u8], plain: bool) -> Option<Self> {
        let (header, data) = payload.split_at_checked(TRANSFER_HEADER)?;
        let original_type = header[..32].try_into().ok()?;
        let recipient = header[32..].try_into().ok()?;
        if original_type == [0; 32] || recipient == [0; 32] {
            return None;
        }
        let parsed = SporeData::decode(data)?;
        if plain && !parsed.is_plain() {
            return None;
        }
        Some(Self {
            original_type,
            recipient,
            data,
            cluster_id: parsed.cluster_id,
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
    let data = SporeData::decode(data)?;
    data.is_plain().then_some((data.content_type, data.content))
}

fn basic_content_type(mime: &[u8]) -> bool {
    matches!(mime, b"text/plain" | b"application/octet-stream")
}

// A canonical parameter-free media type, optionally followed by the supported
// immortal flag. Multipart content and executable extensions have extra rules.
fn portable_mime(mime: &[u8]) -> bool {
    let Some(slash) = mime.iter().position(|byte| *byte == b'/') else {
        return false;
    };
    let token = |name: &[u8]| {
        name.first().is_some_and(u8::is_ascii_alphanumeric)
            && name
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$&-^_.+%'".contains(c))
    };
    let top = &mime[..slash];
    !top.eq_ignore_ascii_case(b"multipart") && token(top) && token(&mime[slash + 1..])
}

fn portable_content(mime: &[u8], content: &[u8]) -> bool {
    if content.is_empty() {
        return false;
    }
    let Some(rest) = mime.strip_prefix(b"multipart/") else {
        return portable_mime(mime);
    };
    let Some(separator) = rest.iter().position(|b| *b == b';') else {
        return false;
    };
    let Some(boundary) = rest[separator..].strip_prefix(b";boundary=") else {
        return false;
    };
    let token = |b: &u8| b.is_ascii_alphanumeric() || b"._-".contains(b);
    if separator == 0
        || !rest[..separator].iter().all(token)
        || !(1..=70).contains(&boundary.len())
        || !boundary.iter().all(token)
    {
        return false;
    }
    // Canonical multipart bodies in this profile have opening and closing
    // delimiter lines and retain the complete original bytes on both chains.
    let Some(first) = content
        .strip_prefix(b"--")
        .and_then(|s| s.strip_prefix(boundary))
        .and_then(|s| s.strip_prefix(b"\r\n"))
    else {
        return false;
    };
    first
        .strip_suffix(b"--\r\n")
        .and_then(|s| s.strip_suffix(boundary))
        .and_then(|s| s.strip_suffix(b"\r\n--"))
        .is_some_and(|body| !body.is_empty())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SporeData<'a> {
    pub content_type: &'a [u8],
    pub content: &'a [u8],
    pub cluster_id: Option<&'a Identity>,
}

impl<'a> SporeData<'a> {
    pub fn unclustered(data: &'a [u8]) -> Option<Self> {
        Self::from_cell(data).filter(|spore| spore.cluster_id.is_none())
    }

    pub fn immortal(&self) -> bool {
        self.content_type.ends_with(b";immortal=true")
    }

    pub fn is_plain(&self) -> bool {
        self.cluster_id.is_none() && basic_content_type(self.content_type)
    }

    pub fn decode(data: &'a [u8]) -> Option<Self> {
        (data.len() <= MAX_SPORE_DATA).then(|| Self::from_cell(data))?
    }

    pub fn from_cell(data: &'a [u8]) -> Option<Self> {
        let (header, _) = data.split_at_checked(16)?;
        let [total, mime, content, cluster] = header.as_chunks::<4>().0 else {
            return None;
        };
        if data.len() > MAX_CELL_SPORE_DATA
            || u32::from_le_bytes(*total) as usize != data.len()
            || u32::from_le_bytes(*mime) != 16
        {
            return None;
        }
        let content = u32::from_le_bytes(*content) as usize;
        let cluster = u32::from_le_bytes(*cluster) as usize;
        let content_type = molecule_bytes(data.get(16..content)?)?;
        let content = molecule_bytes(data.get(content..cluster)?)?;
        let cluster_id = match data.get(cluster..)? {
            [] => None,
            raw => {
                let id: &Identity = molecule_bytes(raw)?.try_into().ok()?;
                if id == &[0; 32] {
                    return None;
                }
                Some(id)
            }
        };
        // Custody preserves the original Cell through deposits and returns.
        // An immortal Spore cannot be melted; its mapped Cell redeems only by releasing that original.
        let mime = content_type
            .strip_suffix(b";immortal=true")
            .unwrap_or(content_type);
        portable_content(mime, content).then_some(Self {
            content_type,
            content,
            cluster_id,
        })
    }
}

fn molecule_bytes(raw: &[u8]) -> Option<&[u8]> {
    let (len, bytes) = raw.split_at_checked(4)?;
    (u32::from_le_bytes(len.try_into().ok()?) as usize == bytes.len()).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_receipts_are_canonical_at_both_size_limits() {
        let make_data = |size: usize| {
            let mut data = [size as u32, 16, 30, size as u32, 10]
                .map(u32::to_le_bytes)
                .concat();
            data.extend_from_slice(b"text/plain");
            data.extend_from_slice(&((size - 34) as u32).to_le_bytes());
            data.resize(size, b'x');
            data
        };
        let mut buf = [0; MAX_PAYLOAD];
        for size in [MAX_SPORE_DATA, MAX_SPORE_DATA + 1, MAX_CELL_SPORE_DATA] {
            let bytes = make_data(size);
            let encoded = CellTransfer::encode(&[1; 32], &[2; 32], &bytes, &mut buf).unwrap();
            let transfer = CellTransfer::decode(encoded).unwrap();
            assert_eq!(transfer.original_type, [1; 32]);
            assert_eq!(transfer.recipient, [2; 32]);
            if size == MAX_SPORE_DATA {
                assert_eq!(transfer.content, TransferContent::Inline(&bytes));
                assert!(transfer.content.matches(&bytes, || panic!("unneeded hash")));
                continue;
            }
            assert_eq!(encoded.len(), TRANSFER_HEADER + 40);
            let digest = hash(b"", &[&bytes]);
            assert!(transfer.content.matches(&bytes, || Some(digest)));
            assert!(
                !transfer
                    .content
                    .matches(&bytes[1..], || panic!("different size"))
            );
            assert!(!transfer.content.matches(&bytes, || Some([0; 32])));
            assert!(CellTransfer::decode(&encoded[..encoded.len() - 1]).is_none());
            assert!(CellTransfer::decode(&[encoded, &[0]].concat()).is_none());
            let mut malformed = encoded.to_vec();
            for bad in [0, MAX_SPORE_DATA, MAX_CELL_SPORE_DATA + 1] {
                malformed[68..72].copy_from_slice(&(bad as u32).to_le_bytes());
                assert!(CellTransfer::decode(&malformed).is_none());
            }
            malformed[..32].fill(0);
            assert!(CellTransfer::decode(&malformed).is_none());
        }
        let huge = make_data(MAX_CELL_SPORE_DATA + 1);
        assert!(CellTransfer::encode(&[1; 32], &[2; 32], &huge, &mut buf).is_none());
        assert!(CellTransfer::encode(&[1; 32], &[2; 32], b"invalid", &mut buf).is_none());
        assert!(CellTransfer::encode(&[0; 32], &[2; 32], &make_data(40), &mut buf).is_none());
    }

    #[test]
    fn multipart_profile_checks_the_full_boundary_and_refuses_unknown_parameters() {
        let mime = b"multipart/mixed;boundary=b1";
        assert!(portable_content(mime, b"--b1\r\nabc\r\n--b1--\r\n"));
        assert!(!portable_content(mime, b"--b1\r\nabc\r\n--b2--\r\n"));
        assert!(!portable_content(mime, b"--b12\r\nabc\r\n--b1--\r\n"));
        assert!(!portable_content(
            b"multipart/mixed;boundary=b1;mutant[]=x",
            b"x"
        ));
        assert!(!portable_content(b"multipart/mixed;boundary=", b"----\r\n"));
    }

    #[test]
    fn portable_mime_accepts_parameter_free_types_with_one_slash() {
        for mime in [
            b"image/svg+xml".as_slice(),
            b"application/json",
            b"image/png",
        ] {
            assert!(portable_mime(mime));
        }
        for mime in [
            b"text".as_slice(),
            b"/plain",
            b"text/",
            b"text//plain",
            b"multipart/mixed",
            b"Multipart/mixed",
            b"text/plain;mutant[]=7",
            b"image/ png",
        ] {
            assert!(!portable_mime(mime));
        }
    }

    #[test]
    fn immortal_custody_does_not_extend_the_plain_native_profile() {
        for mime in [
            b"text/plain;immortal=true".as_slice(),
            b"application/octet-stream;immortal=true",
        ] {
            let content = 20u32 + mime.len() as u32;
            let end = content + 5;
            let mut data = [end, 16, content, end, mime.len() as u32]
                .map(u32::to_le_bytes)
                .concat();
            data.extend_from_slice(mime);
            data.extend_from_slice(&[1, 0, 0, 0, b'x']);
            let mut message = [[1; 32], [2; 32]].concat();
            message.extend_from_slice(&data);
            assert_eq!(SporeData::decode(&data).unwrap().content_type, mime);
            assert!(SporeTransfer::decode(&message).is_some());
            assert!(SporeTransfer::decode_plain(&message).is_none());
            assert!(!plain_spore(&data));
            data[content as usize - 1] = b'X';
            assert!(SporeData::decode(&data).is_none());
        }
    }

    #[test]
    fn canonical_spore_accepts_only_a_complete_optional_cluster_id() {
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
        assert!(SporeData::decode(&data).is_none());
        data[20] = b't';
        data[0] = 72;
        data.extend_from_slice(&32u32.to_le_bytes());
        data.extend_from_slice(&[7; 32]);
        let decoded = SporeData::decode(&data).unwrap();
        assert_eq!(decoded.cluster_id, Some(&[7; 32]));
        assert_eq!(decoded.content, b"hi");
        assert!(!plain_spore(&data));
        let mut message = [[1; 32], [2; 32]].concat();
        message.extend_from_slice(&data);
        assert_eq!(
            SporeTransfer::decode(&message).unwrap().cluster_id,
            Some(&[7; 32])
        );
        assert!(SporeTransfer::decode_plain(&message).is_none());
        data[40..].fill(0);
        assert!(SporeData::decode(&data).is_none());
        data[40] = 7;
        data[36] = 31;
        assert!(SporeData::decode(&data).is_none());
    }
}
