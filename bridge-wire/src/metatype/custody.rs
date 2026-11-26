//! Shared identities and principal projection for Metatype custody.

use super::{Hash, MAX_STATE_BYTES, ManifestCell, MetatypeManifest, fungible::Amounts};
use crate::{
    hash,
    object::{revision, valid_revision_transition},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Profile {
    Immutable = 1,
    Fungible = 2,
    Revisable = 3,
    MultiAsset = 4,
}

impl Profile {
    fn decode(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Immutable),
            2 => Some(Self::Fungible),
            3 => Some(Self::Revisable),
            4 => Some(Self::MultiAsset),
            _ => None,
        }
    }

    fn object_state(self, data: &[u8]) -> Option<&[u8]> {
        match self {
            Self::Immutable if data.len() <= MAX_STATE_BYTES => Some(data),
            Self::Revisable if revision(data).is_some() => Some(data),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Custody {
    pub profile: Profile,
    pub mailbox_code: Hash,
    pub mailbox_id: Hash,
    pub guard_code: Hash,
    pub manifest: Hash,
    pub data_script: Hash,
}

impl Custody {
    pub const SIZE: usize = 1 + 5 * 32;

    pub fn decode(raw: &[u8]) -> Option<Self> {
        let (&tag, raw) = raw.split_first()?;
        let profile = Profile::decode(tag)?;
        let (fields, tail) = raw.as_chunks::<32>();
        let [mailbox_code, mailbox_id, guard_code, manifest, data_script] = fields else {
            return None;
        };
        (tail.is_empty() && !fields.contains(&[0; 32])).then_some(Self {
            profile,
            mailbox_code: *mailbox_code,
            mailbox_id: *mailbox_id,
            guard_code: *guard_code,
            manifest: *manifest,
            data_script: *data_script,
        })
    }

    /// The on-chain projection commits every original Script. One explicitly
    /// identified Script carries the original Cell Data.
    pub fn data<'a>(&self, manifest: &MetatypeManifest<'a>) -> Option<&'a [u8]> {
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

    pub fn authorize_object_state(&self, before: &[u8], after: &[u8], exporting: bool) -> bool {
        match self.profile {
            Profile::Immutable => before == after,
            Profile::Revisable if exporting => before == after && revision(before).is_some(),
            Profile::Revisable => valid_revision_transition(before, after),
            Profile::Fungible | Profile::MultiAsset => false,
        }
    }

    pub fn transition<'a>(
        &self,
        inputs: &[ManifestCell<'a>],
        outputs: &[ManifestCell<'a>],
    ) -> Option<(Boundary, Principal<'a>)> {
        match self.profile {
            Profile::Immutable | Profile::Revisable => {
                let direction = Boundary::for_cardinality(inputs.len(), outputs.len())?;
                let data = match (inputs, outputs) {
                    ([(_, before)], [(_, after)]) => {
                        let previous = self.data(before)?;
                        let next = self.data(after)?;
                        self.authorize_object_state(previous, next, false)
                            .then_some(next)?
                    }
                    ([], [(_, data)]) | ([(_, data)], []) => self.data(data)?,
                    _ => return None,
                };
                self.profile.object_state(data)?;
                Some((direction, Principal::Object(data)))
            }
            Profile::Fungible => {
                let total = |cells: &[ManifestCell<'_>]| {
                    cells.iter().try_fold(0u128, |sum, (_, data)| {
                        sum.checked_add(positive_amount(self.data(data)?)?)
                    })
                };
                let (before, after) = (total(inputs)?, total(outputs)?);
                let (direction, delta) = match (before, after) {
                    (0, 0) => return None,
                    (0, issued) => (Boundary::Import, issued),
                    (a, b) if a == b => (Boundary::Transfer, 0),
                    (a, b) if a > b => (Boundary::Return, a - b),
                    _ => return None,
                };
                Some((direction, Principal::Fungible(delta)))
            }
            Profile::MultiAsset => {
                let (boundary, amounts) = match (self.amounts(inputs)?, self.amounts(outputs)?) {
                    (None, None) => return None,
                    (None, Some(output)) => (Boundary::Import, output),
                    (Some(input), None) => (Boundary::Return, input),
                    (Some(input), Some(output)) if input == output => (Boundary::Transfer, input),
                    (Some(input), Some(output)) => (Boundary::Return, input.decrease(&output)?),
                };
                Some((boundary, Principal::MultiAsset(amounts)))
            }
        }
    }

    fn amounts(&self, cells: &[ManifestCell<'_>]) -> Option<Option<Amounts>> {
        let mut sums: Option<Amounts> = None;
        for (_, manifest) in cells {
            if manifest.id() != self.manifest
                || manifest
                    .entries()
                    .iter()
                    .any(|entry| entry.script.code().1 != 0)
            {
                return None;
            }
            let cell = Amounts::from_states(manifest.entries().iter().filter_map(|entry| {
                (!entry.state.is_empty()).then(|| (entry.script.id(), entry.state))
            }))?;
            if cell.layout_id() != self.data_script {
                return None;
            }
            sums = Some(match sums {
                None => cell,
                Some(total) => total.checked_add(&cell)?,
            });
        }
        Some(sums)
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Principal<'a> {
    Object(&'a [u8]),
    Fungible(u128),
    MultiAsset(Amounts),
}

pub fn positive_amount(data: &[u8]) -> Option<u128> {
    crate::amount(data).filter(|amount| *amount > 0)
}

pub struct Transfer<'a> {
    pub manifest: Hash,
    pub recipient: Hash,
    pub principal: Principal<'a>,
}

impl<'a> Transfer<'a> {
    pub const HEADER: usize = 2 * 32;

    pub fn decode(profile: Profile, raw: &'a [u8]) -> Option<Self> {
        let (manifest, raw) = raw.split_first_chunk::<32>()?;
        let (recipient, data) = raw.split_first_chunk::<32>()?;
        let principal = match profile {
            Profile::Immutable | Profile::Revisable => {
                Principal::Object(profile.object_state(data)?)
            }
            Profile::Fungible => Principal::Fungible(positive_amount(data)?),
            Profile::MultiAsset => Principal::MultiAsset(Amounts::decode(data)?),
        };
        (*manifest != [0; 32] && *recipient != [0; 32]).then_some(Self {
            manifest: *manifest,
            recipient: *recipient,
            principal,
        })
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
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

    #[test]
    fn custody_matches_the_complete_immutable_projection() {
        let identity = LogicalScript::encode(&[1; 32], 0, b"owner").unwrap();
        let policy = LogicalScript::encode(&[2; 32], 0, b"owner").unwrap();
        let identity_id = LogicalScript::decode(&identity).unwrap().id();
        let policy_id = LogicalScript::decode(&policy).unwrap().id();
        let custody = Custody {
            profile: Profile::Immutable,
            mailbox_code: [3; 32],
            mailbox_id: [4; 32],
            guard_code: [5; 32],
            manifest: manifest_id(&[identity_id, policy_id]),
            data_script: identity_id,
        };
        let full = frame(&[(&identity, b"state"), (&policy, &[])]);
        assert_eq!(
            custody.data(&MetatypeManifest::decode(&full).unwrap()),
            Some(b"state".as_slice())
        );
        assert!(
            custody
                .data(&MetatypeManifest::decode(&frame(&[(&identity, b"state")])).unwrap())
                .is_none()
        );
        assert!(
            custody
                .data(
                    &MetatypeManifest::decode(&frame(&[
                        (&identity, b"state"),
                        (&policy, b"extra")
                    ]))
                    .unwrap()
                )
                .is_none()
        );
        let upgraded = LogicalScript::encode(&[2; 32], 1, b"owner").unwrap();
        let type_addressed = frame(&[(&identity, b"state"), (&upgraded, &[])]);
        let policy_id = LogicalScript::decode(&upgraded).unwrap().id();
        let custody = Custody {
            manifest: manifest_id(&[identity_id, policy_id]),
            ..custody
        };
        assert!(
            custody
                .data(&MetatypeManifest::decode(&type_addressed).unwrap())
                .is_none()
        );
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

    fn indexed(index: u16, raw: &[u8]) -> ManifestCell<'_> {
        (index, MetatypeManifest::decode(raw).unwrap())
    }

    #[test]
    fn fungible_projection_sums_all_heads_and_checks_deltas() {
        let asset = LogicalScript::encode(&[1; 32], 0, b"asset").unwrap();
        let policy = LogicalScript::encode(&[2; 32], 0, b"limit").unwrap();
        let asset_id = LogicalScript::decode(&asset).unwrap().id();
        let custody = Custody {
            profile: Profile::Fungible,
            mailbox_code: [3; 32],
            mailbox_id: [4; 32],
            guard_code: [5; 32],
            manifest: manifest_id(&[asset_id, LogicalScript::decode(&policy).unwrap().id()]),
            data_script: asset_id,
        };
        let row = |units: u128| frame(&[(&asset, &units.to_le_bytes()), (&policy, &[])]);
        let a = row(125);
        let b = row(100);
        let outputs = [indexed(2, &a), indexed(4, &a)];
        assert_eq!(
            custody.transition(&[], &outputs),
            Some((Boundary::Import, Principal::Fungible(250)))
        );
        assert_eq!(
            custody.transition(&outputs, &outputs),
            Some((Boundary::Transfer, Principal::Fungible(0)))
        );
        assert_eq!(
            custody.transition(&outputs, &[indexed(6, &b)]),
            Some((Boundary::Return, Principal::Fungible(150)))
        );
        assert_eq!(
            custody.transition(&outputs, &[]),
            Some((Boundary::Return, Principal::Fungible(250)))
        );
        assert_eq!(custody.transition(&[indexed(0, &a)], &outputs), None);
        assert_eq!(custody.transition(&[], &[indexed(0, &row(0))]), None);
        assert_eq!(
            custody.transition(&[], &[indexed(0, &row(u128::MAX)), indexed(1, &b)]),
            None
        );
        let stripped = frame(&[(&asset, &125u128.to_le_bytes())]);
        assert_eq!(custody.transition(&outputs, &[indexed(0, &stripped)]), None);
        let gate = Gate {
            stem_escrow: [6; 32],
            metatype: [7; 32],
            custody,
        };
        assert_eq!(Gate::decode(&gate.args(), custody.guard_code), Some(gate));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Gate {
    pub stem_escrow: Hash,
    pub metatype: Hash,
    pub custody: Custody,
}

impl Gate {
    pub const SIZE: usize = 1 + 6 * 32;

    pub fn decode(raw: &[u8], guard_code: Hash) -> Option<Self> {
        let (&tag, raw) = raw.split_first()?;
        let profile = Profile::decode(tag)?;
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
                profile,
                mailbox_code: *mailbox_code,
                mailbox_id: *mailbox_id,
                guard_code,
                manifest: *manifest,
                data_script: *data_script,
            },
        })
    }

    pub fn args(self) -> [u8; Self::SIZE] {
        let fields = [
            self.stem_escrow,
            self.metatype,
            self.custody.mailbox_code,
            self.custody.mailbox_id,
            self.custody.manifest,
            self.custody.data_script,
        ];
        let mut raw = [0; Self::SIZE];
        raw[0] = self.custody.profile as u8;
        raw[1..].copy_from_slice(fields.as_flattened());
        raw
    }
}
