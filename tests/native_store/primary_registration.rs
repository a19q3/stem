//! Exercise signed registration in the native VM over three live loopback networks.

use std::{fs, path::Path, process::Command, time::Duration};

use ckb_hash::new_blake2b;
use ckb_types::{core::ScriptHashType, packed::Script, prelude::*};
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use stem::jolteon::Client;
use stem_ledger::{
    block::Header,
    light::FinalityVerifier,
    patricia::{Namespace, Patricia, Proof},
};
use stem_stake::Member;

use super::{
    Bytes, Cell, CellDep, CellInput, Context, FEE, FUND, Fixture, OutPoint, Result, Transaction,
    comet::{Process, poll},
    keeper_load::{NetworkOptions, execution, replica_binary, start_network_with_committee, transactions, wait_common},
    record,
};

const WAIT: Duration = Duration::from_secs(25);

struct Network {
    fixture: Fixture,
    processes: Vec<Process>,
    clients: Vec<Client>,
    home: std::path::PathBuf,
}

impl Network {
    fn start(mut fixture: Fixture, home: &Path, id: u8, key_seed: u8) -> Result<Self> {
        fs::create_dir(home)?;
        fixture.spec.network = [id; 32].pack();
        fixture.spec.chain_id = format!("stem-primary-registration-{id}");
        let committee = [1, 2, 1, 3]
            .into_iter()
            .enumerate()
            .map(|(position, weight)| (SigningKey::from_bytes(&[key_seed + position as u8; 32]), weight))
            .collect();
        let (processes, clients) = start_network_with_committee(
            &mut fixture,
            home,
            committee,
            NetworkOptions { interval_ms: 150, timeout_ms: 2400, vm_workers: 2, history_window: None, storage_metrics: false },
        )?;
        Ok(Self { fixture, processes, clients, home: home.to_owned() })
    }

    fn archive(&self) -> &Client {
        self.clients.last().expect("four-replica network")
    }

    fn members(&self) -> Vec<Member> {
        self.fixture
            .spec
            .development_validators
            .iter()
            .map(|&(key, power)| Member { key, power: u64::try_from(power).expect("positive fixture power") })
            .collect()
    }

    fn restart_archive(&mut self) -> Result<()> {
        drop(self.processes.pop().context("missing primary archive process")?);
        let mut cmd = Command::new(replica_binary());
        cmd.args(["run", "--archive", "--home"])
            .arg(self.home.join("node-3"))
            .arg("--configuration")
            .arg(self.home.join("replicas.json"))
            .arg("--genesis")
            .arg(self.home.join("genesis.json"))
            .arg("--key-file")
            .arg(self.home.join("validator-3.key"));
        self.processes.push(Process::spawn(&mut cmd, &self.home.join("archive-restarted.log"))?);
        Ok(())
    }
}

fn registration(
    foreign: &Network,
    primary_domain: &[u8],
    deployment: u8,
    registrar: &SigningKey,
    manager: &SigningKey,
    proof: &[u8],
) -> Result<Vec<u8>> {
    let network = &foreign.fixture.spec.network;
    let genesis = foreign.archive().header(0)?.id(network);
    let checkpoint = foreign.archive().header(1)?.id(network);
    let fixed = [network.as_slice(), &[deployment; 32], genesis.as_slice(), manager.verifying_key().as_bytes(), checkpoint.as_slice()]
        .concat();
    let members = foreign.members();
    let mut roster = Vec::new();
    for member in &members {
        roster.extend_from_slice(&member.key);
        roster.extend_from_slice(&member.power.to_le_bytes());
    }
    let mut digest = new_blake2b();
    digest.update(b"Stem Primary native registration v1\0");
    digest.update(primary_domain);
    digest.update(&fixed);
    digest.update(&roster);
    let mut hash = [0; 32];
    digest.finalize(&mut hash);
    let mut record = fixed;
    record.push(u8::try_from(members.len())?);
    record.extend_from_slice(&roster);
    record.extend_from_slice(&registrar.sign(&hash).to_bytes());
    record.extend_from_slice(&manager.sign(&hash).to_bytes());
    record.extend_from_slice(proof);
    Ok([u16::try_from(record.len())?.to_le_bytes().as_slice(), record.as_slice()].concat())
}

fn registry_tx(primary: &Network, input: OutPoint, registry_type: &Script, capacity: u64, data: Vec<u8>) -> Result<Transaction> {
    let f = &primary.fixture;
    f.seal(Transaction::new(
        f.deps.clone(),
        vec![],
        vec![CellInput::new(input, None)],
        vec![Cell::new(capacity, f.lock.clone(), vec![registry_type.clone()], Bytes::from(data))?],
        vec![],
    )?)
}

struct RegistryProof {
    header: Header,
    path_bytes: usize,
    execution_height: u64,
    cycles: u64,
}

fn prove_registry(primary: &Network, point: &OutPoint, expected: &Cell) -> Result<RegistryProof> {
    let archive = primary.archive();
    let response = poll("finalized registry Cell", WAIT, || {
        let response = archive.query("/stem/cells", 0, point.as_slice(), true)?;
        let rows: Vec<Value> = serde_json::from_slice(&response.value)?;
        anyhow::ensure!(rows.len() == 1, "registry response shape");
        let Some(cell) = rows[0]["cell"].as_str() else { return Ok(None) };
        let proof = rows[0]["proof"].as_str().context("missing registry Cell proof")?;
        Ok(Some((response.height, hex::decode(cell.trim_start_matches("0x"))?, hex::decode(proof.trim_start_matches("0x"))?)))
    })?;
    let (height, encoded, proof_bytes) = response;
    let network = &primary.fixture.spec.network;
    let genesis = archive.header(0)?.id(network);
    let verifier = FinalityVerifier::new(network.clone(), genesis, &primary.members())?;
    let certificate = archive.finality(height)?;
    let verified = verifier.verify(&certificate.proof, certificate.handoff)?;
    let (stored, record) = record::decode_cell(&encoded)?;
    assert_eq!(&stored, point);
    assert_eq!(record.cell.as_slice(), expected.as_slice());
    let proof = Proof::decode(&proof_bytes)?;
    let header = verified.header();
    assert_eq!(header.context.height, height);
    let tree = Patricia::new(network.clone(), Namespace::Cells);
    assert!(record::verify_cell(&tree, &header.state.cell_root.unpack(), point, Some(&record), &proof));
    wait_common(&primary.clients, height)?;
    for client in &primary.clients {
        assert_eq!(&client.header(height)?, header);
    }
    let execution_height = record.origin.height;
    let origin_certificate = archive.finality(execution_height)?;
    let origin = verifier.verify(&origin_certificate.proof, origin_certificate.handoff)?;
    assert_eq!(origin.header(), &archive.header(execution_height)?);
    assert_eq!(origin.header().context.id(network), record.origin.context_id);
    let id = hex::encode(point.tx_hash().as_slice());
    let index = transactions(archive, execution_height)?
        .iter()
        .position(|included| included == &id)
        .context("registry transaction missing from its certified block")?;
    let (receipt, _) = execution(archive, &primary.fixture, execution_height, index, &id)?;
    Ok(RegistryProof { header: header.clone(), path_bytes: proof_bytes.len(), execution_height, cycles: receipt.cycles })
}

fn rejected(primary: &Network, tx: &Transaction) -> Result<()> {
    for client in &primary.clients {
        let Err(error) = client.submit(tx) else { anyhow::bail!("replica admitted invalid registration") };
        anyhow::ensure!(format!("{error:#}").ends_with(" exited with 121"), "expected registry Type rejection: {error:#}");
    }
    Ok(())
}

#[test]
#[ignore = "three live loopback networks; set STEM_PRIMARY_REGISTRATION_CONTRACT to the raw RISC-V example"]
fn native_primary_executes_signed_registrations_from_two_live_chains() -> Result<()> {
    let output = tempfile::tempdir()?;
    let root = output.path();
    let source = Network::start(Fixture::new()?, &root.join("source"), 10, 101)?;
    let destination = Network::start(Fixture::new()?, &root.join("destination"), 30, 109)?;
    let mut fixture = Fixture::new()?;
    let code =
        Bytes::from(fs::read(std::env::var_os("STEM_PRIMARY_REGISTRATION_CONTRACT").context("STEM_PRIMARY_REGISTRATION_CONTRACT")?)?);
    let program = OutPoint::new([91; 32].pack(), 0);
    let occupied = Cell::minimum_capacity(&fixture.lock, &[], code.len())?;
    fixture.spec.development_cells.push((program.clone(), Cell::new(occupied, fixture.lock.clone(), vec![], code.clone())?));
    fixture.deps.push(CellDep::new_builder().out_point(program).build());
    let mut primary = Network::start(fixture, &root.join("primary"), 20, 105)?;
    let seed = primary.fixture.spend.clone();
    let network = &primary.fixture.spec.network;
    let primary_genesis = primary.archive().header(0)?.id(network);
    let domain = [network.as_slice(), primary_genesis.as_slice()].concat();
    let registrar = SigningKey::from_bytes(&[90; 32]);
    let source_manager = SigningKey::from_bytes(&[91; 32]);
    let destination_manager = SigningKey::from_bytes(&[92; 32]);
    let args = [domain.as_slice(), registrar.verifying_key().as_bytes(), seed.as_slice()].concat();
    let registry_type =
        Script::new_builder().code_hash(ckb_hash::blake2b_256(&code)).hash_type(ScriptHashType::Data).args(Bytes::from(args)).build();
    let source_proof = source.archive().finality(1)?.proof;
    let destination_proof = destination.archive().finality(1)?.proof;
    let source_record = registration(&source, &domain, 12, &registrar, &source_manager, &source_proof)?;
    let destination_record = registration(&destination, &domain, 15, &registrar, &destination_manager, &destination_proof)?;

    let mut wrong_primary = domain.clone();
    wrong_primary[32] ^= 1;
    let replay = registration(&source, &wrong_primary, 12, &registrar, &source_manager, &source_proof)?;
    rejected(&primary, &registry_tx(&primary, seed.clone(), &registry_type, FUND - FEE, replay)?)?;
    let mut wrong_network = domain.clone();
    wrong_network[0] ^= 1;
    let foreign_type = registry_type
        .clone()
        .as_builder()
        .args(Bytes::from([wrong_network.as_slice(), registrar.verifying_key().as_bytes(), seed.as_slice()].concat()))
        .build();
    let foreign_registration = registration(&source, &wrong_network, 12, &registrar, &source_manager, &source_proof)?;
    rejected(&primary, &registry_tx(&primary, seed.clone(), &foreign_type, FUND - FEE, foreign_registration)?)?;
    let wrong_proof = registration(&source, &domain, 12, &registrar, &source_manager, &destination_proof)?;
    rejected(&primary, &registry_tx(&primary, seed.clone(), &registry_type, FUND - FEE, wrong_proof)?)?;
    let mut forged = source_record.clone();
    forged[2 + 160 + 1 + source.members().len() * 40 + 64] ^= 1;
    rejected(&primary, &registry_tx(&primary, seed.clone(), &registry_type, FUND - FEE, forged)?)?;

    let first = registry_tx(&primary, seed, &registry_type, FUND - FEE, source_record.clone())?;
    primary.clients[0].submit(&first)?;
    let first_point = OutPoint::new(first.id(network), 0);
    let first_cell = first.outputs().next().context("first registry output")?;
    let first_proof = prove_registry(&primary, &first_point, &first_cell)?;
    rejected(&primary, &primary.fixture.signed(first_point.clone(), None, FUND - 2 * FEE, None)?)?;
    let duplicate = [source_record.as_slice(), source_record.as_slice()].concat();
    rejected(&primary, &registry_tx(&primary, first_point.clone(), &registry_type, FUND - 2 * FEE, duplicate)?)?;
    let rewritten = registry_tx(&primary, first_point.clone(), &registry_type, FUND - 2 * FEE, destination_record.clone())?;
    rejected(&primary, &rewritten)?;

    let complete = [source_record.as_slice(), destination_record.as_slice()].concat();
    let second = registry_tx(&primary, first_point, &registry_type, FUND - 2 * FEE, complete)?;
    primary.clients[0].submit(&second)?;
    let point = OutPoint::new(second.id(network), 0);
    let committed = second.outputs().next().context("second registry output")?;
    let second_proof = prove_registry(&primary, &point, &committed)?;
    assert!(second_proof.execution_height > first_proof.execution_height);
    primary.restart_archive()?;
    wait_common(&primary.clients, second_proof.header.context.height + 1)?;
    let recovered = prove_registry(&primary, &point, &committed)?;
    assert!(recovered.header.context.height > second_proof.header.context.height);
    assert_eq!(recovered.execution_height, second_proof.execution_height);
    assert_eq!(recovered.cycles, second_proof.cycles);
    let evidence = json!({
        "source_initial_certificate_bytes": source_proof.len(),
        "destination_initial_certificate_bytes": destination_proof.len(),
        "registry_record_bytes": committed.data().len(),
        "registered_heights": [first_proof.execution_height, second_proof.execution_height],
        "recovered_height": recovered.header.context.height,
        "patricia_path_bytes": [first_proof.path_bytes, second_proof.path_bytes, recovered.path_bytes],
        "receipt_cycles": [first_proof.cycles, second_proof.cycles],
        "registry_type": hex::encode(registry_type.calc_script_hash().as_slice()),
    });
    println!("{evidence}");
    Ok(())
}
