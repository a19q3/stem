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
    keeper::decode_agenda,
    lineage::{Head, Outcome},
    patricia::Proof,
    proof::InclusionProof,
};

use super::{
    Context, FEE, Fixture, OutPoint, Result, Retention, Store,
    comet::{Asset, Process, compile_types, poll, write_spec},
    intent::{Swap, altered_action},
    keeper,
    lineage::{DynamicLineage, FixtureLineage},
    lineage_batch::BatchFixture,
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
    let action_swap = Swap::fund_with_actions(&mut fixture, &output)?;
    let lineage = FixtureLineage::fund(&mut fixture)?;
    let dynamic = DynamicLineage::fund_using(&mut fixture, &lineage, 5, 2, None)?;
    let batch_markets = BatchFixture::fund(&mut fixture, &lineage)?;
    keeper::fund_carriers(&mut fixture, 2)?;
    fixture.spec.keeper.as_mut().unwrap().time = Some(stem::native_store::KeeperTimePolicy {
        per_height: 2,
        max_delay_ms: 60_000,
    });
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
    let with_actions = action_swap.sign(&fixture, &spec, 500)?;
    let (fragment_index, final_index) = action_swap.policy_indices(&fixture, &with_actions)?;
    let altered = altered_action(&with_actions)?;
    let action_rejections = clients
        .iter()
        .map(|replica| {
            let error = replica.submit(&altered).unwrap_err().to_string();
            assert!(error.contains("Lock Script Group"), "{error}");
            error
        })
        .collect::<Vec<_>>();
    submit(
        "independently sponsored OTX with application Actions",
        &with_actions,
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
    for registration in batch_markets.registrations(&fixture)? {
        submit("Lineage batch reserve registration", &registration, None)?;
    }
    let bad_batch = batch_markets.sdk_batch(&fixture, 501)?;
    let batch_rejections = clients
        .iter()
        .map(|replica| {
            let error = replica.submit(&bad_batch).unwrap_err().to_string();
            assert!(error.contains("Lineage Executor rejected"), "{error}");
            error
        })
        .collect::<Vec<_>>();
    let good_batch = batch_markets.wallet_batch(&fixture, &spec, &output.join("lineage-batch"))?;
    submit("atomic Lineage batch", &good_batch, Some(Outcome::Applied))?;
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
    let before = clients[0].status()?.height;
    let target = clients[0].header(before)?.context.timestamp_ms + 4_000;
    let timed = dynamic.timed(&fixture, 4, (3, 3), 5, target, before + 55)?;
    submit(
        "Keeper time reservation",
        &keeper::schedule(&fixture, 1, &timed)?,
        None,
    )?;
    let point = dynamic.fee_points[4].as_slice();
    let query = clients[0].query("/stem/keeper-time", 0, &[], true)?;
    let agenda: Value = serde_json::from_slice(&query.value)?;
    let record = bytes(&agenda, "record")?;
    let entries = decode_agenda(&record)?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].timestamp_ms, target);
    assert_eq!(entries[0].fee_input.as_slice(), point);
    assert!(Store::verify_keeper_agenda(
        network,
        &clients[0].header(query.height)?,
        Some(&record),
        &Proof::decode(&bytes(&agenda, "proof")?)?
    ));
    let query = clients[0].query("/stem/keeper-time", 0, point, true)?;
    let pending: Value = serde_json::from_slice(&query.value)?;
    assert_eq!(bytes(&pending, "record")?, timed.as_slice());
    assert!(Store::verify_keeper_timed_command(
        network,
        &clients[0].header(query.height)?,
        &dynamic.fee_points[4],
        Some(timed.as_slice()),
        &Proof::decode(&bytes(&pending, "proof")?)?
    ));

    let restart = std::time::Instant::now();
    drop(replicas.pop().context("missing replica")?);
    let mut command = Command::new(env!("CARGO_BIN_EXE_stem"));
    command
        .args(["run", "--home"])
        .arg(output.join("node-3"))
        .arg("--configuration")
        .arg(&configuration)
        .arg("--genesis")
        .arg(&spec)
        .arg("--key-file")
        .arg(output.join("validator-3.key"));
    replicas.push(Process::spawn(
        &mut command,
        &output.join("node-3-restart.log"),
    )?);

    let mut next = before + 2;
    let timed_height = poll("time-triggered Keeper execution", WAIT, || {
        while next <= clients[0].status()?.height {
            let candidate = next;
            next += 1;
            if clients[0].header(candidate - 1)?.context.timestamp_ms >= target {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    })?;
    let timestamp = clients[0].header(timed_height - 1)?.context.timestamp_ms;
    assert!(clients[0].header(timed_height - 2)?.context.timestamp_ms < target);
    let timed_wake = prove_execution(
        &clients,
        &verifier,
        timed_height,
        0,
        &timed,
        Some(Outcome::Applied),
    )?;
    let restart_to_visibility_ms = restart.elapsed().as_millis();
    let query = clients[0].query("/stem/keeper-time", 0, point, true)?;
    let consumed: Value = serde_json::from_slice(&query.value)?;
    assert!(consumed["record"].is_null());
    assert!(Store::verify_keeper_timed_command(
        network,
        &clients[0].header(query.height)?,
        &dynamic.fee_points[4],
        None,
        &Proof::decode(&bytes(&consumed, "proof")?)?
    ));
    let heads = clients
        .iter()
        .map(|client| -> Result<Value> {
            let query = client.query("/stem/lineage", 0, dynamic.ids[0].as_slice(), true)?;
            let payload: Value = serde_json::from_slice(&query.value)?;
            let head = Head::decode(&bytes(&payload, "record")?)?;
            assert_eq!(head.version, 4);
            assert_eq!(head.out_point, OutPoint::new(timed.id(network), 1));
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
    let mut batch_heads = Vec::new();
    for member in good_batch.lineage_batch().unwrap() {
        let child_id = member.id(network);
        for (index, target) in member.lineage_command().unwrap().targets.iter().enumerate() {
            let mut replicas = Vec::new();
            for client in &clients {
                let query = client.query("/stem/lineage", 0, target.id.as_slice(), true)?;
                let payload: Value = serde_json::from_slice(&query.value)?;
                let head = Head::decode(&bytes(&payload, "record")?)?;
                assert_eq!(head.version, 1);
                assert_eq!(
                    head.out_point,
                    OutPoint::new(child_id.clone(), index as u32 + 1)
                );
                assert!(Store::verify_lineage(
                    network,
                    &client.header(query.height)?,
                    &target.id,
                    Some(&head),
                    &Proof::decode(&bytes(&payload, "proof")?)?,
                ));
                replicas.push(json!({"height": query.height, "proof": payload}));
            }
            batch_heads
                .push(json!({"id": hex::encode(target.id.as_slice()), "replicas": replicas}));
        }
    }
    fs::write(
        output.join("native-composition.json"),
        serde_json::to_vec_pretty(&json!({
            "rejected_policy_removal": invalid, "executions": executions,
            "otx_actions": {"fragment_index": fragment_index, "final_index": final_index,
                "rejections": action_rejections, "invalid_transaction": hex::encode(altered.as_slice())},
            "lineage_batch": {"rejections": batch_rejections, "heads": batch_heads,
                "rejected_transaction": hex::encode(bad_batch.as_slice())},
            "keeper_wake_height": due, "keeper_wake": wake, "heads": heads,
            "timed": {"requested_ms": target, "parent_ms": timestamp, "height": timed_height,
                "pending": pending, "agenda": agenda, "consumed": consumed,
                "proof": timed_wake, "restart_to_visibility_ms": restart_to_visibility_ms},
        }))?,
    )?;
    println!(
        "four native replicas verified Polytype, sponsored OTX, atomic Lineage batches and Keeper at heights {due} and {timed_height}"
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
