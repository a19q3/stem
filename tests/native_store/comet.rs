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
use fern::{native_abci, socket};
use fern_ledger::block::Header;
use serde_json::{Value, json};

use super::*;

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
    fingerprint.update(b"Fern four-node fixture network\0");
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
    for rule in [1, 3] {
        let output =
            Command::new(std::env::var_os("FERN_RISCV_CC").unwrap_or_else(|| "clang".into()))
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

#[test]
#[ignore = "launched by the four-node network test"]
fn serve_application() -> Result<()> {
    let dir =
        PathBuf::from(std::env::var_os("FERN_TEST_NATIVE_NODE").context("fixture node directory")?);
    let genesis = read_json(&dir.join("config/genesis.json"))?;
    let store = Store::open(
        &dir.join("fern-db"),
        &spec(&genesis, dir.parent().context("network directory")?)?.spec,
        Retention::Archive,
    )?;
    socket::serve(
        &dir.join("fern.sock"),
        native_abci::App::new(store, 2)?,
        2,
        16,
    )
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
        let path = self.dir.join("fern.sock");
        if path.exists() {
            fs::remove_file(&path)?;
        }
        self.app = Some(Process::spawn(
            Command::new(std::env::current_exe()?)
                .args([
                    "--ignored",
                    "--exact",
                    "comet::serve_application",
                    "--nocapture",
                ])
                .env("FERN_TEST_NATIVE_NODE", &self.dir),
            &self.dir.join("fern.log"),
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
            &format!("abci_query?path=%22%2Ffern%2Fheader%22&height={height}"),
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
#[ignore = "runs four real CometBFT binaries; set FERN_TEST_COMET"]
fn four_node_native_finality_restart_and_app_hash() -> Result<()> {
    let comet =
        PathBuf::from(std::env::var_os("FERN_TEST_COMET").context("path to CometBFT binary")?)
            .canonicalize()?;
    let output = tempfile::Builder::new().prefix("fernn-").tempdir()?.keep();
    eprintln!("native test evidence in {}", output.display());
    let home = output.join("net");
    ensure!(
        Command::new(&comet)
            .args(["testnet", "--v", "4", "--o"])
            .arg(&home)
            .status()?
            .success(),
        "create CometBFT testnet"
    );
    let mut genesis = read_json(&home.join("node0/config/genesis.json"))?;
    genesis["genesis_time"] = json!(GENESIS_TIME);
    genesis["chain_id"] = json!("fern-native-abci-test");
    compile_types(&home)?;
    let fixture = spec(&genesis, &home)?;
    let store = Store::open(
        &output.join("genesis-db"),
        &fixture.spec,
        Retention::Archive,
    )?;
    genesis["app_state"] = json!({ "fern_genesis": hex::encode(store.genesis_id().as_slice()) });
    drop(store);

    let ports = (0..8)
        .map(|_| TcpListener::bind("127.0.0.1:0"))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut nodes = Vec::new();
    for index in 0..4 {
        let dir = home.join(format!("node{index}"));
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
            p2p: ports[index + 4].local_addr()?.port(),
            app: None,
            comet: None,
        });
    }
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
        "first height on four validators",
        Duration::from_secs(25),
        || {
            let heights = nodes
                .iter()
                .map(|node| node.signed_height(&client))
                .collect::<Result<Vec<_>>>()?;
            Ok(heights.iter().all(|&height| height >= 1).then_some(()))
        },
    )?;
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
    poll(
        "restarted validator catching up",
        Duration::from_secs(20),
        || Ok((nodes[3].signed_height(&client)? >= target).then_some(())),
    )?;
    assert_eq!(
        nodes[3].header(&client, target)?,
        nodes[0].header(&client, target)?
    );
    println!(
        "native payment H={height}, two Type Groups H={composed_height} cycles={total_cycles}, four replicas share H+1 AppHash, restarted replica agrees at H={target}"
    );
    Ok(())
}
