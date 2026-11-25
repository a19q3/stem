use std::{
    collections::BTreeMap,
    env,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::*;

pub(super) struct Benchmark {
    path: PathBuf,
    loadgen: PathBuf,
    rate: u32,
    count: u32,
    workload: String,
    limits: Limits,
}

impl Benchmark {
    pub(super) fn prepare(
        fixture: &mut Fixture,
        home: &Path,
        evidence: &Path,
    ) -> Result<Option<Self>> {
        let Some(path) = env::var_os("STEM_TEST_LOADGEN") else {
            return Ok(None);
        };
        let rate = env::var("STEM_TEST_LOAD_RATE")
            .unwrap_or_else(|_| "64".into())
            .parse::<u32>()?;
        let count = env::var("STEM_TEST_LOAD_COUNT")
            .unwrap_or_else(|_| "512".into())
            .parse::<u32>()?;
        let workload = env::var("STEM_TEST_LOAD_KIND").unwrap_or_else(|_| "payment".into());
        ensure!(
            (1..=20_000).contains(&count) && (1..=5_000).contains(&rate),
            "unsupported test load"
        );
        let transaction_limit = env::var("STEM_TEST_BLOCK_TX_LIMIT")
            .unwrap_or_else(|_| "256".into())
            .parse()?;
        let block_cycles = env::var("STEM_TEST_BLOCK_CYCLES")
            .map_or(Ok(fixture.spec.limits.block_cycles), |value| value.parse())?;
        ensure!(
            transaction_limit > 0 && block_cycles > 0,
            "benchmark block budget must be positive"
        );
        fixture.spec.limits.transaction_count = transaction_limit;
        fixture.spec.limits.block_cycles = block_cycles;
        fixture.spec.limits.cell_reads = 4096;
        fixture.spec.limits.cell_read_bytes = 64 * 1024 * 1024;
        fixture.spec.limits.cell_write_bytes = 1024 * 1024;
        let rules: &[u8] = match workload.as_str() {
            "payment" => &[],
            "single" => &[1],
            "double" => &[1, 3],
            "multi" => &[1, 2, 3],
            "settlement" => &[1, 2, 3, 6],
            _ => anyhow::bail!("unknown benchmark workload"),
        };
        let mut deps = fixture.deps.clone();
        let mut scripts = Vec::new();
        for rule in rules {
            let program = Bytes::from(fs::read(home.join(format!("rule-{rule}")))?);
            let point = OutPoint::new([91; 32].pack(), u32::from(*rule));
            let bond = Cell::minimum_capacity(&fixture.lock, &[], program.len())?;
            fixture.spec.development_cells.push((
                point.clone(),
                Cell::new(bond, fixture.lock.clone(), vec![], program.clone())?,
            ));
            deps.push(CellDep::new_builder().out_point(point).build());
            scripts.push(
                Script::new_builder()
                    .code_hash(blake2b_256(&program))
                    .hash_type(ScriptHashType::Data)
                    .build(),
            );
        }
        let mut cells = vec![scripts.clone()];
        if workload == "settlement" {
            let mut other = scripts.clone();
            other[0] = other[0]
                .clone()
                .as_builder()
                .args(Bytes::from_static(b"second asset"))
                .build();
            cells.push(other);
        }
        let mut transactions = Vec::new();
        for index in 0..count {
            let mut inputs = Vec::new();
            let mut outputs = Vec::new();
            for (asset, scripts) in cells.iter().enumerate() {
                let point = OutPoint::new([32 + asset as u8; 32].pack(), index);
                let data = if scripts.is_empty() {
                    Bytes::new()
                } else {
                    let mut bytes = 25u64.to_le_bytes().to_vec();
                    bytes.push(1);
                    Bytes::from(bytes)
                };
                fixture.spec.development_cells.push((
                    point.clone(),
                    Cell::new(FUND, fixture.lock.clone(), scripts.clone(), data.clone())?,
                ));
                inputs.push(CellInput::new(point, None));
                outputs.push(Cell::new(
                    FUND - FEE,
                    fixture.lock.clone(),
                    scripts.clone(),
                    data,
                )?);
            }
            let tx = fixture.seal(Transaction::new(
                deps.clone(),
                vec![],
                inputs,
                outputs,
                vec![],
            )?)?;
            transactions.push(format!("0x{}", hex::encode(tx.as_slice())));
        }
        let path_out = evidence.join("workload.json");
        fs::write(
            &path_out,
            serde_json::to_vec(&json!({"transactions": transactions}))?,
        )?;
        Ok(Some(Self {
            path: path_out,
            loadgen: PathBuf::from(path).canonicalize()?,
            rate,
            count,
            workload,
            limits: fixture.spec.limits,
        }))
    }

    pub(super) fn run(
        &self,
        evidence: &Path,
        nodes: &[Node],
        rpc: &reqwest::blocking::Client,
    ) -> Result<()> {
        let ports = nodes
            .iter()
            .map(|node| node.rpc.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let cpus = getconf("CLK_TCK")? as f64;
        let pages = getconf("PAGESIZE")?;
        let initial = sample(nodes, rpc, cpus, pages)?;
        let mut memory = initial.memory;
        let events = evidence.join("loadgen.json");
        let start = unix()?.as_nanos() + 500_000_000;
        let mut sender = Process(
            Command::new(&self.loadgen)
                .arg("-workload")
                .arg(&self.path)
                .arg("-ports")
                .arg(ports)
                .arg("-rate")
                .arg(self.rate.to_string())
                .args(["-batch", "8", "-workers", "32"])
                .arg("-start-ns")
                .arg(start.to_string())
                .stdout(fs::File::create(&events)?)
                .stderr(fs::File::create(evidence.join("loadgen.log"))?)
                .spawn()?,
        );
        let mut cursor = nodes
            .iter()
            .map(|node| node.signed_height(rpc))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .min()
            .context("benchmark replica list is empty")?
            .saturating_sub(1);
        let deadline = Instant::now()
            + Duration::from_secs(u64::from(self.count).div_ceil(u64::from(self.rate)) + 22);
        let mut visibility = BTreeMap::new();
        let mut blocks = Vec::new();
        let mut client = None;
        loop {
            if client.is_none()
                && let Some(status) = sender.0.try_wait()?
            {
                ensure!(status.success(), "benchmark client failed");
                let rows: Vec<Event> = serde_json::from_slice(&fs::read(&events)?)?;
                client = Some(rows);
            }
            let heights = nodes
                .iter()
                .map(|node| node.signed_height(rpc))
                .collect::<Result<Vec<_>>>()?;
            let limit = heights
                .into_iter()
                .min()
                .context("benchmark replica list is empty")?
                .saturating_sub(1);
            // H+1 on every replica commits application execution at H.
            while cursor < limit {
                cursor += 1;
                let reply = nodes[0].rpc(rpc, &format!("block?height={cursor}"))?;
                let mut load = BlockLoad {
                    height: cursor,
                    observed_s: ((unix()?.as_nanos() as i128 - start as i128) as f64) / 1e9,
                    transactions: 0,
                    successful: 0,
                    cycles: 0,
                };
                if let Some(txs) = reply["block"]["data"]["txs"]
                    .as_array()
                    .filter(|txs| !txs.is_empty())
                {
                    let receipts = nodes[0].rpc(rpc, &format!("block_results?height={cursor}"))?;
                    let results = receipts["txs_results"]
                        .as_array()
                        .context("missing execution receipts")?;
                    ensure!(
                        results.len() == txs.len(),
                        "block missing execution receipts"
                    );
                    load.transactions = txs.len();
                    load.observed_s = ((unix()?.as_nanos() as i128 - start as i128) as f64) / 1e9;
                    for (tx, result) in txs.iter().zip(results) {
                        let gas = match result["gas_used"].as_u64() {
                            Some(gas) => gas,
                            None => result["gas_used"]
                                .as_str()
                                .context("missing VM cycles")?
                                .parse()?,
                        };
                        load.cycles = load
                            .cycles
                            .checked_add(gas)
                            .context("block cycle overflow")?;
                        if result["code"].as_u64().unwrap_or(0) == 0 {
                            let bytes =
                                STANDARD.decode(tx.as_str().context("network transaction")?)?;
                            visibility.insert(hex::encode(Sha256::digest(bytes)), load.observed_s);
                            load.successful += 1;
                        }
                    }
                }
                ensure!(
                    load.cycles <= self.limits.block_cycles,
                    "block exceeds configured cycle limit"
                );
                blocks.push(load);
            }
            memory = memory.max(sample_processes(nodes, cpus, pages)?.memory);
            if client.as_ref().is_some_and(|rows| {
                rows.iter()
                    .filter(|row| row.offered_s.is_some() && row.code.is_none_or(|code| code == 0))
                    .all(|row| visibility.contains_key(&row.id.to_lowercase()))
            }) || Instant::now() >= deadline
            {
                break;
            }
            thread::sleep(Duration::from_millis(60));
        }
        let rows = client.context("benchmark client did not finish")?;
        let final_ = sample(nodes, rpc, cpus, pages)?;
        let mut confirmed = Vec::new();
        let mut finalized_at: f64 = 0.0;
        let mut duration: f64 = 0.0;
        let mut offered = 0;
        let mut admitted = 0;
        let mut drops = 0;
        let scheduled_s = f64::from(self.count) / f64::from(self.rate);
        let window_start = scheduled_s / 3.0;
        let in_window = |at: f64| (window_start..scheduled_s).contains(&at);
        let mut in_window_offered = 0;
        let mut in_window_admitted = 0;
        let mut admitted_at_end = 0;
        let mut pending_at_end = 0;
        for event in &rows {
            drops += usize::from(event.client_dropped);
            admitted += usize::from(event.code == Some(0));
            let seen = visibility.get(&event.id.to_lowercase()).copied();
            if let Some(ack) = event.ack_s.filter(|_| event.code == Some(0)) {
                in_window_admitted += usize::from(in_window(ack));
                if ack < scheduled_s {
                    admitted_at_end += 1;
                    pending_at_end += usize::from(seen.is_none_or(|at| at >= scheduled_s));
                }
            }
            if let Some(offered_at) = event.offered_s {
                offered += 1;
                in_window_offered += usize::from(in_window(offered_at));
                duration = duration.max(offered_at);
                if let Some(last) = seen {
                    confirmed.push((last - offered_at).max(0.0) * 1000.0);
                    finalized_at = finalized_at.max(last);
                }
            }
        }
        let offered_duration = duration.max(scheduled_s);
        confirmed.sort_unstable_by(f64::total_cmp);
        let percentile = |p: usize| {
            confirmed
                .get((p * confirmed.len()).div_ceil(100).saturating_sub(1))
                .copied()
        };
        let window = blocks
            .iter()
            .filter(|block| in_window(block.observed_s))
            .collect::<Vec<_>>();
        let window_duration = scheduled_s - window_start;
        let window_finalized: usize = window.iter().map(|block| block.successful).sum();
        let mut cycle_samples = window.iter().map(|block| block.cycles).collect::<Vec<_>>();
        cycle_samples.sort_unstable();
        let cycles_p95 =
            cycle_samples.get((cycle_samples.len() * 95).div_ceil(100).saturating_sub(1));
        let report = json!({
            "workload": self.workload, "requested_tps": self.rate, "transactions": self.count,
            "replicas": nodes.len(), "vm_workers_per_replica": vm_workers().parse::<u32>()?,
            "offered": offered, "admitted": admitted, "finalized_all_replicas": confirmed.len(), "client_drops": drops,
            "offered_tps": offered as f64 / offered_duration, "admitted_tps": admitted as f64 / offered_duration,
            "finalized_tps": confirmed.len() as f64 / finalized_at.max(offered_duration),
            "replica_visible_ms": {"p50": percentile(50), "p95": percentile(95), "p99": percentile(99)},
            "cpu_seconds": final_.cpu - initial.cpu, "peak_sampled_rss_bytes": memory,
            "process_write_bytes": final_.writes.saturating_sub(initial.writes),
            "p2p_sent_bytes": final_.sent.saturating_sub(initial.sent),
            "p2p_received_bytes": final_.recv.saturating_sub(initial.recv),
            "execution_height_attestation": "H+1 on every replica",
            "limits": self.limits,
            "steady": {
                "start_s": window_start, "end_s": scheduled_s,
                "offered_tps": in_window_offered as f64 / window_duration,
                "admitted_tps": in_window_admitted as f64 / window_duration,
                "finalized_tps": window_finalized as f64 / window_duration,
                "admitted_by_end": admitted_at_end, "pending_at_end": pending_at_end,
                "blocks": window.len(), "cycles_per_block_p95": cycles_p95,
                "max_block_cycles": cycle_samples.last(),
                "max_transactions_per_block": window.iter().map(|block| block.transactions).max(),
            },
        });
        fs::write(
            evidence.join("block-load.json"),
            serde_json::to_vec(&blocks)?,
        )?;
        fs::write(
            evidence.join("first-seen.json"),
            serde_json::to_vec(&visibility)?,
        )?;
        fs::write(
            evidence.join("native-benchmark.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        eprintln!("native benchmark {report}");
        ensure!(
            !confirmed.is_empty(),
            "no transactions finalized on all replicas"
        );
        Ok(())
    }
}

#[derive(Deserialize)]
struct Event {
    #[serde(default)]
    id: String,
    offered_s: Option<f64>,
    ack_s: Option<f64>,
    code: Option<u32>,
    #[serde(default)]
    client_dropped: bool,
}

#[derive(Serialize)]
struct BlockLoad {
    height: u64,
    observed_s: f64,
    transactions: usize,
    successful: usize,
    cycles: u64,
}

#[derive(Default)]
struct Resource {
    cpu: f64,
    memory: u64,
    writes: u64,
    sent: u64,
    recv: u64,
}

fn sample_processes(nodes: &[Node], ticks: f64, pages: u64) -> Result<Resource> {
    let mut result = Resource::default();
    for process in nodes
        .iter()
        .flat_map(|node| [node.app.as_ref(), node.comet.as_ref()])
        .flatten()
    {
        let pid = process.0.id();
        let state = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let (_, fields) = state.rsplit_once(") ").context("invalid process stat")?;
        let fields: Vec<_> = fields.split_whitespace().collect();
        result.cpu += (fields[11].parse::<u64>()? + fields[12].parse::<u64>()?) as f64 / ticks;
        result.memory += fields[21].parse::<u64>()? * pages;
        let io = fs::read_to_string(format!("/proc/{pid}/io"))?;
        result.writes += io
            .lines()
            .find_map(|line| line.strip_prefix("write_bytes: "))
            .context("process writes unavailable")?
            .parse::<u64>()?;
    }
    Ok(result)
}

fn sample(
    nodes: &[Node],
    rpc: &reqwest::blocking::Client,
    ticks: f64,
    pages: u64,
) -> Result<Resource> {
    fn counter(value: &Value) -> u64 {
        value
            .as_u64()
            .or_else(|| value.as_str()?.parse().ok())
            .unwrap_or_default()
    }
    let mut stats = sample_processes(nodes, ticks, pages)?;
    for node in nodes {
        let network = node.rpc(rpc, "net_info")?;
        for peer in network["peers"].as_array().into_iter().flatten() {
            let connection = &peer["connection_status"];
            stats.sent += counter(&connection["SendMonitor"]["Bytes"]);
            stats.recv += counter(&connection["RecvMonitor"]["Bytes"]);
        }
    }
    Ok(stats)
}

fn unix() -> Result<Duration> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?)
}

fn getconf(name: &str) -> Result<u64> {
    let output = Command::new("getconf").arg(name).output()?;
    ensure!(output.status.success(), "getconf {name} failed");
    Ok(std::str::from_utf8(&output.stdout)?.trim().parse()?)
}
