//! The stable identity of a sequence of ordinary Live Cells.

use anyhow::{Result, ensure};
use ckb_types::{
    bytes::Bytes,
    core::ScriptHashType,
    packed::{Byte32, OutPoint, Script},
    prelude::*,
};

use crate::{
    LiveCell,
    block::Header,
    hash, light,
    patricia::{Namespace, Patricia, Proof},
    record,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

pub const REGISTER: u16 = 0;
pub const REPLACE_EXECUTOR: u16 = 1;
pub const RETIRE: u16 = 2;
const HEAD_PREFIX: &[u8] = b"lineage/head/";
pub const HEAD_KEY_BYTES: usize = HEAD_PREFIX.len() + 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Head {
    pub out_point: OutPoint,
    pub version: u64,
    pub executor: Byte32,
}

impl Head {
    pub const MAX_ENCODED_BYTES: usize = 36 + 10 + 32;

    pub fn encode(&self) -> Bytes {
        wire::LineageHead::new_builder()
            .out_point(wire::OutPoint::new_unchecked(self.out_point.as_bytes()))
            .version(
                wire::CompactUint64::new_builder()
                    .set(self.version.into())
                    .build(),
            )
            .executor(wire::Byte32::new_unchecked(self.executor.as_bytes()))
            .build()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let record = wire::LineageHead::from_slice(bytes)?;
        let head = Self {
            out_point: OutPoint::new_unchecked(record.out_point().as_bytes()),
            version: record.version().as_reader().value().try_into()?,
            executor: Byte32::new_unchecked(record.executor().as_bytes()),
        };
        ensure!(
            !head.out_point.is_null() && head.executor != Byte32::default(),
            "invalid Lineage Head"
        );
        Ok(head)
    }
}

pub fn head_key(id: &Byte32) -> Vec<u8> {
    [HEAD_PREFIX, id.as_slice()].concat()
}

pub fn is_marker(code: &Byte32, script: &Script) -> bool {
    &script.code_hash() == code && script.hash_type().as_slice()[0] == ScriptHashType::Data as u8
}

pub fn matches_head(cell: &LiveCell, code: &Byte32, id: &Byte32) -> bool {
    cell.cell
        .type_scripts()
        .any(|script| is_marker(code, &script) && script.args().raw_data() == id.as_bytes())
}

pub struct LineageProof {
    pub head: Option<(Head, LiveCell, Proof)>,
    pub system_path: Proof,
}

impl LineageProof {
    pub fn decode(
        record: Option<&[u8]>,
        system_path: &[u8],
        cell: Option<&[u8]>,
        cell_path: Option<&[u8]>,
    ) -> Result<Self> {
        let head = match (record, cell, cell_path) {
            (Some(raw), Some(cell), Some(path)) => {
                let head = Head::decode(raw)?;
                let (point, cell) = record::decode_cell(cell)?;
                ensure!(
                    point == head.out_point,
                    "Lineage proof uses a different OutPoint"
                );
                Some((head, cell, Proof::decode(path)?))
            }
            (None, None, None) => None,
            _ => anyhow::bail!("incomplete Lineage proof"),
        };
        Ok(Self {
            head,
            system_path: Proof::decode(system_path)?,
        })
    }

    /// Proves the head (or its absence for Latest) at the supplied Header.
    /// For Exact, proof of a different version or of an absent head is insufficient.
    pub fn verify(
        &self,
        network: &Byte32,
        header: &Header,
        reference: &LineageRef,
        marker_code: &Byte32,
    ) -> bool {
        let id = reference.id();
        let encoded = self.head.as_ref().map(|(head, _, _)| head.encode());
        let eligible = match &self.head {
            Some((head, cell, path)) => {
                reference.matches_version(head.version)
                    && matches_head(cell, marker_code, id)
                    && record::verify_cell(
                        &Patricia::new(network.clone(), Namespace::Cells),
                        &header.state.cell_root.unpack(),
                        &head.out_point,
                        Some(cell),
                        path,
                    )
            }
            None => reference.version().is_none(),
        };
        eligible
            && light::verify_system_record(
                network,
                header,
                &head_key(id),
                encoded.as_deref(),
                &self.system_path,
            )
    }
}

/// The registration transaction must consume this anchor as its first input.
/// Distinct outputs of that transaction occupy distinct registration slots.
pub fn id(network: &Byte32, anchor: &OutPoint, output_index: u32) -> Byte32 {
    hash(
        b"Stem Lineage ID\0",
        network,
        &[anchor.as_slice(), &output_index.to_le_bytes()],
    )
}

/// A stable Lineage identity and either its latest or one exact version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineageRef {
    id: Byte32,
    version: Option<u64>,
}

impl LineageRef {
    pub fn new(id: Byte32, version: Option<u64>) -> Result<Self> {
        ensure!(id != Byte32::default(), "zero Lineage ID");
        Ok(Self { id, version })
    }

    pub fn id(&self) -> &Byte32 {
        &self.id
    }

    /// `None` selects the current head in the supplied execution or proof context.
    pub fn version(&self) -> Option<u64> {
        self.version
    }

    pub fn matches_version(&self, actual: u64) -> bool {
        self.version.is_none_or(|expected| expected == actual)
    }

    pub fn encode(&self) -> Bytes {
        self.to_wire().as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::from_wire(wire::LineageRef::from_slice(bytes)?)
    }

    fn to_wire(&self) -> wire::LineageRef {
        let selector = self.version.map_or(0, |version| u128::from(version) + 1);
        wire::LineageRef::new_builder()
            .id(wire::Byte32::new_unchecked(self.id.as_bytes()))
            .selector(wire::LineageSelector::new_builder().set(selector).build())
            .build()
    }

    fn from_wire(reference: wire::LineageRef) -> Result<Self> {
        let selector = reference.selector().as_reader().value();
        let version = selector.checked_sub(1).map(u64::try_from).transpose()?;
        Self::new(Byte32::new_unchecked(reference.id().as_bytes()), version)
    }
}

pub fn marker(code_hash: &Byte32, id: &Byte32) -> Script {
    Script::new_builder()
        .code_hash(code_hash.clone())
        .hash_type(ScriptHashType::Data)
        .args(id.as_bytes())
        .build()
}

/// Standard Lineage Lock args are the role byte and the owner's Blake160.
pub fn lock(code_hash: &Byte32, owner: &[u8; 20], is_head: bool) -> Script {
    let mut args = [0; 21];
    args[0] = u8::from(is_head);
    args[1..].copy_from_slice(owner);
    Script::new_builder()
        .code_hash(code_hash.clone())
        .hash_type(ScriptHashType::Data)
        .args(Bytes::copy_from_slice(&args))
        .build()
}

pub const MAX_TARGETS: usize = 16;
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024;
pub const MAX_DATA_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub reference: LineageRef,
    pub latest_bounds: Option<(u64, u64)>,
    pub min_capacity: u64,
}

impl Target {
    pub fn matches_version(&self, actual: u64) -> bool {
        self.reference.matches_version(actual)
            && self
                .latest_bounds
                .is_none_or(|(min, max)| (min..=max).contains(&actual))
    }

    fn validate(&self) -> Result<()> {
        if let Some((min, max)) = self.latest_bounds {
            ensure!(
                self.reference.version().is_none() && min < max && (min, max) != (0, u64::MAX),
                "Lineage version bounds require Latest and a nontrivial range"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    pub targets: Vec<Target>,
    pub executor: Byte32,
    pub executor_dep: OutPoint,
    pub executor_cycles: u64,
    pub validation_cycles: u64,
    pub fee: u64,
    pub payload: Bytes,
}

impl Command {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            (1..=MAX_TARGETS).contains(&self.targets.len())
                && self
                    .targets
                    .windows(2)
                    .all(|pair| pair[0].reference.id() < pair[1].reference.id()),
            "invalid or unordered Lineage targets"
        );
        for target in &self.targets {
            target.validate()?;
        }
        ensure!(
            self.executor != Byte32::default()
                && !self.executor_dep.is_null()
                && self.executor_cycles > 0
                && self.validation_cycles > 0
                && self.fee > 0
                && self.payload.len() <= MAX_PAYLOAD_BYTES,
            "invalid Lineage Executor or fee bounds"
        );
        Ok(())
    }

    pub(crate) fn encode(&self) -> wire::LineageCommand {
        fn compact(value: u64) -> wire::CompactUint64 {
            wire::CompactUint64::new_builder().set(value.into()).build()
        }

        let targets = self.targets.iter().map(|target| {
            wire::LineageTarget::new_builder()
                .reference(target.reference.to_wire())
                .min_capacity(compact(target.min_capacity))
                .latest_bounds(
                    wire::LineageVersionBoundsOpt::new_builder()
                        .set(target.latest_bounds.map(|(min, max)| {
                            wire::LineageVersionBounds::new_builder()
                                .min_version(compact(min))
                                .max_version(compact(max))
                                .build()
                        }))
                        .build(),
                )
                .build()
        });
        wire::LineageCommand::new_builder()
            .targets(
                wire::LineageTargetVec::new_builder()
                    .set(targets.collect())
                    .build(),
            )
            .executor(wire::Byte32::new_unchecked(self.executor.as_bytes()))
            .executor_dep(wire::OutPoint::new_unchecked(self.executor_dep.as_bytes()))
            .executor_cycles(compact(self.executor_cycles))
            .validation_cycles(compact(self.validation_cycles))
            .fee(compact(self.fee))
            .payload(wire::Bytes::new_unchecked(self.payload.pack().as_bytes()))
            .build()
    }

    pub(crate) fn decode(command: wire::LineageCommand) -> Result<Self> {
        fn compact(value: wire::CompactUint64) -> Result<u64> {
            Ok(value.as_reader().value().try_into()?)
        }

        let targets = command.targets();
        ensure!(targets.len() <= MAX_TARGETS, "too many Lineage targets");
        let targets = targets
            .into_iter()
            .map(|target| {
                Ok(Target {
                    reference: LineageRef::from_wire(target.reference())?,
                    latest_bounds: target
                        .latest_bounds()
                        .to_opt()
                        .map(|bounds| {
                            Ok::<_, anyhow::Error>((
                                compact(bounds.min_version())?,
                                compact(bounds.max_version())?,
                            ))
                        })
                        .transpose()?,
                    min_capacity: compact(target.min_capacity())?,
                })
            })
            .collect::<Result<_>>()?;
        let result = Self {
            targets,
            executor: Byte32::new_unchecked(command.executor().as_bytes()),
            executor_dep: OutPoint::new_unchecked(command.executor_dep().as_bytes()),
            executor_cycles: compact(command.executor_cycles())?,
            validation_cycles: compact(command.validation_cycles())?,
            fee: compact(command.fee())?,
            payload: command.payload().raw_data(),
        };
        result.validate()?;
        Ok(result)
    }
}

/// Interpreted only against a transaction proof for a Lineage command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Rejected,
    Applied,
    Skipped,
    Retrying,
}

impl Outcome {
    pub fn from_applied(applied: bool) -> Self {
        if applied {
            Self::Applied
        } else {
            Self::Rejected
        }
    }

    pub fn encode(self) -> Bytes {
        match self {
            Self::Rejected => Bytes::from_static(&[0]),
            Self::Applied => Bytes::from_static(&[1]),
            Self::Skipped => Bytes::from_static(&[2]),
            Self::Retrying => Bytes::from_static(&[3]),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        match bytes {
            [0] => Ok(Self::Rejected),
            [1] => Ok(Self::Applied),
            [2] => Ok(Self::Skipped),
            [3] => Ok(Self::Retrying),
            _ => anyhow::bail!("invalid Lineage outcome"),
        }
    }
}

pub const MAX_ACTORS: usize = 8;
pub const MAX_BATCH_MEMBERS: usize = 8;
pub const SEAL_LEN: usize = 65;

/// Public identity in the application authorization namespace.
pub fn actor_id(compressed_key: &[u8; 33]) -> Byte32 {
    let mut digest = [0; 32];
    let mut hasher = ckb_hash::new_blake2b();
    hasher.update(b"Stem Lineage actor\0");
    hasher.update(compressed_key);
    hasher.finalize(&mut digest);
    digest.pack()
}

/// All listed business parties sign the complete operation with only the exact
/// fee amount and fee input left for a sponsor to choose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Intent {
    pub actors: Vec<Byte32>,
    pub max_fee: u64,
    pub batch: Option<Byte32>,
}

impl Intent {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            (1..=MAX_ACTORS).contains(&self.actors.len())
                && self.actors[0] != Byte32::default()
                && self.actors.windows(2).all(|pair| pair[0] < pair[1]),
            "invalid or unordered Lineage business actors"
        );
        ensure!(self.max_fee > 0, "Lineage fee cap must be positive");
        ensure!(
            self.batch
                .as_ref()
                .is_none_or(|id| *id != Byte32::default()),
            "Lineage batch plan cannot be zero"
        );
        Ok(())
    }

    pub(crate) fn encode(&self) -> wire::LineageIntent {
        let actors = self
            .actors
            .iter()
            .map(|actor| wire::Byte32::new_unchecked(actor.as_bytes()));
        wire::LineageIntent::new_builder()
            .actors(wire::Byte32Vec::new_builder().set(actors.collect()).build())
            .max_fee(
                wire::CompactUint64::new_builder()
                    .set(self.max_fee.into())
                    .build(),
            )
            .batch(
                wire::LineageBatchPlanOpt::new_builder()
                    .set(
                        self.batch
                            .as_ref()
                            .map(|id| wire::Byte32::new_unchecked(id.as_bytes())),
                    )
                    .build(),
            )
            .build()
    }

    pub(crate) fn decode(intent: wire::LineageIntent) -> Result<Self> {
        let actors = intent.actors();
        ensure!(
            actors.len() <= MAX_ACTORS,
            "too many Lineage business actors"
        );
        let intent = Self {
            actors: actors
                .into_iter()
                .map(|actor| Byte32::new_unchecked(actor.as_bytes()))
                .collect(),
            max_fee: intent.max_fee().as_reader().value().try_into()?,
            batch: intent
                .batch()
                .to_opt()
                .map(|id| Byte32::new_unchecked(id.as_bytes())),
        };
        intent.validate()?;
        Ok(intent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_uses_one_minimal_selector_for_every_version() -> Result<()> {
        let id: Byte32 = [7; 32].pack();
        let cases: &[(Option<u64>, &[u8])] = &[
            (None, &[0]),
            (Some(0), &[1]),
            (Some(42), &[43]),
            (Some(126), &[127]),
            (Some(127), &[0x80, 1]),
            (
                Some(u64::MAX),
                &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 2],
            ),
        ];
        for &(version, selector) in cases {
            let reference = LineageRef::new(id.clone(), version)?;
            let encoded = reference.encode();
            assert_eq!(&encoded[..32], id.as_slice());
            assert_eq!(&encoded[32..], selector);
            assert_eq!(LineageRef::decode(&encoded)?, reference);
        }
        for selector in [
            &[][..],
            &[0, 1],
            &[0x80, 0],
            &[0x80],
            &[0x81, 0],
            &[0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 2],
            &[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 4],
        ] {
            assert!(LineageRef::decode(&[id.as_slice(), selector].concat()).is_err());
        }
        assert!(LineageRef::decode(&[0; 33]).is_err());
        Ok(())
    }
}
