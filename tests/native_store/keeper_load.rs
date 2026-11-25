use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::ensure;
use ckb_types::prelude::*;
use ed25519_dalek::SigningKey;
use serde::Serialize;
use serde_json::{Value, json};
use stem::jolteon::{Client, Status, verify_finality};
use stem_ledger::{
    block::Receipt,
    keeper::{Retry, Schedule, Trigger, decode_agenda},
    lineage::{self, Outcome},
    proof::InclusionProof,
};

use super::{
    Bytes, Cell, Context, FEE, FUND, Fixture, OutPoint, Result, Retention, Store, Transaction,
    comet::{Process, poll, write_spec},
    keeper,
    lineage::{DynamicLineage, FixtureLineage},
};

#[derive(Clone, Copy, Serialize)]
struct Workload {
    replicas: usize,
    seconds: usize,
    keeper_per_second: usize,
    payments_per_second: usize,
    drain_seconds: usize,
    hot_height: bool,
}

impl Workload {
    fn load() -> Result<Self> {
        let read = |name: &str, default: usize| -> Result<usize> {
            Ok(std::env::var(name)
                .ok()
                .map(|value| value.parse())
                .transpose()?
                .unwrap_or(default))
        };
        let load = Self {
            replicas: read("STEM_KEEPER_REPLICAS", 4)?,
            seconds: read("STEM_KEEPER_SECONDS", 30)?,
            keeper_per_second: read("STEM_KEEPER_RATE", 6)?,
            payments_per_second: read("STEM_KEEPER_PAYMENTS", 24)?,
            drain_seconds: read("STEM_KEEPER_DRAIN", 18)?,
            hot_height: read("STEM_KEEPER_HOT_HEIGHT", 0)? == 1,
        };
        ensure!(
            (4..=16).contains(&load.replicas)
                && (2..=120).contains(&load.seconds)
                && (1..=32).contains(&load.keeper_per_second)
                && (1..=64).contains(&load.payments_per_second)
                && (4..=90).contains(&load.drain_seconds)
                && load.seconds * (load.keeper_per_second + load.payments_per_second) <= 4096,
            "invalid Keeper load fixture configuration"
        );
        Ok(load)
    }
}

#[derive(Serialize)]
struct Delivery {
    kind: &'static str,
    planned_ms: u64,
    sent_ms: Option<u64>,
    reply_ms: Option<u64>,
    carrier: Option<String>,
    command: Option<String>,
    trigger: Option<u64>,
    error: Option<String>,
}

#[derive(Serialize)]
struct Block {
    height: u64,
    seen_ms: u64,
    transactions: Vec<String>,
}

#[derive(Serialize)]
struct Sample {
    at_ms: u64,
    height: u64,
    agenda_entries: usize,
    pending_retries: usize,
    pools: Vec<usize>,
}

#[derive(Serialize)]
struct Observation {
    blocks: Vec<Block>,
    samples: Vec<Sample>,
    failures: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct ProcessStats {
    cpu_ticks: u64,
    write_bytes: u64,
    resident_kib: u64,
}

#[test]
#[ignore = "sustained real native Jolteon load with Keeper and ordinary payments"]
fn sustained_keeper_load() -> Result<()> {
    let load = Workload::load()?;
    let mut builder = tempfile::Builder::new();
    builder.prefix("stem-keeper-load-");
    let output = match std::env::var_os("STEM_TEST_EVIDENCE_ROOT") {
        Some(path) => {
            fs::create_dir_all(&path)?;
            builder.tempdir_in(PathBuf::from(path))?
        }
        None => builder.tempdir()?,
    }
    .keep();
    eprintln!("sustained Keeper evidence in {}", output.display());
    let jobs = load.seconds * load.keeper_per_second;
    let transfers = load.seconds * load.payments_per_second;
    let mut fixture = Fixture::new()?;
    fixture.spec.chain_id = "stem-keeper-load".into();
    fixture.spec.network = [128; 32].pack();
    let registry = FixtureLineage::fund(&mut fixture)?;
    let lineage = DynamicLineage::fund_with_prefix(&mut fixture, &registry, jobs, jobs, None, 151)?;
    keeper::fund_carriers(&mut fixture, u32::try_from(jobs)?)?;
    fixture.spec.keeper.as_mut().unwrap().max_delay = 128;
    fixture.spec.keeper.as_mut().unwrap().per_height = 2;
    fixture.spec.keeper.as_mut().unwrap().time = Some(stem::native_store::KeeperTimePolicy {
        per_height: 2,
        max_delay_ms: 10_000,
    });
    fixture.spec.limits.transaction_count = 64;
    fixture.spec.limits.cell_reads = 512;
    fixture.spec.limits.cell_write_bytes = 256 * 1024;
    let payment_points = (0..transfers)
        .map(|index| OutPoint::new([129; 32].pack(), index as u32))
        .collect::<Vec<_>>();
    for point in &payment_points {
        fixture.spec.development_cells.push((
            point.clone(),
            Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?,
        ));
    }
    let payments = payment_points
        .into_iter()
        .map(|point| fixture.signed(point, None, FUND - FEE, None))
        .collect::<Result<Vec<_>>>()?;
    let registrations = (0..jobs)
        .map(|index| lineage.register(&fixture, index))
        .collect::<Result<Vec<_>>>()?;
    let (replicas, clients) = start_network(&mut fixture, &output, load.replicas)?;
    for chunk in registrations.chunks(32) {
        let mut height = clients[0].status()?.height + 1;
        for tx in chunk {
            clients[0].submit(tx)?;
        }
        let pending: BTreeSet<_> = chunk.iter().map(|tx| id(tx, &fixture)).collect();
        let mut pending = pending;
        poll(
            "registered Keeper load Heads",
            Duration::from_secs(15),
            || {
                let top = clients[0].status()?.height;
                while height <= top && !pending.is_empty() {
                    for id in transactions(&clients[0], height)? {
                        pending.remove(&id);
                    }
                    height += 1;
                }
                Ok(pending.is_empty().then_some(()))
            },
        )?;
    }
    let start_height = clients[0].status()?.height;
    wait_common(&clients, start_height)?;
    let initial_status = statuses(&clients)?;
    let initial_stats = replicas
        .iter()
        .map(|process| stats(process.pid()))
        .collect::<Result<Vec<_>>>()?;
    let hot_height = load
        .hot_height
        .then_some(start_height + 8 + 4 * load.seconds as u64);
    let start = Instant::now();
    let end = start + Duration::from_secs((load.seconds + load.drain_seconds) as u64);
    let (ordinary, bookings, observation) = thread::scope(|scope| {
        let observer = scope.spawn(|| observe(&clients, start_height, start, end));
        let payment = scope.spawn(|| {
            payments
                .iter()
                .enumerate()
                .map(|(index, tx)| {
                    let planned = pace(start, index, load.payments_per_second);
                    deliver(
                        &clients[0],
                        start,
                        planned,
                        "payment",
                        || Ok((tx.clone(), None, None)),
                        &fixture.spec.network,
                    )
                })
                .collect::<Vec<_>>()
        });
        let bookings = (0..jobs)
            .map(|index| {
                let planned = pace(start, index, load.keeper_per_second);
                let kind = if load.hot_height {
                    "height"
                } else {
                    match index % 4 {
                        0 => "height",
                        1 => "retry",
                        _ => "time",
                    }
                };
                deliver(
                    &clients[0],
                    start,
                    planned,
                    kind,
                    || {
                        let (carrier, command, target) =
                            booking(&fixture, &lineage, &clients[0], index, kind, hot_height)?;
                        Ok((carrier, Some(command), Some(target)))
                    },
                    &fixture.spec.network,
                )
            })
            .collect::<Vec<_>>();
        (payment.join().unwrap(), bookings, observer.join().unwrap())
    });
    let observation = observation?;
    let final_status = statuses(&clients)?;
    let final_stats = replicas
        .iter()
        .map(|process| stats(process.pid()))
        .collect::<Result<Vec<_>>>()?;
    fs::write(
        output.join("observations.json"),
        serde_json::to_vec_pretty(&json!({
            "workload": load, "start_height": start_height, "hot_height": hot_height,
            "ordinary": ordinary, "bookings": bookings,
            "observation": observation, "initial_status": initial_status, "final_status": final_status,
            "initial_stats": initial_stats, "final_stats": final_stats,
        }))?,
    )?;
    let latest = observation
        .blocks
        .last()
        .context("no common finalized block during load")?
        .height;
    let header = clients[0].header(latest)?;
    for client in &clients {
        assert_eq!(client.header(latest)?, header);
    }
    let verifier_dir = tempfile::tempdir()?;
    let verifier = Store::open(verifier_dir.path(), &fixture.spec, Retention::Archive)?;
    let finality = clients[0].finality(latest)?;
    let mut certified = verify_finality(&verifier, &[], &finality.proof, false)?;
    while certified.context.height > latest {
        let parent = clients[0].header(certified.context.height - 1)?;
        assert_eq!(parent.id(verifier.network()), certified.context.parent);
        certified = parent;
    }
    assert_eq!(certified, header);
    let requested = bookings
        .iter()
        .filter_map(|booking| booking.command.as_deref())
        .collect::<BTreeSet<_>>();
    let mut outcomes = Vec::new();
    let mut applied = BTreeSet::new();
    for block in &observation.blocks {
        for (index, command_id) in block.transactions.iter().enumerate() {
            if requested.contains(command_id.as_str()) {
                let (receipt, evidence) =
                    execution(&clients[0], &fixture, block.height, index, command_id)?;
                let outcome = Outcome::decode(&receipt.protocol_result)?;
                if outcome == Outcome::Applied {
                    assert!(
                        applied.insert(command_id.clone()),
                        "a Keeper execution paid twice"
                    );
                }
                outcomes.push(json!({"id": command_id, "height": block.height, "seen_ms": block.seen_ms,
                    "index": index, "outcome": format!("{outcome:?}"), "fee": receipt.fee, "cycles": receipt.cycles,
                    "evidence": evidence}));
            }
        }
    }
    let representative = outcomes
        .iter()
        .find(|row| row["outcome"] == "Applied")
        .context("no applied Keeper command")?;
    for client in &clients[1..] {
        let (_, other) = execution(
            client,
            &fixture,
            representative["height"].as_u64().unwrap(),
            representative["index"].as_u64().unwrap() as usize,
            representative["id"].as_str().unwrap(),
        )?;
        assert_eq!(other, representative["evidence"]);
    }
    let mut headers = BTreeMap::new();
    for outcome in &outcomes {
        let height = outcome["height"].as_u64().unwrap();
        if let std::collections::btree_map::Entry::Vacant(entry) = headers.entry(height) {
            entry.insert(hex::encode(clients[0].header(height)?.encode()));
        }
    }
    fs::write(
        output.join("keeper-load.json"),
        serde_json::to_vec_pretty(&json!({
            "keeper_attempts": outcomes, "headers": headers, "sampled_all_replicas": representative,
            "common_height": latest, "common_header": hex::encode(header.encode()),
            "finality": hex::encode(finality.proof),
        }))?,
    )?;
    let booked = bookings
        .iter()
        .filter(|entry| entry.reply_ms.is_some() && entry.error.is_none())
        .count();
    println!(
        "Keeper load: {} replicas, {} of {jobs} bookings admitted, {} attempts, {} applied at common height {latest}",
        load.replicas,
        booked,
        outcomes.len(),
        applied.len()
    );
    if let Some(target) = hot_height {
        ensure!(latest >= target, "hot-height deadline was not observed");
        ensure!(
            booked > 2 && applied.len() == 2,
            "hot-height fixture did not overbook its two slots"
        );
        ensure!(
            final_status
                .iter()
                .all(|status| { status.mempool_transactions == 0 && status.mempool_bytes == 0 }),
            "expired Keeper bookings remain in a replica mempool"
        );
    }
    Ok(())
}

fn start_network(
    fixture: &mut Fixture,
    output: &Path,
    count: usize,
) -> Result<(Vec<Process>, Vec<Client>)> {
    start_network_configured(
        fixture,
        output,
        count,
        NetworkOptions {
            interval_ms: 250,
            timeout_ms: 2400,
            vm_workers: 2,
            history_window: None,
            storage_metrics: false,
        },
    )
}

pub(super) struct NetworkOptions {
    pub interval_ms: u64,
    pub timeout_ms: u64,
    pub vm_workers: usize,
    pub history_window: Option<u64>,
    pub storage_metrics: bool,
}

pub(super) fn start_network_configured(
    fixture: &mut Fixture,
    output: &Path,
    count: usize,
    options: NetworkOptions,
) -> Result<(Vec<Process>, Vec<Client>)> {
    let mut keys = (1..=count)
        .map(|seed| SigningKey::from_bytes(&[seed as u8; 32]))
        .collect::<Vec<_>>();
    keys.sort_by_key(|key| key.verifying_key().to_bytes());
    fixture.spec.development_validators = keys
        .iter()
        .map(|key| (key.verifying_key().to_bytes(), 1))
        .collect();
    let spec = output.join("genesis.json");
    write_spec(&spec, &fixture.spec)?;
    let listeners = (0..count)
        .map(|_| TcpListener::bind("127.0.0.1:0"))
        .collect::<std::io::Result<Vec<_>>>()?;
    let addresses = listeners
        .iter()
        .map(TcpListener::local_addr)
        .collect::<std::io::Result<Vec<_>>>()?;
    let peers = keys.iter().zip(&addresses).map(|(key, address)|
        json!({"key": hex::encode(key.verifying_key().to_bytes()), "address": address})).collect::<Vec<_>>();
    let configuration = output.join("replicas.json");
    fs::write(
        &configuration,
        serde_json::to_vec_pretty(&json!({
            "peers": peers, "view_timeout_ms": options.timeout_ms,
            "proposal_interval_ms": options.interval_ms, "vm_workers": options.vm_workers,
        }))?,
    )?;
    drop(listeners);
    let mut replicas = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let key_file = output.join(format!("validator-{index}.key"));
        fs::write(&key_file, key.to_bytes())?;
        fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600))?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_stem"));
        command
            .args(["run", "--initialize", "--home"])
            .arg(output.join(format!("node-{index}")))
            .arg("--configuration")
            .arg(&configuration)
            .arg("--genesis")
            .arg(&spec)
            .arg("--key-file")
            .arg(key_file);
        match options.history_window.filter(|_| index + 1 != count) {
            Some(window) => {
                command.arg("--history-window").arg(window.to_string());
            }
            None => {
                command.arg("--archive");
            }
        }
        if options.storage_metrics {
            command.arg("--storage-metrics");
        }
        replicas.push(Process::spawn(
            &mut command,
            &output.join(format!("node-{index}.log")),
        )?);
    }
    let clients = addresses
        .into_iter()
        .map(|address| Client::new(address, Duration::from_secs(2)))
        .collect::<Vec<_>>();
    wait_common(&clients, 3)?;
    Ok((replicas, clients))
}

fn booking(
    fixture: &Fixture,
    work: &DynamicLineage,
    client: &Client,
    index: usize,
    kind: &str,
    hot_height: Option<u64>,
) -> Result<(Transaction, String, u64)> {
    let height = client.status()?.height;
    let (trigger, target) = if kind == "height" {
        let target = hot_height.unwrap_or(height + 8);
        (Trigger::Height(target), target)
    } else {
        let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
        (Trigger::Time(now + 2400), now + 2400)
    };
    let retries = kind == "retry";
    let increment = u64::from(!retries);
    let draft = Transaction::new_lineage_command(
        work.deps.clone(),
        work.fee_points[index].clone(),
        height + 56,
        lineage::Command {
            targets: vec![lineage::Target {
                id: work.ids[index].clone(),
                min_version: 0,
                max_version: 0,
                min_capacity: FUND - FEE,
            }],
            executor: work.executor.clone(),
            executor_dep: work.executor_dep.clone(),
            executor_cycles: 100_000,
            validation_cycles: 5_000_000,
            fee: FEE,
            payload: Bytes::copy_from_slice(&increment.to_le_bytes()),
        },
    )?
    .with_keeper_schedule(Schedule {
        trigger,
        retry: retries.then_some(Retry {
            max_attempts: 2,
            delay_blocks: 2,
        }),
    })?;
    let command = work.sign_command(fixture, draft)?;
    let carrier = keeper::schedule_with_fee(
        fixture,
        index as u32,
        &command,
        if retries { 3 * FEE } else { 2 * FEE },
    )?;
    Ok((carrier, id(&command, fixture), target))
}

fn id(tx: &Transaction, fixture: &Fixture) -> String {
    hex::encode(tx.id(&fixture.spec.network).as_slice())
}

fn deliver(
    client: &Client,
    start: Instant,
    planned_ms: u64,
    kind: &'static str,
    build: impl FnOnce() -> Result<(Transaction, Option<String>, Option<u64>)>,
    network: &ckb_types::packed::Byte32,
) -> Delivery {
    let mut result = Delivery {
        kind,
        planned_ms,
        sent_ms: None,
        reply_ms: None,
        carrier: None,
        command: None,
        trigger: None,
        error: None,
    };
    if elapsed(start) > planned_ms + 1000 {
        result.error = Some("pacer dropped a request more than one second late".into());
    } else {
        match build() {
            Ok((tx, command, trigger)) => {
                result.carrier = Some(hex::encode(tx.id(network).as_slice()));
                result.command = command;
                result.trigger = trigger;
                result.sent_ms = Some(elapsed(start));
                result.error = client.submit(&tx).err().map(|error| format!("{error:#}"));
                result.reply_ms = Some(elapsed(start));
            }
            Err(error) => result.error = Some(format!("client construction: {error:#}")),
        }
    }
    result
}

fn pace(start: Instant, index: usize, rate: usize) -> u64 {
    let planned = ((index as u64) * 1000) / rate as u64;
    let at = start + Duration::from_millis(planned);
    thread::sleep(at.saturating_duration_since(Instant::now()));
    planned
}

fn elapsed(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

pub(super) fn statuses(clients: &[Client]) -> Result<Vec<Status>> {
    clients.iter().map(Client::status).collect()
}

pub(super) fn wait_common(clients: &[Client], height: u64) -> Result<()> {
    poll("common Keeper load height", Duration::from_secs(30), || {
        Ok(statuses(clients)?
            .iter()
            .all(|status| status.height >= height)
            .then_some(()))
    })
}

fn observe(
    clients: &[Client],
    mut height: u64,
    start: Instant,
    end: Instant,
) -> Result<Observation> {
    let mut records = Observation {
        blocks: Vec::new(),
        samples: Vec::new(),
        failures: Vec::new(),
    };
    while Instant::now() <= end {
        let sample = (|| -> Result<()> {
            let stats = statuses(clients)?;
            let common = stats.iter().map(|status| status.height).min().unwrap();
            let seen = elapsed(start);
            for next in height + 1..=common {
                records.blocks.push(Block {
                    height: next,
                    seen_ms: seen,
                    transactions: transactions(&clients[0], next)?,
                });
                height = next;
            }
            let agenda = clients[0].query("/stem/keeper-time", 0, &[], true)?;
            let response: Value = serde_json::from_slice(&agenda.value)?;
            let entries = if let Some(bytes) = response["record"].as_str() {
                decode_agenda(&hex::decode(bytes.trim_start_matches("0x"))?)?
            } else {
                vec![]
            };
            records.samples.push(Sample {
                at_ms: seen,
                height: common,
                agenda_entries: entries.len(),
                pending_retries: entries
                    .iter()
                    .filter(|entry| entry.attempts_left > 1)
                    .count(),
                pools: stats
                    .iter()
                    .map(|stats| stats.mempool_transactions)
                    .collect(),
            });
            Ok(())
        })();
        if let Err(error) = sample {
            records
                .failures
                .push(format!("{} ms: {error:#}", elapsed(start)));
        }
        thread::sleep(Duration::from_millis(100));
    }
    Ok(records)
}

pub(super) fn transactions(client: &Client, height: u64) -> Result<Vec<String>> {
    let result = client.query("/stem/transactions", height, &[], false)?;
    Ok(serde_json::from_slice(&result.value)?)
}

fn execution(
    client: &Client,
    fixture: &Fixture,
    height: u64,
    index: usize,
    expected_id: &str,
) -> Result<(Receipt, Value)> {
    let result = client.query(
        "/stem/execution",
        height,
        &u32::try_from(index)?.to_le_bytes(),
        true,
    )?;
    let evidence: Value = serde_json::from_slice(&result.value)?;
    let parse = |key: &str| -> Result<Vec<u8>> {
        Ok(hex::decode(
            evidence[key]
                .as_str()
                .context("missing execution field")?
                .trim_start_matches("0x"),
        )?)
    };
    let tx = Transaction::from_slice(&parse("transaction")?)?;
    assert_eq!(id(&tx, fixture), expected_id);
    let receipt = Receipt::decode(&parse("receipt")?)?;
    let (tx_path, receipt_path) = InclusionProof::decode_execution_pair(&parse("proof")?)?;
    assert!(tx_path.verify_execution(
        &fixture.spec.network,
        &client.header(height)?.execution(),
        &tx,
        &receipt_path,
        &receipt
    ));
    Ok((receipt, evidence))
}

pub(super) fn stats(pid: u32) -> Result<ProcessStats> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(')')
        .context("invalid process stat")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let cpu_ticks = fields[11].parse::<u64>()? + fields[12].parse::<u64>()?;
    let io = fs::read_to_string(format!("/proc/{pid}/io"))?;
    let write_bytes = io
        .lines()
        .find_map(|line| line.strip_prefix("write_bytes: "))
        .context("missing write bytes")?
        .parse()?;
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    let resident_kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .context("missing resident memory")?
        .split_whitespace()
        .next()
        .context("missing resident size")?
        .parse()?;
    Ok(ProcessStats {
        cpu_ticks,
        write_bytes,
        resident_kib,
    })
}
