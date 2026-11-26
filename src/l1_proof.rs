//! CKB transaction inclusion against a caller-authenticated CKB Header.

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{MerkleProof as Proof, TransactionAndWitnessProof};
use ckb_types::{
    bytes::Bytes,
    core::{HeaderView, TransactionView},
    packed::{Byte32, CellOutput, OutPoint, Transaction},
    prelude::*,
    utilities::{MerkleProof, merkle_root},
};

/// Authenticate both the raw CKB transaction and its witnesses. The caller
/// supplies the hash of an independently trusted CKB block.
pub fn verify_transaction(
    trusted_block: &Byte32,
    header: &HeaderView,
    tx: &TransactionView,
    proof: &TransactionAndWitnessProof,
) -> Result<()> {
    let packed = tx.data();
    verify_commitments(
        trusted_block,
        header,
        packed.calc_tx_hash(),
        packed.calc_witness_hash(),
        proof,
    )
}

/// Calculate both transaction commitments from these packed bytes and return the authenticated view.
pub fn verify_packed_transaction(
    trusted_block: &Byte32,
    header: &HeaderView,
    packed: Transaction,
    proof: &TransactionAndWitnessProof,
) -> Result<TransactionView> {
    let tx = packed.into_view();
    verify_commitments(trusted_block, header, tx.hash(), tx.witness_hash(), proof)?;
    Ok(tx)
}

fn verify_commitments(
    trusted_block: &Byte32,
    header: &HeaderView,
    raw_hash: Byte32,
    witness_hash: Byte32,
    proof: &TransactionAndWitnessProof,
) -> Result<()> {
    ensure!(
        &header.data().calc_header_hash() == trusted_block
            && proof.block_hash.as_bytes() == trusted_block.as_slice(),
        "CKB proof is outside the trusted block"
    );
    let raw = &proof.transactions_proof;
    let witness = &proof.witnesses_proof;
    ensure!(
        raw.indices.len() == 1 && raw.indices == witness.indices,
        "CKB proof must identify one matching transaction and witness position"
    );
    let index = raw.indices[0].value();
    ensure!(index < u32::MAX, "CKB inclusion position is out of range");
    let depth = (u64::from(index) + 1).ilog2() as usize;
    ensure!(
        raw.lemmas.len() == depth && witness.lemmas.len() == depth,
        "CKB inclusion proof path length differs from its position"
    );
    let root = |proof: &Proof, leaf: Byte32| {
        MerkleProof::new(vec![index], proof.lemmas.iter().map(Into::into).collect())
            .root(&[leaf])
            .context("invalid CKB inclusion proof path")
    };
    ensure!(
        merkle_root(&[root(raw, raw_hash)?, root(witness, witness_hash)?])
            == header.transactions_root(),
        "CKB transaction or witness proof does not match the Header"
    );
    Ok(())
}

/// Returns the historical output committed in the named block. The caller
/// authenticates the block hash and chooses the confirmation policy. A
/// transaction proof contains no later spend information.
pub fn verify_output(
    trusted_block: &Byte32,
    header: &HeaderView,
    point: &OutPoint,
    tx: &TransactionView,
    proof: &TransactionAndWitnessProof,
) -> Result<(CellOutput, Bytes)> {
    ensure!(
        point.tx_hash() == tx.data().calc_tx_hash(),
        "CKB OutPoint transaction hash differs"
    );
    verify_transaction(trusted_block, header, tx, proof)?;
    let index = usize::try_from(u32::from(point.index()))?;
    Ok((
        tx.output(index)
            .context("CKB output index is out of range")?,
        tx.outputs_data()
            .get(index)
            .context("CKB output data is missing")?
            .raw_data(),
    ))
}
