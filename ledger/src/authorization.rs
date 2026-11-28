use std::collections::{BTreeSet, HashSet};

use anyhow::{Result, ensure};
use ckb_hash::{Blake2b, new_blake2b};
use ckb_types::{
    core::DepType,
    packed::{Byte32, OutPoint, Script},
    prelude::*,
};

use crate::{Cell, Header, LiveCell, ResolutionBudget, ScriptGroupKind, Transaction, hash};

impl AsRef<Cell> for Cell {
    fn as_ref(&self) -> &Cell {
        self
    }
}

impl AsRef<Cell> for LiveCell {
    fn as_ref(&self) -> &Cell {
        &self.cell
    }
}

pub(crate) struct Cells<T> {
    pub inputs: Vec<T>,
    pub code: Vec<T>,
    pub commitment: Byte32,
    pub dependencies: Option<Byte32>,
}

#[cfg(not(feature = "inline-cell-resolution"))]
type OutPointSet = HashSet<OutPoint>;

#[cfg(feature = "inline-cell-resolution")]
enum OutPointSet {
    Inline([Option<OutPoint>; 4]),
    Heap(HashSet<OutPoint>),
}

#[cfg(feature = "inline-cell-resolution")]
impl Default for OutPointSet {
    fn default() -> Self {
        Self::Inline([const { None }; 4])
    }
}

#[cfg(feature = "inline-cell-resolution")]
impl OutPointSet {
    fn contains(&self, point: &OutPoint) -> bool {
        match self {
            Self::Inline(points) => points.iter().flatten().any(|previous| previous == point),
            Self::Heap(points) => points.contains(point),
        }
    }

    fn insert(&mut self, point: OutPoint) -> bool {
        match self {
            Self::Inline(points) => {
                for slot in points.iter_mut() {
                    match slot {
                        Some(previous) if previous == &point => return false,
                        Some(_) => {}
                        None => {
                            *slot = Some(point);
                            return true;
                        }
                    }
                }
                let mut overflow = HashSet::with_capacity(points.len() + 1);
                overflow.extend(points.iter_mut().filter_map(Option::take));
                overflow.insert(point);
                *self = Self::Heap(overflow);
                true
            }
            Self::Heap(points) => points.insert(point),
        }
    }
}

#[cfg(feature = "inline-cell-resolution")]
impl FromIterator<OutPoint> for OutPointSet {
    fn from_iter<T: IntoIterator<Item = OutPoint>>(points: T) -> Self {
        let mut set = Self::default();
        for point in points {
            set.insert(point);
        }
        set
    }
}

pub(crate) fn resolve_cells<T: AsRef<Cell>>(
    tx: &Transaction,
    budget: &mut ResolutionBudget,
    commit_intent_dependencies: bool,
    mut load: impl FnMut(&OutPoint) -> Result<T>,
) -> Result<Cells<T>> {
    let mut h = new_blake2b();
    h.update(b"Stem authorized Cells\0");
    let inputs = tx.inputs();
    h.update(&(inputs.len() as u64).to_le_bytes());
    let mut consumed = OutPointSet::default();
    let mut resolved = Vec::with_capacity(inputs.len());
    for input in inputs {
        let point = input.previous_output;
        let record = budget.load(&point, &mut load)?;
        h.update(point.as_slice());
        h.update(record.as_ref().content_hash().as_slice());
        consumed.insert(point);
        resolved.push(record);
    }
    let mut dep_hash = commit_intent_dependencies.then(dependency_hasher);
    let code = resolve_dependencies(tx, budget, &consumed, load, |bytes| {
        h.update(bytes);
        if let Some(hash) = &mut dep_hash {
            hash.update(bytes);
        }
    })?;
    Ok(Cells {
        inputs: resolved,
        code,
        commitment: finish(h),
        dependencies: dep_hash.map(finish),
    })
}

fn resolve_dependencies<T: AsRef<Cell>>(
    tx: &Transaction,
    budget: &mut ResolutionBudget,
    consumed: &OutPointSet,
    mut load: impl FnMut(&OutPoint) -> Result<T>,
    mut commit: impl FnMut(&[u8]),
) -> Result<Vec<T>> {
    let deps = tx.cell_deps();
    commit(&(deps.len() as u64).to_le_bytes());
    let mut unique = OutPointSet::default();
    let mut code = Vec::new();
    for dep in deps {
        let point = dep.out_point();
        ensure!(
            !consumed.contains(&point),
            "Cell Dep is consumed by this transaction"
        );
        let record = budget.load(&point, &mut load)?;
        commit(dep.dep_type().as_slice());
        commit(point.as_slice());
        commit(record.as_ref().content_hash().as_slice());
        match DepType::try_from(dep.dep_type())? {
            DepType::Code => {
                ensure!(unique.insert(point), "duplicate resolved Cell Dep");
                code.push(record);
            }
            DepType::DepGroup => {
                let members = record.as_ref().dep_group_members()?;
                ensure!(members.len() != 0, "empty Dep Group");
                commit(&(members.len() as u64).to_le_bytes());
                for point in members {
                    ensure!(
                        !consumed.contains(&point),
                        "Dep Group member is consumed by this transaction"
                    );
                    ensure!(unique.insert(point.clone()), "duplicate resolved Cell Dep");
                    let record = budget.load(&point, &mut load)?;
                    commit(point.as_slice());
                    commit(record.as_ref().content_hash().as_slice());
                    code.push(record);
                }
            }
        }
    }
    Ok(code)
}

fn dependency_hasher() -> Blake2b {
    let mut hash = new_blake2b();
    hash.update(b"Stem intent Cell Deps\0");
    hash
}

fn header_commitment(headers: &[Header]) -> Byte32 {
    let mut hash = new_blake2b();
    hash.update(b"Stem authorized Header Deps\0");
    hash.update(&(headers.len() as u64).to_le_bytes());
    for item in headers {
        hash.update(&item.encode());
    }
    finish(hash)
}

#[derive(Clone, Debug)]
pub(crate) struct LockAuthorization {
    pub(crate) raw_id: Byte32,
    pub(crate) cells: Byte32,
    dependencies: Option<Byte32>,
    headers: Byte32,
    proofs: Byte32,
}

impl LockAuthorization {
    pub(crate) fn new(
        network: &Byte32,
        tx: &Transaction,
        cells: Byte32,
        dependencies: Option<Byte32>,
        headers: &[Header],
    ) -> Self {
        let mut proof = new_blake2b();
        proof.update(b"Stem group proofs\0");
        for witness in tx
            .witnesses()
            .filter(|witness| !witness.proof_data.is_empty())
        {
            proof.update(&witness.group_index.to_le_bytes());
            proof.update(&(witness.proof_data.len() as u32).to_le_bytes());
            proof.update(&witness.proof_data);
        }
        Self {
            raw_id: tx.id(network),
            cells,
            dependencies,
            headers: header_commitment(headers),
            proofs: finish(proof),
        }
    }

    pub(crate) fn intent_dependencies(&self, network: &Byte32) -> Option<Byte32> {
        self.dependencies
            .as_ref()
            .map(|deps| intent_commitment(network, deps, &self.headers))
    }

    pub(crate) fn digest(&self, network: &Byte32, lock: &Script) -> Byte32 {
        hash(
            b"Stem complete Lock authorization\0",
            network,
            &[
                self.raw_id.as_slice(),
                &[ScriptGroupKind::Lock as u8],
                lock.calc_script_hash().as_slice(),
                self.cells.as_slice(),
                self.headers.as_slice(),
                self.proofs.as_slice(),
            ],
        )
    }
}

impl Transaction {
    /// Commit to the exact declared and expanded dependencies of an intent.
    /// The solver preserves these ordered dependencies when assembling inputs
    /// and outputs. The signer obtains trusted contents before authorization.
    pub fn intent_dependency_commitment(
        &self,
        network: &Byte32,
        cells: impl FnMut(&OutPoint) -> Result<Cell>,
        headers: impl FnMut(&Byte32) -> Result<Header>,
    ) -> Result<Byte32> {
        ensure!(
            self.operation().is_none()
                && self.keeper_action().is_none()
                && self.l1_reference_certificate().is_none()
                && !self.has_lineage_command(),
            "paired intents cannot authorize this transaction's controls or L1 references"
        );
        let consumed = self.inputs().map(|input| input.previous_output).collect();
        let mut digest = dependency_hasher();
        resolve_dependencies(
            self,
            &mut ResolutionBudget::default(),
            &consumed,
            cells,
            |bytes| digest.update(bytes),
        )?;
        let header = header_commitment(&self.resolve_signing_headers(network, headers)?);
        Ok(intent_commitment(network, &finish(digest), &header))
    }

    /// Resolve the exact signing material without a candidate block. The caller
    /// supplies independently trusted Cell and Header contents for each reference.
    /// Mempool admission separately checks parent time, liveness and Capacity.
    pub fn signing_digest(
        &self,
        network: &Byte32,
        group_index: u16,
        lock: &Script,
        cells: impl FnMut(&OutPoint) -> Result<Cell>,
        headers: impl FnMut(&Byte32) -> Result<Header>,
    ) -> Result<Byte32> {
        ensure!(
            self.operation().is_none(),
            "control transactions use certificate authorization"
        );
        ensure!(
            !self.has_lineage_command(),
            "Lineage commands use lineage_signing_digest"
        );
        let resolved = resolve_cells(self, &mut ResolutionBudget::default(), false, cells)?;
        let locks: BTreeSet<_> = resolved
            .inputs
            .iter()
            .map(|cell| cell.lock().as_bytes())
            .collect();
        ensure!(
            locks
                .iter()
                .nth(usize::from(group_index))
                .is_some_and(|item| item == &lock.as_bytes()),
            "Lock Script does not match the derived group index"
        );
        let headers = self.resolve_signing_headers(network, headers)?;
        Ok(LockAuthorization::new(
            network,
            self,
            resolved.commitment,
            resolved.dependencies,
            &headers,
        )
        .digest(network, lock))
    }

    fn resolve_signing_headers(
        &self,
        network: &Byte32,
        mut headers: impl FnMut(&Byte32) -> Result<Header>,
    ) -> Result<Vec<Header>> {
        self.header_deps()
            .map(|id| {
                let header = headers(&id)?;
                ensure!(header.id(network) == id, "Header Dep does not match its ID");
                Ok(header)
            })
            .collect()
    }
}

fn intent_commitment(network: &Byte32, cells: &Byte32, headers: &Byte32) -> Byte32 {
    hash(
        b"Stem intent dependencies\0",
        network,
        &[cells.as_slice(), headers.as_slice()],
    )
}

fn finish(h: Blake2b) -> Byte32 {
    let mut output = [0; 32];
    h.finalize(&mut output);
    output.pack()
}

#[cfg(test)]
mod tests {
    use ckb_types::{
        packed::{CellDep, OutPointVec},
        prelude::*,
    };

    use super::*;
    use crate::CellInput;

    #[cfg(feature = "shared-dep-group-members")]
    #[test]
    fn dep_group_members_slice_only_verified_cell_data() -> Result<()> {
        let point = |index| OutPoint::new([29; 32].pack(), index);
        let expected = vec![point(9), point(2), point(17)];
        let group = |data| Cell::new(1_000_000_000, Script::default(), vec![], data);
        let data = OutPointVec::new_builder().set(expected.clone()).build().as_bytes();
        let record = group(data.clone())?;
        let members = record.dep_group_members()?.collect::<Vec<_>>();
        assert_eq!(members, expected);
        let base = record.data().as_ptr();
        for (index, member) in members.iter().enumerate() {
            assert_eq!(
                member.as_slice().as_ptr(),
                base.wrapping_add(4 + index * OutPoint::TOTAL_SIZE)
            );
        }
        assert!(group(data.slice(..data.len() - 1))?.dep_group_members().is_err());
        assert_eq!(
            group(OutPointVec::default().as_bytes())?.dep_group_members()?.len(),
            0
        );
        Ok(())
    }

    #[test]
    fn expanded_dependencies_keep_read_order_and_reject_conflicting_outpoints() -> Result<()> {
        let point = |index| OutPoint::new([43; 32].pack(), index);
        let direct = point(11);
        let group = point(12);
        let members = [7, 4, 6, 1, 3].map(point);
        let cell = Cell::new(1_000_000_000, Script::default(), vec![], Default::default())?;
        let group_record = |extra: Option<OutPoint>| {
            let points = members.iter().cloned().chain(extra).collect();
            Cell::new(
                1_000_000_000,
                Script::default(),
                vec![],
                OutPointVec::new_builder().set(points).build().as_bytes(),
            )
        };
        let tx = Transaction::new(
            vec![
                CellDep::new_builder().out_point(direct.clone()).build(),
                CellDep::new_builder()
                    .dep_type(DepType::DepGroup)
                    .out_point(group.clone())
                    .build(),
            ],
            vec![],
            vec![CellInput::new(point(0), None)],
            vec![],
            vec![],
        )?;
        let verify = |extra, consumed: &OutPointSet| {
            let group_record = group_record(extra)?;
            let mut reads = Vec::new();
            let mut budget = ResolutionBudget::default();
            let code = resolve_dependencies(
                &tx,
                &mut budget,
                consumed,
                |point| {
                    reads.push(point.clone());
                    Ok(if point == &group {
                        group_record.clone()
                    } else {
                        cell.clone()
                    })
                },
                |_| {},
            )?;
            assert_eq!(budget.references, reads.len());
            assert_eq!(
                budget.bytes,
                (reads.len() - 1) * cell.as_slice().len() + group_record.as_slice().len()
            );
            assert_eq!(code.len(), reads.len() - 1);
            Ok::<_, anyhow::Error>(reads)
        };
        let consumed = [point(0)].into_iter().collect();
        let mut expected = vec![direct.clone(), group.clone()];
        expected.extend(members.iter().cloned());
        assert_eq!(verify(None, &consumed)?, expected);
        assert_eq!(
            verify(Some(direct), &consumed).unwrap_err().to_string(),
            "duplicate resolved Cell Dep"
        );
        let consumed = (20..25).map(point).chain([point(3)]).collect();
        assert_eq!(
            verify(None, &consumed).unwrap_err().to_string(),
            "Dep Group member is consumed by this transaction"
        );
        Ok(())
    }
}
