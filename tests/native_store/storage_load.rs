#[path = "storage_load/external.rs"]
mod external;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::ensure;
use ckb_types::{
    core::ScriptHashType,
    packed::{CellDep, Script},
    prelude::*,
};
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
    shard_senders: bool,
    ready_senders: bool,
    pause_on_full: bool,
    cadence_ms: u64,
    vm_workers: usize,
    block_cycles: u64,
    block_transactions: u32,
    retention: u64,
    measured: bool,
    reuse_submissions: bool,
    complete_volume: bool,
    payment_data_bytes: usize,
    payment_chains: usize,
    prefund_payments: bool,
    payment_type_groups: usize,
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
            shard_senders: read("STEM_STORAGE_SHARD_SENDERS", 0)? != 0,
            ready_senders: read("STEM_STORAGE_READY_SENDERS", 0)? != 0,
            pause_on_full: read("STEM_STORAGE_PAUSE_ON_FULL", 0)? != 0,
            cadence_ms: read("STEM_STORAGE_CADENCE", 250)?,
            vm_workers: read("STEM_STORAGE_VM_WORKERS", 2)?.try_into()?,
            block_cycles: read("STEM_STORAGE_BLOCK_CYCLES", 2_000_000_000)?,
            block_transactions: read("STEM_STORAGE_BLOCK_TRANSACTIONS", 1024)?.try_into()?,
            retention: read("STEM_STORAGE_RETENTION", 64)?,
            measured: read("STEM_STORAGE_METRICS", 1)? != 0,
            reuse_submissions: read("STEM_STORAGE_REUSE_SUBMISSIONS", 0)? != 0,
            complete_volume: read("STEM_STORAGE_COMPLETE_VOLUME", 0)? != 0,
            payment_chains: read("STEM_STORAGE_PAYMENT_CHAINS", 0)?.try_into()?,
            prefund_payments: read("STEM_STORAGE_PREFUND_PAYMENTS", 0)? != 0,
            payment_data_bytes: read("STEM_STORAGE_PAYMENT_DATA_BYTES", 0)?.try_into()?,
            payment_type_groups: read("STEM_STORAGE_PAYMENT_TYPE_GROUPS", 0)?.try_into()?,
        };
        ensure!(
            (4..=16).contains(&load.replicas)
                && (1..=if load.prefund_payments { 10_000 } else { 5_000 }).contains(&load.rate)
                && (1..=3600).contains(&load.seconds)
                && (2..=45).contains(&load.drain)
                && (1..=if load.prefund_payments { 256 } else { 64 }).contains(&load.senders)
                && !(load.shard_senders && load.ready_senders)
                && (!(load.shard_senders || load.ready_senders)
                    || load.senders.is_multiple_of(load.replicas))
                && (50..=1000).contains(&load.cadence_ms)
                && (1..=16).contains(&load.vm_workers)
                && (32..=512).contains(&load.retention)
                && load.rate as u64 * load.seconds
                    <= if load.prefund_payments {
                        2_000_000
                    } else if load.payment_chains == 0 {
                        100_000
                    } else {
                        400_000
                    }
                && load.payment_chains <= 100_000
                && load.payment_chains <= load.rate * load.seconds as usize
                && !(load.prefund_payments && load.payment_chains != 0)
                && load.block_transactions > 0
                && load.block_cycles >= 20_000_000
                && load.payment_data_bytes <= 16_384
                && load.payment_type_groups <= 12,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    queue_full_us: Option<u64>,
    started_us: Option<u64>,
    connected_us: Option<u64>,
    reused_connection: bool,
    offered_us: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    endpoint: Option<usize>,
    response_us: Option<u64>,
    acknowledged_us: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_pause_us: Option<[u64; 2]>,
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

// Bracket a wall-clock reading with the same monotonic clock as the dispatch trace.
fn clock_sample(start: Instant) -> Result<Value> {
    let before_us = micros(start);
    let unix_us = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros())?;
    Ok(json!({"before_us":before_us,"unix_us":unix_us,"after_us":micros(start)}))
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
    let mut fixture = Fixture::new()?;
    let polytype = load.payment_type_groups != 0;
    ensure!(
        !polytype
            || (mode.is_none()
                && !load.prefund_payments
                && load.payment_chains == 0
                && load.payment_data_bytes == 0),
        "mixed Type Groups require independent payments funded directly at genesis"
    );
    (fixture.spec.chain_id, fixture.spec.network) = if polytype {
        ("stem-polytype-load".into(), [142; 32].pack())
    } else if mode.is_some() {
        ("stem-lineage-load".into(), [139; 32].pack())
    } else {
        ("stem-storage-load".into(), [137; 32].pack())
    };
    let limits = &mut fixture.spec.limits;
    limits.transaction_count = load.block_transactions;
    limits.transaction_bytes = 2 * 1024 * 1024;
    limits.transaction_cycles = 20_000_000;
    limits.block_cycles = load.block_cycles;
    limits.cell_reads = if polytype { 32_768 } else { 8_192 };
    limits.cell_read_bytes = 128 * 1024 * 1024;
    limits.cell_write_bytes = 4 * 1024 * 1024;
    limits.header_dep_age = 32;
    let work = load.rate * load.seconds as usize;
    ensure!(
        mode.is_none() || (load.payment_chains == 0 && !load.prefund_payments),
        "payment funding requires a payment workload"
    );
    let funding = if load.prefund_payments {
        funding_splits(&mut fixture, work)?
    } else {
        Vec::new()
    };
    let topology_path = std::env::var_os("STEM_STORAGE_TOPOLOGY").map(PathBuf::from);
    let topology = if let Some(path) = &topology_path {
        ensure!(
            mode.is_none() && load.prefund_payments,
            "external trials require funded payments"
        );
        Some(external::Topology::load(path, &mut fixture, load)?)
    } else {
        None
    };
    if let Some(path) = std::env::var_os("STEM_STORAGE_GENESIS_OUTPUT") {
        return topology
            .as_ref()
            .context("preparing genesis requires a validator topology")?
            .prepare(&fixture, path.into());
    }

    let root =
        std::env::var_os("STEM_TEST_EVIDENCE_ROOT").map_or_else(std::env::temp_dir, PathBuf::from);
    fs::create_dir_all(&root)?;
    let output = tempfile::Builder::new()
        .prefix("stem-storage-")
        .tempdir_in(root)?
        .keep();
    eprintln!("sustained load evidence in {}", output.display());
    let (lineage, transactions_to_offer) = if let Some(mode) = mode {
        ensure!(
            (load.seconds + load.drain + 10) * 1000 / load.cadence_ms < 1000,
            "offer horizon exceeds the fixture's signed expiration"
        );
        let (workload, commands) = lineage_load::Workload::fund(&mut fixture, mode, work)?;
        (Some(workload), commands)
    } else if polytype {
        (
            None,
            polytype_payments(&mut fixture, &output, work, load.payment_type_groups)?,
        )
    } else {
        (
            None,
            payments(
                &mut fixture,
                work,
                load.payment_data_bytes,
                load.payment_chains,
                &funding,
            )?,
        )
    };
    let mut jobs = Vec::with_capacity(work);
    let replay = std::env::var_os("STEM_STORAGE_VERIFY_REPORT");
    let mut file: Box<dyn std::io::Write> = if replay.is_some() {
        Box::new(std::io::sink())
    } else {
        Box::new(std::io::BufWriter::new(fs::File::create(
            output.join("workload.bin"),
        )?))
    };
    let mut hash = ckb_hash::new_blake2b();
    for tx in transactions_to_offer {
        let length = u32::try_from(tx.as_slice().len())?.to_le_bytes();
        for bytes in [length.as_slice(), tx.as_slice()] {
            std::io::Write::write_all(&mut file, bytes)?;
            hash.update(bytes);
        }
        jobs.push((hex::encode(tx.id(&fixture.spec.network).as_slice()), tx));
    }
    std::io::Write::flush(&mut file)?;
    drop(file);
    let mut hash_bytes = [0; 32];
    hash.finalize(&mut hash_bytes);
    let workload_hash = hex::encode(hash_bytes);

    if let Some(report) = replay {
        ensure!(
            topology.is_some(),
            "offline verification requires the prepared topology"
        );
        return external::verify_report(
            std::path::Path::new(&report),
            &fixture,
            &jobs,
            &workload_hash,
            load,
        );
    }
    let (replicas, clients) = if let Some(external) = &topology {
        (Vec::new(), external.connect(&fixture)?)
    } else {
        start_network_configured(
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
        )?
    };
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
    if !funding.is_empty() {
        let proof = finalize_funding(&fixture, &clients, &funding)?;
        fs::write(
            output.join("payment-funding.json"),
            serde_json::to_vec(&proof)?,
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
    let clock_start = clock_sample(start)?;
    let offer_us = load.seconds * 1_000_000;
    let end_us = (load.seconds + load.drain) * 1_000_000;
    let dispatch_deadline = if load.complete_volume {
        end_us
    } else {
        offer_us
    };
    let queues = if load.shard_senders { load.replicas } else { 1 };
    let (dispatch, pending): (Vec<_>, Vec<_>) = (0..queues)
        .map(|_| {
            let (sender, requests) = mpsc::sync_channel::<(usize, Delivery)>(load.senders / queues);
            (sender, Mutex::new(requests))
        })
        .unzip();
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
            .map(|worker| {
                let queue = &pending[worker % queues];
                let clients = &clients;
                let jobs = &jobs;
                scope.spawn(move || {
                    let mut rows = Vec::new();
                    loop {
                        let request = queue.lock().expect("dispatch queue").recv();
                        let Ok((index, mut row)) = request else {
                            break;
                        };
                        // Fixed pools pull from one shared queue when a sender is ready.
                        let route = if load.ready_senders { worker } else { index };
                        let endpoint = route % clients.len();
                        row.submit(
                            &clients[endpoint],
                            &jobs[index].1,
                            start,
                            dispatch_deadline,
                            load.complete_volume,
                        );
                        if load.ready_senders && row.offered_us.is_some() {
                            row.endpoint = Some(endpoint);
                        }
                        if load.pause_on_full
                            && row.error.as_deref() == Some("mempool capacity exceeded")
                        {
                            let began = micros(start);
                            let remaining = dispatch_deadline.saturating_sub(began);
                            thread::sleep(
                                Duration::from_millis(load.cadence_ms)
                                    .min(Duration::from_micros(remaining)),
                            );
                            row.full_pause_us = Some([began, micros(start)]);
                        }
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
                let sender = &dispatch[index % queues];
                if load.complete_volume {
                    assert!(sender.send((index, row)).is_ok(), "no load senders");
                    continue;
                }
                match sender.try_send((index, row)) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full((_, mut row))) => {
                        row.queued_us = None;
                        row.queue_full_us = Some(micros(start));
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
    let clock_end = clock_sample(start)?;
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
    let mut sampled_execution = false;
    let mut cursor = first_height + 1;
    // The measurement scans blocks after the timed observer stops. Rebuild each external
    // archive root from the actual signed workload and link it to certified finality.
    let archive_jobs: HashMap<_, _> = if topology.is_some() {
        jobs.iter().map(|(id, tx)| (id.as_str(), tx)).collect()
    } else {
        HashMap::new()
    };
    let start_header = topology
        .as_ref()
        .map(|_| archive.header(first_height))
        .transpose()?;
    let mut parent = start_header.as_ref().map(|h| h.id(&fixture.spec.network));
    for sample in &samples {
        let height = sample
            .statuses
            .iter()
            .map(|status| status.height)
            .min()
            .unwrap();
        while cursor <= height {
            let ids = transactions(archive, cursor)?;
            let mut block = json!({"height": cursor, "seen_us": sample.at_us, "ids": ids});
            if let Some(previous) = &mut parent {
                let header = archive.header(cursor)?;
                if !sampled_execution && sample.at_us >= offer_us / 2 && !ids.is_empty() {
                    let mut indices = vec![0, ids.len() / 2, ids.len() - 1];
                    indices.dedup();
                    let mut receipts = Vec::new();
                    for index in indices {
                        let data = u32::try_from(index)?.to_le_bytes();
                        let query = archive.query("/stem/execution", cursor, &data, true)?;
                        let receipt: external::SampleExecution =
                            serde_json::from_slice(&query.value)?;
                        receipt.verify(&fixture.spec.network, &header, &ids)?;
                        receipts.push(receipt);
                    }
                    block["receipts"] = json!(receipts);
                    sampled_execution = true;
                }
                external::check_block(
                    &fixture.spec.network,
                    &header,
                    cursor,
                    previous,
                    &ids,
                    &archive_jobs,
                )?;
                *previous = header.id(&fixture.spec.network);
                block["header"] = json!(hex::encode(header.encode()));
            }
            for id in &ids {
                ensure!(
                    seen.insert(id.clone(), sample.at_us).is_none(),
                    "repeated finalized transaction"
                );
            }
            sampled_blocks.push(block);
            cursor += 1;
        }
    }
    if let Some(parent) = parent {
        ensure!(
            parent == final_header.id(&fixture.spec.network),
            "measured chain does not reach the agreed Header"
        );
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
            "start_header": start_header.map(|h| hex::encode(h.encode())),
            "driver_clock": {"start":clock_start,"end":clock_end},
            "external_endpoints": topology.as_ref().map(|t| t.addresses()),
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

    if topology.is_none() {
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
    }
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

const FUNDING_OUTPUTS: usize = 256;

fn funding_splits(fixture: &mut Fixture, work: usize) -> Result<Vec<Transaction>> {
    let leaf = Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?;
    let (code_point, code) = fixture.spec.development_cells[0].clone();
    let mut parents = Vec::new();
    for start in (0..work).step_by(FUNDING_OUTPUTS) {
        let count = (work - start).min(FUNDING_OUTPUTS);
        let seed = OutPoint::new([141; 32].pack(), u32::try_from(parents.len())?);
        let cell = Cell::new(
            FUND * count as u64 + FEE,
            fixture.lock.clone(),
            vec![],
            Bytes::new(),
        )?;
        fixture
            .spec
            .development_cells
            .push((seed.clone(), cell.clone()));
        let split = Transaction::new(
            fixture.deps.clone(),
            vec![],
            vec![stem_ledger::CellInput::new(seed.clone(), None)],
            vec![leaf.clone(); count],
            vec![],
        )?;
        parents.push(fixture.seal_with(split, |point| {
            if point == &seed {
                return Ok(cell.clone());
            }
            ensure!(point == &code_point, "unexpected funding dependency");
            Ok(code.clone())
        })?);
    }
    Ok(parents)
}

fn finalize_funding(
    fixture: &Fixture,
    clients: &[Client],
    funding: &[Transaction],
) -> Result<Value> {
    let archive = clients.last().context("no archive replica")?;
    let mut height = statuses(clients)?
        .iter()
        .map(|s| s.height)
        .min()
        .context("no replicas")?;
    let mut committed = Vec::new();
    for batch in funding.chunks(64) {
        let mut pending: HashSet<_> = batch
            .iter()
            .map(|tx| hex::encode(tx.id(&fixture.spec.network).as_slice()))
            .collect();
        for (i, tx) in batch.iter().enumerate() {
            clients[i % clients.len()].submit(tx)?;
        }
        super::comet::poll(
            "payment funding on all replicas",
            Duration::from_secs(120),
            || {
                let common = statuses(clients)?.iter().map(|s| s.height).min().unwrap();
                while height < common {
                    height += 1;
                    for id in transactions(archive, height)? {
                        if pending.remove(&id) {
                            committed.push((height, id));
                        }
                    }
                }
                Ok(pending.is_empty().then_some(()))
            },
        )?;
    }
    let header = archive.header(height)?;
    for client in clients {
        ensure!(
            client.header(height)? == header,
            "funding header disagreement"
        );
    }
    let outputs: usize = funding.iter().map(|tx| tx.outputs().len()).sum();
    ensure!(
        committed.len() == funding.len(),
        "incomplete payment funding"
    );
    Ok(
        json!({"height":height, "header":hex::encode(header.encode()),
              "outputs": outputs,"outputs_per_split": FUNDING_OUTPUTS,"committed":committed}),
    )
}

fn payments(
    fixture: &mut Fixture,
    work: usize,
    data_bytes: usize,
    chains: usize,
    funding: &[Transaction],
) -> Result<Vec<Transaction>> {
    let base = Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?;
    let data = Bytes::from(vec![0x6a; data_bytes]);
    let (code_point, code) = fixture.spec.development_cells[0].clone();
    let mut payments = Vec::with_capacity(work);
    let parents: Vec<_> = funding
        .iter()
        .map(|tx| tx.id(&fixture.spec.network))
        .collect();
    let width = if chains == 0 { work } else { chains };
    let mut heads: Vec<(OutPoint, Cell)> = Vec::with_capacity(chains);
    for index in 0..work {
        let slot = index % width;
        let (point, spent) = if !parents.is_empty() {
            (
                OutPoint::new(
                    parents[index / FUNDING_OUTPUTS].clone(),
                    (index % FUNDING_OUTPUTS) as u32,
                ),
                base.clone(),
            )
        } else if index < width {
            let point = OutPoint::new([138; 32].pack(), index as u32);
            fixture
                .spec
                .development_cells
                .push((point.clone(), base.clone()));
            (point, base.clone())
        } else {
            heads[slot].clone()
        };
        let change = Cell::new(
            spent.capacity() - FEE,
            fixture.lock.clone(),
            vec![],
            data.clone(),
        )?;
        let tx = Transaction::new(
            fixture.deps.clone(),
            vec![],
            vec![stem_ledger::CellInput::new(point.clone(), None)],
            vec![change.clone()],
            vec![],
        )?;
        let tx = fixture.seal_with(tx, |input| {
            if input == &point {
                Ok(spent.clone())
            } else {
                ensure!(input == &code_point, "unexpected signing dependency");
                Ok(code.clone())
            }
        })?;
        if chains != 0 {
            let next = (OutPoint::new(tx.id(&fixture.spec.network), 0), change);
            if index < width {
                heads.push(next);
            } else {
                heads[slot] = next;
            }
        }
        payments.push(tx);
    }
    Ok(payments)
}

fn polytype_payments(
    fixture: &mut Fixture,
    directory: &Path,
    work: usize,
    type_groups: usize,
) -> Result<Vec<Transaction>> {
    let mut programs = vec![fixture.spec.development_cells[0].clone()];
    let mut deps = fixture.deps.clone();
    let mut type_hashes = Vec::new();
    for rule in [1, 2, 3] {
        super::comet::compile_type(directory, rule)?;
        let code = Bytes::from(fs::read(directory.join(format!("rule-{rule}")))?);
        let point = OutPoint::new([19; 32].pack(), rule);
        let bond = Cell::minimum_capacity(&fixture.lock, &[], code.len())?;
        type_hashes.push(ckb_hash::blake2b_256(&code));
        let cell = Cell::new(bond, fixture.lock.clone(), vec![], code)?;
        programs.push((point.clone(), cell.clone()));
        fixture.spec.development_cells.push((point.clone(), cell));
        deps.push(CellDep::new_builder().out_point(point).build());
    }
    let forms = (0..=type_groups)
        .map(|count| {
            let types: Vec<_> = (0..count)
                .map(|index| {
                    Script::new_builder()
                        .hash_type(ScriptHashType::Data)
                        .code_hash(type_hashes[index % type_hashes.len()])
                        .args(Bytes::copy_from_slice(&(index as u32).to_le_bytes()))
                        .build()
                })
                .collect();
            let data = Bytes::from_static(&[25, 0, 0, 0, 0, 0, 0, 0, 1]);
            let input = Cell::new(FUND, fixture.lock.clone(), types.clone(), data.clone())?;
            let output = Cell::new(FUND - FEE, fixture.lock.clone(), types, data)?;
            Ok((input, output))
        })
        .collect::<Result<Vec<_>>>()?;
    (0..work)
        .map(|index| {
            let (input, output) = &forms[index % forms.len()];
            let point = OutPoint::new([138; 32].pack(), index as u32);
            fixture
                .spec
                .development_cells
                .push((point.clone(), input.clone()));
            let tx = Transaction::new(
                deps.clone(),
                vec![],
                vec![stem_ledger::CellInput::new(point.clone(), None)],
                vec![output.clone()],
                vec![],
            )?;
            fixture.seal_with(tx, |input_point| {
                if input_point == &point {
                    Ok(input.clone())
                } else {
                    programs
                        .iter()
                        .find(|(point, _)| point == input_point)
                        .map(|(_, cell)| cell.clone())
                        .context("unexpected signing dependency")
                }
            })
        })
        .collect()
}

#[test]
fn mixed_type_groups_match_serial_execution() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.spec.limits.transaction_count = 260;
    fixture.spec.limits.transaction_cycles = 20_000_000;
    fixture.spec.limits.block_cycles = 4_000_000_000;
    fixture.spec.limits.cell_reads = 32_768;
    fixture.spec.limits.cell_read_bytes = 128 * 1024 * 1024;
    fixture.spec.limits.cell_write_bytes = 4 * 1024 * 1024;
    let home = tempfile::tempdir()?;
    let transactions = polytype_payments(&mut fixture, home.path(), 260, 12)?;
    let open = |name| {
        super::Store::open(&home.path().join(name), &fixture.spec, super::Retention::Archive)
    };
    let serial = open("serial")?.prepare(&transactions, super::TIME, false)?;
    let parallel = open("parallel")?;
    let workers = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let prepared = workers.install(|| parallel.prepare(&transactions, super::TIME, true))?;
    assert_eq!(prepared.header().encode(), serial.header().encode());
    assert_eq!(prepared.receipts(), serial.receipts());
    assert!(
        serial.receipts()[..13]
            .windows(2)
            .all(|pair| pair[0].cycles < pair[1].cycles)
    );
    Ok(())
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
