//! Portable accounting for a CKB security domain through a pinned transition.

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{
    HeaderView, JsonBytes, OutPoint as JsonPoint, TransactionAndWitnessProof, Uint64, Uint128,
};
use ckb_types::{
    H256,
    core::{self, Capacity, ScriptHashType},
    packed::{CellOutput, OutPoint, Script, Transaction, WitnessArgs},
    prelude::*,
};
use serde::{Deserialize, Serialize};
use stem_stake::security::{self, Certificate, Domain, Payee, Policy};

use crate::l1_proof;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub ckb_genesis: H256,
    pub network: H256,
    pub code_hash: H256,
    /// Complete hash of the domain Type Script.
    pub domain: H256,
    pub through: Through,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Through {
    Cell(JsonPoint),
    Closure(H256),
}

impl Through {
    pub fn transaction(&self) -> &H256 {
        match self {
            Self::Cell(point) => &point.tx_hash,
            Self::Closure(transaction) => transaction,
        }
    }
}

/// One transaction and its witness inclusion in CKB. Entries follow spend order.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub header: HeaderView,
    pub transaction: JsonBytes,
    pub proof: TransactionAndWitnessProof,
}

#[derive(Serialize)]
pub struct Report {
    pub domain: H256,
    pub through: Through,
    pub ckb_height: Uint64,
    pub ckb_block: H256,
    pub transactions: usize,
    pub paid_services: usize,
    pub state: Status,
    pub capacity: Uint64,
    pub locked_bond: Uint64,
    pub eligible_bond: Uint64,
    pub required_quorum_bond: Uint64,
    pub can_attest: bool,
    pub slashed_bond: Uint64,
    pub returned_bond: Uint64,
    pub service_income: Uint128,
    pub members: Vec<Member>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Retired,
    Closed,
}

#[derive(Serialize)]
pub struct Member {
    pub public_key: H256,
    pub beneficiary: JsonBytes,
    pub initial_bond: Uint64,
    pub locked_bond: Uint64,
    pub slashed_bond: Uint64,
    pub returned_bond: Uint64,
    pub service_income: Uint128,
}

fn domain_output(
    tx: &core::TransactionView,
    scope: &Scope,
) -> Result<Option<(usize, CellOutput, Script)>> {
    let mut matching = tx
        .outputs()
        .into_iter()
        .enumerate()
        .filter_map(|(index, output)| {
            let script = output.type_().to_opt()?;
            (script.calc_script_hash().as_slice() == scope.domain.as_bytes())
                .then_some((index, output, script))
        });
    let result = matching.next();
    ensure!(matching.next().is_none(), "duplicate CKB security domain");
    Ok(result)
}

fn original(tx: &core::TransactionView, index: usize, policy: &Policy) -> bool {
    tx.inputs().get(0).is_some_and(|seed| {
        let mut hash = ckb_hash::new_blake2b();
        hash.update(seed.as_slice());
        hash.update(&(index as u64).to_le_bytes());
        let mut type_id = [0; 32];
        hash.finalize(&mut type_id);
        type_id == policy.type_id
    })
}

impl Scope {
    /// Locate the preceding transaction for a proof collector. Verification
    /// separately checks the specific consumed OutPoint and Type Script.
    pub fn predecessor(&self, tx: &core::TransactionView) -> Result<Option<H256>> {
        if let Some((index, _, script)) = domain_output(tx, self)? {
            let policy = Policy::decode(&script.args().raw_data())?;
            if original(tx, index, &policy) {
                return Ok(None);
            }
        }
        let input = tx
            .inputs()
            .get(0)
            .context("missing CKB security domain predecessor")?;
        Ok(Some(input.previous_output().tx_hash().unpack()))
    }
}

fn authenticate(
    entry: &Entry,
    trusted_header: &mut impl FnMut(u64) -> Result<H256>,
) -> Result<(core::TransactionView, core::HeaderView)> {
    let header: core::HeaderView = entry.header.clone().into();
    let tx = Transaction::from_slice(entry.transaction.as_bytes())?.into_view();
    l1_proof::verify_transaction(
        &trusted_header(header.number())?.pack(),
        &header,
        &tx,
        &entry.proof,
    )?;
    Ok((tx, header))
}

fn data(tx: &core::TransactionView, index: usize) -> Result<ckb_types::bytes::Bytes> {
    Ok(tx
        .outputs_data()
        .get(index)
        .context("missing CKB security domain output data")?
        .raw_data())
}

fn payout(tx: &core::TransactionView, index: usize, payee: &Payee, amount: u64) -> Result<()> {
    let output = tx
        .output(index)
        .context("missing CKB security domain payout")?;
    let lock = output.lock();
    ensure!(
        output.type_().to_opt().is_none()
            && data(tx, index)?.is_empty()
            && lock.code_hash().as_slice() == security::SIGHASH_TYPE
            && lock.hash_type() == ScriptHashType::Type.into()
            && lock.args().raw_data().as_ref() == payee
            && u64::from(output.capacity()) == amount,
        "incorrect CKB security domain payout"
    );
    Ok(())
}

fn service(
    tx: &core::TransactionView,
    scope: &Scope,
    script: &Script,
    policy: &Policy,
    domain: &Domain,
    operation: &[u8],
    income: &mut [u128],
) -> Result<()> {
    let certificate = Certificate::decode(operation, domain.positions.len())?;
    let request = tx
        .inputs()
        .get(1)
        .context("missing CKB security service request")?
        .previous_output();
    let mut reward = 0u64;
    for index in 1..=certificate.signatures.len() {
        let output = tx
            .output(index)
            .context("missing CKB security service payout")?;
        reward = reward
            .checked_add(output.capacity().unpack())
            .context("service reward overflow")?;
    }
    let assigned = domain.verify_service(
        policy,
        scope.domain.as_bytes().try_into()?,
        request.as_slice().try_into()?,
        &certificate.result,
        reward,
        certificate.bitmap,
        certificate.signatures,
    )?;
    let receipt = assigned.len() + 1;
    let receipt_type = script
        .clone()
        .as_builder()
        .args(
            [scope.domain.as_bytes(), request.as_slice()]
                .concat()
                .pack(),
        )
        .build();
    ensure!(
        tx.output(receipt)
            .and_then(|output| output.type_().to_opt())
            .as_ref()
            == Some(&receipt_type)
            && data(tx, receipt)?.as_ref() == certificate.result,
        "CKB security service receipt differs from its certificate"
    );
    for (output_index, (member, amount)) in assigned.into_iter().enumerate() {
        payout(
            tx,
            output_index + 1,
            &domain.positions[member].beneficiary,
            amount,
        )?;
        income[member] = income[member]
            .checked_add(u128::from(amount))
            .context("service income overflow")?;
    }
    Ok(())
}

/// Verify a complete spend history through the independently selected Cell or
/// closure. The caller authenticates hashes on one valid CKB chain, including
/// genesis. Proof inclusion alone cannot attest that a Cell remains live.
pub fn verify(
    scope: &Scope,
    history: &[Entry],
    mut trusted_header: impl FnMut(u64) -> Result<H256>,
) -> Result<Report> {
    ensure!(
        trusted_header(0)? == scope.ckb_genesis,
        "wrong trusted CKB genesis"
    );
    let (first, successors) = history
        .split_first()
        .context("missing CKB security domain history")?;
    let (tx, mut header) = authenticate(first, &mut trusted_header)?;
    let (index, output, script) =
        domain_output(&tx, scope)?.context("missing CKB security domain creation")?;
    let policy = Policy::decode(&script.args().raw_data())?;
    let lock = script
        .clone()
        .as_builder()
        .args(scope.domain.as_bytes().pack())
        .build();
    ensure!(
        script.hash_type() == ScriptHashType::Data2.into()
            && script.code_hash().as_slice() == scope.code_hash.as_bytes()
            && policy.ckb_genesis == scope.ckb_genesis.as_bytes()
            && policy.network == scope.network.as_bytes()
            && tx
                .header_deps()
                .get(0)
                .is_some_and(|genesis| genesis.as_slice() == scope.ckb_genesis.as_bytes())
            && original(&tx, index, &policy)
            && output.lock() == lock,
        "CKB security domain creation or scope mismatch"
    );
    let initial_data = data(&tx, index)?;
    let mut domain = Domain::decode(&initial_data)?;
    let mut capacity = u64::from(output.capacity());
    let occupied = output
        .occupied_capacity(Capacity::bytes(initial_data.len())?)?
        .as_u64();
    let initial_bond = domain.bonded()?;
    ensure!(
        !domain.retired
            && domain.positions.iter().all(|member| !member.slashed)
            && capacity
                .checked_sub(occupied)
                .is_some_and(|free| free >= initial_bond),
        "invalid initial CKB security domain backing"
    );
    let mut head = Some(OutPoint::new(tx.hash(), u32::try_from(index)?));
    let mut closure = None;
    let mut income = vec![0u128; domain.positions.len()];
    let mut services = 0;
    for entry in successors {
        let (tx, next_header) = authenticate(entry, &mut trusted_header)?;
        ensure!(
            next_header.number() >= header.number(),
            "CKB security domain history regressed"
        );
        let input = tx
            .inputs()
            .get(0)
            .context("missing CKB security domain input")?;
        ensure!(
            head.as_ref() == Some(&input.previous_output()),
            "CKB security domain history is missing a predecessor"
        );
        let witness = tx
            .witnesses()
            .get(0)
            .context("missing CKB security domain witness")?;
        let witness = WitnessArgs::from_slice(&witness.raw_data())?;
        let bytes = witness
            .input_type()
            .to_opt()
            .context("missing CKB security domain operation")?
            .raw_data();
        let (kind, body) = bytes
            .split_first()
            .context("empty CKB security domain operation")?;
        let next = domain_output(&tx, scope)?;
        match *kind {
            security::SERVE => {
                service(&tx, scope, &script, &policy, &domain, body, &mut income)?;
                services += 1;
            }
            security::RETIRE => {
                ensure!(
                    !domain.retired && body.is_empty(),
                    "invalid CKB security domain retirement"
                );
                domain.retired = true;
            }
            security::SLASH => {
                let member =
                    domain.verify_double_report(scope.domain.as_bytes().try_into()?, body)?;
                let position = &mut domain.positions[member];
                payout(&tx, 1, &policy.penalty, position.bond)?;
                capacity = capacity
                    .checked_sub(position.bond)
                    .context("invalid CKB security domain penalty")?;
                position.slashed = true;
            }
            security::CLOSE => {
                ensure!(
                    domain.retired
                        && next.is_none()
                        && body.is_empty()
                        && u64::from(input.since())
                            == (security::RELATIVE_HEIGHT | policy.challenge_blocks),
                    "invalid CKB security domain closure"
                );
                let remaining = domain.positions.iter().filter(|p| !p.slashed);
                let count = remaining.clone().count();
                for (output, position) in remaining.enumerate() {
                    payout(&tx, output, &position.beneficiary, position.bond)?;
                }
                let reserve = capacity
                    .checked_sub(domain.bonded()?)
                    .context("invalid closure backing")?;
                payout(&tx, count, &policy.operator, reserve)?;
                capacity = 0;
                closure = Some(tx.hash().unpack());
            }
            _ => anyhow::bail!("unknown CKB security domain operation"),
        }
        if closure.is_some() {
            head = None;
        } else {
            let (index, output, _) = next.context("missing CKB security domain successor")?;
            ensure!(
                index == 0
                    && output.lock() == lock
                    && u64::from(output.capacity()) == capacity
                    && data(&tx, index)?.as_ref() == domain.encode(),
                "invalid CKB security domain successor"
            );
            head = Some(OutPoint::new(tx.hash(), 0));
        }
        header = next_header;
    }
    let closed = closure.is_some();
    let through_matches = match &scope.through {
        Through::Cell(point) => head == Some(point.clone().into()),
        Through::Closure(transaction) => closure.as_ref() == Some(transaction),
    };
    ensure!(
        through_matches,
        "CKB security domain history ends at the wrong transition"
    );
    let locked = if closed { 0 } else { domain.bonded()? };
    let total_slashed = domain
        .positions
        .iter()
        .filter(|p| p.slashed)
        .try_fold(0u64, |sum, p| {
            sum.checked_add(p.bond).context("slashed bond overflow")
        })?;
    let service_income = income.iter().try_fold(0u128, |sum, amount| {
        sum.checked_add(*amount).context("service income overflow")
    })?;
    let eligible = if domain.retired { 0 } else { locked };
    let quorum =
        u64::try_from((u128::from(initial_bond) * u128::from(policy.quorum_bps)).div_ceil(10_000))?;
    Ok(Report {
        domain: scope.domain.clone(),
        through: scope.through.clone(),
        ckb_height: header.number().into(),
        ckb_block: header.data().calc_header_hash().unpack(),
        transactions: history.len(),
        paid_services: services,
        state: if closed {
            Status::Closed
        } else if domain.retired {
            Status::Retired
        } else {
            Status::Active
        },
        capacity: capacity.into(),
        locked_bond: locked.into(),
        eligible_bond: eligible.into(),
        required_quorum_bond: quorum.into(),
        can_attest: eligible >= quorum,
        slashed_bond: total_slashed.into(),
        returned_bond: if closed { domain.bonded()? } else { 0 }.into(),
        service_income: service_income.into(),
        members: domain
            .positions
            .into_iter()
            .zip(income)
            .map(|(position, income)| Member {
                public_key: H256::from(position.key),
                beneficiary: JsonBytes::from_vec(position.beneficiary.to_vec()),
                initial_bond: position.bond.into(),
                locked_bond: if !position.slashed && !closed {
                    position.bond
                } else {
                    0
                }
                .into(),
                slashed_bond: if position.slashed { position.bond } else { 0 }.into(),
                returned_bond: if !position.slashed && closed {
                    position.bond
                } else {
                    0
                }
                .into(),
                service_income: income.into(),
            })
            .collect(),
    })
}
