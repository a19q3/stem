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
use stem_ledger::{
    lineage::{Command, Outcome},
    record,
};

use super::{
    Context, Fixture, Result, Retention, Store, TIME, Transaction,
    clob, comet::poll,
    keeper_load::{statuses, transactions},
    lineage::{DynamicLineage, FixtureLineage, fund_amm_market, swap_payload},
};

const DEFAULT_COUNTERS: usize = 64;
const TRADERS: usize = 32;
const TRADER_INVENTORY: u128 = 40_000;
const POOL_INVENTORY: u128 = 1_000_000;

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Mode {
    Independent,
    HotspotAmm,
    DistributedAmm,
    Atomic,
    Mixed,
    Clob,
}

impl Mode {
    pub(super) fn load() -> Result<Self> {
        match std::env::var("STEM_LINEAGE_WORKLOAD").as_deref() {
            Ok("independent") | Err(std::env::VarError::NotPresent) => Ok(Self::Independent),
            Ok("hotspot_amm") => Ok(Self::HotspotAmm),
            Ok("distributed_amm") => Ok(Self::DistributedAmm),
            Ok("atomic") => Ok(Self::Atomic),
            Ok("mixed") => Ok(Self::Mixed),
            Ok("clob") => Ok(Self::Clob),
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
    pools: usize,
    traders: usize,
}

struct AmmOracle {
    reserves: BTreeMap<Byte32, (bool, [u128; 2])>,
}

impl AmmOracle {
    fn new(ids: &[Byte32], traders: usize) -> Self {
        let reserves = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let pool = index == 0 || index > traders + 1;
                let amounts = if pool { [POOL_INVENTORY; 2] } else { [TRADER_INVENTORY, 0] };
                (id.clone(), (pool, amounts))
            })
            .collect();
        Self { reserves }
    }

    fn apply(&mut self, command: &Command) -> Result<()> {
        let [left, right] = command.targets.as_slice() else {
            anyhow::bail!("expected exactly one AMM pool and trader");
        };
        let left = left.reference.id();
        let right = right.reference.id();
        let (left_pool, left_amounts) = *self.reserves.get(left).context("unknown AMM target")?;
        let (right_pool, right_amounts) = *self.reserves.get(right).context("unknown AMM target")?;
        ensure!(left_pool != right_pool, "AMM targets must have distinct roles");
        let (pool_id, mut pool, trader_id, mut trader) = if left_pool {
            (left, left_amounts, right, right_amounts)
        } else {
            (right, right_amounts, left, left_amounts)
        };
        let payload = &command.payload;
        ensure!(
            payload.len() == 33 && payload[32] <= 1,
            "invalid AMM swap payload"
        );
        let sell = usize::from(payload[32]);
        let buy = 1 - sell;
        let amount = u128::from_le_bytes(payload[..16].try_into()?);
        let minimum = u128::from_le_bytes(payload[16..32].try_into()?);
        ensure!(
            amount > 0 && !pool.contains(&0) && trader[sell] >= amount,
            "invalid sell balance"
        );
        // The remaining buy reserve is the least integer that preserves the product.
        let product = pool[sell].checked_mul(pool[buy]).context("AMM product overflow")?;
        pool[sell] = pool[sell].checked_add(amount).context("AMM sell overflow")?;
        let remainder = product.div_ceil(pool[sell]);
        let received = pool[buy] - remainder;
        ensure!(received > 0 && received >= minimum, "applied AMM swap violates slippage");
        pool[buy] = remainder;
        trader[sell] -= amount;
        trader[buy] = trader[buy].checked_add(received).context("AMM trader balance overflow")?;
        self.reserves.insert(pool_id.clone(), (true, pool));
        self.reserves.insert(trader_id.clone(), (false, trader));
        Ok(())
    }

    fn verify(&self, id: &Byte32, data: &[u8]) -> Result<(bool, [u128; 2])> {
        let (pool, expected) = *self.reserves.get(id).context("missing market reserve")?;
        ensure!(data.len() == 65, "invalid reserve length");
        let actual = [
            u128::from_le_bytes(data[..16].try_into()?),
            u128::from_le_bytes(data[16..32].try_into()?),
        ];
        ensure!(
            actual == expected && data[32] == u8::from(!pool),
            "AMM reserve differs from oracle"
        );
        Ok((pool, actual))
    }
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
    let (workload, commands) = Workload::fund(&mut fixture, Mode::HotspotAmm, COUNT, None, None)?;
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
        counters: Option<usize>,
        pools: Option<usize>,
    ) -> Result<(Self, Vec<Transaction>)> {
        ensure!(
            !mode.amm() || counters.is_none(),
            "STEM_LINEAGE_HEADS applies only to counter workloads"
        );
        let counters = counters.unwrap_or(DEFAULT_COUNTERS);
        ensure!(
            (1..=1024).contains(&counters)
                && (mode != Mode::Mixed || counters > 1)
                && (mode != Mode::Atomic || counters.is_multiple_of(8)),
            "STEM_LINEAGE_HEADS must be 1–1024, with at least two for mixed workloads and a multiple of eight for atomic workloads"
        );
        ensure!(
            mode.amm() || pools.is_none(),
            "STEM_LINEAGE_AMM_POOLS applies only to AMM workloads"
        );
        let pools = pools.unwrap_or(TRADERS);
        ensure!((1..=1024).contains(&pools), "invalid STEM_LINEAGE_AMM_POOLS");
        let traders = TRADERS.max(pools);
        let mut dynamic = if mode.amm() {
            ensure!(count.div_ceil(traders) as u128 * 10 <= TRADER_INVENTORY);
            fund_amm_market(fixture, count, pools, traders, TRADER_INVENTORY)?
        } else {
            let registry = FixtureLineage::fund(fixture)?;
            DynamicLineage::fund_using(fixture, &registry, count, counters, None)?
        };
        if mode == Mode::Clob {
            let (registrations, commands) = clob::fund(fixture, &mut dynamic)?;
            return Ok((Self { mode, dynamic, registrations, pools, traders }, commands));
        }
        let registrations = (0..dynamic.ids.len())
            .map(|index| dynamic.register(fixture, index))
            .collect::<Result<_>>()?;
        let versions = (0, count as u64);
        let commands = (0..count)
            .map(|index| match mode {
                Mode::Independent => {
                    dynamic.signed(fixture, index, &[index % counters], versions, 1)
                }
                Mode::HotspotAmm | Mode::DistributedAmm => {
                    let trader = index % traders;
                    let market = index % pools;
                    let pool = if mode == Mode::HotspotAmm || market == 0 {
                        0
                    } else {
                        traders + 1 + market
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
                    let first = (index % (counters / 8)) * 8;
                    let targets: Vec<_> = (first..first + 8).collect();
                    dynamic.signed(fixture, index, &targets, versions, 1)
                }
                Mode::Mixed => {
                    let independent = 1 + index % (counters - 1);
                    let targets = [0, independent];
                    let offset = usize::from(index % 3 != 0);
                    dynamic.signed(fixture, index, &targets[offset..], versions, 1)
                }
                Mode::Clob => unreachable!("CLOB fixture was funded above"),
            })
            .collect::<Result<_>>()?;
        Ok((
            Self {
                mode,
                dynamic,
                registrations,
                pools,
                traders,
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
        let mut market = self.mode.amm().then(|| AmmOracle::new(&self.dynamic.ids, self.traders));
        let mut books = (self.mode == Mode::Clob).then(|| clob::Oracle::new(&self.dynamic.ids));
        let mut included = 0usize;
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
            let mut changed_reserves = HashSet::new();
            for (tx, receipt) in txs.iter().zip(receipts) {
                if let Some(command) = tx.lineage_command() {
                    included += 1;
                    let outcome = Outcome::decode(&receipt.protocol_result)?;
                    if let Some(books) = &mut books {
                        ensure!(matches!(outcome, Outcome::Applied | Outcome::Rejected));
                        books.apply(&self.dynamic, &fixture.spec.network, tx, command, outcome == Outcome::Applied)?;
                        changed_reserves.extend(command.targets.iter().map(|target| target.reference.id()));
                    }
                    cycle_sum += receipt.cycles;
                    total_fee += receipt.fee;
                    if books.is_some() && outcome == Outcome::Rejected {
                        continue;
                    }
                    ensure!(
                        outcome == Outcome::Applied,
                        "unexpected business rejection at height {index}"
                    );
                    applied += 1;
                    if let Some(market) = &mut market {
                        market.apply(command)?;
                        changed_reserves.extend(command.targets.iter().map(|target| target.reference.id()));
                    }
                    for target in &command.targets {
                        *counts.entry(target.reference.id().clone()).or_default() += 1;
                    }
                }
            }
            serial.commit(replay)?;
            for id in changed_reserves {
                let (_, cell, _) = serial.prove_current_lineage(id)?.head.context("missing market Head")?;
                if let Some(market) = &market {
                    market.verify(id, &cell.cell.data())?;
                }
                if let Some(books) = &books {
                    books.verify(id, &cell.cell.data())?;
                }
            }
        }
        ensure!(
            included == expected_count,
            "sampled command count differs after replay"
        );
        let mut reserves = [0u128; 2];
        let mut proofs = Vec::new();
        let mut market_fills = Vec::new();
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
            if let Some(market) = &market {
                let (pool, [first, second]) = market.verify(id, &data)?;
                if pool && count > 0 {
                    market_fills.push(count);
                }
                reserves[0] += first;
                reserves[1] += second;
                ensure!(cell.cell.type_scripts().len() == 3);
            } else if let Some(books) = &books {
                books.verify(id, &data)?;
            } else {
                ensure!(
                    u128::from_le_bytes(data[..16].try_into()?) == 240 + u128::from(count),
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
            let pool_supply = self.pools as u128 * POOL_INVENTORY;
            ensure!(market_fills.iter().sum::<u64>() == applied as u64, "market fill count differs from applied swaps");
            ensure!(
                reserves
                    == [
                        pool_supply + (self.traders as u128 + 1) * TRADER_INVENTORY,
                        pool_supply
                    ],
                "AMM asset supply changed"
            );
        }
        Ok(json!({
            "height": height, "included": included, "applied": applied, "cycles": cycle_sum, "fee": total_fee,
            "clob": books.as_ref().map(clob::Oracle::evidence),
            "registered_heads": self.dynamic.ids.len(),
            "market_active": market_fills.len(), "market_minimum_fills": market_fills.iter().min(),
            "market_maximum_fills": market_fills.iter().max(),
            "head_updates": counts.values().sum::<u64>(), "heads": proofs,
            "serial_replay_us": started.elapsed().as_micros(),
        }))
    }
}
