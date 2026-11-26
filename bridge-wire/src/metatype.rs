//! Canonical Metatype Cell data and virtual Type Group projection.

extern crate alloc;

use alloc::{collections::BTreeMap, vec::Vec};

use crate::HASH_SIZE;

pub mod custody;
pub mod fungible;
pub mod object_policy;
pub mod rotating_approval;
pub mod royalty;
pub mod witness;
pub mod workflow;
use custody::{Boundary, Custody, Profile, manifest_id};
use witness::{GroupWitness, MAX_FIELDS_BYTES};

pub const MAX_TYPE_SCRIPTS: usize = 16;
pub const MAX_GROUPS: usize = 32;
pub const MAX_CELLS_PER_SIDE: usize = 64;
pub const MAX_TRANSACTION_CELLS_PER_SIDE: usize = 128;
pub const MAX_SCRIPT_BYTES: usize = 256;
pub const MAX_STATE_BYTES: usize = 2048;
pub const MAX_MANIFEST_BYTES: usize = 8192;
pub const MAX_MANIFEST_DEFINITION_BYTES: usize = 1 + MAX_TYPE_SCRIPTS * (4 + MAX_SCRIPT_BYTES);
pub const MAX_ADAPTERS: usize = 64;
pub const MAX_ADAPTER_TABLE_BYTES: usize = 1 + 65 * MAX_ADAPTERS + Custody::SIZE;
pub const MAX_LIVE_DATA: usize = 128 * 1024;

type Hash = [u8; HASH_SIZE];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetatypeDeployment {
    pub ckb_genesis: Hash,
    pub namespace: Hash,
    pub adapter_table_data_hash: Hash,
}

impl MetatypeDeployment {
    pub fn decode(args: &[u8]) -> Option<Self> {
        let [ckb_genesis, namespace, adapter_table_data_hash] = args.as_chunks::<HASH_SIZE>().0
        else {
            return None;
        };
        (args.len() == 3 * HASH_SIZE
            && *ckb_genesis != [0; HASH_SIZE]
            && *namespace != [0; HASH_SIZE]
            && *adapter_table_data_hash != [0; HASH_SIZE])
            .then_some(Self {
                ckb_genesis: *ckb_genesis,
                namespace: *namespace,
                adapter_table_data_hash: *adapter_table_data_hash,
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogicalScript<'a> {
    encoded: &'a [u8],
}

impl<'a> LogicalScript<'a> {
    /// A complete CKB Molecule Script with Stem's Data or Type addressing.
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        const HEADER: [u8; 12] = [16, 0, 0, 0, 48, 0, 0, 0, 49, 0, 0, 0];
        if !(53..=MAX_SCRIPT_BYTES).contains(&raw.len())
            || raw.get(..4)? != (raw.len() as u32).to_le_bytes()
            || raw.get(4..16)? != HEADER
            || raw.get(16..48)? == [0; HASH_SIZE]
            || !matches!(raw[48], 0 | 1)
            || raw.get(49..53)? != ((raw.len() - 53) as u32).to_le_bytes()
        {
            return None;
        }
        Some(Self { encoded: raw })
    }

    pub fn encode(code_hash: &Hash, addressing: u8, args: &[u8]) -> Option<Vec<u8>> {
        let length = 53usize.checked_add(args.len())?;
        if length > MAX_SCRIPT_BYTES || !matches!(addressing, 0 | 1) || *code_hash == [0; 32] {
            return None;
        }
        let mut raw = Vec::with_capacity(length);
        for field in [length as u32, 16, 48, 49] {
            raw.extend_from_slice(&field.to_le_bytes());
        }
        raw.extend_from_slice(code_hash);
        raw.push(addressing);
        raw.extend_from_slice(&(args.len() as u32).to_le_bytes());
        raw.extend_from_slice(args);
        Some(raw)
    }

    pub fn as_slice(self) -> &'a [u8] {
        self.encoded
    }

    pub fn code(self) -> (&'a Hash, u8) {
        (
            self.encoded[16..48].try_into().expect("decoded Script"),
            self.encoded[48],
        )
    }

    pub fn args(self) -> &'a [u8] {
        &self.encoded[53..]
    }

    pub fn id(self) -> Hash {
        ckb_hash::blake2b_256(self.encoded)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Constraint<'a> {
    pub script: LogicalScript<'a>,
    pub state: &'a [u8],
}

#[derive(Debug, Eq, PartialEq)]
pub struct MetatypeManifest<'a> {
    entries: Vec<Constraint<'a>>,
}

pub type ManifestCell<'a> = (u16, MetatypeManifest<'a>);

impl<'a> MetatypeManifest<'a> {
    pub fn decode(mut raw: &'a [u8]) -> Option<Self> {
        if raw.len() > MAX_MANIFEST_BYTES {
            return None;
        }
        let count = usize::from(*raw.split_first()?.0);
        if !(1..=MAX_TYPE_SCRIPTS).contains(&count) {
            return None;
        }
        raw = &raw[1..];
        let mut entries: Vec<Constraint<'a>> = Vec::with_capacity(count);
        for _ in 0..count {
            let script = LogicalScript::decode(take_bytes(&mut raw)?)?;
            let state = take_bytes(&mut raw)?;
            if state.len() > MAX_STATE_BYTES
                || entries
                    .last()
                    .is_some_and(|item| item.script.encoded >= script.encoded)
            {
                return None;
            }
            entries.push(Constraint { script, state });
        }
        raw.is_empty().then_some(Self { entries })
    }

    pub fn entries(&self) -> &[Constraint<'a>] {
        &self.entries
    }

    pub fn state_for(&self, script: LogicalScript<'_>) -> Option<&'a [u8]> {
        self.entries
            .binary_search_by(|entry| entry.script.encoded.cmp(script.encoded))
            .ok()
            .map(|index| self.entries[index].state)
    }

    pub fn id(&self) -> Hash {
        let ids: Vec<_> = self.entries.iter().map(|entry| entry.script.id()).collect();
        manifest_id(&ids)
    }
}

/// Canonical Script ranges in a validated, owned definition Cell.
pub struct ManifestDefinition {
    raw: Vec<u8>,
    ranges: Vec<core::ops::Range<usize>>,
}

impl ManifestDefinition {
    pub fn decode(raw: Vec<u8>) -> Option<Self> {
        if raw.len() > MAX_MANIFEST_DEFINITION_BYTES {
            return None;
        }
        let manifest = MetatypeManifest::decode(&raw)?;
        let mut ranges = Vec::with_capacity(manifest.entries.len());
        let mut offset = 1;
        for entry in manifest.entries {
            if !entry.state.is_empty() {
                return None;
            }
            let start = offset + 2;
            let end = start + entry.script.as_slice().len();
            ranges.push(start..end);
            offset = end + 2;
        }
        Some(Self { raw, ranges })
    }
}

/// A Cell's state vector and a commitment to its shared Script set.
/// Zero is reserved for references in the Cell Data discriminant.
pub struct ManifestReference<'a> {
    pub definition_hash: Hash,
    states: &'a [u8],
}

impl<'a> ManifestReference<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (&tag, rest) = raw.split_first()?;
        let (definition_hash, states) = rest.split_first_chunk::<HASH_SIZE>()?;
        (tag == 0 && *definition_hash != [0; HASH_SIZE] && raw.len() <= MAX_MANIFEST_BYTES)
            .then_some(Self {
                definition_hash: *definition_hash,
                states,
            })
    }

    pub fn expanded_len(&self, definition: &ManifestDefinition, limit: usize) -> Option<usize> {
        let length = definition
            .raw
            .len()
            .checked_add(self.states.len().checked_sub(2 * definition.ranges.len())?)?;
        (length <= MAX_MANIFEST_BYTES.min(limit)).then_some(length)
    }

    /// Borrow the logical view from a definition whose CKB data hash the
    /// host authenticated against `definition_hash`.
    pub fn view(
        &self,
        definition: &'a ManifestDefinition,
        limit: usize,
    ) -> Option<MetatypeManifest<'a>> {
        self.expanded_len(definition, limit)?;
        let mut entries = Vec::with_capacity(definition.ranges.len());
        let mut states = self.states;
        for range in &definition.ranges {
            let state = take_bytes(&mut states)?;
            if state.len() > MAX_STATE_BYTES {
                return None;
            }
            entries.push(Constraint {
                script: LogicalScript {
                    encoded: &definition.raw[range.clone()],
                },
                state,
            });
        }
        states.is_empty().then_some(MetatypeManifest { entries })
    }
}

pub struct MetatypeAdapterTable<'a> {
    entries: &'a [[u8; 65]],
    pub custody: Option<Custody>,
}

impl<'a> MetatypeAdapterTable<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (&count, raw) = raw.split_first()?;
        if !(1..=MAX_ADAPTERS).contains(&usize::from(count)) {
            return None;
        }
        let (entries, tail) = raw.split_at_checked(65 * usize::from(count))?;
        let (entries, _) = entries.as_chunks::<65>();
        if !entries.windows(2).all(|pair| pair[0][..33] < pair[1][..33])
            || entries.iter().any(|entry| {
                entry[..32] == [0; HASH_SIZE]
                    || !matches!(entry[32], 0 | 1)
                    || entry[33..] == [0; HASH_SIZE]
            })
        {
            return None;
        }
        let custody = match tail {
            [0] => None,
            raw => Some(Custody::decode(raw)?),
        };
        Some(Self { entries, custody })
    }

    pub fn adapter(&self, script: LogicalScript<'_>) -> Option<&'a Hash> {
        let (code, addressing) = script.code();
        self.entries
            .binary_search_by(|entry| entry[..32].cmp(code).then(entry[32].cmp(&addressing)))
            .ok()
            .map(|index| {
                self.entries[index][33..]
                    .try_into()
                    .expect("decoded adapter table")
            })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct VirtualTypeGroup<'a> {
    pub script: LogicalScript<'a>,
    pub input_indices: Vec<u16>,
    pub output_indices: Vec<u16>,
}

/// Project absolute CKB positions to virtual groups after the host checks
/// the full physical Type Script of every participating Cell.
pub fn project<'a>(
    inputs: &[ManifestCell<'a>],
    outputs: &[ManifestCell<'a>],
) -> Option<Vec<VirtualTypeGroup<'a>>> {
    let mut groups = BTreeMap::new();
    for (is_output, cells) in [(false, inputs), (true, outputs)] {
        if cells.len() > MAX_CELLS_PER_SIDE || !cells.windows(2).all(|pair| pair[0].0 < pair[1].0) {
            return None;
        }
        for (index, manifest) in cells {
            let index = *index;
            if usize::from(index) >= MAX_TRANSACTION_CELLS_PER_SIDE {
                return None;
            }
            for constraint in &manifest.entries {
                let group =
                    groups
                        .entry(constraint.script.encoded)
                        .or_insert_with(|| VirtualTypeGroup {
                            script: constraint.script,
                            input_indices: Vec::new(),
                            output_indices: Vec::new(),
                        });
                if is_output {
                    group.output_indices.push(index);
                } else {
                    group.input_indices.push(index);
                }
                if groups.len() > MAX_GROUPS {
                    return None;
                }
            }
        }
    }
    (!groups.is_empty()).then(|| groups.into_values().collect())
}

pub const MAX_INVOCATION_BYTES: usize =
    33 + 2 + MAX_SCRIPT_BYTES + 2 * (1 + 2 * MAX_CELLS_PER_SIDE) + MAX_FIELDS_BYTES;

pub struct Invocation<'a> {
    pub deployment_type: &'a Hash,
    pub boundary: Boundary,
    pub profile: Option<Profile>,
    pub group: VirtualTypeGroup<'a>,
    pub witness: Option<GroupWitness<'a>>,
}

impl<'a> Invocation<'a> {
    pub fn encode(&self) -> Vec<u8> {
        let mut raw = self.deployment_type.to_vec();
        raw.push(self.profile.map_or(0, |profile| (profile as u8) << 2) | self.boundary as u8);
        let script = self.group.script.as_slice();
        raw.extend_from_slice(&(script.len() as u16).to_le_bytes());
        raw.extend_from_slice(script);
        for indices in [&self.group.input_indices, &self.group.output_indices] {
            raw.push(indices.len() as u8);
            for index in indices {
                raw.extend_from_slice(&index.to_le_bytes());
            }
        }
        if let Some(witness) = self.witness {
            witness.write_into(&mut raw);
        }
        raw
    }

    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        if raw.len() > MAX_INVOCATION_BYTES {
            return None;
        }
        let (deployment, raw) = raw.split_at_checked(HASH_SIZE)?;
        let (&scope, mut raw) = raw.split_first()?;
        let boundary = Boundary::decode(scope & 3)?;
        let profile = match (boundary, scope >> 2) {
            (Boundary::Standalone, 0) => None,
            (Boundary::Standalone, _) => return None,
            (_, tag) => Some(Profile::decode(tag)?),
        };
        let script = LogicalScript::decode(take_bytes(&mut raw)?)?;
        let input_indices = take_indices(&mut raw)?;
        let output_indices = take_indices(&mut raw)?;
        let witness = if raw.is_empty() {
            None
        } else {
            Some(GroupWitness::take(&mut raw)?)
        };
        (raw.is_empty()
            && deployment != [0; HASH_SIZE]
            && !(input_indices.is_empty() && output_indices.is_empty()))
        .then_some(Self {
            witness,
            boundary,
            profile,
            deployment_type: deployment.try_into().ok()?,
            group: VirtualTypeGroup {
                script,
                input_indices,
                output_indices,
            },
        })
    }
}

fn take_bytes<'a>(raw: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (len, tail) = raw.split_first_chunk::<2>()?;
    let (bytes, rest) = tail.split_at_checked(usize::from(u16::from_le_bytes(*len)))?;
    *raw = rest;
    Some(bytes)
}

fn take_indices(raw: &mut &[u8]) -> Option<Vec<u16>> {
    let (&count, tail) = raw.split_first()?;
    if usize::from(count) > MAX_CELLS_PER_SIDE {
        return None;
    }
    let (encoded, rest) = tail.split_at_checked(2 * usize::from(count))?;
    let indices: Vec<_> = encoded
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes(*bytes))
        .collect();
    if indices
        .last()
        .is_some_and(|&index| usize::from(index) >= MAX_TRANSACTION_CELLS_PER_SIDE)
        || !indices.windows(2).all(|pair| pair[0] < pair[1])
    {
        return None;
    }
    *raw = rest;
    Some(indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(code: u8, arg: u8) -> Vec<u8> {
        LogicalScript::encode(&[code; HASH_SIZE], 0, &[arg]).unwrap()
    }

    fn manifest(entries: &[&[u8]]) -> Vec<u8> {
        let mut encoded = alloc::vec![entries.len() as u8];
        for script in entries {
            encoded.extend_from_slice(&(script.len() as u16).to_le_bytes());
            encoded.extend_from_slice(script);
            encoded.extend_from_slice(&0_u16.to_le_bytes());
        }
        encoded
    }

    fn indexed(index: u16, raw: &[u8]) -> ManifestCell<'_> {
        (index, MetatypeManifest::decode(raw).unwrap())
    }

    #[test]
    fn heterogeneous_manifests_share_complete_virtual_groups() {
        let [a, b, c] = [script(1, 0), script(1, 1), script(2, 0)];
        let ab = manifest(&[&a, &b]);
        let ac = manifest(&[&a, &c]);
        let bc = manifest(&[&b, &c]);
        let groups = project(
            &[indexed(1, &ab), indexed(3, &ac), indexed(5, &bc)],
            &[indexed(2, &bc)],
        )
        .unwrap();
        assert_eq!(groups.len(), 3);
        let indices = [
            (&a, &[1, 3][..], &[][..]),
            (&b, &[1, 5][..], &[2][..]),
            (&c, &[3, 5][..], &[2][..]),
        ];
        for (group, (script, inputs, outputs)) in groups.into_iter().zip(indices) {
            assert_eq!(group.script.as_slice(), script);
            assert_eq!(group.input_indices, inputs);
            assert_eq!(group.output_indices, outputs);
            let invocation = Invocation {
                deployment_type: &[8; HASH_SIZE],
                boundary: Boundary::Standalone,
                profile: None,
                group,
                witness: Some(GroupWitness {
                    action: 1,
                    message: b"hello",
                    proof: &[],
                    seal: &[],
                }),
            };
            let encoded = invocation.encode();
            let decoded = Invocation::decode(&encoded).unwrap();
            assert_eq!(decoded.group, invocation.group);
            assert_eq!(decoded.witness, invocation.witness);
            assert_eq!(decoded.boundary, invocation.boundary);
            assert_eq!(decoded.profile, invocation.profile);
            assert_eq!(decoded.deployment_type, invocation.deployment_type);

            let mut encoded = Invocation {
                boundary: Boundary::Return,
                profile: Some(Profile::MultiAsset),
                ..invocation
            }
            .encode();
            let decoded = Invocation::decode(&encoded).unwrap();
            assert_eq!(decoded.boundary, Boundary::Return);
            assert_eq!(decoded.profile, Some(Profile::MultiAsset));
            for malformed in [3, 4 << 2, (5 << 2) | 3] {
                encoded[HASH_SIZE] = malformed;
                assert!(Invocation::decode(&encoded).is_none());
            }
        }
    }

    #[test]
    fn rejects_noncanonical_manifest_bounds_and_group_positions() {
        let [a, b] = [script(1, 0), script(2, 0)];
        assert!(MetatypeManifest::decode(&manifest(&[&b, &a])).is_none());
        assert!(MetatypeManifest::decode(&manifest(&[&a, &a])).is_none());
        assert!(MetatypeManifest::decode(&manifest(&[a.as_slice(); 17])).is_none());
        let mut ab = manifest(&[&a, &b]);
        assert!(project(&[indexed(1, &ab), indexed(1, &ab)], &[]).is_none());
        ab.push(0);
        assert!(MetatypeManifest::decode(&ab).is_none());
    }

    #[test]
    fn shared_script_sets_expand_into_the_complete_group_view() {
        let [a, b] = [script(1, 0), script(2, 0)];
        let definition = manifest(&[&a, &b]);
        let hash = ckb_hash::blake2b_256(&definition);
        let mut reference = alloc::vec![0];
        reference.extend_from_slice(&hash);
        reference.extend_from_slice(b"\x03\x00one\x03\x00two");
        let parsed = ManifestReference::decode(&reference).unwrap();
        assert_eq!(parsed.definition_hash, hash);
        let mut expanded = alloc::vec![2];
        for (script, state) in [(&a, b"one"), (&b, b"two")] {
            for field in [script.as_slice(), state.as_slice()] {
                expanded.extend_from_slice(&(field.len() as u16).to_le_bytes());
                expanded.extend_from_slice(field);
            }
        }
        let definition = ManifestDefinition::decode(definition).unwrap();
        let view = parsed.view(&definition, MAX_MANIFEST_BYTES).unwrap();
        assert_eq!(view, MetatypeManifest::decode(&expanded).unwrap());
        assert_eq!(
            parsed.expanded_len(&definition, MAX_MANIFEST_BYTES),
            Some(expanded.len())
        );
        assert!(parsed.view(&definition, expanded.len() - 1).is_none());

        let groups = project(
            &[(1, view), indexed(7, &definition.raw)],
            &[indexed(4, &expanded)],
        )
        .unwrap();
        assert_eq!(groups.len(), 2);
        assert!(
            groups
                .iter()
                .all(|group| group.input_indices == [1, 7] && group.output_indices == [4])
        );

        assert!(ManifestDefinition::decode(expanded).is_none());
        assert!(ManifestDefinition::decode(reference.clone()).is_none());
        for invalid in [&reference[..reference.len() - 1], &reference[..33]] {
            assert!(
                ManifestReference::decode(invalid)
                    .unwrap()
                    .view(&definition, MAX_MANIFEST_BYTES)
                    .is_none()
            );
        }
        reference.extend_from_slice(&0_u16.to_le_bytes());
        assert!(
            ManifestReference::decode(&reference)
                .unwrap()
                .view(&definition, MAX_MANIFEST_BYTES)
                .is_none()
        );
    }

    #[test]
    fn dispatch_pins_addressing_and_code_independently_of_args() {
        let mut table = alloc::vec![2];
        for (kind, adapter) in [(0, 4), (1, 5)] {
            table.extend_from_slice(&[1; HASH_SIZE]);
            table.push(kind);
            table.extend_from_slice(&[adapter; HASH_SIZE]);
        }
        table.push(0);
        let table = MetatypeAdapterTable::decode(&table).unwrap();
        let a = script(1, 9);
        let mut b = script(1, 9);
        b[48] = 1;
        assert_eq!(
            table.adapter(LogicalScript::decode(&a).unwrap()),
            Some(&[4; HASH_SIZE])
        );
        assert_eq!(
            table.adapter(LogicalScript::decode(&b).unwrap()),
            Some(&[5; HASH_SIZE])
        );
        assert!(
            table
                .adapter(LogicalScript::decode(&script(3, 9)).unwrap())
                .is_none()
        );
    }
}
