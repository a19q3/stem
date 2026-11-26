use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    process::Command,
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::ensure;
use ckb_types::prelude::*;
use serde::Serialize;
use serde_json::{Value, json};
use stem::jolteon::{Client, Status, SubmissionProgress};
use stem_ledger::light::FinalityVerifier;
use stem_stake::Member;

use super::{
    Bytes, Cell, Context, FEE, FUND, Fixture, OutPoint, Result, Transaction,
    comet::Process,
    keeper_load::{
        NetworkOptions, ProcessStats, replica_binary, start_network_configured, stats, statuses,
        transactions,
    },
    lineage_load,
};

#[derive(Clone, Copy, Serialize)]
struct Load {
    replicas: usize,
    rate: usize,
    seconds: u64,
    drain: u64,
    senders: usize,
    cadence_ms: u64,
    vm_workers: usize,
    block_cycles: u64,
    retention: u64,
    measured: bool,
    reuse_submissions: bool,
    complete_volume: bool,
    payment_data_bytes: usize,
}

impl Load {
    fn load() -> Result<Self> {
        let read = |name: &str, default: u64| -> Result<u64> {
            Ok(std::env::var(name)
                .ok()
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(default))
        };
        let load = Self {
            replicas: read("STEM_STORAGE_REPLICAS", 4)?.try_into()?,
            rate: read("STEM_STORAGE_RATE", 400)?.try_into()?,
            seconds: read("STEM_STORAGE_SECONDS", 45)?,
            drain: read("STEM_STORAGE_DRAIN", 12)?,
            senders: read("STEM_STORAGE_SENDERS", 24)?.try_into()?,
            cadence_ms: read("STEM_STORAGE_CADENCE", 250)?,
            vm_workers: read("STEM_STORAGE_VM_WORKERS", 2)?.try_into()?,
            block_cycles: read("STEM_STORAGE_BLOCK_CYCLES", 2_000_000_000)?,
            retention: read("STEM_STORAGE_RETENTION", 64)?,
            measured: read("STEM_STORAGE_METRICS", 1)? != 0,
            reuse_submissions: read("STEM_STORAGE_REUSE_SUBMISSIONS", 0)? != 0,
            complete_volume: read("STEM_STORAGE_COMPLETE_VOLUME", 0)? != 0,
            payment_data_bytes: read("STEM_STORAGE_PAYMENT_DATA_BYTES", 0)?.try_into()?,
        };
        ensure!(
            (4..=16).contains(&load.replicas)
                && (1..=5000).contains(&load.rate)
                && (1..=600).contains(&load.seconds)
                && (2..=45).contains(&load.drain)
                && (1..=64).contains(&load.senders)
                && (50..=1000).contains(&load.cadence_ms)
                && (1..=16).contains(&load.vm_workers)
                && (32..=512).contains(&load.retention)
                && load.rate as u64 * load.seconds <= 100_000
                && load.block_cycles >= 20_000_000
                && load.payment_data_bytes <= 16_384,
            "invalid sustained storage experiment settings"
        );
        Ok(load)
    }
}

#[derive(Default, Serialize)]
struct Delivery {
    id: String,
    planned_us: u64,
    queued_us: Option<u64>,
    started_us: Option<u64>,
    connected_us: Option<u64>,
    reused_connection: bool,
    offered_us: Option<u64>,
    response_us: Option<u64>,
    acknowledged_us: Option<u64>,
    error: Option<String>,
}

const DISPATCH_GRACE_US: u64 = 250_000;

impl Delivery {
    fn expired(&self, now: u64, deadline: u64, complete_volume: bool) -> bool {
        now >= deadline || (!complete_volume && now > self.planned_us + DISPATCH_GRACE_US)
    }

    fn submit(
        &mut self,
        client: &Client,
        tx: &Transaction,
        start: Instant,
        deadline: u64,
        complete_volume: bool,
    ) {
        let now = micros(start);
        if self.expired(now, deadline, complete_volume) {
            self.error = Some("queue deadline".into());
            return;
        }
        self.started_us = Some(now);
        let mut progress = SubmissionProgress::default();
        let result = client.submit_with_progress(tx, &mut progress);
        let elapsed = |at: Instant| at.duration_since(start).as_micros() as u64;
        self.connected_us = progress.connected_at.map(elapsed);
        self.reused_connection = progress.reused_connection;
        self.offered_us = progress.frame_written_at.map(elapsed);
        self.response_us = progress.response_read_at.map(elapsed);
        match result {
            Ok(_) => self.acknowledged_us = Some(micros(start)),
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }
}

#[derive(Serialize)]
struct Sample {
    at_us: u64,
    statuses: Vec<Status>,
    resources: Vec<ProcessStats>,
}

#[derive(Serialize)]
struct Capture {
    statuses: Vec<Status>,
    resources: Vec<ProcessStats>,
    storage: Vec<Value>,
}

fn capture(replicas: &[Process], clients: &[Client], measured: bool) -> Result<Capture> {
    let statuses = statuses(clients)?;
    let resources = replicas
        .iter()
        .map(|process| stats(process.pid()))
        .collect::<Result<_>>()?;
    let storage = if measured {
        clients
            .iter()
            .map(|client| {
                let sample = client.query("/stem/storage-metrics", 0, &[], false)?;
                Ok(serde_json::from_slice(&sample.value)?)
            })
            .collect::<Result<_>>()?
    } else {
        Vec::new()
    };
    Ok(Capture {
        statuses,
        resources,
        storage,
    })
}

fn micros(start: Instant) -> u64 {
    start.elapsed().as_micros() as u64
}

#[test]
#[ignore = "locally replicated sustained payment and storage experiment"]
fn sustained_payment_storage() -> Result<()> {
    sustained_storage(None)
}

#[test]
#[ignore = "sustained real native Jolteon Lineage commands"]
fn sustained_lineage_load() -> Result<()> {
    sustained_storage(Some(lineage_load::Mode::load()?))
}

fn sustained_storage(mode: Option<lineage_load::Mode>) -> Result<()> {
    let load = Load::load()?;
    let root =
        std::env::var_os("STEM_TEST_EVIDENCE_ROOT").map_or_else(std::env::temp_dir, PathBuf::from);
    fs::create_dir_all(&root)?;
    let output = tempfile::Builder::new()
        .prefix("stem-storage-")
        .tempdir_in(root)?
        .keep();
    eprintln!("sustained load evidence in {}", output.display());
    let mut fixture = Fixture::new()?;
    (fixture.spec.chain_id, fixture.spec.network) = if mode.is_some() {
        ("stem-lineage-load".into(), [139; 32].pack())
    } else {
        ("stem-storage-load".into(), [137; 32].pack())
    };
    let limits = &mut fixture.spec.limits;
    limits.transaction_count = 1024;
    limits.transaction_bytes = 2 * 1024 * 1024;
    limits.transaction_cycles = 20_000_000;
    limits.block_cycles = load.block_cycles;
    limits.cell_reads = 8192;
    limits.cell_read_bytes = 128 * 1024 * 1024;
    limits.cell_write_bytes = 4 * 1024 * 1024;
    limits.header_dep_age = 32;
    let work = load.rate * load.seconds as usize;
    let (lineage, transactions_to_offer) = if let Some(mode) = mode {
        ensure!(
            (load.seconds + load.drain + 10) * 1000 / load.cadence_ms < 1000,
            "offer horizon exceeds the fixture's signed expiration"
        );
        let (workload, commands) = lineage_load::Workload::fund(&mut fixture, mode, work)?;
        (Some(workload), commands)
    } else {
        (None, payments(&mut fixture, work, load.payment_data_bytes)?)
    };
    let mut jobs = Vec::with_capacity(work);
    let mut workload = Vec::new();
    for tx in transactions_to_offer {
        workload.extend_from_slice(&(tx.as_slice().len() as u32).to_le_bytes());
        workload.extend_from_slice(tx.as_slice());
        jobs.push((hex::encode(tx.id(&fixture.spec.network).as_slice()), tx));
    }
    fs::write(output.join("workload.bin"), &workload)?;
    let workload_hash = hex::encode(ckb_hash::blake2b_256(&workload));
    drop(workload);

    let (replicas, clients) = start_network_configured(
        &mut fixture,
        &output,
        load.replicas,
        NetworkOptions {
            interval_ms: load.cadence_ms,
            timeout_ms: 2400,
            vm_workers: load.vm_workers,
            history_window: Some(load.retention),
            storage_metrics: load.measured,
        },
    )?;
    let clients = if load.reuse_submissions {
        clients
            .into_iter()
            .map(Client::with_submission_reuse)
            .collect()
    } else {
        clients
    };
    if let Some(lineage) = &lineage {
        let registration = lineage.register(&fixture, &clients)?;
        fs::write(
            output.join("lineage-registration.json"),
            serde_json::to_vec(&registration)?,
        )?;
    }
    let before = capture(&replicas, &clients, load.measured)?;
    let first_height = before
        .statuses
        .iter()
        .map(|status| status.height)
        .min()
        .context("no replicas")?;
    fs::write(output.join("before.json"), serde_json::to_vec(&before)?)?;
    let start = Instant::now();
    let offer_us = load.seconds * 1_000_000;
    let end_us = (load.seconds + load.drain) * 1_000_000;
    let dispatch_deadline = if load.complete_volume {
        end_us
    } else {
        offer_us
    };
    let (dispatch, pending) = mpsc::sync_channel::<(usize, Delivery)>(load.senders);
    let pending = Mutex::new(pending);
    let (deliveries, samples, failures) = thread::scope(|scope| {
        let observer = scope.spawn(|| {
            let mut samples = Vec::new();
            let mut failures = Vec::new();
            while micros(start) <= end_us {
                match statuses(&clients).and_then(|values| {
                    let resources = replicas
                        .iter()
                        .map(|p| stats(p.pid()))
                        .collect::<Result<_>>()?;
                    Ok(Sample {
                        at_us: micros(start),
                        statuses: values,
                        resources,
                    })
                }) {
                    Ok(sample) => samples.push(sample),
                    Err(error) => failures.push(format!("{error:#}")),
                }
                thread::sleep(Duration::from_millis(50));
            }
            (samples, failures)
        });
        let senders: Vec<_> = (0..load.senders)
            .map(|_| {
                scope.spawn(|| {
                    let mut rows = Vec::new();
                    loop {
                        let request = pending.lock().expect("dispatch queue").recv();
                        let Ok((index, mut row)) = request else {
                            break;
                        };
                        row.submit(
                            &clients[index % clients.len()],
                            &jobs[index].1,
                            start,
                            dispatch_deadline,
                            load.complete_volume,
                        );
                        rows.push(row);
                    }
                    rows
                })
            })
            .collect();
        let mut rows = Vec::new();
        for (index, (id, _)) in jobs.iter().enumerate() {
            let planned_us = index as u64 * 1_000_000 / load.rate as u64;
            let when = start + Duration::from_micros(planned_us);
            thread::sleep(when.saturating_duration_since(Instant::now()));
            let now = micros(start);
            let mut row = Delivery {
                id: id.clone(),
                planned_us,
                ..Delivery::default()
            };
            if row.expired(now, dispatch_deadline, load.complete_volume) {
                row.error = Some("dispatch deadline".into());
                rows.push(row);
            } else {
                row.queued_us = Some(now);
                if load.complete_volume {
                    assert!(dispatch.send((index, row)).is_ok(), "no load senders");
                    continue;
                }
                match dispatch.try_send((index, row)) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full((_, mut row))) => {
                        row.queued_us = None;
                        row.error = Some("dispatch queue full".into());
                        rows.push(row);
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => panic!("no load senders"),
                }
            }
        }
        drop(dispatch);
        for sender in senders {
            rows.extend(sender.join().expect("load submitter"));
        }
        rows.sort_unstable_by_key(|row| row.planned_us);
        let (samples, failures) = observer.join().expect("replica observer");
        (rows, samples, failures)
    });
    ensure!(deliveries.len() == work, "incomplete client dispatch trace");
    let after = capture(&replicas, &clients, load.measured)?;
    let common_height = samples
        .last()
        .context("no common samples")?
        .statuses
        .iter()
        .map(|status| status.height)
        .min()
        .context("no replicas")?;
    let archive = clients.last().context("no archive replica")?;
    let final_header = archive.header(common_height)?;
    for client in &clients {
        ensure!(
            client.header(common_height)? == final_header,
            "replicas differ at sampled finality height"
        );
    }

    let mut seen = HashMap::new();
    let mut sampled_blocks = Vec::new();
    let mut cursor = first_height + 1;
    for sample in &samples {
        let height = sample
            .statuses
            .iter()
            .map(|status| status.height)
            .min()
            .unwrap();
        while cursor <= height {
            let ids = transactions(archive, cursor)?;
            for id in &ids {
                ensure!(
                    seen.insert(id.clone(), sample.at_us).is_none(),
                    "repeated finalized transaction"
                );
            }
            sampled_blocks.push(json!({"height": cursor, "seen_us": sample.at_us, "ids": ids}));
            cursor += 1;
        }
    }
    let certificate = archive.finality(common_height)?;
    let members = fixture
        .spec
        .development_validators
        .iter()
        .map(|(key, power)| Member {
            key: *key,
            power: *power as u64,
        })
        .collect::<Vec<_>>();
    let genesis = archive.header(0)?.id(&fixture.spec.network);
    let verifier = FinalityVerifier::new(fixture.spec.network.clone(), genesis, &members)?;
    ensure!(
        verifier.verify(&certificate.proof, false)?.header() == &final_header,
        "finality differs from sampled Header"
    );
    let attempted = deliveries
        .iter()
        .filter(|row| row.started_us.is_some())
        .count();
    let offered = deliveries
        .iter()
        .filter(|row| row.offered_us.is_some())
        .count();
    let offered_in_window = deliveries
        .iter()
        .filter(|row| row.offered_us.is_some_and(|at| at <= offer_us))
        .count();
    let admitted = deliveries
        .iter()
        .filter(|row| row.acknowledged_us.is_some())
        .count();
    let offered_ids: HashSet<_> = deliveries
        .iter()
        .filter(|row| row.offered_us.is_some())
        .map(|row| &row.id)
        .collect();
    ensure!(
        seen.keys().all(|id| offered_ids.contains(id)),
        "archive contains unknown transaction"
    );
    let admitted_in_window = deliveries
        .iter()
        .filter(|row| row.acknowledged_us.is_some_and(|time| time <= offer_us))
        .count();
    let visible_without_ack = deliveries
        .iter()
        .filter(|row| row.acknowledged_us.is_none() && seen.contains_key(&row.id))
        .count();
    let in_window = seen.values().filter(|&&time| time <= offer_us).count();
    let stages = |from: fn(&Delivery) -> Option<u64>, to: fn(&Delivery) -> Option<u64>| {
        percentiles(
            deliveries
                .iter()
                .filter_map(|row| Some(to(row)? - from(row)?)),
        )
    };
    let final_time = seen.values().copied().max().unwrap_or(0).max(offer_us);
    let summary = json!({
        "reused_connections": deliveries.iter().filter(|row| row.reused_connection).count(),
        "planned": work, "attempted": attempted, "offered": offered, "admitted": admitted, "visible": seen.len(),
        "client_drops": work - attempted, "transport_write_failures": attempted - offered, "visible_during_offer": in_window,
        "offered_during_window": offered_in_window, "offer_tps": offered_in_window as f64 / load.seconds as f64,
        "admitted_during_offer": admitted_in_window, "visible_without_ack": visible_without_ack,
        "admission_tps": admitted_in_window as f64 / load.seconds as f64,
        "visibility_tps": in_window as f64 / load.seconds as f64,
        "finalized_tps_with_drain": seen.len() as f64 * 1_000_000.0 / final_time as f64,
        "latency_us": percentiles(deliveries.iter().filter_map(|row| {
            Some(seen.get(&row.id)?.saturating_sub(row.offered_us?))
        })),
        "submission_us": {
            "dispatch": stages(|r| Some(r.planned_us), |r| r.queued_us),
            "queue": stages(|r| r.queued_us, |r| r.started_us),
            "connect": stages(|r| r.started_us, |r| r.connected_us),
            "write": stages(|r| r.connected_us, |r| r.offered_us),
            "response": stages(|r| r.offered_us, |r| r.response_us),
        },
    });
    fs::write(
        output.join("report.json"),
        serde_json::to_vec(&json!({
            "mode": mode, "load": load, "limits": fixture.spec.limits, "workload_id": workload_hash,
            "summary": summary, "before": before, "after": after,
            "samples": samples, "blocks": sampled_blocks, "deliveries": deliveries, "observation_errors": failures,
            "agreed_height": common_height, "header": hex::encode(final_header.encode()),
            "finality": hex::encode(certificate.proof), "network": hex::encode(fixture.spec.network.as_slice()),
        }))?,
    )?;

    if let Some(lineage) = lineage {
        drop(replicas);
        let audit = lineage.audit(
            &fixture,
            &output,
            clients.len() - 1,
            common_height,
            seen.len(),
        )?;
        fs::write(
            output.join("lineage-audit.json"),
            serde_json::to_vec(&audit)?,
        )?;
    }

    // This separate process opens only the disposable archive and a new index.
    let mut history = Command::new(
        std::env::var_os("STEM_TEST_GNU_TIME").unwrap_or_else(|| "/usr/bin/time".into()),
    );
    let index = output.join("history-index");
    let indexed = history
        .arg("-v")
        .arg(replica_binary())
        .args(["checkpoint", "history", "--archive-db"])
        .arg(output.join(format!("node-{}/db", clients.len() - 1)))
        .arg("--index")
        .arg(index)
        .arg("--network")
        .arg(hex::encode(fixture.spec.network.as_slice()))
        .arg("--height")
        .arg(common_height.to_string())
        .args(["--header", "1"])
        .output()?;
    fs::write(output.join("index-process.stderr"), indexed.stderr)?;
    fs::write(output.join("history-index-proof.json"), indexed.stdout)?;
    ensure!(
        indexed.status.success(),
        "standalone history index measurement failed"
    );
    println!("{summary}");
    ensure!(
        offered != 0 && !seen.is_empty(),
        "no replicated load evidence"
    );
    Ok(())
}

fn percentiles(from: impl Iterator<Item = u64>) -> Value {
    let mut samples: Vec<_> = from.collect();
    samples.sort_unstable();
    let at = |p: usize| samples.get((p * samples.len()).div_ceil(100).saturating_sub(1));
    json!({"p50": at(50), "p95": at(95), "p99": at(99)})
}

fn payments(fixture: &mut Fixture, work: usize, data_bytes: usize) -> Result<Vec<Transaction>> {
    let base = Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?;
    let change = Cell::new(
        FUND - FEE,
        fixture.lock.clone(),
        vec![],
        Bytes::from(vec![0x6a; data_bytes]),
    )?;
    let (code_point, code) = fixture.spec.development_cells[0].clone();
    let mut payments = Vec::with_capacity(work);
    for index in 0..work {
        let point = OutPoint::new([138; 32].pack(), index as u32);
        fixture
            .spec
            .development_cells
            .push((point.clone(), base.clone()));
        let tx = Transaction::new(
            fixture.deps.clone(),
            vec![],
            vec![stem_ledger::CellInput::new(point.clone(), None)],
            vec![change.clone()],
            vec![],
        )?;
        let tx = fixture.seal_with(tx, |input| {
            if input == &point {
                Ok(base.clone())
            } else {
                ensure!(input == &code_point, "unexpected signing dependency");
                Ok(code.clone())
            }
        })?;
        payments.push(tx);
    }
    Ok(payments)
}

#[test]
fn storage_metrics_partition_committed_batches_and_snapshot_restore() -> Result<()> {
    let fixture = Fixture::new()?;
    let home = tempfile::tempdir()?;
    let mut store = super::Store::open_with_storage_metrics(
        home.path(),
        &fixture.spec,
        super::Retention::Archive,
        true,
    )?;
    let tx = fixture.signed(fixture.spend.clone(), None, FUND - FEE, None)?;
    let valid = store.prepare(std::slice::from_ref(&tx), super::TIME, true)?;
    let stale = store.prepare(&[tx], super::TIME, false)?;
    store.commit(valid)?;
    assert!(store.commit(stale).is_err());
    let sample = serde_json::to_value(store.storage_metrics()?.context("missing metrics")?)?;
    assert_eq!(sample["writes"]["bootstrap"]["writes"], 1);
    assert_eq!(sample["writes"]["finality"]["writes"], 1);
    let bytes = sample["writes"]["finality"]["batch_bytes"]
        .as_object()
        .unwrap();
    assert!(bytes["execution"].as_u64().unwrap() > 0);
    assert!(bytes["live_cells"].as_u64().unwrap() > 0);
    assert!(bytes["cell_trie"].as_u64().unwrap() > 0);
    let parts = bytes
        .iter()
        .filter(|(key, _)| *key != "serialized")
        .map(|(_, bytes)| bytes.as_u64().unwrap())
        .sum::<u64>();
    assert_eq!(parts, bytes["serialized"].as_u64().unwrap());
    assert!(sample["rocksdb"]["wal_bytes"].as_u64().unwrap() > 0);

    let destination = tempfile::tempdir()?;
    let mut restored = super::Store::open_with_storage_metrics(
        destination.path(),
        &fixture.spec,
        super::Retention::Archive,
        true,
    )?;
    restored.install_state(
        store.export_state()?,
        &store.tip().app_hash(store.network()),
    )?;
    assert_eq!(store.tip(), restored.tip());
    let sample = serde_json::to_value(restored.storage_metrics()?.unwrap())?;
    assert_eq!(sample["writes"]["restore"]["writes"], 1);
    assert_eq!(sample["writes"]["finality"]["writes"], 0);
    assert!(
        sample["writes"]["restore"]["batch_bytes"]["cell_trie"]
            .as_u64()
            .unwrap()
            > 0
    );
    Ok(())
}
