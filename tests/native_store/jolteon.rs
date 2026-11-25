use std::{
    fs, net::TcpListener, os::unix::fs::PermissionsExt, path::PathBuf, process::Command,
    time::Duration,
};

use ckb_db::internal::{Direction, IteratorMode, Options, ReadOnlyDB, prelude::*};
use ckb_types::prelude::*;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use stem::jolteon::{Client, verify_finality};
use stem_ledger::{
    Transaction,
    block::{Header, Receipt},
    lineage::{Head, Outcome},
    patricia::Proof,
    proof::InclusionProof,
};

use super::{
    Context, FEE, Fixture, OutPoint, Result, Retention, Store,
    comet::{Asset, Process, compile_types, poll, write_spec},
    intent::Swap,
    keeper,
    lineage::{DynamicLineage, FixtureLineage},
};

const WAIT: Duration = Duration::from_secs(20);

#[test]
#[ignore = "inspect a stopped four-replica run produced by --check-pruning"]
fn ordinary_jolteon_databases_drop_expired_proposals_and_certificates() -> Result<()> {
    let root =
        PathBuf::from(std::env::var_os("STEM_TEST_PRUNED_NET").context("path to the fixture run")?);
    let report: Value = serde_json::from_slice(&fs::read(root.join("jolteon-report.json"))?)?;
    let archive = report["archival_replica"]
        .as_u64()
        .context("archive index")?;
    let window = report["ordinary_history_window"]
        .as_u64()
        .context("history window")?;
    let mut evidence = Vec::new();
    for replica in 0..report["validators"].as_u64().context("validator count")? {
        let db = ReadOnlyDB::open(&Options::default(), root.join(format!("node{replica}/db")))?;
        let finality = db
            .iterator(IteratorMode::From(b"jfinality", Direction::Forward))
            .take_while(|(key, _)| key.starts_with(b"jfinality"))
            .map(|(key, _)| u64::from_be_bytes(key[9..].try_into().unwrap()))
            .collect::<Vec<_>>();
        let proposal_count = db
            .iterator(IteratorMode::From(b"jbody", Direction::Forward))
            .take_while(|(key, _)| key.starts_with(b"jbody"))
            .count() as u64;
        let first = *finality
            .first()
            .context("missing first local certificate")?;
        let last = *finality.last().context("missing last local certificate")?;
        if replica == archive {
            assert_eq!(first, 1);
            assert!(proposal_count >= last);
        } else {
            assert_eq!(last - first + 1, window);
            assert_eq!(finality.len() as u64, window);
            assert!((window..=window + 8).contains(&proposal_count));
        }
        evidence.push(json!({"replica": replica, "first": first, "last": last,
            "certificates": finality.len(), "proposal_bodies": proposal_count}));
    }
    fs::write(
        root.join("storage-retention.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    Ok(())
}

#[test]
#[ignore = "runs four native Stem processes and real RISC-V contracts"]
fn native_default_preserves_polytype_intent_lineage_and_keeper() -> Result<()> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("stem-native-run-");
    let output = match std::env::var_os("STEM_TEST_EVIDENCE_ROOT") {
        Some(path) => {
            fs::create_dir_all(&path)?;
            builder.tempdir_in(PathBuf::from(path))?
        }
        None => builder.tempdir()?,
    }
    .keep();
    eprintln!("native runtime evidence in {}", output.display());
    let mut fixture = Fixture::new()?;
    fixture.spec.chain_id = "stem-native-runtime-fixture".into();
    fixture.spec.network = [103; 32].pack();
    let mut keys = [11, 12, 13, 14].map(|seed| SigningKey::from_bytes(&[seed; 32]));
    keys.sort_by_key(|key| key.verifying_key().to_bytes());
    fixture.spec.development_validators = keys
        .iter()
        .map(|key| (key.verifying_key().to_bytes(), 1))
        .collect();
    compile_types(&output)?;
    let asset = Asset::new(&fixture, &output)?;
    fixture.spec.development_cells.extend(asset.code.clone());
    fixture
        .spec
        .development_cells
        .push((asset.point.clone(), asset.cell.clone()));
    let swap = Swap::fund(&mut fixture)?;
    let lineage = FixtureLineage::fund(&mut fixture)?;
    let dynamic = DynamicLineage::fund_using(&mut fixture, &lineage, 5, 2, None)?;
    keeper::fund_carriers(&mut fixture, 2)?;
    let spec = output.join("genesis.json");
    write_spec(&spec, &fixture.spec)?;
    let verifier_dir = tempfile::tempdir()?;
    let verifier = Store::open(verifier_dir.path(), &fixture.spec, Retention::Archive)?;
    let network = &fixture.spec.network;

    let listeners = (0..keys.len())
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
        json!({"peers": peers, "view_timeout_ms": 1400, "proposal_interval_ms": 180}).to_string(),
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
        replicas.push(Process::spawn(
            &mut command,
            &output.join(format!("node-{index}.log")),
        )?);
    }
    let clients = addresses
        .into_iter()
        .map(|address| Client::new(address, Duration::from_secs(2)))
        .collect::<Vec<_>>();
    wait_height(&clients, 3)?;
    assert!(clients.iter().all(|client| {
        client
            .status()
            .is_ok_and(|status| status.history_window == Some(256))
    }));

    let mut executions = Vec::new();
    let invalid = clients[0]
        .submit(&asset.transfer(&fixture, true)?)
        .unwrap_err()
        .to_string();
    let mut submit = |label, tx: &Transaction, expected: Option<Outcome>| -> Result<()> {
        let first = clients[0].status()?.height + 1;
        clients[0].submit(tx)?;
        let id = hex::encode(tx.id(network).as_slice());
        let (height, index) = poll(label, WAIT, || {
            for height in first..=clients[0].status()?.height {
                let result = clients[0].query("/stem/transactions", height, &[], false)?;
                let ids: Vec<String> = serde_json::from_slice(&result.value)?;
                if let Some(index) = ids.iter().position(|item| item == &id) {
                    return Ok(Some((height, u32::try_from(index)?)));
                }
            }
            Ok(None)
        })?;
        let evidence = prove_execution(&clients, &verifier, height, index, tx, expected)?;
        executions.push(json!({"label": label, "height": height, "proof": evidence}));
        Ok(())
    };
    submit("two Type Scripts", &asset.transfer(&fixture, false)?, None)?;
    submit(
        "independently sponsored OTX swap",
        &swap.sign(&fixture, &spec, 500)?,
        None,
    )?;
    submit(
        "first Lineage registration",
        &dynamic.register(&fixture, 0)?,
        None,
    )?;
    submit(
        "second Lineage registration",
        &dynamic.register(&fixture, 1)?,
        None,
    )?;
    submit(
        "one-head Lineage",
        &dynamic.signed(&fixture, 0, &[0], (0, 2), 2)?,
        Some(Outcome::Applied),
    )?;
    let joint = dynamic.signed(&fixture, 1, &[0, 1], (0, 2), 3)?;
    submit("two-head atomic Lineage", &joint, Some(Outcome::Applied))?;
    submit(
        "fee-only Lineage rollback",
        &dynamic.signed(&fixture, 2, &[0, 1], (0, 2), 0)?,
        Some(Outcome::Rejected),
    )?;
    let due = clients[0].status()?.height + 15;
    let scheduled = dynamic.scheduled(&fixture, 3, (2, 2), 4, due)?;
    submit(
        "Keeper reservation",
        &keeper::schedule(&fixture, 0, &scheduled)?,
        None,
    )?;
    wait_height(&clients, due)?;
    let wake = prove_execution(
        &clients,
        &verifier,
        due,
        0,
        &scheduled,
        Some(Outcome::Applied),
    )?;
    let heads = clients
        .iter()
        .map(|client| -> Result<Value> {
            let query = client.query("/stem/lineage", 0, dynamic.ids[0].as_slice(), true)?;
            let payload: Value = serde_json::from_slice(&query.value)?;
            let head = Head::decode(&bytes(&payload, "record")?)?;
            assert_eq!(head.version, 3);
            assert_eq!(head.out_point, OutPoint::new(scheduled.id(network), 1));
            assert!(Store::verify_lineage(
                network,
                &client.header(query.height)?,
                &dynamic.ids[0],
                Some(&head),
                &Proof::decode(&bytes(&payload, "proof")?)?
            ));
            Ok(payload)
        })
        .collect::<Result<Vec<_>>>()?;
    fs::write(
        output.join("native-composition.json"),
        serde_json::to_vec_pretty(&json!({
            "rejected_policy_removal": invalid, "executions": executions,
            "keeper_wake_height": due, "keeper_wake": wake, "heads": heads,
        }))?,
    )?;
    println!(
        "four default native replicas verified Polytype, sponsored OTX, atomic Lineage and Keeper at height {due}"
    );
    Ok(())
}

fn wait_height(clients: &[Client], height: u64) -> Result<()> {
    poll("finalized height on all native replicas", WAIT, || {
        let statuses = clients
            .iter()
            .map(Client::status)
            .collect::<Result<Vec<_>>>()?;
        Ok(statuses
            .iter()
            .all(|status| status.height >= height)
            .then_some(()))
    })
}

fn prove_execution(
    clients: &[Client],
    verifier: &Store,
    height: u64,
    index: u32,
    tx: &Transaction,
    expected: Option<Outcome>,
) -> Result<Value> {
    wait_height(clients, height)?;
    let header = clients[0].header(height)?;
    let certificate = clients[0].finality(height)?;
    assert_eq!(certificate.epoch, 0);
    assert!(!certificate.handoff);
    let final_header = verify_finality(verifier, &[], &certificate.proof, false)?;
    let mut descendant: Header = final_header.clone();
    while descendant.context.height > height {
        let parent = clients[0].header(descendant.context.height - 1)?;
        assert_eq!(parent.id(verifier.network()), descendant.context.parent);
        descendant = parent;
    }
    assert_eq!(descendant, header);
    let mut outcomes = Vec::new();
    for client in clients {
        assert_eq!(client.header(height)?, header);
        let query = client.query("/stem/execution", height, &index.to_le_bytes(), true)?;
        assert_eq!(query.height, height);
        let payload: Value = serde_json::from_slice(&query.value)?;
        assert_eq!(bytes(&payload, "transaction")?, tx.as_slice());
        let receipt = Receipt::decode(&bytes(&payload, "receipt")?)?;
        assert!(receipt.cycles > 0 && receipt.fee >= FEE);
        if let Some(outcome) = expected {
            assert_eq!(Outcome::decode(&receipt.protocol_result)?, outcome);
        }
        let (tx_path, receipt_path) =
            InclusionProof::decode_execution_pair(&bytes(&payload, "proof")?)?;
        assert!(tx_path.verify_execution(
            verifier.network(),
            &header.execution(),
            tx,
            &receipt_path,
            &receipt
        ));
        outcomes.push(payload);
    }
    assert!(outcomes.windows(2).all(|pair| pair[0] == pair[1]));
    Ok(
        json!({"header": hex::encode(header.encode()), "certified_header": hex::encode(final_header.encode()),
        "certificate": hex::encode(certificate.proof), "replicas": outcomes}),
    )
}

fn bytes(payload: &Value, key: &str) -> Result<Vec<u8>> {
    let text = payload[key]
        .as_str()
        .with_context(|| format!("missing {key}"))?;
    Ok(hex::decode(text.trim_start_matches("0x"))?)
}
