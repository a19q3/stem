use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

use anyhow::ensure;
use ckb_jsonrpc_types::JsonBytes;
use ckb_types::prelude::*;
use serde_json::{Value, json};
use stem_ledger::{
    keeper::{Action, Reservation, Trigger, decode_agenda, decode_queue},
    lineage::{self, Outcome},
};

use super::{
    Bytes, Cell, Context, FEE, Fixture, GroupWitness, OutPoint, Privkey, Result, Retention, Store,
    TIME, Transaction, blake2b_256, contract, lineage::FixtureLineage,
    sponsored_lineage::SponsoredLineage,
};

pub(super) struct BatchFixture {
    pub(super) markets: [SponsoredLineage; 2],
    sponsors: [Privkey; 2],
}

pub(super) struct Schedule {
    pub(super) trigger: Trigger,
    pub(super) expires: [u64; 2],
    pub(super) version: u64,
    pub(super) fees: [usize; 2],
}

impl BatchFixture {
    pub(super) fn fund(fixture: &mut Fixture, registry: &FixtureLineage) -> Result<Self> {
        Self::fund_with_fees(fixture, registry, 1)
    }

    pub(super) fn fund_with_fees(
        fixture: &mut Fixture,
        registry: &FixtureLineage,
        fees: usize,
    ) -> Result<Self> {
        let mut markets = [
            SponsoredLineage::fund_at(fixture, registry, fees, 130, [96, 97])?,
            SponsoredLineage::fund_at(fixture, registry, fees, 134, [98, 99])?,
        ];
        let sponsors = [101, 102].map(|seed| Privkey::from_slice(&[seed; 32]));
        for (market, sponsor) in markets.iter_mut().zip(&sponsors) {
            let owner = blake2b_256(sponsor.pubkey()?.serialize());
            let lock = lineage::lock(
                &market.dynamic.fee_cell.lock().code_hash(),
                owner[..20].try_into()?,
                false,
            );
            let cell = Cell::new(
                market.dynamic.fee_cell.capacity(),
                lock,
                vec![],
                Bytes::new(),
            )?;
            market.dynamic.fee_cell = cell.clone();
            for fee in &market.dynamic.fee_points {
                fixture
                    .spec
                    .development_cells
                    .iter_mut()
                    .find(|(point, _)| point == fee)
                    .context("missing batch sponsor Cell")?
                    .1 = cell.clone();
            }
        }
        Ok(Self { markets, sponsors })
    }

    pub(super) fn registrations(&self, fixture: &Fixture) -> Result<Vec<Transaction>> {
        Ok(self.markets[0]
            .registers(fixture)?
            .into_iter()
            .chain(self.markets[1].registers(fixture)?)
            .collect())
    }

    fn drafts(&self, second_amount: u128) -> Result<[Transaction; 2]> {
        Ok([
            self.markets[0].draft(0, 40, 70)?,
            self.markets[1].draft(0, second_amount, 60)?,
        ])
    }

    fn scheduled_drafts(
        &self,
        second_amount: u128,
        schedule: &Schedule,
    ) -> Result<[Transaction; 2]> {
        let [a, b] = [
            self.markets[0].draft(schedule.version, 40, 70)?,
            self.markets[1].draft(schedule.version, second_amount, 60)?,
        ];
        let bind = |(index, draft): (usize, Transaction)| {
            draft
                .with_lineage_fee(
                    self.markets[index].dynamic.fee_points[schedule.fees[index]].clone(),
                    FEE,
                )?
                .with_valid_before_height(schedule.expires[index])?
                .with_keeper_trigger(schedule.trigger)
        };
        Ok([bind((0, a))?, bind((1, b))?])
    }

    pub(super) fn scheduled(
        &self,
        fixture: &Fixture,
        second_amount: u128,
        schedule: &Schedule,
    ) -> Result<Transaction> {
        Transaction::new_lineage_batch(
            self.signed_members(fixture, self.scheduled_drafts(second_amount, schedule)?)?,
        )
    }

    fn signed_members(
        &self,
        fixture: &Fixture,
        drafts: [Transaction; 2],
    ) -> Result<Vec<Transaction>> {
        let plan = Transaction::lineage_batch_plan(&fixture.spec.network, &drafts)?;
        drafts
            .iter()
            .zip(self.markets.iter().zip(&self.sponsors))
            .map(|(draft, (market, sponsor))| {
                let bound = draft.with_lineage_batch_plan(plan.clone())?;
                seal_member(fixture, &bound, market, sponsor)
            })
            .collect()
    }

    pub(super) fn sdk_batch(&self, fixture: &Fixture, second_amount: u128) -> Result<Transaction> {
        Transaction::new_lineage_batch(self.signed_members(fixture, self.drafts(second_amount)?)?)
    }

    pub(super) fn wallet_batch(
        &self,
        fixture: &Fixture,
        genesis: &Path,
        directory: &Path,
    ) -> Result<Transaction> {
        self.wallet_for_drafts(fixture, genesis, directory, self.drafts(30)?)
    }

    pub(super) fn wallet_scheduled(
        &self,
        fixture: &Fixture,
        genesis: &Path,
        directory: &Path,
        schedule: &Schedule,
    ) -> Result<Transaction> {
        self.wallet_for_drafts(
            fixture,
            genesis,
            directory,
            self.scheduled_drafts(30, schedule)?,
        )
    }

    fn wallet_for_drafts(
        &self,
        fixture: &Fixture,
        genesis: &Path,
        directory: &Path,
        drafts: [Transaction; 2],
    ) -> Result<Transaction> {
        fs::create_dir_all(directory)?;
        let command = |verb: &str| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_stem"));
            command
                .args(["wallet", "lineage", verb])
                .arg("--genesis")
                .arg(genesis);
            command
        };
        let mut plan = command("plan");
        for (index, draft) in drafts.iter().enumerate() {
            let file = directory.join(format!("draft-{index}.json"));
            fs::write(
                &file,
                serde_json::to_vec(&JsonBytes::from_vec(draft.as_slice().to_vec()))?,
            )?;
            plan.arg("--member").arg(file);
        }
        let plan_file = directory.join("plan.json");
        let review = checked(plan)?;
        fs::write(&plan_file, serde_json::to_vec_pretty(&review)?)?;
        let cells = directory.join("signing-context.json");
        let allocations: Vec<_> = fixture
            .spec
            .development_cells
            .iter()
            .map(|(point, cell)| {
                json!({
                    "out_point": ckb_jsonrpc_types::OutPoint::from(point.clone()),
                    "cell": JsonBytes::from_vec(cell.as_slice().to_vec()),
                })
            })
            .collect();
        fs::write(&cells, serde_json::to_vec(&json!({"cells": allocations}))?)?;
        let code = directory.join("lineage-lock");
        fs::write(
            &code,
            contract::native_contract(Path::new(env!("CARGO_MANIFEST_DIR")), "stem-lineage-lock")?,
        )?;
        let mut batch = command("batch");
        for (index, market) in self.markets.iter().enumerate() {
            let bound = directory.join(format!("bound-{index}.json"));
            fs::write(&bound, serde_json::to_vec(&review["members"][index])?)?;
            let fee = directory.join(format!("fee-{index}.json"));
            fs::write(
                &fee,
                serde_json::to_vec(&ckb_jsonrpc_types::OutPoint::from(
                    drafts[index].input(0).unwrap().previous_output,
                ))?,
            )?;
            let mut assemble = command("assemble");
            assemble
                .arg("--transaction")
                .arg(&bound)
                .arg("--fee-input")
                .arg(fee)
                .arg("--fee")
                .arg(FEE.to_string());
            let executor = format!("0x{}", hex::encode(market.dynamic.executor.as_slice()));
            for actor in 0..2 {
                let secret = directory.join(format!("actor-{index}-{actor}.key"));
                let seed = 96 + index as u8 * 2 + actor as u8;
                secret_file(&secret, &[seed; 32])?;
                let mut sign = command("sign-business");
                sign.arg("--transaction")
                    .arg(&bound)
                    .arg("--plan")
                    .arg(&plan_file)
                    .arg("--executor-hash")
                    .arg(&executor)
                    .arg("--key-file")
                    .arg(secret);
                let seal_file = directory.join(format!("seal-{index}-{actor}.json"));
                fs::write(&seal_file, serde_json::to_vec(&checked(sign)?)?)?;
                assemble.arg("--seal").arg(seal_file);
            }
            let unsigned = directory.join(format!("unsigned-{index}.json"));
            fs::write(&unsigned, serde_json::to_vec(&checked(assemble)?)?)?;
            let secret = directory.join(format!("sponsor-{index}.key"));
            secret_file(&secret, &[101 + index as u8; 32])?;
            let mut sign = command("sign");
            sign.arg("--transaction")
                .arg(unsigned)
                .arg("--plan")
                .arg(&plan_file)
                .arg("--context")
                .arg(&cells)
                .arg("--code")
                .arg(&code)
                .arg("--executor-hash")
                .arg(executor)
                .arg("--max-fee")
                .arg(FEE.to_string())
                .arg("--key-file")
                .arg(secret);
            let signed_file = directory.join(format!("signed-{index}.json"));
            fs::write(&signed_file, serde_json::to_vec(&checked(sign)?)?)?;
            batch.arg("--member").arg(signed_file);
        }
        let signed = checked(batch)?;
        fs::write(directory.join("batch.json"), serde_json::to_vec(&signed)?)?;
        let bytes: JsonBytes = serde_json::from_value(signed)?;
        Transaction::from_slice(bytes.as_bytes())
    }

    fn cancel(
        &self,
        fixture: &Fixture,
        market: usize,
        fee: usize,
        command: &Transaction,
    ) -> Result<Transaction> {
        let market_config = &self.markets[market].dynamic;
        let point = market_config.fee_points[fee].clone();
        let cell = &market_config.fee_cell;
        let lock = cell.lock();
        let tx = Transaction::new(
            market_config.deps.clone(),
            vec![],
            vec![super::CellInput::new(point.clone(), None)],
            vec![Cell::new(
                cell.capacity() - FEE,
                lock.clone(),
                vec![],
                Bytes::new(),
            )?],
            vec![],
        )?
        .with_keeper_action(Action::Cancel {
            trigger: command.keeper_trigger().unwrap(),
            fee_input: point,
            command_id: command.id(&fixture.spec.network),
        })?;
        let digest = tx.signing_digest(
            &fixture.spec.network,
            0,
            &lock,
            |point| {
                fixture
                    .spec
                    .development_cells
                    .iter()
                    .find(|(id, _)| id == point)
                    .map(|(_, cell)| cell.clone())
                    .context("missing fixture Cell")
            },
            |_| anyhow::bail!("no Header Deps"),
        )?;
        let seal = self.sponsors[market].sign_recoverable(&digest.unpack())?;
        tx.with_witnesses(vec![GroupWitness {
            group_index: 0,
            proof_data: Bytes::new(),
            seal: Bytes::from(seal.serialize()),
        }])
    }
}

fn seal_member(
    fixture: &Fixture,
    draft: &Transaction,
    market: &SponsoredLineage,
    sponsor: &Privkey,
) -> Result<Transaction> {
    let business = market.business_seal(fixture, draft)?;
    let tx = draft.with_lineage_seals(business)?;
    let digest = tx.lineage_signing_digest(&fixture.spec.network, &market.dynamic.fee_cell)?;
    tx.with_witnesses(vec![GroupWitness {
        group_index: 0,
        proof_data: Bytes::new(),
        seal: Bytes::copy_from_slice(&sponsor.sign_recoverable(&digest.unpack())?.serialize()),
    }])
}

fn checked(mut command: Command) -> Result<Value> {
    let output = command.output()?;
    ensure!(
        output.status.success(),
        "Lineage wallet {command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn secret_file(path: &Path, seed: &[u8; 32]) -> Result<()> {
    fs::write(path, seed)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[test]
fn batch_recombination_authenticates_order_and_rolls_back_a_late_failure() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let registry = FixtureLineage::fund(&mut fixture)?;
    let markets = BatchFixture::fund(&mut fixture, &registry)?;
    let temp = tempfile::tempdir()?;
    let spec = temp.path().join("genesis.json");
    super::comet::write_spec(&spec, &fixture.spec)?;
    let mut store = Store::open(&temp.path().join("db"), &fixture.spec, Retention::Archive)?;
    store.commit(store.prepare(&markets.registrations(&fixture)?, TIME, true)?)?;
    let valid = markets.wallet_batch(&fixture, &spec, &temp.path().join("wallet"))?;
    let members = valid.lineage_batch().unwrap();
    assert_eq!(
        valid.as_slice(),
        markets.sdk_batch(&fixture, 30)?.as_slice()
    );

    // The second Executor rejects the amount after the first one's full validation.
    let invalid = markets.sdk_batch(&fixture, 501)?;
    let failure = store.check(&invalid).unwrap_err();
    assert!(format!("{failure:#}").contains("batch member 1: Lineage Executor rejected"));
    eprintln!("late child rollback: {failure:#}");
    let selected = store.select(vec![invalid.clone(), valid.clone()], TIME, u64::MAX)?;
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].as_slice(), valid.as_slice());
    assert!(
        store
            .check(&members[0])
            .unwrap_err()
            .to_string()
            .contains("cannot execute alone")
    );
    let reordered = Transaction::new_lineage_batch(vec![members[1].clone(), members[0].clone()])?;
    assert!(
        store
            .check(&reordered)
            .unwrap_err()
            .to_string()
            .contains("differs from the signed plan")
    );
    let replacement = Transaction::new_lineage_batch(vec![
        members[0].clone(),
        invalid.lineage_batch().unwrap()[1].clone(),
    ])?;
    assert!(
        store
            .check(&replacement)
            .unwrap_err()
            .to_string()
            .contains("differs from the signed plan")
    );
    assert!(Transaction::new_lineage_batch(vec![members[0].clone()]).is_err());
    assert!(Transaction::new_lineage_batch(vec![valid.clone(), members[1].clone()]).is_err());
    let same_fee = members[1]
        .with_witnesses(vec![])?
        .with_lineage_fee(members[0].input(0).unwrap().previous_output, FEE)?
        .with_witnesses(members[1].witnesses().collect())?;
    assert!(Transaction::new_lineage_batch(vec![members[0].clone(), same_fee]).is_err());
    assert!(
        Transaction::lineage_batch_plan(store.network(), &[members[0].clone(), members[0].clone()])
            .is_err()
    );
    let mut invalid_seals = members[1].lineage_seals().to_vec();
    invalid_seals[0] ^= 1;
    let forged = members[1]
        .with_witnesses(vec![])?
        .with_lineage_seals(invalid_seals.into())?
        .with_witnesses(members[1].witnesses().collect())?;
    assert!(
        store
            .check(&Transaction::new_lineage_batch(vec![
                members[0].clone(),
                forged
            ])?)
            .is_err()
    );

    let separate = markets
        .drafts(30)?
        .iter()
        .zip(markets.markets.iter().zip(&markets.sponsors))
        .map(|(draft, (market, sponsor))| seal_member(&fixture, draft, market, sponsor))
        .collect::<Result<Vec<_>>>()?;
    let baseline = store.prepare(&separate, TIME, false)?;
    assert!(
        baseline
            .receipts()
            .iter()
            .all(|item| Outcome::decode(&item.protocol_result).ok() == Some(Outcome::Applied))
    );
    let separate_bytes = separate.iter().map(|tx| tx.as_slice().len()).sum::<usize>();
    let separate_cycles = baseline
        .receipts()
        .iter()
        .map(|item| item.cycles)
        .sum::<u64>();
    eprintln!(
        "separate matched commands: {separate_bytes} bytes, {separate_cycles} charged VM cycles"
    );

    let serial = store.prepare(std::slice::from_ref(&valid), TIME, false)?;
    let parallel = store.prepare(std::slice::from_ref(&valid), TIME, true)?;
    assert_eq!(serial.header(), parallel.header());
    assert_eq!(serial.receipts(), parallel.receipts());
    let receipt = &parallel.receipts()[0];
    assert_eq!(receipt.fee, 2 * FEE);
    assert_eq!(Outcome::decode(&receipt.protocol_result)?, Outcome::Applied);
    eprintln!(
        "atomic batch: {} bytes, {} charged VM cycles",
        valid.as_slice().len(),
        receipt.cycles
    );
    store.commit(parallel)?;
    assert!(store.check(&valid).is_err());
    let expected = [[(460, 100), (60, 730)], [(470, 90), (50, 740)]];
    for ((member, market), expected) in members.iter().zip(&markets.markets).zip(expected) {
        let txid = member.id(store.network());
        assert!(
            store
                .live_cell(&member.input(0).unwrap().previous_output)?
                .is_none()
        );
        let refund = store
            .live_cell(&OutPoint::new(txid.clone(), 0))?
            .context("missing sponsor refund")?;
        assert_eq!(refund.cell.lock(), market.dynamic.fee_cell.lock());
        assert_eq!(
            refund.cell.capacity(),
            market.dynamic.fee_cell.capacity() - FEE
        );
        for (rank, (id, (x, y))) in market.head_ids().iter().zip(expected).enumerate() {
            let state = store.prove_current_lineage(id)?;
            let marker = fixture.spec.lineage_type.as_ref().unwrap();
            assert!(state.verify(store.network(), store.tip(), id, marker));
            let point = OutPoint::new(txid.clone(), rank as u32 + 1);
            assert_eq!(
                store
                    .live_cell(&point)?
                    .context("missing batch successor")?
                    .cell
                    .data(),
                market.reserve(rank, x, y)
            );
        }
    }
    fixture.spec.limits.transaction_cycles = 12_000_000;
    let mut capped = Store::open(
        &temp.path().join("capped"),
        &fixture.spec,
        Retention::Archive,
    )?;
    capped.commit(capped.prepare(&markets.registrations(&fixture)?, TIME, true)?)?;
    let exhausted = capped.check(&valid).unwrap_err();
    assert!(format!("{exhausted:#}").contains("Lineage batch member 1"));
    assert!(format!("{exhausted:#}").contains("cycles"));
    eprintln!("aggregate cycle limit rejected the batch: {exhausted:#}");
    for member in members {
        assert!(
            capped
                .live_cell(&member.input(0).unwrap().previous_output)?
                .is_some()
        );
    }
    Ok(())
}

#[test]
fn a_child_type_script_vetoes_the_entire_lineage_batch() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let registry = FixtureLineage::fund(&mut fixture)?;
    let mut batch = BatchFixture::fund(&mut fixture, &registry)?;
    let temp = tempfile::tempdir()?;
    super::comet::compile_type(temp.path(), 13)?;
    let code: Bytes = fs::read(temp.path().join("rule-13"))?.into();
    let script = super::Script::new_builder()
        .code_hash(blake2b_256(&code))
        .hash_type(super::ScriptHashType::Data)
        .build();
    let point = OutPoint::new([136; 32].pack(), 8);
    fixture.spec.development_cells.push((
        point.clone(),
        Cell::new(
            Cell::minimum_capacity(&fixture.lock, &[], code.len())?,
            fixture.lock.clone(),
            vec![],
            code,
        )?,
    ));
    let guarded = &mut batch.markets[1].dynamic;
    guarded
        .deps
        .push(super::CellDep::new_builder().out_point(point).build());
    let cell = &mut fixture
        .spec
        .development_cells
        .iter_mut()
        .find(|(point, _)| point == &guarded.anchors[0])
        .context("missing protected reserve")?
        .1;
    let types = cell.type_scripts().chain([script]).collect();
    *cell = Cell::new(cell.capacity(), cell.lock(), types, cell.data())?;

    let mut store = Store::open(
        &temp.path().join("protected"),
        &fixture.spec,
        Retention::Archive,
    )?;
    store.commit(store.prepare(&batch.registrations(&fixture)?, TIME, true)?)?;
    let attempt = batch.sdk_batch(&fixture, 30)?;
    let failure = store.check(&attempt).unwrap_err();
    assert!(format!("{failure:#}").contains("Lineage batch member 1: Type Script Group"));
    assert!(format!("{failure:#}").ends_with("exited with 38"));
    eprintln!("late Type Script veto: {failure:#}");
    assert!(store.select(vec![attempt], TIME, u64::MAX)?.is_empty());
    for market in &batch.markets {
        for id in market.head_ids() {
            assert_eq!(store.prove_current_lineage(&id)?.head.unwrap().0.version, 0);
        }
        assert!(store.live_cell(&market.dynamic.fee_points[0])?.is_some());
    }
    Ok(())
}

#[test]
fn keeper_batches_recover_and_release_all_sponsor_reservations() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let registry = FixtureLineage::fund(&mut fixture)?;
    let batch = BatchFixture::fund_with_fees(&mut fixture, &registry, 3)?;
    super::keeper::fund_carriers(&mut fixture, 5)?;
    fixture.spec.limits.cell_reads = 512;
    fixture.spec.keeper.as_mut().unwrap().time = Some(stem::native_store::KeeperTimePolicy {
        per_height: 2,
        max_delay_ms: 60_000,
    });
    let temp = tempfile::tempdir()?;
    let mut store = Store::open(
        &temp.path().join("origin"),
        &fixture.spec,
        Retention::Archive,
    )?;
    store.commit(store.prepare(&batch.registrations(&fixture)?, TIME, false)?)?;
    let plan = Schedule {
        trigger: Trigger::Height(4),
        expires: [7, 6],
        version: 0,
        fees: [0, 0],
    };
    let command = batch.scheduled(&fixture, 30, &plan)?;
    let points = |tx: &Transaction| -> Vec<_> {
        tx.lineage_batch()
            .unwrap()
            .iter()
            .map(|member| member.input(0).unwrap().previous_output)
            .collect()
    };
    let assert_links = |state: &Store, command: &Transaction, pending: bool| -> Result<()> {
        let fees = points(command);
        for fee in &fees {
            let (record, proof) = state.prove_keeper_reservation(fee)?;
            assert_eq!(record.is_some(), pending);
            assert!(Store::verify_keeper_reservation(
                state.network(),
                state.tip(),
                fee,
                record.as_deref(),
                &proof,
            ));
            if let Some(record) = record {
                let reservation = Reservation::decode(&record)?;
                assert_eq!(reservation.primary_fee, fees[0]);
                assert_eq!(reservation.command_id, command.id(state.network()));
                assert_eq!(Some(reservation.trigger), command.keeper_trigger());
            }
        }
        Ok(())
    };
    assert_eq!(command.valid_before_height(), Some(6));
    assert!(store.check(&command).is_err());
    let children = command.lineage_batch().unwrap();
    assert!(Action::schedule(&children[0]).is_err());
    let [a, b] = batch.scheduled_drafts(30, &plan)?;
    assert!(Transaction::lineage_batch_plan(store.network(), &[a, b.with_execute_at(5)?]).is_err());
    let mut forged_seal = children[1].witness(0).unwrap();
    forged_seal.seal = Bytes::from_static(&[255; 65]);
    let forged = Transaction::new_lineage_batch(vec![
        children[0].clone(),
        children[1].with_witnesses(vec![forged_seal])?,
    ])?;
    assert!(
        store
            .check(&super::keeper::schedule(&fixture, 0, &forged)?)
            .is_err()
    );
    let carrier = super::keeper::schedule(&fixture, 0, &command)?;
    store.commit(store.prepare(&[carrier], TIME, false)?)?;
    assert_links(&store, &command, true)?;
    let queue = store.prove_keeper_queue(4)?.0.unwrap();
    assert_eq!(decode_queue(&queue, 4)?[0].as_slice(), command.as_slice());
    let conflict = batch.scheduled(
        &fixture,
        25,
        &Schedule {
            fees: [1, 0],
            ..plan
        },
    )?;
    let error = store
        .check(&super::keeper::schedule(&fixture, 1, &conflict)?)
        .unwrap_err();
    assert!(error.to_string().contains("requires fresh fee Cells"));
    eprintln!("secondary fee cannot be reserved twice: {error:#}");

    let mut restored = Store::open(
        &temp.path().join("restored"),
        &fixture.spec,
        Retention::Archive,
    )?;
    restored.install_state(
        store.export_state()?,
        &store.tip().app_hash(store.network()),
    )?;
    assert_links(&restored, &command, true)?;
    for state in [&mut store, &mut restored] {
        state.commit(state.prepare(&[], TIME, false)?)?;
    }
    let serial = store.prepare(&[], TIME, false)?;
    let parallel = restored.prepare(&[], TIME, true)?;
    assert_eq!(serial.header(), parallel.header());
    assert_eq!(serial.receipts(), parallel.receipts());
    let receipt = &parallel.receipts()[0];
    assert_eq!(receipt.fee, 2 * FEE);
    assert_eq!(Outcome::decode(&receipt.protocol_result)?, Outcome::Applied);
    eprintln!(
        "height batch after snapshot restore: {} bytes, {} charged cycles",
        command.as_slice().len(),
        receipt.cycles
    );
    restored.commit(parallel)?;
    assert_links(&restored, &command, false)?;
    assert!(restored.prove_keeper_queue(4)?.0.is_none());
    for fee in points(&command) {
        assert!(restored.live_cell(&fee)?.is_none());
    }

    let timed = Schedule {
        trigger: Trigger::Time(TIME + 100),
        expires: [12, 10],
        version: 1,
        fees: [1, 1],
    };
    let failure = batch.scheduled(&fixture, 501, &timed)?;
    let claim = super::keeper::schedule(&fixture, 1, &failure)?;
    restored.commit(restored.prepare(&[claim], TIME, false)?)?;
    let agenda = decode_agenda(&restored.prove_keeper_agenda()?.0.unwrap())?;
    assert_eq!(agenda[0].valid_before_height, 10);
    assert_links(&restored, &failure, true)?;
    let jumped = restored.prepare(&[], TIME + 100, false)?;
    assert!(jumped.receipts().is_empty());
    restored.commit(jumped)?;
    let attempt = restored.prepare(&[], TIME + 100, true)?;
    assert_eq!(attempt.receipts()[0].fee, 0);
    assert_eq!(
        Outcome::decode(&attempt.receipts()[0].protocol_result)?,
        Outcome::Skipped
    );
    eprintln!(
        "failed timed batch: {} execution fee, {} reserved cycles",
        attempt.receipts()[0].fee,
        attempt.receipts()[0].cycles
    );
    restored.commit(attempt)?;
    assert_links(&restored, &failure, false)?;
    assert!(restored.prove_keeper_agenda()?.0.is_none());
    for fee in points(&failure) {
        assert!(restored.live_cell(&fee)?.is_some());
    }
    for market in &batch.markets {
        for target in market.head_ids() {
            assert_eq!(
                restored
                    .prove_current_lineage(&target)?
                    .head
                    .unwrap()
                    .0
                    .version,
                1
            );
        }
    }

    let cancellable = Schedule {
        trigger: Trigger::Time(TIME + 1_000),
        expires: [17, 16],
        ..timed
    };
    let old = batch.scheduled(&fixture, 25, &cancellable)?;
    let replacement = batch.scheduled(
        &fixture,
        30,
        &Schedule {
            fees: [1, 2],
            ..cancellable
        },
    )?;
    let stale_cancel = batch.cancel(&fixture, 0, 1, &old)?;
    let secondary_cancel = batch.cancel(&fixture, 1, 1, &old)?;
    let reserve_old = super::keeper::schedule(&fixture, 2, &old)?;
    restored.commit(restored.prepare(&[reserve_old], TIME + 100, false)?)?;
    restored.commit(restored.prepare(&[secondary_cancel], TIME + 100, false)?)?;
    assert_links(&restored, &old, false)?;
    let next = super::keeper::schedule(&fixture, 3, &replacement)?;
    restored.commit(restored.prepare(&[next], TIME + 100, false)?)?;
    let error = restored.check(&stale_cancel).unwrap_err();
    assert!(error.to_string().contains("does not match a reservation"));
    assert_links(&restored, &replacement, true)?;
    eprintln!(
        "secondary cancellation releases both links; old cancellation rejected against replacement"
    );

    fixture.spec.limits.block_cycles = 100_000_000;
    let mut capped = Store::open(
        &temp.path().join("capped"),
        &fixture.spec,
        Retention::Archive,
    )?;
    capped.commit(capped.prepare(&batch.registrations(&fixture)?, TIME, false)?)?;
    let error = capped
        .check(&super::keeper::schedule(&fixture, 0, &command)?)
        .unwrap_err();
    assert!(format!("{error:#}").contains("cycles"));
    assert_links(&capped, &command, false)?;
    eprintln!("combined signing and VM budgets rejected by the Keeper slot: {error:#}");
    Ok(())
}
