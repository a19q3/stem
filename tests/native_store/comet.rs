use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use stem_ledger::block::Header;

use super::*;

#[path = "comet/bench.rs"]
mod bench;

fn vm_workers() -> String {
    std::env::var("STEM_TEST_VM_WORKERS").unwrap_or_else(|_| "2".into())
}

fn history_window() -> String {
    std::env::var("STEM_TEST_HISTORY_WINDOW").unwrap_or_else(|_| "256".into())
}

const GENESIS_TIME: &str = "2025-01-01T00:00:00Z";
const GENESIS_MS: u64 = 1_735_689_600_000;

fn spec(genesis: &Value, home: &Path) -> Result<Fixture> {
    let mut fixture = Fixture::new()?;
    fixture.spec.chain_id = genesis["chain_id"].as_str().context("chain ID")?.into();
    fixture.spec.genesis_time_ms = GENESIS_MS;
    fixture.spec.development_validators = genesis["validators"]
        .as_array()
        .context("validators")?
        .iter()
        .map(|member| {
            let key =
                STANDARD.decode(member["pub_key"]["value"].as_str().context("public key")?)?;
            Ok((
                key.as_slice().try_into()?,
                member["power"].as_str().context("power")?.parse()?,
            ))
        })
        .collect::<Result<_>>()?;
    let mut fingerprint = ckb_hash::new_blake2b();
    fingerprint.update(b"Stem four-node fixture network\0");
    fingerprint.update(fixture.spec.chain_id.as_bytes());
    for (key, power) in &fixture.spec.development_validators {
        fingerprint.update(key);
        fingerprint.update(&power.to_le_bytes());
    }
    let mut network = [0; 32];
    fingerprint.finalize(&mut network);
    fixture.spec.network = network.pack();
    let asset = Asset::new(&fixture, home)?;
    fixture.spec.development_cells.extend(asset.code);
    fixture
        .spec
        .development_cells
        .push((asset.point, asset.cell));
    Ok(fixture)
}

struct Asset {
    code: Vec<(OutPoint, Cell)>,
    point: OutPoint,
    cell: Cell,
    asset: Script,
}

impl Asset {
    fn new(fixture: &Fixture, home: &Path) -> Result<Self> {
        let mut code = Vec::new();
        let mut types = Vec::new();
        for (position, rule) in [1, 3].into_iter().enumerate() {
            let bytes = Bytes::from(fs::read(home.join(format!("rule-{rule}")))?);
            types.push(
                Script::new_builder()
                    .hash_type(ScriptHashType::Data)
                    .code_hash(blake2b_256(&bytes))
                    .build(),
            );
            let data = bytes.clone();
            let bond = Cell::minimum_capacity(&fixture.lock, &[], data.len())?;
            code.push((
                OutPoint::new([19; 32].pack(), position as u32),
                Cell::new(bond, fixture.lock.clone(), vec![], data)?,
            ));
        }
        let asset = types[0].clone();
        let mut data = 25u64.to_le_bytes().to_vec();
        data.push(1);
        let cell = Cell::new(FUND, fixture.lock.clone(), types, data.into())?;
        Ok(Self {
            code,
            point: OutPoint::new([20; 32].pack(), 0),
            cell,
            asset,
        })
    }

    fn transfer(&self, fixture: &Fixture, drop_policy: bool) -> Result<Transaction> {
        let types = if drop_policy {
            vec![self.asset.clone()]
        } else {
            self.cell.type_scripts().collect()
        };
        let output = Cell::new(FUND - FEE, fixture.lock.clone(), types, self.cell.data())?;
        let mut deps = fixture.deps.clone();
        deps.extend(
            self.code
                .iter()
                .map(|(point, _)| CellDep::new_builder().out_point(point.clone()).build()),
        );
        fixture.seal(Transaction::new(
            deps,
            vec![],
            vec![CellInput::new(self.point.clone(), None)],
            vec![output],
            vec![],
        )?)
    }
}

fn compile_types(home: &Path) -> Result<()> {
    for rule in [1, 2, 3, 6] {
        let output =
            Command::new(std::env::var_os("STEM_RISCV_CC").unwrap_or_else(|| "clang".into()))
                .args([
                    "--target=riscv64-unknown-elf",
                    "-march=rv64im",
                    "-mabi=lp64",
                    "-ffreestanding",
                    "-fno-builtin",
                    "-nostdlib",
                    "-fuse-ld=lld",
                    "-O2",
                    "-Wl,--no-relax",
                    "-Wl,-e,_start",
                ])
                .arg(format!("-DRULE={rule}"))
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("contracts/examples/type_rules.c"))
                .arg("-o")
                .arg(home.join(format!("rule-{rule}")))
                .output()?;
        ensure!(
            output.status.success(),
            "RISC-V rule {rule}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub(super) fn write_spec(path: &Path, spec: &Spec) -> Result<()> {
    let committee = spec
        .development_validators
        .iter()
        .map(|(key, power)| json!({ "ed25519": format!("0x{}", hex::encode(key)), "power": power }))
        .collect::<Vec<_>>();
    let cells = spec
        .development_cells
        .iter()
        .map(|(point, cell)| {
            let point = ckb_jsonrpc_types::OutPoint::from(point.clone());
            json!({ "out_point": point, "cell": format!("0x{}", hex::encode(cell.as_slice())) })
        })
        .collect::<Vec<_>>();
    let spec = json!({
        "chain_id": spec.chain_id,
        "network_id": format!("0x{}", hex::encode(spec.network.as_slice())),
        "genesis_time_ms": spec.genesis_time_ms,
        "capacity": { "bond_per_byte": spec.capacity.bond_per_byte, "state_overhead": spec.capacity.state_overhead },
        "limits": spec.limits,
        "development_validators": committee,
        "development_cells": cells,
        "keeper": spec.keeper,
        "lineage_type": spec.lineage_type.as_ref().map(|hash| format!("0x{}", hex::encode(hash.as_slice()))),
    });
    fs::write(path, serde_json::to_vec(&spec)?)?;
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

struct Process(Child);

impl Process {
    fn spawn(command: &mut Command, log: &Path) -> Result<Self> {
        let out = fs::OpenOptions::new().create(true).append(true).open(log)?;
        Ok(Self(
            command
                .stdout(Stdio::from(out.try_clone()?))
                .stderr(Stdio::from(out))
                .spawn()?,
        ))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Node {
    dir: PathBuf,
    rpc: u16,
    p2p: u16,
    id: String,
    app: Option<Process>,
    comet: Option<Process>,
}

impl Node {
    fn start(&mut self, comet: &Path, peers: &str) -> Result<()> {
        let app_home = self.dir.join("stem");
        let path = app_home.join("abci.sock");
        if path.exists() {
            fs::remove_file(&path)?;
        }
        let spec = self
            .dir
            .parent()
            .context("network directory")?
            .join("stem-genesis.json");
        self.app = Some(Process::spawn(
            Command::new(env!("CARGO_BIN_EXE_stem"))
                .args(["run", "--home"])
                .arg(app_home)
                .arg("--genesis")
                .arg(spec)
                .args([
                    "--workers",
                    &vm_workers(),
                    "--abci-response-queue",
                    "16",
                    "--snapshot-interval",
                    "3",
                    "--history-window",
                    &history_window(),
                ]),
            &self.dir.join("stem.log"),
        )?);
        poll("native ABCI socket", Duration::from_secs(10), || {
            Ok(path.exists().then_some(()))
        })?;
        self.comet = Some(Process::spawn(
            Command::new(comet)
                .arg("start")
                .arg("--home")
                .arg(&self.dir)
                .arg("--proxy_app")
                .arg(format!("unix://{}", path.display()))
                .arg("--rpc.laddr")
                .arg(format!("tcp://127.0.0.1:{}", self.rpc))
                .arg("--p2p.laddr")
                .arg(format!("tcp://127.0.0.1:{}", self.p2p))
                .arg("--p2p.persistent_peers")
                .arg(peers)
                .args([
                    "--p2p.pex=false",
                    "--consensus.create_empty_blocks=false",
                    "--consensus.create_empty_blocks_interval=5s",
                ]),
            &self.dir.join("comet.log"),
        )?);
        Ok(())
    }

    fn stop(&mut self) {
        self.comet = None;
        self.app = None;
    }

    fn rpc(&self, client: &reqwest::blocking::Client, path: &str) -> Result<Value> {
        let url = format!("http://127.0.0.1:{}/{}", self.rpc, path);
        let response: Value = client.get(url).send()?.error_for_status()?.json()?;
        if let Some(error) = response.get("error") {
            anyhow::bail!("CometBFT RPC {path}: {error}");
        }
        Ok(response["result"].clone())
    }

    fn signed_height(&self, client: &reqwest::blocking::Client) -> Result<u64> {
        Ok(
            self.rpc(client, "status")?["sync_info"]["latest_block_height"]
                .as_str()
                .context("RPC height")?
                .parse()?,
        )
    }

    fn header(&self, client: &reqwest::blocking::Client, height: u64) -> Result<Header> {
        let value = self.rpc(
            client,
            &format!("abci_query?path=%22%2Fstem%2Fheader%22&height={height}"),
        )?;
        let response = &value["response"];
        ensure!(
            response["code"].as_u64().unwrap_or(0) == 0,
            "ABCI Header query: {response}"
        );
        let bytes = STANDARD.decode(response["value"].as_str().context("ABCI Header data")?)?;
        Header::decode(&bytes)
    }
}

fn poll<T>(
    label: &str,
    timeout: Duration,
    mut read: impl FnMut() -> Result<Option<T>>,
) -> Result<T> {
    let deadline = Instant::now() + timeout;
    loop {
        let last = match read() {
            Ok(Some(value)) => return Ok(value),
            Err(error) => format!("{error:#}"),
            Ok(None) => "not ready".into(),
        };
        ensure!(
            Instant::now() < deadline,
            "timed out waiting for {label}: {last}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "runs four real CometBFT binaries; set STEM_TEST_COMET"]
fn four_node_native_finality_restart_and_app_hash() -> Result<()> {
    let validators = std::env::var("STEM_TEST_VALIDATORS")
        .unwrap_or_else(|_| "4".into())
        .parse::<usize>()?;
    ensure!(
        (4..=32).contains(&validators)
            && (validators == 4 || std::env::var_os("STEM_TEST_LOADGEN").is_some()),
        "a replica count outside the four-node correctness fixture requires benchmark mode (maximum 32)"
    );
    let directories = validators + 1;
    let comet =
        PathBuf::from(std::env::var_os("STEM_TEST_COMET").context("path to CometBFT binary")?)
            .canonicalize()?;
    let mut temporary = tempfile::Builder::new();
    temporary.prefix("stem-");
    let output = if let Some(root) = std::env::var_os("STEM_TEST_EVIDENCE_ROOT") {
        fs::create_dir_all(&root)?;
        temporary.tempdir_in(PathBuf::from(root).canonicalize()?)?
    } else {
        temporary.tempdir()?
    }
    .keep();
    eprintln!("native test evidence in {}", output.display());
    let home = output.join("net");
    ensure!(
        Command::new(&comet)
            .args(["testnet", "--v", &validators.to_string(), "--n", "1", "--o"])
            .arg(&home)
            .status()?
            .success(),
        "create CometBFT testnet"
    );
    let mut genesis = read_json(&home.join("node0/config/genesis.json"))?;
    genesis["genesis_time"] = json!(GENESIS_TIME);
    genesis["chain_id"] = json!("stem-native-abci-test");
    compile_types(&home)?;
    let mut fixture = spec(&genesis, &home)?;
    let benchmark = bench::Benchmark::prepare(&mut fixture, &home, &output)?;
    let scenarios = if benchmark.is_none() {
        let swap = super::intent::Swap::fund(&mut fixture)?;
        let lineage = super::lineage::FixtureLineage::fund(&mut fixture)?;
        let dynamic =
            super::lineage::DynamicLineage::fund_using(&mut fixture, &lineage, 5, 2, None)?;
        super::keeper::fund_carriers(&mut fixture, 3)?;
        Some((swap, lineage, dynamic))
    } else {
        None
    };
    let spec_path = home.join("stem-genesis.json");
    write_spec(&spec_path, &fixture.spec)?;
    let identity = Command::new(env!("CARGO_BIN_EXE_stem"))
        .args(["genesis-hash", "--genesis"])
        .arg(&spec_path)
        .output()?;
    ensure!(
        identity.status.success(),
        "Stem genesis-hash: {}",
        String::from_utf8_lossy(&identity.stderr)
    );
    let id: String = serde_json::from_slice(&identity.stdout)?;
    genesis["app_state"] = json!({ "stem_genesis": id.trim_start_matches("0x") });

    let ports = (0..2 * directories)
        .map(|_| TcpListener::bind("127.0.0.1:0"))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut nodes = Vec::new();
    for index in 0..directories {
        let dir = home.join(format!("node{index}"));
        if let Some(benchmark) = &benchmark {
            benchmark.configure(&dir.join("config/config.toml"))?;
        }
        fs::write(
            dir.join("config/genesis.json"),
            serde_json::to_vec(&genesis)?,
        )?;
        let id = Command::new(&comet)
            .args(["show-node-id", "--home"])
            .arg(&dir)
            .output()?;
        ensure!(id.status.success(), "read CometBFT node ID");
        nodes.push(Node {
            dir,
            id: String::from_utf8(id.stdout)?.trim().to_owned(),
            rpc: ports[index].local_addr()?.port(),
            p2p: ports[index + directories].local_addr()?.port(),
            app: None,
            comet: None,
        });
    }
    let mut observer = nodes.pop().context("missing state sync observer")?;
    drop(ports);
    let peers = nodes
        .iter()
        .map(|node| format!("{}@127.0.0.1:{}", node.id, node.p2p))
        .collect::<Vec<_>>();
    let peer_list = |index| {
        peers
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, peer)| peer.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    for (index, node) in nodes.iter_mut().enumerate() {
        node.start(&comet, &peer_list(index))?;
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()?;
    poll(
        "first height on all validators",
        Duration::from_secs(55),
        || {
            let heights = nodes
                .iter()
                .map(|node| node.signed_height(&client))
                .collect::<Result<Vec<_>>>()?;
            Ok(heights.iter().all(|&height| height >= 1).then_some(()))
        },
    )?;
    if let Some(benchmark) = benchmark {
        return benchmark.run(&output, &nodes, &client);
    }
    let tx = fixture.signed(fixture.spend.clone(), None, FUND - FEE, None)?;
    let tx_result = nodes[0].rpc(
        &client,
        &format!("broadcast_tx_commit?tx=0x{}", hex::encode(tx.as_slice())),
    )?;
    ensure!(
        tx_result["check_tx"]["code"].as_u64().unwrap_or(0) == 0
            && tx_result["tx_result"]["code"].as_u64().unwrap_or(0) == 0,
        "payment failed: {tx_result}"
    );
    let height: u64 = tx_result["height"]
        .as_str()
        .context("payment height")?
        .parse()?;
    ensure!(height > 0, "transaction was not committed");
    let asset = Asset::new(&fixture, &home)?;
    let rejected = asset.transfer(&fixture, true)?;
    let invalid = nodes[0].rpc(
        &client,
        &format!(
            "broadcast_tx_sync?tx=0x{}",
            hex::encode(rejected.as_slice())
        ),
    )?;
    ensure!(
        invalid["code"].as_u64().unwrap_or(0) != 0,
        "transaction dropped its input Transfer Policy"
    );
    let transfer = asset.transfer(&fixture, false)?;
    let confirmation = nodes[1].rpc(
        &client,
        &format!(
            "broadcast_tx_commit?tx=0x{}",
            hex::encode(transfer.as_slice())
        ),
    )?;
    let composed_height: u64 = confirmation["height"]
        .as_str()
        .context("composed transfer height")?
        .parse()?;
    ensure!(
        composed_height > height && confirmation["tx_result"]["code"].as_u64().unwrap_or(0) == 0,
        "native composed transfer: {confirmation}"
    );
    let total_cycles: u64 = confirmation["tx_result"]["gas_used"]
        .as_str()
        .context("VM cycles")?
        .parse()?;
    ensure!(
        total_cycles > 0,
        "CometBFT receipt did not charge VM cycles"
    );
    let next = composed_height + 1;

    poll("H+1 on four validators", Duration::from_secs(15), || {
        let heights = nodes
            .iter()
            .map(|node| node.signed_height(&client))
            .collect::<Result<Vec<_>>>()?;
        Ok(heights.iter().all(|&height| height >= next).then_some(()))
    })?;
    let header = nodes[0].header(&client, composed_height)?;
    let app_hash = header.app_hash(&fixture.spec.network);
    for (index, node) in nodes.iter().enumerate() {
        assert_eq!(node.header(&client, composed_height)?, header);
        let signed = node.rpc(&client, &format!("commit?height={next}"))?;
        let value = signed["signed_header"]["header"]["app_hash"]
            .as_str()
            .context("H+1 AppHash")?;
        assert_eq!(hex::decode(value)?, app_hash.as_slice());
        fs::write(
            output.join(format!("commit-{index}.json")),
            serde_json::to_vec_pretty(&signed)?,
        )?;
    }
    fs::write(output.join("app-hash"), hex::encode(app_hash.as_slice()))?;
    fs::write(output.join("header.bin"), header.encode())?;
    fs::write(
        output.join("network-id"),
        hex::encode(fixture.spec.network.as_slice()),
    )?;
    nodes[3].stop();
    let target = next + 2;
    poll(
        "three validators advancing after stop",
        Duration::from_secs(15),
        || Ok((nodes[0].signed_height(&client)? >= target).then_some(())),
    )?;
    nodes[3].start(&comet, &peer_list(3))?;
    let replayed = poll(
        "restarted validator executing through the target height",
        Duration::from_secs(20),
        || nodes[3].header(&client, target).map(Some),
    )?;
    assert_eq!(replayed, nodes[0].header(&client, target)?);
    let checkpoint = target.div_ceil(3) * 3;
    poll(
        "state sync checkpoint successors",
        Duration::from_secs(15),
        || Ok((nodes[0].signed_height(&client)? >= checkpoint + 3).then_some(())),
    )?;
    let trusted = nodes[0].rpc(&client, &format!("block?height={checkpoint}"))?;
    let hash = trusted["block_id"]["hash"]
        .as_str()
        .context("trusted CometBFT block ID")?;
    configure_state_sync(
        &observer.dir.join("config/config.toml"),
        checkpoint,
        hash,
        [nodes[0].rpc, nodes[1].rpc],
    )?;
    observer.start(&comet, &peers.join(","))?;
    let restored = poll(
        "native remote state snapshot",
        Duration::from_secs(40),
        || {
            let log = fs::read_to_string(observer.dir.join("stem.log"))?;
            let installed = log
                .lines()
                .find_map(|line| line.split_once("event=snapshot_installed height="))
                .map(|(_, height)| height.parse::<u64>())
                .transpose()?;
            Ok(installed)
        },
    )?;
    let installed = poll("restored Header RPC", Duration::from_secs(10), || {
        observer.header(&client, restored).map(Some)
    })?;
    assert_eq!(installed, nodes[0].header(&client, restored)?);
    let window: u64 = history_window().parse()?;
    if checkpoint + 3 > window {
        poll(
            "CometBFT pruning the first block body",
            Duration::from_secs(8),
            || {
                let status = nodes[0].rpc(&client, "status")?;
                let earliest = status["sync_info"]["earliest_block_height"]
                    .as_str()
                    .context("missing earliest CometBFT block height")?
                    .parse::<u64>()?;
                Ok((earliest > 1).then_some(()))
            },
        )?;
        assert!(nodes[0].rpc(&client, "block?height=1").is_err());
    }
    let fee_query = nodes[0].rpc(
        &client,
        "abci_query?path=%22%2Fstem%2Fextinguished-fees%22&prove=true",
    )?;
    let response = &fee_query["response"];
    ensure!(
        response["code"].as_u64() == Some(0),
        "fee proof query failed: {response}"
    );
    let height_with_fees: u64 = response["height"]
        .as_str()
        .context("fee proof height")?
        .parse()?;
    let fees = STANDARD.decode(response["value"].as_str().context("fee proof response")?)?;
    let payload: serde_json::Value = serde_json::from_slice(&fees)?;
    let amount = serde_json::from_value::<Option<ckb_jsonrpc_types::Uint64>>(
        payload["extinguished_shannons"].clone(),
    )?
    .map(|value| value.value());
    let path: ckb_jsonrpc_types::JsonBytes = serde_json::from_value(payload["proof"].clone())?;
    let proof = stem_ledger::patricia::Proof::decode(path.as_bytes())?;
    assert_eq!(amount, Some(2 * FEE));
    let fee_header = nodes[0].header(&client, height_with_fees)?;
    assert!(Store::verify_extinguished_fees(
        &fixture.spec.network,
        &fee_header,
        amount,
        &proof
    ));
    poll(
        "signed successor for fee proof",
        Duration::from_secs(10),
        || Ok((nodes[0].signed_height(&client)? > height_with_fees).then_some(())),
    )?;
    let successor = nodes[0].rpc(&client, &format!("commit?height={}", height_with_fees + 1))?;
    let root = successor["signed_header"]["header"]["app_hash"]
        .as_str()
        .context("fee proof AppHash")?;
    assert_eq!(
        hex::decode(root)?,
        fee_header.app_hash(&fixture.spec.network).as_slice()
    );
    fs::write(output.join("extinguished-fees.json"), fees)?;
    println!(
        "native payment H={height}, two Type Groups H={composed_height} cycles={total_cycles}, four replicas share H+1 AppHash, restarted replica agrees at H={target}, remote snapshot H={restored}"
    );
    let (swap, lineage, dynamic) = scenarios.context("missing network scenarios")?;
    let intent = swap.sign(&fixture, &spec_path, 500)?;
    let confirmed = nodes[2].rpc(
        &client,
        &format!(
            "broadcast_tx_commit?tx=0x{}",
            hex::encode(intent.as_slice())
        ),
    )?;
    ensure!(
        confirmed["check_tx"]["code"].as_u64() == Some(0)
            && confirmed["tx_result"]["code"].as_u64() == Some(0),
        "sponsored atomic asset exchange failed: {confirmed}"
    );
    let trade_at: u64 = confirmed["height"]
        .as_str()
        .context("trade height")?
        .parse()?;
    ensure!(trade_at > height_with_fees, "trade height did not advance");
    poll(
        "signed sponsor and trade on all replicas",
        Duration::from_secs(15),
        || {
            let heights = nodes
                .iter()
                .chain([&observer])
                .map(|node| node.signed_height(&client))
                .collect::<Result<Vec<_>>>()?;
            Ok(heights
                .into_iter()
                .all(|height| height > trade_at)
                .then_some(()))
        },
    )?;
    let trade_header = nodes[0].header(&client, trade_at)?;
    for node in nodes.iter().chain([&observer]) {
        assert_eq!(node.header(&client, trade_at)?, trade_header);
        let signed = node.rpc(&client, &format!("commit?height={}", trade_at + 1))?;
        let app_hash = signed["signed_header"]["header"]["app_hash"]
            .as_str()
            .context("trade AppHash")?;
        assert_eq!(
            hex::decode(app_hash)?,
            trade_header.app_hash(&fixture.spec.network).as_slice()
        );
    }
    fs::write(
        output.join("sponsored-trade.json"),
        serde_json::to_vec_pretty(&confirmed)?,
    )?;
    println!(
        "sponsored two-asset trade H={trade_at} cycles={}",
        confirmed["tx_result"]["gas_used"]
    );
    let (lineage_id, register, update) = lineage.example(&fixture)?;
    let mut lifecycle = Vec::new();
    for tx in [&register, &update] {
        let committed = nodes[0].rpc(
            &client,
            &format!("broadcast_tx_commit?tx=0x{}", hex::encode(tx.as_slice())),
        )?;
        ensure!(
            committed["check_tx"]["code"].as_u64() == Some(0)
                && committed["tx_result"]["code"].as_u64() == Some(0),
            "Lineage transition failed: {committed}"
        );
        lifecycle.push(committed);
    }
    let successor_at: u64 = lifecycle[1]["height"]
        .as_str()
        .context("Lineage height")?
        .parse()?;
    poll(
        "Lineage successor on all replicas",
        Duration::from_secs(15),
        || {
            let heights = nodes
                .iter()
                .chain([&observer])
                .map(|node| node.signed_height(&client))
                .collect::<Result<Vec<_>>>()?;
            Ok(heights
                .into_iter()
                .all(|height| height > successor_at)
                .then_some(()))
        },
    )?;
    let proofs = verify_lineage_heads(
        &nodes,
        &observer,
        &client,
        &fixture,
        &lineage_id,
        1,
        &OutPoint::new(update.id(&fixture.spec.network), 0),
        None,
    )?;
    fs::write(
        output.join("lineage-lifecycle.json"),
        serde_json::to_vec_pretty(&json!({
            "lineage_id": hex::encode(lineage_id.as_slice()),
            "transitions": lifecycle,
            "proofs": proofs,
        }))?,
    )?;
    use stem_ledger::{block::Receipt, lineage::Outcome};
    let first = dynamic.signed(&fixture, 0, &[0], (0, 2), 2)?;
    let second = dynamic.signed(&fixture, 1, &[0, 1], (0, 2), 3)?;
    let rejected = dynamic.signed(&fixture, 2, &[0, 1], (0, 2), 0)?;
    let mut commands = Vec::new();
    let mut applied_height = 0;
    for (tx, expected) in [
        (dynamic.register(&fixture, 0)?, None),
        (dynamic.register(&fixture, 1)?, None),
        (first, Some(Outcome::Applied)),
        (second.clone(), Some(Outcome::Applied)),
        (rejected, Some(Outcome::Rejected)),
    ] {
        let confirmed = nodes[0].rpc(
            &client,
            &format!("broadcast_tx_commit?tx=0x{}", hex::encode(tx.as_slice())),
        )?;
        ensure!(
            confirmed["check_tx"]["code"].as_u64() == Some(0)
                && confirmed["tx_result"]["code"].as_u64() == Some(0),
            "Lineage command failed: {confirmed}"
        );
        if let Some(expected) = expected {
            let data = STANDARD.decode(
                confirmed["tx_result"]["data"]
                    .as_str()
                    .context("missing command receipt")?,
            )?;
            let receipt = Receipt::decode(&data)?;
            assert_eq!(Outcome::decode(&receipt.protocol_result)?, expected);
            assert_eq!(receipt.fee, FEE);
            applied_height = confirmed["height"]
                .as_str()
                .context("command height")?
                .parse()?;
            commands.push(json!({ "result": confirmed, "cycles": receipt.cycles, "applied": expected == Outcome::Applied }));
        }
    }
    poll(
        "signed dynamic Lineage block on all replicas",
        Duration::from_secs(15),
        || {
            let heights = nodes
                .iter()
                .chain([&observer])
                .map(|node| node.signed_height(&client))
                .collect::<Result<Vec<_>>>()?;
            Ok(heights
                .into_iter()
                .all(|height| height > applied_height)
                .then_some(()))
        },
    )?;
    let second_id = second.id(&fixture.spec.network);
    let target_index = |id| {
        1 + second
            .lineage_command()
            .unwrap()
            .targets
            .iter()
            .position(|target| &target.id == id)
            .unwrap() as u32
    };
    let mut dynamic_proofs = Vec::new();
    for (index, version, value) in [(0, 2, 245u128), (1, 1, 243)] {
        let id = &dynamic.ids[index];
        dynamic_proofs.push(verify_lineage_heads(
            &nodes,
            &observer,
            &client,
            &fixture,
            id,
            version,
            &OutPoint::new(second_id.clone(), target_index(id)),
            Some(&value.to_le_bytes()),
        )?);
    }
    fs::write(
        output.join("lineage-commands.json"),
        serde_json::to_vec_pretty(&json!({"commands": commands, "proofs": dynamic_proofs}))?,
    )?;
    println!(
        "pre-signed Lineage commands and fee-only rollback verified on four validators and a snapshot observer"
    );
    verify_keeper_execution(&nodes, &observer, &client, &fixture, &dynamic, &output)?;
    Ok(())
}

fn verify_keeper_execution(
    nodes: &[Node],
    observer: &Node,
    client: &reqwest::blocking::Client,
    fixture: &Fixture,
    dynamic: &super::lineage::DynamicLineage,
    output: &Path,
) -> Result<()> {
    use stem_ledger::{
        block::Receipt, keeper::decode_queue, lineage::Outcome, proof::InclusionProof,
    };

    let due = nodes[0].signed_height(client)? + 9;
    let command = dynamic.scheduled(fixture, 3, (2, 2), 4, due)?;
    let rejected = dynamic.scheduled(fixture, 4, (3, 3), 0, due)?;
    for (index, tx) in [&rejected, &command].into_iter().enumerate() {
        let carrier = super::keeper::schedule(fixture, index as u32, tx)?;
        let result = nodes[0].rpc(
            client,
            &format!(
                "broadcast_tx_commit?tx=0x{}",
                hex::encode(carrier.as_slice())
            ),
        )?;
        ensure!(
            result["check_tx"]["code"].as_u64() == Some(0)
                && result["tx_result"]["code"].as_u64() == Some(0),
            "Keeper reservation failed: {result}"
        );
    }
    let query = nodes[0].rpc(
        client,
        &format!(
            "abci_query?path=%22%2Fstem%2Fkeeper%22&prove=true&data=0x{}",
            hex::encode(due.to_le_bytes())
        ),
    )?;
    let response = &query["response"];
    assert_eq!(response["code"].as_u64(), Some(0));
    let at: u64 = response["height"]
        .as_str()
        .context("Keeper proof height")?
        .parse()?;
    let data = STANDARD.decode(response["value"].as_str().context("Keeper proof value")?)?;
    let payload: Value = serde_json::from_slice(&data)?;
    let record: ckb_jsonrpc_types::JsonBytes = serde_json::from_value(payload["record"].clone())?;
    let proof: ckb_jsonrpc_types::JsonBytes = serde_json::from_value(payload["proof"].clone())?;
    let path = stem_ledger::patricia::Proof::decode(proof.as_bytes())?;
    let network = &fixture.spec.network;
    let pending_header = nodes[0].header(client, at)?;
    assert!(Store::verify_keeper_queue(
        network,
        &pending_header,
        due,
        Some(record.as_bytes()),
        &path
    ));
    let pending = decode_queue(record.as_bytes(), due)?;
    assert_eq!(
        pending.iter().map(|tx| tx.id(network)).collect::<Vec<_>>(),
        [command.id(network), rejected.id(network)]
    );

    poll(
        "signed Keeper wake on every replica",
        Duration::from_secs(25),
        || {
            let heights = nodes
                .iter()
                .chain([observer])
                .map(|node| node.signed_height(client))
                .collect::<Result<Vec<_>>>()?;
            Ok(heights.into_iter().all(|height| height > due).then_some(()))
        },
    )?;
    let pending_successor = nodes[0].rpc(client, &format!("commit?height={}", at + 1))?;
    let pending_hash = pending_header.app_hash(network);
    assert_eq!(
        hex::decode(
            pending_successor["signed_header"]["header"]["app_hash"]
                .as_str()
                .context("pending Keeper proof AppHash")?
        )?,
        pending_hash.as_slice()
    );
    let comet = nodes[0].rpc(client, &format!("block?height={due}"))?;
    assert!(
        comet["block"]["data"]["txs"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );

    let header = nodes[0].header(client, due)?;
    let mut evidence = Vec::new();
    for node in nodes.iter().chain([observer]) {
        assert_eq!(node.header(client, due)?, header);
        let successor = node.rpc(client, &format!("commit?height={}", due + 1))?;
        assert_eq!(
            hex::decode(
                successor["signed_header"]["header"]["app_hash"]
                    .as_str()
                    .context("Keeper AppHash")?
            )?,
            header.app_hash(network).as_slice()
        );
        let mut outcomes = Vec::new();
        for (i, (tx, outcome)) in [(&command, Outcome::Applied), (&rejected, Outcome::Rejected)]
            .into_iter()
            .enumerate()
        {
            let query = node.rpc(
                client,
                &format!(
                    "abci_query?path=%22%2Fstem%2Fexecution%22&prove=true&height={due}&data=0x{}",
                    hex::encode((i as u32).to_le_bytes())
                ),
            )?;
            let response = &query["response"];
            ensure!(
                response["code"].as_u64() == Some(0),
                "Keeper execution query failed: {query}"
            );
            let value = STANDARD.decode(
                response["value"]
                    .as_str()
                    .context("Keeper execution value")?,
            )?;
            let value: Value = serde_json::from_slice(&value)?;
            assert_eq!(
                hex::decode(
                    value["transaction"]
                        .as_str()
                        .context("Keeper transaction")?
                )?,
                tx.as_slice()
            );
            let receipt = Receipt::decode(&hex::decode(
                value["receipt"].as_str().context("Keeper receipt")?,
            )?)?;
            assert_eq!(Outcome::decode(&receipt.protocol_result)?, outcome);
            assert_eq!(receipt.fee, FEE);
            let proof: ckb_jsonrpc_types::JsonBytes =
                serde_json::from_value(value["proof"].clone())?;
            let (tx_path, receipt_path) = InclusionProof::decode_execution_pair(proof.as_bytes())?;
            assert!(tx_path.verify_execution(
                network,
                &header.execution(),
                tx,
                &receipt_path,
                &receipt
            ));
            outcomes.push(value);
        }
        evidence.push(json!({"executions": outcomes, "signed_successor": successor}));
    }
    let heads = verify_lineage_heads(
        nodes,
        observer,
        client,
        fixture,
        &dynamic.ids[0],
        3,
        &OutPoint::new(command.id(network), 1),
        Some(&249u128.to_le_bytes()),
    )?;
    fs::write(
        output.join("keeper-wake.json"),
        serde_json::to_vec_pretty(&json!({
            "due": due, "pending": payload, "pending_height": at,
            "pending_header": hex::encode(pending_header.encode()),
            "pending_app_hash": hex::encode(pending_hash.as_slice()),
            "pending_successor": pending_successor,
            "header": hex::encode(header.encode()),
            "app_hash": hex::encode(header.app_hash(network).as_slice()),
            "wakes": evidence, "heads": heads,
        }))?,
    )?;
    println!(
        "Keeper executed two sealed commands at H={due} on four validators and the restored observer"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_lineage_heads(
    nodes: &[Node],
    observer: &Node,
    client: &reqwest::blocking::Client,
    fixture: &Fixture,
    id: &ckb_types::packed::Byte32,
    expected_version: u64,
    expected_head: &OutPoint,
    expected_data: Option<&[u8]>,
) -> Result<Vec<Value>> {
    let mut proofs = Vec::new();
    for node in nodes.iter().chain([observer]) {
        let payload = node.rpc(
            client,
            &format!(
                "abci_query?path=%22%2Fstem%2Flineage%22&prove=true&data=0x{}",
                hex::encode(id.as_slice())
            ),
        )?;
        let response = &payload["response"];
        ensure!(
            response["code"].as_u64() == Some(0),
            "Lineage query failed: {payload}"
        );
        let height = response["height"]
            .as_str()
            .context("Lineage proof height")?
            .parse()?;
        let payload = STANDARD.decode(response["value"].as_str().context("Lineage proof")?)?;
        let payload: Value = serde_json::from_slice(&payload)?;
        let record: ckb_jsonrpc_types::JsonBytes =
            serde_json::from_value(payload["record"].clone())?;
        let proof: ckb_jsonrpc_types::JsonBytes = serde_json::from_value(payload["proof"].clone())?;
        let head = stem_ledger::lineage::Head::decode(record.as_bytes())?;
        let cell: ckb_jsonrpc_types::JsonBytes = serde_json::from_value(payload["cell"].clone())?;
        let cell_proof: ckb_jsonrpc_types::JsonBytes =
            serde_json::from_value(payload["cell_proof"].clone())?;
        let (out_point, cell) = record::decode_cell(cell.as_bytes())?;
        assert_eq!(head.version, expected_version);
        assert_eq!(out_point, head.out_point);
        assert_eq!(&head.out_point, expected_head);
        if let Some(expected) = expected_data {
            assert_eq!(cell.cell.data().as_ref(), expected);
        }
        let header = node.header(client, height)?;
        let proof = stem::native_store::LineageProof {
            head: Some((
                head,
                cell,
                stem_ledger::patricia::Proof::decode(cell_proof.as_bytes())?,
            )),
            system_path: stem_ledger::patricia::Proof::decode(proof.as_bytes())?,
        };
        assert!(
            proof.verify(
                &fixture.spec.network,
                &header,
                id,
                fixture
                    .spec
                    .lineage_type
                    .as_ref()
                    .context("fixture Lineage code hash")?,
            )
        );
        poll(
            "signed successor for Lineage proof",
            Duration::from_secs(10),
            || Ok((node.signed_height(client)? > height).then_some(())),
        )?;
        let signed = node.rpc(client, &format!("commit?height={}", height + 1))?;
        let app_hash = signed["signed_header"]["header"]["app_hash"]
            .as_str()
            .context("Lineage AppHash")?;
        assert_eq!(
            hex::decode(app_hash)?,
            header.app_hash(&fixture.spec.network).as_slice()
        );
        proofs.push(json!({
            "height": height,
            "app_hash": hex::encode(header.app_hash(&fixture.spec.network).as_slice()),
            "payload": payload,
            "signed_successor": signed
        }));
    }
    Ok(proofs)
}

fn configure_state_sync(path: &Path, height: u64, hash: &str, ports: [u16; 2]) -> Result<()> {
    let original = fs::read_to_string(path)?;
    let peers = format!(
        "http://127.0.0.1:{},http://127.0.0.1:{}",
        ports[0], ports[1]
    );
    let replacements = [
        ("enable", "true".to_string()),
        ("rpc_servers", format!("{peers:?}")),
        ("trust_height", height.to_string()),
        ("trust_hash", format!("{hash:?}")),
        ("discovery_time", r#""5s""#.into()),
    ];
    let mut config = String::new();
    let mut state_sync = false;
    let mut changed = 0;
    for line in original.lines() {
        if line.starts_with('[') {
            state_sync = line == "[statesync]";
        }
        if state_sync
            && let Some((key, _)) = line.split_once(" = ")
            && let Some((_, value)) = replacements.iter().find(|(name, _)| *name == key)
        {
            config.push_str(&format!("{key} = {value}\n"));
            changed += 1;
        } else {
            config.push_str(line);
            config.push('\n');
        }
    }
    ensure!(
        changed == replacements.len(),
        "CometBFT state sync config keys missing"
    );
    fs::write(path, config)?;
    Ok(())
}
