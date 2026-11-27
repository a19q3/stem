use std::{fs, path::Path, process::Command, time::Duration};

use anyhow::ensure;
use serde_json::json;
use stem_ledger::{ScriptGroupKind, derive_script_groups, light::FinalityVerifier};
use stem_stake::Member;

use super::{
    comet::poll,
    keeper_load::{NetworkOptions, execution, start_network_configured, statuses, transactions},
    *,
};

pub(super) fn build_contract(kind: &str, name: &str) -> Result<Bytes> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target = root.join("target/stem-contracts");
    let triple = "riscv64imac-unknown-none-elf";
    let result = Command::new(env!("CARGO"))
        .args(["build", "--locked", "--release", "--target", triple, kind, name])
        .arg("--manifest-path")
        .arg(root.join("contracts/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env("RUSTFLAGS", "-C passes=lower-atomic -C target-feature=-c")
        .output()?;
    ensure!(result.status.success(), "contract {name}: {}", String::from_utf8_lossy(&result.stderr));
    let release = target.join(triple).join("release");
    let directory = if kind == "--example" { release.join("examples") } else { release };
    Ok(fs::read(directory.join(name))?.into())
}

#[test]
fn independent_creditors_require_exclusive_collateral_accounting() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let directory = tempfile::tempdir()?;
    let asset_program = build_contract("--bin", "stem-native-udt")?;
    let policy_program = build_contract("--example", "polytype-obligation")?;
    let data_script = |program: &[u8], args: Vec<u8>| {
        Script::new_builder()
            .hash_type(ScriptHashType::Data)
            .code_hash(blake2b_256(program))
            .args(Bytes::from(args))
            .build()
    };
    let issuer = fixture.lock.calc_script_hash();
    let asset = data_script(&asset_program, [issuer.as_slice(), &[17; 32]].concat());
    let asset_id = asset.calc_script_hash();
    let creditor_a = data_script(&policy_program, [asset_id.as_slice(), &[33; 32]].concat());
    let creditor_b = data_script(&policy_program, [asset_id.as_slice(), &[44; 32]].concat());
    for (index, code) in [asset_program, policy_program].into_iter().enumerate() {
        let point = OutPoint::new([84; 32].pack(), index as u32);
        let capacity = Cell::minimum_capacity(&fixture.lock, &[], code.len())?;
        let cell = Cell::new(capacity, fixture.lock.clone(), vec![], code)?;
        fixture
            .deps
            .push(CellDep::new_builder().out_point(point.clone()).build());
        fixture.spec.development_cells.push((point, cell));
    }
    let spend = OutPoint::new([86; 32].pack(), 0);
    let payment_fund = Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?;
    fixture
        .spec
        .development_cells
        .push((spend.clone(), payment_fund));
    let mut store = Store::open(directory.path(), &fixture.spec, Retention::Archive)?;
    let make = |inputs: &[OutPoint], rows: &[(u128, &[&Script])], budget| -> Result<Transaction> {
        let capacity = (budget - FEE) / rows.len() as u64;
        let cells = rows
            .iter()
            .map(|(units, creditors)| {
                let types = std::iter::once(&asset)
                    .chain(creditors.iter().copied())
                    .cloned()
                    .collect();
                Cell::new(
                    capacity,
                    fixture.lock.clone(),
                    types,
                    Bytes::copy_from_slice(&units.to_le_bytes()),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        fixture.seal(Transaction::new(
            fixture.deps.clone(),
            vec![],
            inputs
                .iter()
                .cloned()
                .map(|point| CellInput::new(point, None))
                .collect(),
            cells,
            vec![],
        )?)
    };
    let check_collateral = |tx: &Transaction| -> Result<[u128; 2]> {
        let mut holdings = [0; 2];
        let mut supply = 0;
        for cell in tx
            .outputs()
            .filter(|cell| cell.type_scripts().any(|script| script == asset))
        {
            let units = u128::from_le_bytes(cell.data().as_ref().try_into()?);
            let a = cell.type_scripts().any(|script| script == creditor_a);
            let b = cell.type_scripts().any(|script| script == creditor_b);
            ensure!(!(a && b), "same output pledged to both creditors");
            supply += units;
            holdings[0] += if a { units } else { 0 };
            holdings[1] += if b { units } else { 0 };
        }
        ensure!(supply == 100, "asset supply differs from issuance");
        Ok(holdings)
    };
    let point = |tx: &Transaction, index| OutPoint::new(tx.id(&fixture.spec.network), index);
    let issue = make(
        std::slice::from_ref(&fixture.spend),
        &[(60, &[&creditor_a]), (40, &[&creditor_b])],
        FUND,
    )?;
    assert_eq!(check_collateral(&issue)?, [60, 40]);
    store.commit(store.prepare(std::slice::from_ref(&issue), TIME + 1, false)?)?;
    let inputs = [point(&issue, 0), point(&issue, 1)];
    let budget = issue.outputs().map(|cell| cell.capacity()).sum();
    let overlap = make(&inputs, &[(100, &[&creditor_a, &creditor_b])], budget)?;
    store.check(&overlap)?;
    assert!(
        check_collateral(&overlap)
            .unwrap_err()
            .to_string()
            .contains("both creditors")
    );

    let payment = fixture.signed(spend.clone(), None, FUND - FEE, None)?;
    let payment_output = point(&payment, 0);
    let tip = store.tip().clone();
    let proofs = |store: &Store| -> Result<Vec<_>> {
        inputs
            .iter()
            .chain([&spend, &payment_output])
            .map(|point| {
                let (record, proof) = store.prove_cell(point)?;
                Ok((record.map(|live| record::encode_cell(point, &live)), proof.encode()?))
            })
            .collect()
    };
    let before = proofs(&store)?;
    let workers = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    for attempted in [
        vec![(60, &[&creditor_a][..]), (40, &[][..])],
        vec![
            (60, &[&creditor_a][..]),
            (1, &[&creditor_b][..]),
            (39, &[][..]),
        ],
    ] {
        let invalid = make(&inputs, &attempted, budget)?;
        assert!(check_collateral(&invalid)?[1] < 40);
        let batch = [payment.clone(), invalid];
        for parallel in [false, true] {
            let error = workers
                .install(|| store.prepare(&batch, TIME + 2, parallel))
                .err()
                .context("unauthorized collateral release accepted")?;
            assert!(format!("{error:#}").contains("exited with 64"));
            assert_eq!(store.tip(), &tip);
            assert_eq!(proofs(&store)?, before);
        }
    }
    let separated = make(
        &inputs,
        &[
            (10, &[&creditor_a]),
            (50, &[&creditor_a]),
            (40, &[&creditor_b]),
        ],
        budget,
    )?;
    assert_eq!(check_collateral(&separated)?, [60, 40]);
    let settlements = [
        point(&separated, 0),
        point(&separated, 1),
        point(&separated, 2),
    ];
    let batch = [payment, separated];
    let serial = store.prepare(&batch, TIME + 2, false)?;
    let parallel = workers.install(|| store.prepare(&batch, TIME + 2, true))?;
    assert_eq!(serial.header(), parallel.header());
    store.commit(parallel)?;
    assert!(store.prove_cell(&payment_output)?.0.is_some());
    for output in settlements {
        assert!(store.prove_cell(&output)?.0.is_some());
    }
    for input in inputs.iter().chain([&spend]) {
        assert!(store.prove_cell(input)?.0.is_none());
    }
    Ok(())
}

#[test]
#[ignore = "four-replica RISC-V late attachment and discharge with finalized evidence"]
fn independent_type_attaches_after_issuance_and_discharges_by_authority() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let root = tempfile::tempdir()?;
    let authority = Privkey::from_slice(&[77; 32]);
    let authority_lock =
        fixture.lock.clone().as_builder().args(Bytes::copy_from_slice(&blake2b_256(authority.pubkey()?.serialize())[..20])).build();
    let asset_program = build_contract("--bin", "stem-native-udt")?;
    let mut asset_args = fixture.lock.calc_script_hash().as_slice().to_vec();
    asset_args.extend_from_slice(&[11; 32]);
    let asset = Script::new_builder()
        .hash_type(ScriptHashType::Data)
        .code_hash(blake2b_256(&asset_program))
        .args(Bytes::from(asset_args))
        .build();
    let asset_id = asset.calc_script_hash();
    let code_point = OutPoint::new([71; 32].pack(), 0);
    let deploy_point = OutPoint::new([71; 32].pack(), 1);
    let authority_point = OutPoint::new([71; 32].pack(), 2);
    let deploy_fund = 10_000_000_000_000;
    let deposit = Cell::new(FUND, authority_lock.clone(), vec![], Bytes::new())?;
    fixture.spec.development_cells.extend([
        (
            code_point.clone(),
            Cell::new(Cell::minimum_capacity(&fixture.lock, &[], asset_program.len())?, fixture.lock.clone(), vec![], asset_program)?,
        ),
        (deploy_point.clone(), Cell::new(deploy_fund, fixture.lock.clone(), vec![], Bytes::new())?),
        (authority_point.clone(), deposit.clone()),
    ]);
    let (_nodes, clients) = start_network_configured(
        &mut fixture,
        root.path(),
        4,
        NetworkOptions { interval_ms: 100, timeout_ms: 1500, vm_workers: 2, history_window: None, storage_metrics: false },
    )?;
    let archive = &clients[3];
    let network = &fixture.spec.network;
    let genesis = archive.header(0)?.id(network);
    let members = fixture
        .spec
        .development_validators
        .iter()
        .map(|(key, weight)| Member { key: *key, power: *weight as u64 })
        .collect::<Vec<_>>();
    let verifier = FinalityVerifier::new(network.clone(), genesis, &members)?;
    let mut scanned = 0;
    let mut evidence = Vec::new();
    let mut commit = |tx: &Transaction| -> Result<u64> {
        clients[0].submit(tx)?;
        let id = hex::encode(tx.id(network).as_slice());
        let (height, index) = poll("late Type transaction", Duration::from_secs(20), || {
            let common = statuses(&clients)?.iter().map(|s| s.height).min().unwrap();
            while scanned < common {
                scanned += 1;
                if let Some(index) = transactions(archive, scanned)?.iter().position(|item| item == &id) {
                    return Ok(Some((scanned, index)));
                }
            }
            Ok(None)
        })?;
        let (receipt, execution) = execution(archive, &fixture, height, index, &id)?;
        assert!(receipt.cycles > 0);
        let inclusion = archive.header(height)?;
        for node in &clients {
            assert_eq!(node.header(height)?, inclusion);
        }
        let certificate = archive.finality(height)?;
        assert!(!certificate.handoff);
        let verified = verifier.verify(&certificate.proof, false)?;
        let mut anchor = verified.header().clone();
        let mut ancestry = Vec::new();
        while anchor.context.height > height {
            let previous = archive.header(anchor.context.height - 1)?;
            assert_eq!(anchor.context.parent, previous.id(network));
            ancestry.push(hex::encode(previous.encode()));
            anchor = previous;
        }
        assert_eq!(anchor, inclusion);
        evidence.push(json!({
            "height": height, "index": index, "header": hex::encode(inclusion.encode()),
            "finality_proof": hex::encode(certificate.proof), "ancestry": ancestry, "execution": execution,
        }));
        Ok(height)
    };
    let dep = |point: OutPoint| CellDep::new_builder().out_point(point).build();
    let mut asset_deps = fixture.deps.clone();
    asset_deps.push(dep(code_point));
    let out = |tx: &Transaction, index| OutPoint::new(tx.id(network), index);
    let find = |point: &OutPoint| -> Result<Cell> {
        fixture
            .outputs
            .borrow()
            .get(point)
            .cloned()
            .or_else(|| fixture.spec.development_cells.iter().find(|(key, _)| key == point).map(|(_, cell)| cell.clone()))
            .context("missing signing Cell")
    };
    let signed = |inputs: &[OutPoint], outputs: Vec<Cell>, deps: &[CellDep]| -> Result<Transaction> {
        let input_cells = inputs.iter().map(find).collect::<Result<Vec<_>>>()?;
        let groups = derive_script_groups(&input_cells, &outputs)?;
        let tx = Transaction::new(
            deps.to_vec(),
            vec![],
            inputs.iter().cloned().map(|point| CellInput::new(point, None)).collect(),
            outputs,
            vec![],
        )?;
        let witnesses = groups
            .iter()
            .enumerate()
            .take_while(|(_, g)| g.kind == ScriptGroupKind::Lock)
            .map(|(index, group)| -> Result<_> {
                ensure!(group.script == fixture.lock || group.script == authority_lock, "unknown signer");
                let key = if group.script == fixture.lock { &fixture.key } else { &authority };
                let digest =
                    tx.signing_digest(network, index as u16, &group.script, find, |_| anyhow::bail!("unexpected Header Dep"))?;
                Ok(GroupWitness {
                    group_index: index as u16,
                    proof_data: Bytes::new(),
                    seal: Bytes::from(key.sign_recoverable(&digest.unpack())?.serialize()),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let tx = tx.with_witnesses(witnesses)?;
        fixture.outputs.borrow_mut().extend(tx.outputs().enumerate().map(|(i, cell)| (out(&tx, i as u32), cell)));
        Ok(tx)
    };
    let holdings = |inputs: &[OutPoint], units: &[(u128, Option<&Script>)], authority_return: bool| -> Result<Vec<Cell>> {
        let total = inputs.iter().map(|point| Ok(find(point)?.capacity())).sum::<Result<u64>>()?;
        let distributable = total - FEE - if authority_return { deposit.capacity() } else { 0 };
        let share = distributable / units.len() as u64;
        let mut outputs = units
            .iter()
            .map(|(quantity, policy)| {
                Cell::new(
                    share,
                    fixture.lock.clone(),
                    [Some(&asset), *policy].into_iter().flatten().cloned().collect(),
                    Bytes::copy_from_slice(&quantity.to_le_bytes()),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        if authority_return {
            outputs.push(deposit.clone());
        }
        Ok(outputs)
    };

    let input = [fixture.spend.clone()];
    let issue = signed(&input, holdings(&input, &[(70, None), (30, None)], false)?, &asset_deps)?;
    let issued_at = commit(&issue)?;
    let program = build_contract("--example", "polytype-obligation")?;
    let mut args = asset_id.as_slice().to_vec();
    args.extend_from_slice(authority_lock.calc_script_hash().as_slice());
    let policy =
        Script::new_builder().hash_type(ScriptHashType::Data).code_hash(blake2b_256(&program)).args(Bytes::from(args)).build();
    let deployment =
        signed(&[deploy_point], vec![Cell::new(deploy_fund - FEE, fixture.lock.clone(), vec![], program)?], &fixture.deps)?;
    let deployed_at = commit(&deployment)?;
    assert!(issued_at < deployed_at);
    let mut deps = asset_deps.clone();
    deps.push(dep(out(&deployment, 0)));
    let input = [out(&issue, 0)];
    let attach = signed(&input, holdings(&input, &[(70, Some(&policy))], false)?, &deps)?;
    commit(&attach)?;

    let input = [out(&attach, 0), out(&issue, 1)];
    let split_outputs = holdings(&input, &[(40, Some(&policy)), (30, Some(&policy)), (30, None)], false)?;
    let input_cells = input.iter().map(find).collect::<Result<Vec<_>>>()?;
    let groups = derive_script_groups(&input_cells, &split_outputs)?;
    let conservation = groups.iter().find(|group| group.script == asset).context("base asset group")?;
    assert_eq!((&conservation.inputs[..], &conservation.outputs[..]), (&[0, 1][..], &[0, 1, 2][..]));
    let obligation = groups.iter().find(|group| group.script == policy).context("attached Type group")?;
    assert_eq!((&obligation.inputs[..], &obligation.outputs[..]), (&[0][..], &[0, 1][..]));
    let split = signed(&input, split_outputs, &deps)?;
    commit(&split)?;
    let input = [out(&split, 0), out(&split, 1)];
    for attempted in [vec![(70, None)], vec![(1, Some(&policy)), (69, None)]] {
        let rejected = signed(&input, holdings(&input, &attempted, false)?, &deps)?;
        let error = clients[0].submit(&rejected).unwrap_err();
        assert!(format!("{error:#}").contains("exited with 64"));
    }
    let mut substituted = holdings(&input, &[(70, None), (1, Some(&policy))], false)?;
    substituted[1] = Cell::new(substituted[1].capacity(), fixture.lock.clone(), vec![policy.clone()], substituted[1].data())?;
    let rejected = signed(&input, substituted, &deps)?;
    assert!(format!("{:#}", clients[0].submit(&rejected).unwrap_err()).contains("exited with 63"));
    let merge = signed(&input, holdings(&input, &[(70, Some(&policy))], false)?, &deps)?;
    commit(&merge)?;
    let input = [out(&merge, 0), authority_point];
    let partial = signed(&input, holdings(&input, &[(45, Some(&policy)), (25, None)], true)?, &deps)?;
    let cells = input.iter().map(find).collect::<Result<Vec<_>>>()?;
    let outputs = partial.outputs().collect::<Vec<_>>();
    let groups = derive_script_groups(&cells, &outputs)?;
    for required in [&fixture.lock, &authority_lock] {
        let unsigned = partial
            .with_witnesses(partial.witnesses().filter(|w| &groups[usize::from(w.group_index)].script != required).collect())?;
        assert!(clients[0].submit(&unsigned).is_err());
    }
    commit(&partial)?;
    let input = [out(&partial, 0), out(&partial, 1), out(&split, 2), out(&partial, 2)];
    let release = signed(&input, holdings(&input, &[(100, None)], true)?, &deps)?;
    let released_at = commit(&release)?;
    let output = release.outputs().next().context("released asset")?;
    assert_eq!(output.type_scripts().collect::<Vec<_>>(), vec![asset]);
    assert_eq!(output.data().as_ref(), &100u128.to_le_bytes());
    if let Some(root) = std::env::var_os("STEM_TEST_EVIDENCE_ROOT") {
        let root = Path::new(&root);
        fs::create_dir_all(root)?;
        fs::write(
            root.join("polytype-late-attachment.json"),
            serde_json::to_vec_pretty(&json!({
                "network": hex::encode(network.as_slice()), "genesis": hex::encode(archive.header(0)?.encode()),
                "validators": members.iter().map(|m| json!({"key": hex::encode(m.key), "power": m.power})).collect::<Vec<_>>(),
                "base_asset": hex::encode(asset_id.as_slice()), "policy": hex::encode(policy.calc_script_hash().as_slice()),
                "split_asset_inputs": conservation.inputs, "split_asset_outputs": conservation.outputs,
                "split_policy_inputs": obligation.inputs, "split_policy_outputs": obligation.outputs,
                "executions": evidence,
            }))?,
        )?;
    }
    println!(
        "Four replicas verified issue at {issued_at}, later policy deployment at {deployed_at}, and full discharge at {released_at}"
    );
    Ok(())
}
