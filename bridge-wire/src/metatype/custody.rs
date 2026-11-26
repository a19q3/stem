//! Shared identities and immutable data projection for Metatype custody.

use super::{Hash, MAX_STATE_BYTES, MetatypeManifest};
use crate::hash;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Custody {
    pub mailbox_code: Hash,
    pub mailbox_id: Hash,
    pub guard_code: Hash,
    pub manifest: Hash,
    pub data_script: Hash,
}

impl Custody {
    pub const SIZE: usize = 5 * 32;

    pub fn decode(raw: &[u8]) -> Option<Self> {
        let (fields, tail) = raw.as_chunks::<32>();
        let [mailbox_code, mailbox_id, guard_code, manifest, data_script] = fields else {
            return None;
        };
        (tail.is_empty() && !fields.contains(&[0; 32])).then_some(Self {
            mailbox_code: *mailbox_code,
            mailbox_id: *mailbox_id,
            guard_code: *guard_code,
            manifest: *manifest,
            data_script: *data_script,
        })
    }

    /// The on-chain projection commits every original Script. One explicitly
    /// identified Script carries the original immutable Cell Data.
    pub fn data<'a>(&self, raw: &'a [u8]) -> Option<&'a [u8]> {
        let manifest = MetatypeManifest::decode(raw)?;
        if manifest.id() != self.manifest {
            return None;
        }
        let mut data = None;
        for entry in manifest.entries() {
            if entry.script.code().1 != 0 {
                return None;
            }
            if entry.script.id() == self.data_script {
                data = Some(entry.state);
            } else if !entry.state.is_empty() {
                return None;
            }
        }
        data
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Boundary {
    Standalone = 0,
    Transfer = 1,
    Import = 2,
    Return = 3,
}

impl Boundary {
    pub(super) fn decode(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Standalone,
            1 => Self::Transfer,
            2 => Self::Import,
            3 => Self::Return,
            _ => return None,
        })
    }

    pub fn for_cardinality(inputs: usize, outputs: usize) -> Option<Self> {
        Some(match (inputs, outputs) {
            (1, 1) => Self::Transfer,
            (0, 1) => Self::Import,
            (1, 0) => Self::Return,
            _ => return None,
        })
    }
}

pub fn manifest_id(script_ids: &[Hash]) -> Hash {
    hash(b"Stem Metatype manifest\0", &[script_ids.as_flattened()])
}

pub fn escrow_nonce(outpoint: &[u8; 36]) -> u64 {
    let raw = hash(b"Stem Metatype escrow nonce\0", &[outpoint]);
    u64::from_le_bytes(raw[..8].try_into().expect("fixed nonce"))
}

pub struct ImmutableTransfer<'a> {
    pub manifest: Hash,
    pub recipient: Hash,
    pub data: &'a [u8],
}

impl<'a> ImmutableTransfer<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (manifest, raw) = raw.split_first_chunk::<32>()?;
        let (recipient, data) = raw.split_first_chunk::<32>()?;
        (*manifest != [0; 32] && *recipient != [0; 32] && data.len() <= MAX_STATE_BYTES).then_some(
            Self {
                manifest: *manifest,
                recipient: *recipient,
                data,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;

    #[test]
    fn custody_matches_the_complete_immutable_projection() {
        use crate::metatype::LogicalScript;
        use alloc::vec::Vec;

        fn frame(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
            let mut raw = alloc::vec![entries.len() as u8];
            for (script, data) in entries {
                for field in [script, data] {
                    raw.extend_from_slice(&(field.len() as u16).to_le_bytes());
                    raw.extend_from_slice(field);
                }
            }
            raw
        }

        let identity = LogicalScript::encode(&[1; 32], 0, b"owner").unwrap();
        let policy = LogicalScript::encode(&[2; 32], 0, b"owner").unwrap();
        let identity_id = LogicalScript::decode(&identity).unwrap().id();
        let policy_id = LogicalScript::decode(&policy).unwrap().id();
        let custody = Custody {
            mailbox_code: [3; 32],
            mailbox_id: [4; 32],
            guard_code: [5; 32],
            manifest: manifest_id(&[identity_id, policy_id]),
            data_script: identity_id,
        };
        let full = frame(&[(&identity, b"state"), (&policy, &[])]);
        assert_eq!(custody.data(&full), Some(b"state".as_slice()));
        assert!(custody.data(&frame(&[(&identity, b"state")])).is_none());
        assert!(
            custody
                .data(&frame(&[(&identity, b"state"), (&policy, b"extra")]))
                .is_none()
        );
        let upgraded = LogicalScript::encode(&[2; 32], 1, b"owner").unwrap();
        let type_addressed = frame(&[(&identity, b"state"), (&upgraded, &[])]);
        let policy_id = LogicalScript::decode(&upgraded).unwrap().id();
        let custody = Custody {
            manifest: manifest_id(&[identity_id, policy_id]),
            ..custody
        };
        assert!(custody.data(&type_addressed).is_none());
    }

    #[test]
    fn custody_boundary_disallows_duplicates_and_bypasses() {
        assert_eq!(Boundary::for_cardinality(1, 1), Some(Boundary::Transfer));
        assert_eq!(Boundary::for_cardinality(0, 1), Some(Boundary::Import));
        assert_eq!(Boundary::for_cardinality(1, 0), Some(Boundary::Return));
        assert_eq!(Boundary::for_cardinality(0, 0), None);
        assert_eq!(Boundary::for_cardinality(2, 1), None);
        assert_eq!(Boundary::for_cardinality(1, 2), None);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gate {
    pub stem_escrow: Hash,
    pub metatype: Hash,
    pub custody: Custody,
}

impl Gate {
    pub fn decode(raw: &[u8], guard_code: Hash) -> Option<Self> {
        let (items, tail) = raw.as_chunks::<32>();
        let [
            stem_escrow,
            metatype,
            mailbox_code,
            mailbox_id,
            manifest,
            data_script,
        ] = items
        else {
            return None;
        };
        (tail.is_empty() && !items.contains(&[0; 32]) && guard_code != [0; 32]).then_some(Self {
            stem_escrow: *stem_escrow,
            metatype: *metatype,
            custody: Custody {
                mailbox_code: *mailbox_code,
                mailbox_id: *mailbox_id,
                guard_code,
                manifest: *manifest,
                data_script: *data_script,
            },
        })
    }

    pub fn args(self) -> [[u8; 32]; 6] {
        [
            self.stem_escrow,
            self.metatype,
            self.custody.mailbox_code,
            self.custody.mailbox_id,
            self.custody.manifest,
            self.custody.data_script,
        ]
    }
}
