use std::{
    fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use ckb_db::internal::{Direction, IteratorMode, Options, ReadOnlyDB, prelude::*};
use ckb_types::prelude::*;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use stem::jolteon::{Client, SubmissionProgress, verify_finality};
use stem_ledger::{
    Transaction,
    block::{Header, Receipt},
    keeper::{Reservation, Retry, Trigger, decode_agenda, primary_fee},
    lineage::{Head, LineageProof, LineageRef, Outcome},
    patricia::{Namespace, Patricia, Proof},
    proof::InclusionProof,
    record,
};

use super::{
    Context, FEE, Fixture, OutPoint, Result, Retention, Store,
    comet::{Asset, Process, compile_types, poll, write_spec},
    intent::{Swap, altered_action},
    keeper,
    keeper_load::{NetworkOptions, start_network_configured, statuses},
    lineage::{DynamicLineage, FixtureLineage},
    lineage_batch::{BatchFixture, Schedule},
};

const WAIT: Duration = Duration::from_secs(20);

#[test]
fn concurrent_admission_checks_each_signed_wire_identity() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let signed = fixture.signed(fixture.spend.clone(), None, super::FUND - FEE, None)?;
    let unsigned = signed.with_witnesses(vec![])?;
    assert_eq!(
        signed.id(&fixture.spec.network),
        unsigned.id(&fixture.spec.network)
    );
    assert_ne!(signed.as_slice(), unsigned.as_slice());
    let root = tempfile::tempdir()?;
    let (_processes, clients) = start_network_configured(
        &mut fixture,
        root.path(),
        4,
        NetworkOptions {
            interval_ms: 200,
            timeout_ms: 2000,
            vm_workers: 2,
            history_window: None,
            storage_metrics: false,
        },
    )?;
    let first = &clients[0];
    let gate = std::sync::Barrier::new(12);
    std::thread::scope(|scope| -> Result<()> {
        let gate = &gate;
        let attempts = (0..12)
            .map(|index| {
                let tx = if index % 2 == 0 { &signed } else { &unsigned };
                scope.spawn(move || {
                    gate.wait();
                    let mut progress = SubmissionProgress::default();
                    let result = first.submit_with_progress(tx, &mut progress);
                    (result, progress)
                })
            })
            .collect::<Vec<_>>();
        for (index, attempt) in attempts.into_iter().enumerate() {
            let (result, progress) = attempt.join().expect("admission submitter");
            let connected = progress.connected_at.context("connected submission")?;
            let written = progress.frame_written_at.context("written submission")?;
            let read = progress.response_read_at.context("admission response")?;
            assert!(connected <= written && written <= read);
            if index % 2 == 0 {
                assert_eq!(result?, ckb_hash::blake2b_256(signed.as_slice()));
            } else {
                assert!(
                    result.is_err(),
                    "an unsigned variant received an admission reply"
                );
            }
        }
        Ok(())
    })?;
    let values = poll("deduplicated admission finality", WAIT, || {
        let values = statuses(&clients)?;
        Ok(values
            .iter()
            .all(|status| status.committed_transactions == 1)
            .then_some(values))
    })?;
    let height = values.iter().map(|status| status.height).min().unwrap();
    let expected = first.header(height)?;
    for client in &clients {
        assert_eq!(client.header(height)?, expected);
    }
    let admission = values[0].admission.context("admission metrics")?;
    assert_eq!(admission.client_requests, 12);
    assert_eq!(admission.client_queue.buckets.iter().sum::<u64>(), 12);
    assert_eq!(admission.client_service.buckets.iter().sum::<u64>(), 12);
    assert!(
        admission.checks <= 7,
        "valid duplicates should use the admitted entry"
    );

    let mut clients = clients.into_iter();
    let pooled = clients
        .next()
        .context("missing replica")?
        .with_submission_reuse();
    let next_point = OutPoint::new(signed.id(&fixture.spec.network), 0);
    let next = fixture.signed(next_point, None, super::FUND - 2 * FEE, None)?;
    let mut progress = SubmissionProgress::default();
    assert!(
        pooled
            .submit_with_progress(&next.with_witnesses(vec![])?, &mut progress)
            .is_err()
    );
    assert!(!progress.reused_connection);
    assert!(progress.response_read_at.is_some());
    assert_eq!(
        pooled.submit_with_progress(&next, &mut progress)?,
        ckb_hash::blake2b_256(next.as_slice())
    );
    assert!(progress.reused_connection);
    let clients = std::iter::once(pooled).chain(clients).collect::<Vec<_>>();
    poll("pooled submission finality", WAIT, || {
        let values = statuses(&clients)?;
        Ok(values
            .iter()
            .all(|status| status.committed_transactions == 2)
            .then_some(()))
    })?;
    Ok(())
}

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
    let batch_markets = BatchFixture::fund_with_fees(&mut fixture, &lineage, 4)?;
    keeper::fund_carriers(&mut fixture, 5)?;
    fixture.spec.limits.cell_reads = 512;
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
            .is_ok_and(|status| {
                status.history_window == Some(256)
                    && status.certificate_experiment.is_some()
                        == cfg!(feature = "jolteon-certificate-experiment")
            })
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
    let transfer = asset.transfer(&fixture, false)?;
    submit("two Type Scripts", &transfer, None)?;
    let output_point = OutPoint::new(transfer.id(network), 0);
    let points = [&asset.point, &output_point, &asset.point];
    let requested = points
        .iter()
        .flat_map(|point| point.as_slice().iter().copied())
        .collect::<Vec<_>>();
    let batch = clients[0].query("/stem/cells", 0, &requested, true)?;
    let header = clients[0].header(batch.height)?;
    let rows: Vec<Value> = serde_json::from_slice(&batch.value)?;
    assert_eq!(rows.len(), points.len());
    assert_eq!(rows[0], rows[2]);
    assert!(rows[0]["cell"].is_null());
    assert!(!rows[1]["cell"].is_null());
    let tree = Patricia::new(network.clone(), Namespace::Cells);
    let root = header.state.cell_root.unpack();
    for (point, row) in points.into_iter().zip(rows) {
        let cell = if row["cell"].is_null() {
            None
        } else {
            let (stored, cell) = record::decode_cell(&bytes(&row, "cell")?)?;
            assert_eq!(&stored, point);
            Some(cell)
        };
        let proof = Proof::decode(&bytes(&row, "proof")?)?;
        assert!(record::verify_cell(&tree, &root, point, cell.as_ref(), &proof));
    }
    assert!(clients[0].query("/stem/cells", 0, &[0], true).is_err());
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
    let original = LineageRef::new(dynamic.ids[0].clone(), Some(0))?;
    let historical = lineage_evidence(&clients[0], &verifier, &original, &dynamic.marker)?;
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
    let height_batch = batch_markets.wallet_scheduled(
        &fixture,
        &spec,
        &output.join("lineage-height-batch"),
        &Schedule {
            trigger: Trigger::Height(due),
            expires: [due + 8, due + 6],
            version: 1,
            fees: [1, 1],
        },
    )?;
    submit(
        "Keeper height batch reservation",
        &keeper::schedule(&fixture, 2, &height_batch)?,
        None,
    )?;
    let height_pending = keeper_batch_links(&clients[0], &verifier, &height_batch, true)?;
    let height_index = u32::from(primary_fee(&height_batch) > primary_fee(&scheduled));
    wait_height(&clients, due)?;
    let wake = prove_execution(
        &clients,
        &verifier,
        due,
        1 - height_index,
        &scheduled,
        Some(Outcome::Applied),
    )?;
    let height_batch_wake = prove_execution(
        &clients,
        &verifier,
        due,
        height_index,
        &height_batch,
        Some(Outcome::Applied),
    )?;
    let height_cleared = keeper_batch_links(&clients[0], &verifier, &height_batch, false)?;
    let before = clients[0].status()?.height;
    let target = clients[0].header(before)?.context.timestamp_ms + 7_000;
    let timed = dynamic.timed(&fixture, 4, (3, 3), 5, target, before + 55)?;
    submit(
        "Keeper time reservation",
        &keeper::schedule(&fixture, 1, &timed)?,
        None,
    )?;
    let time_batch = batch_markets.wallet_scheduled(
        &fixture,
        &spec,
        &output.join("lineage-time-batch"),
        &Schedule {
            trigger: Trigger::Time(target),
            expires: [before + 55, before + 50],
            version: 2,
            fees: [2, 2],
        },
    )?;
    submit(
        "Keeper time batch reservation",
        &keeper::schedule(&fixture, 3, &time_batch)?,
        None,
    )?;
    let time_pending = keeper_batch_links(&clients[0], &verifier, &time_batch, true)?;
    let time_index = u32::from(primary_fee(&time_batch) > primary_fee(&timed));
    let point = dynamic.fee_points[4].as_slice();
    let query = clients[0].query("/stem/keeper-time", 0, &[], true)?;
    let agenda: Value = serde_json::from_slice(&query.value)?;
    let record = bytes(&agenda, "record")?;
    let entries = decode_agenda(&record)?;
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|entry| entry.trigger == Trigger::Time(target))
    );
    assert_eq!(entries[1 - time_index as usize].fee_input.as_slice(), point);
    assert_eq!(
        Some(entries[time_index as usize].fee_input.clone()),
        primary_fee(&time_batch)
    );
    assert_eq!(
        entries[time_index as usize].valid_before_height,
        before + 50
    );
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
    restart_last_replica(
        &mut replicas,
        &output,
        &configuration,
        &spec,
        "node-3-restart.log",
    )?;

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
        1 - time_index,
        &timed,
        Some(Outcome::Applied),
    )?;
    let time_batch_wake = prove_execution(
        &clients,
        &verifier,
        timed_height,
        time_index,
        &time_batch,
        Some(Outcome::Applied),
    )?;
    let time_cleared = keeper_batch_links(&clients[3], &verifier, &time_batch, false)?;
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
    let latest = LineageRef::new(dynamic.ids[0].clone(), None)?;
    let exact = LineageRef::new(dynamic.ids[0].clone(), Some(4))?;
    let heads = clients
        .iter()
        .map(|client| -> Result<Value> {
            for rejected in [original.encode().as_ref(), latest.id().as_slice()] {
                assert!(client.query("/stem/lineage", 0, rejected, true).is_err());
            }
            let query = client.query("/stem/lineage", 0, &latest.encode(), true)?;
            let payload: Value = serde_json::from_slice(&query.value)?;
            let head = Head::decode(&bytes(&payload, "record")?)?;
            assert_eq!(head.version, 4);
            assert_eq!(head.out_point, OutPoint::new(timed.id(network), 1));
            let proof = LineageProof::decode(
                Some(&bytes(&payload, "record")?),
                &bytes(&payload, "proof")?,
                Some(&bytes(&payload, "cell")?),
                Some(&bytes(&payload, "cell_proof")?),
            )?;
            let header = client.header(query.height)?;
            assert!(proof.verify(network, &header, &latest, &dynamic.marker));
            assert!(proof.verify(network, &header, &exact, &dynamic.marker));
            assert!(!proof.verify(network, &header, &original, &dynamic.marker));
            let exact_query = client.query("/stem/lineage", 0, &exact.encode(), true)?;
            assert_eq!(exact_query.value, query.value);
            Ok(payload)
        })
        .collect::<Result<Vec<_>>>()?;
    let current = lineage_evidence(&clients[0], &verifier, &exact, &dynamic.marker)?;
    fs::write(
        output.join("lineage-references.json"),
        serde_json::to_vec_pretty(&json!({
            "id": hex::encode(latest.id().as_slice()),
            "registration_anchor": hex::encode(dynamic.anchors[0].as_slice()),
            "marker_code": hex::encode(dynamic.marker.as_slice()),
            "historical": historical,
            "current": current,
        }))?,
    )?;
    let first_retry_height = clients[0].status()?.height + 12;
    let retry_delay = 8;
    let second_retry_height = first_retry_height + u64::from(retry_delay);
    let retrying = batch_markets.wallet_retry(
        &fixture,
        &spec,
        &output.join("lineage-retry-batch"),
        &Schedule {
            trigger: Trigger::Height(first_retry_height),
            expires: [first_retry_height + 22, first_retry_height + 20],
            version: 3,
            fees: [3, 3],
        },
        Retry {
            max_attempts: 3,
            delay_blocks: retry_delay,
        },
        second_retry_height,
    )?;
    let carrier = keeper::schedule_with_fee(&fixture, 4, &retrying, 4 * FEE)?;
    submit("Keeper retry batch reservation", &carrier, None)?;
    let first_attempt = prove_execution(
        &clients,
        &verifier,
        first_retry_height,
        0,
        &retrying,
        Some(Outcome::Retrying),
    )?;
    let pending_retry = clients[0].query("/stem/keeper-time", 0, &[], true)?;
    let pending_record: Value = serde_json::from_slice(&pending_retry.value)?;
    let record = bytes(&pending_record, "record")?;
    let entry = decode_agenda(&record)?.remove(0);
    assert_eq!(entry.eligible_height, second_retry_height);
    assert_eq!(entry.attempts_left, 2);
    assert!(Store::verify_keeper_agenda(
        network,
        &clients[0].header(pending_retry.height)?,
        Some(&record),
        &Proof::decode(&bytes(&pending_record, "proof")?)?,
    ));
    let retry_links = keeper_batch_links(&clients[0], &verifier, &retrying, true)?;
    restart_last_replica(
        &mut replicas,
        &output,
        &configuration,
        &spec,
        "node-3-retry-restart.log",
    )?;
    let second_attempt = prove_execution(
        &clients,
        &verifier,
        second_retry_height,
        0,
        &retrying,
        Some(Outcome::Applied),
    )?;
    let retry_cleared = keeper_batch_links(&clients[3], &verifier, &retrying, false)?;
    let mut batch_heads = Vec::new();
    for member in retrying.lineage_batch().unwrap() {
        let child_id = member.id(network);
        for (index, target) in member.lineage_command().unwrap().targets.iter().enumerate() {
            let mut replicas = Vec::new();
            let reference = LineageRef::new(target.reference.id().clone(), Some(4))?;
            for client in &clients {
                let query = client.query("/stem/lineage", 0, &reference.encode(), true)?;
                let payload: Value = serde_json::from_slice(&query.value)?;
                let head = Head::decode(&bytes(&payload, "record")?)?;
                assert_eq!(head.version, 4);
                assert_eq!(
                    head.out_point,
                    OutPoint::new(child_id.clone(), index as u32 + 1)
                );
                assert!(Store::verify_lineage(
                    network,
                    &client.header(query.height)?,
                    reference.id(),
                    Some(&head),
                    &Proof::decode(&bytes(&payload, "proof")?)?,
                ));
                replicas.push(json!({"height": query.height, "proof": payload}));
            }
            batch_heads
                .push(json!({"id": hex::encode(reference.id().as_slice()), "replicas": replicas}));
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
            "keeper_batches": {
                "height": {"height": due, "pending": height_pending, "cleared": height_cleared, "proof": height_batch_wake},
                "time": {"height": timed_height, "pending": time_pending, "cleared": time_cleared, "proof": time_batch_wake},
            },
            "keeper_retry": {"first_height": first_retry_height, "second_height": second_retry_height,
                "first_attempt": first_attempt, "second_attempt": second_attempt,
                "pending": pending_record, "links": retry_links, "cleared": retry_cleared},
            "timed": {"requested_ms": target, "parent_ms": timestamp, "height": timed_height,
                "pending": pending, "agenda": agenda, "consumed": consumed,
                "proof": timed_wake, "restart_to_visibility_ms": restart_to_visibility_ms},
        }))?,
    )?;
    println!(
        "four native replicas verified Polytype, sponsored OTX, atomic Lineage and scheduled batches at heights {due}, {timed_height}, {first_retry_height} and {second_retry_height}"
    );
    Ok(())
}

fn restart_last_replica(
    replicas: &mut Vec<Process>,
    output: &Path,
    configuration: &Path,
    spec: &Path,
    log: &str,
) -> Result<()> {
    drop(replicas.pop().context("missing replica")?);
    let mut command = Command::new(env!("CARGO_BIN_EXE_stem"));
    command
        .args(["run", "--home"])
        .arg(output.join("node-3"))
        .arg("--configuration")
        .arg(configuration)
        .arg("--genesis")
        .arg(spec)
        .arg("--key-file")
        .arg(output.join("validator-3.key"));
    replicas.push(Process::spawn(&mut command, &output.join(log))?);
    Ok(())
}

fn keeper_batch_links(
    client: &Client,
    store: &Store,
    batch: &Transaction,
    pending: bool,
) -> Result<Vec<Value>> {
    batch.lineage_batch().unwrap().iter().map(|member| {
        let fee = member.input(0).unwrap().previous_output;
        let query = client.query("/stem/keeper-reservation", 0, fee.as_slice(), true)?;
        let payload: Value = serde_json::from_slice(&query.value)?;
        let record = (!payload["record"].is_null()).then(|| bytes(&payload, "record")).transpose()?;
        assert_eq!(record.is_some(), pending);
        if let Some(record) = &record {
            let link = Reservation::decode(record)?;
            assert_eq!(Some(link.primary_fee), primary_fee(batch));
            assert_eq!(link.command_id, batch.id(store.network()));
            assert_eq!(Some(link.trigger), batch.keeper_trigger());
        }
        assert!(Store::verify_keeper_reservation(
            store.network(), &client.header(query.height)?, &fee, record.as_deref(),
            &Proof::decode(&bytes(&payload, "proof")?)?,
        ));
        Ok(json!({"fee_input": hex::encode(fee.as_slice()), "height": query.height, "proof": payload}))
    }).collect()
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
        assert!(receipt.cycles > 0);
        if matches!(expected, Some(Outcome::Retrying | Outcome::Skipped)) {
            assert_eq!(receipt.fee, 0);
        } else {
            assert!(receipt.fee >= FEE);
        }
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

fn lineage_evidence(
    client: &Client,
    store: &Store,
    reference: &LineageRef,
    marker: &ckb_types::packed::Byte32,
) -> Result<Value> {
    let query = client.query("/stem/lineage", 0, &reference.encode(), true)?;
    let payload: Value = serde_json::from_slice(&query.value)?;
    let certificate = client.finality(query.height)?;
    let header = verify_finality(store, &[], &certificate.proof, false)?;
    let proof = LineageProof::decode(
        Some(&bytes(&payload, "record")?),
        &bytes(&payload, "proof")?,
        Some(&bytes(&payload, "cell")?),
        Some(&bytes(&payload, "cell_proof")?),
    )?;
    assert!(proof.verify(store.network(), &header, reference, marker));
    Ok(json!({"head": payload, "height": header.context.height,
        "finality": hex::encode(certificate.proof)}))
}
