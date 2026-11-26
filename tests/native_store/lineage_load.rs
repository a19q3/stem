use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::ensure;
use ckb_types::{packed::Byte32, prelude::*};
use rayon::prelude::*;
use serde::Serialize;
use serde_json::{Value, json};
use stem::jolteon::Client;
use stem_ledger::{lineage::Outcome, record};

use super::{
    Context, Fixture, Result, Retention, Store, TIME, Transaction,
    comet::poll,
    keeper_load::{statuses, transactions},
    lineage::{DynamicLineage, FixtureLineage, fund_amm_market, swap_payload},
};

const COUNTERS: usize = 64;
const TRADERS: usize = 32;
const TRADER_INVENTORY: u128 = 40_000;

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Mode {
    Independent,
    HotspotAmm,
    DistributedAmm,
    Atomic,
    Mixed,
}

impl Mode {
    pub(super) fn load() -> Result<Self> {
        match std::env::var("STEM_LINEAGE_WORKLOAD").as_deref() {
            Ok("independent") | Err(std::env::VarError::NotPresent) => Ok(Self::Independent),
            Ok("hotspot_amm") => Ok(Self::HotspotAmm),
            Ok("distributed_amm") => Ok(Self::DistributedAmm),
            Ok("atomic") => Ok(Self::Atomic),
            Ok("mixed") => Ok(Self::Mixed),
            value => anyhow::bail!("invalid STEM_LINEAGE_WORKLOAD: {value:?}"),
        }
    }

    fn amm(self) -> bool {
        matches!(self, Self::HotspotAmm | Self::DistributedAmm)
    }
}

pub(super) struct Workload {
    mode: Mode,
    dynamic: DynamicLineage,
    registrations: Vec<Transaction>,
}

#[test]
#[ignore = "matched admission checks over signed AMM commands and a fixed finalized tip"]
fn measure_lineage_admission_groups() -> Result<()> {
    const COUNT: usize = 512;
    const WIDTH: usize = 4;
    let mut fixture = Fixture::new()?;
    fixture.spec.limits.transaction_count = 128;
    fixture.spec.limits.cell_reads = 8192;
    fixture.spec.limits.cell_read_bytes = 128 * 1024 * 1024;
    fixture.spec.limits.cell_write_bytes = 4 * 1024 * 1024;
    let (workload, commands) = Workload::fund(&mut fixture, Mode::HotspotAmm, COUNT)?;
    let temp = tempfile::tempdir()?;
    let store = |name| -> Result<Store> {
        let mut store = Store::open(&temp.path().join(name), &fixture.spec, Retention::Archive)?;
        store.commit(store.prepare(&workload.registrations, TIME, false)?)?;
        Ok(store)
    };
    let sequential = store("single-request")?;
    let grouped = store("grouped")?;
    let direct = store("direct")?;
    assert_eq!(sequential.tip(), grouped.tip());
    assert_eq!(sequential.tip(), direct.tip());
    let workers = || rayon::ThreadPoolBuilder::new().num_threads(2).build();
    let one_at_a_time = workers()?;
    let parallel = workers()?;
    let mut single_samples = Vec::new();
    let mut group_samples = Vec::new();
    let mut direct_samples = Vec::new();
    let measure_single = || -> Result<_> {
        let start = Instant::now();
        let receipts = commands
            .iter()
            .map(|tx| one_at_a_time.install(|| sequential.check(tx)))
            .collect::<Result<Vec<_>>>()?;
        Ok((receipts, start.elapsed().as_micros()))
    };
    let measure_grouped = || -> Result<_> {
        let start = Instant::now();
        let mut receipts = Vec::with_capacity(COUNT);
        for wave in commands.chunks(WIDTH) {
            let checked = parallel.install(|| {
                wave.par_iter()
                    .map(|tx| grouped.check(tx))
                    .collect::<Result<Vec<_>>>()
            })?;
            receipts.extend(checked);
        }
        Ok((receipts, start.elapsed().as_micros()))
    };
    let measure_direct = || -> Result<_> {
        let start = Instant::now();
        let receipts = commands
            .iter()
            .map(|tx| direct.check(tx))
            .collect::<Result<Vec<_>>>()?;
        Ok((receipts, start.elapsed().as_micros()))
    };
    for sample in 0..11 {
        let ((serial, a), (batched, b), (inline, c)) = match sample % 3 {
            0 => (measure_single()?, measure_grouped()?, measure_direct()?),
            1 => {
                let grouped = measure_grouped()?;
                let direct = measure_direct()?;
                (measure_single()?, grouped, direct)
            }
            _ => {
                let direct = measure_direct()?;
                (measure_single()?, measure_grouped()?, direct)
            }
        };
        assert_eq!(serial, batched);
        assert_eq!(serial, inline);
        assert!(serial.iter().all(|receipt| {
            Outcome::decode(&receipt.protocol_result)
                .is_ok_and(|outcome| outcome == Outcome::Applied)
        }));
        single_samples.push(a);
        group_samples.push(b);
        direct_samples.push(c);
    }
    let raw = commands
        .iter()
        .flat_map(Transaction::as_slice)
        .copied()
        .collect::<Vec<_>>();
    let report = json!({
        "commands": COUNT, "group_width": WIDTH, "workers": 2,
        "signed_workload_id": hex::encode(ckb_hash::blake2b_256(raw)),
        "header": hex::encode(sequential.tip().encode()),
        "single_us": single_samples, "grouped_us": group_samples, "direct_us": direct_samples,
    });
    if let Some(path) = std::env::var_os("STEM_ADMISSION_REPORT") {
        std::fs::write(path, serde_json::to_vec(&report)?)?;
    }
    println!("{report}");
    Ok(())
}

impl Workload {
    pub(super) fn fund(
        fixture: &mut Fixture,
        mode: Mode,
        count: usize,
    ) -> Result<(Self, Vec<Transaction>)> {
        let dynamic = if mode.amm() {
            ensure!(count.div_ceil(TRADERS) as u128 * 10 <= TRADER_INVENTORY);
            fund_amm_market(fixture, count, TRADERS, TRADERS, TRADER_INVENTORY)?
        } else {
            let registry = FixtureLineage::fund(fixture)?;
            DynamicLineage::fund_using(fixture, &registry, count, COUNTERS, None)?
        };
        let registrations = (0..dynamic.ids.len())
            .map(|index| dynamic.register(fixture, index))
            .collect::<Result<_>>()?;
        let versions = (0, count as u64);
        let commands = (0..count)
            .map(|index| match mode {
                Mode::Independent => {
                    dynamic.signed(fixture, index, &[index % COUNTERS], versions, 1)
                }
                Mode::HotspotAmm | Mode::DistributedAmm => {
                    let trader = index % TRADERS;
                    let pool = if mode == Mode::HotspotAmm || trader == 0 {
                        0
                    } else {
                        TRADERS + 1 + trader
                    };
                    dynamic.signed_payload(
                        fixture,
                        index,
                        &[pool, 1 + trader],
                        versions,
                        swap_payload(10, 1),
                    )
                }
                Mode::Atomic => {
                    let first = (index % (COUNTERS / 8)) * 8;
                    let targets: Vec<_> = (first..first + 8).collect();
                    dynamic.signed(fixture, index, &targets, versions, 1)
                }
                Mode::Mixed => {
                    let independent = 1 + index % (COUNTERS - 1);
                    let targets = [0, independent];
                    let offset = usize::from(index % 3 != 0);
                    dynamic.signed(fixture, index, &targets[offset..], versions, 1)
                }
            })
            .collect::<Result<_>>()?;
        Ok((
            Self {
                mode,
                dynamic,
                registrations,
            },
            commands,
        ))
    }

    pub(super) fn register(&self, fixture: &Fixture, clients: &[Client]) -> Result<Value> {
        let archive = clients.last().context("no archive replica")?;
        let mut height = archive.status()?.height;
        let mut remaining: HashSet<_> = self
            .registrations
            .iter()
            .map(|tx| hex::encode(tx.id(&fixture.spec.network).as_slice()))
            .collect();
        let mut committed = Vec::new();
        for (index, tx) in self.registrations.iter().enumerate() {
            clients[index % clients.len()].submit(tx)?;
        }
        poll("all Lineage registrations", Duration::from_secs(30), || {
            let common = statuses(clients)?
                .iter()
                .map(|status| status.height)
                .min()
                .unwrap();
            while height < common {
                for id in transactions(archive, height + 1)? {
                    if remaining.remove(&id) {
                        committed.push((height + 1, id));
                    }
                }
                height += 1;
            }
            Ok(remaining.is_empty().then_some(()))
        })?;
        Ok(json!({"height": height, "committed": committed}))
    }

    pub(super) fn audit(
        &self,
        fixture: &Fixture,
        output: &Path,
        archive_node: usize,
        height: u64,
        expected_count: usize,
    ) -> Result<Value> {
        let started = Instant::now();
        let archive = Store::open(
            &output.join(format!("node-{archive_node}/db")),
            &fixture.spec,
            Retention::Archive,
        )?;
        let mut serial = Store::open(
            &output.join("serial-replay"),
            &fixture.spec,
            Retention::Archive,
        )?;
        let mut counts = BTreeMap::<Byte32, u64>::new();
        let mut applied = 0usize;
        let mut cycle_sum = 0u64;
        let mut total_fee = 0u64;
        for index in 1..=height {
            let header = archive.header(index)?.context("missing finalized Header")?;
            let (txs, receipts) = archive
                .execution(index)?
                .context("missing finalized execution")?;
            let replay = serial.prepare(&txs, header.context.timestamp_ms, false)?;
            ensure!(
                replay.header() == &header && replay.receipts() == receipts,
                "serial replay diverged at height {index}"
            );
            for (tx, receipt) in txs.iter().zip(receipts) {
                if let Some(command) = tx.lineage_command() {
                    ensure!(
                        Outcome::decode(&receipt.protocol_result)? == Outcome::Applied,
                        "unexpected business rejection at height {index}"
                    );
                    applied += 1;
                    cycle_sum += receipt.cycles;
                    total_fee += receipt.fee;
                    for target in &command.targets {
                        *counts.entry(target.reference.id().clone()).or_default() += 1;
                    }
                }
            }
            serial.commit(replay)?;
        }
        ensure!(
            applied == expected_count,
            "sampled command count differs after replay"
        );
        let mut reserves = [0u128; 2];
        let mut proofs = Vec::new();
        for id in &self.dynamic.ids {
            let proof = serial.prove_current_lineage(id)?;
            ensure!(proof.verify(
                &fixture.spec.network,
                serial.tip(),
                &stem_ledger::lineage::LineageRef::new(id.clone(), None)?,
                &self.dynamic.marker
            ));
            let (head, cell, cell_path) = proof.head.context("missing final Lineage Head")?;
            let count = counts.get(id).copied().unwrap_or_default();
            ensure!(head.version == count, "unexpected final Lineage version");
            let data = cell.cell.data();
            let first = u128::from_le_bytes(data[..16].try_into()?);
            if self.mode.amm() {
                reserves[0] += first;
                reserves[1] += u128::from_le_bytes(data[16..32].try_into()?);
                ensure!(cell.cell.type_scripts().len() == 3);
            } else {
                ensure!(
                    first == 240 + u128::from(count),
                    "counter replay lost an increment"
                );
            }
            proofs.push(json!({
                "id": hex::encode(id.as_slice()), "version": count,
                "head": hex::encode(head.encode()),
                "cell": hex::encode(record::encode_cell(&head.out_point, &cell)),
                "cell_path": hex::encode(cell_path.encode()?),
                "lineage_path": hex::encode(proof.system_path.encode()?),
            }));
        }
        if self.mode.amm() {
            let pool_supply = TRADERS as u128 * 1_000_000;
            ensure!(
                reserves
                    == [
                        pool_supply + (TRADERS as u128 + 1) * TRADER_INVENTORY,
                        pool_supply
                    ],
                "AMM asset supply changed"
            );
        }
        Ok(json!({
            "height": height, "applied": applied, "cycles": cycle_sum, "fee": total_fee,
            "head_updates": counts.values().sum::<u64>(), "heads": proofs,
            "serial_replay_us": started.elapsed().as_micros(),
        }))
    }
}
