use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    time::{Duration, Instant},
};

use anyhow::ensure;
use ckb_types::{packed::Byte32, prelude::*};
use serde::Serialize;
use serde_json::{Value, json};
use stem::jolteon::Client;
use stem_ledger::{lineage::Outcome, record};

use super::{
    Context, Fixture, Result, Retention, Store, Transaction,
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
                        *counts.entry(target.id.clone()).or_default() += 1;
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
                id,
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
