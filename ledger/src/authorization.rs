use std::collections::{BTreeSet, HashSet};

use anyhow::{Result, ensure};
use ckb_hash::{Blake2b, new_blake2b};
use ckb_types::{
    core::DepType,
    packed::{Byte32, OutPoint, OutPointVecReader, Script},
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
}

pub(crate) fn resolve_cells<T: AsRef<Cell>>(
    tx: &Transaction,
    budget: &mut ResolutionBudget,
    mut load: impl FnMut(&OutPoint) -> Result<T>,
) -> Result<Cells<T>> {
    let mut h = new_blake2b();
    h.update(b"Stem authorized Cells\0");
    let inputs = tx.inputs();
    h.update(&(inputs.len() as u64).to_le_bytes());
    let mut consumed = HashSet::new();
    let mut resolved = Vec::with_capacity(inputs.len());
    for input in inputs {
        let point = input.previous_output;
        let record = budget.load(&point, &mut load)?;
        commit_cell(&mut h, &point, record.as_ref());
        consumed.insert(point);
        resolved.push(record);
    }
    let deps = tx.cell_deps();
    h.update(&(deps.len() as u64).to_le_bytes());
    let mut unique = HashSet::new();
    let mut code = Vec::new();
    for dep in deps {
        let point = dep.out_point();
        ensure!(
            !consumed.contains(&point),
            "Cell Dep is consumed by this transaction"
        );
        let record = budget.load(&point, &mut load)?;
        h.update(dep.dep_type().as_slice());
        commit_cell(&mut h, &point, record.as_ref());
        match DepType::try_from(dep.dep_type())? {
            DepType::Code => {
                ensure!(unique.insert(point), "duplicate resolved Cell Dep");
                code.push(record);
            }
            DepType::DepGroup => {
                let data = record.as_ref().data();
                let members = OutPointVecReader::from_slice(&data)?;
                ensure!(!members.is_empty(), "empty Dep Group");
                h.update(&(members.len() as u64).to_le_bytes());
                for member in members.iter() {
                    let point = member.to_entity();
                    ensure!(
                        !consumed.contains(&point),
                        "Dep Group member is consumed by this transaction"
                    );
                    ensure!(unique.insert(point.clone()), "duplicate resolved Cell Dep");
                    let record = budget.load(&point, &mut load)?;
                    commit_cell(&mut h, &point, record.as_ref());
                    code.push(record);
                }
            }
        }
    }
    Ok(Cells {
        inputs: resolved,
        code,
        commitment: finish(h),
    })
}

fn commit_cell(h: &mut Blake2b, point: &OutPoint, cell: &Cell) {
    h.update(point.as_slice());
    h.update(cell.hashes().content.as_slice());
}

#[derive(Clone, Debug)]
pub(crate) struct LockAuthorization {
    raw_id: Byte32,
    pub(crate) cells: Byte32,
    headers: Byte32,
    proofs: Byte32,
}

impl LockAuthorization {
    pub(crate) fn new(
        network: &Byte32,
        tx: &Transaction,
        cells: Byte32,
        headers: &[Header],
    ) -> Self {
        let mut header = new_blake2b();
        header.update(b"Stem authorized Header Deps\0");
        header.update(&(headers.len() as u64).to_le_bytes());
        for item in headers {
            header.update(&item.encode());
        }
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
            headers: finish(header),
            proofs: finish(proof),
        }
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
    /// Resolve the exact signing material without a candidate block. The caller
    /// supplies independently trusted Cell and Header contents for each reference.
    /// Mempool admission separately checks parent time, liveness and Capacity.
    pub fn signing_digest(
        &self,
        network: &Byte32,
        group_index: u16,
        lock: &Script,
        cells: impl FnMut(&OutPoint) -> Result<Cell>,
        mut headers: impl FnMut(&Byte32) -> Result<Header>,
    ) -> Result<Byte32> {
        ensure!(
            self.operation().is_none(),
            "control transactions use certificate authorization"
        );
        let resolved = resolve_cells(self, &mut ResolutionBudget::default(), cells)?;
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
        let headers = self
            .header_deps()
            .map(|id| {
                let header = headers(&id)?;
                ensure!(header.id(network) == id, "Header Dep does not match its ID");
                Ok(header)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(
            LockAuthorization::new(network, self, resolved.commitment, &headers)
                .digest(network, lock),
        )
    }
}

fn finish(h: Blake2b) -> Byte32 {
    let mut output = [0; 32];
    h.finalize(&mut output);
    output.pack()
}
