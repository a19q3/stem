//! Run the signed payment benchmark against an operator-started Jolteon fixture.
use std::{
    collections::{HashMap, HashSet},
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use ckb_types::{packed::Byte32, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::json;
use stem::jolteon::Client;
use stem_ledger::{
    Transaction,
    block::{Header, Receipt},
    light::FinalityVerifier,
    proof::{InclusionProof, Tree},
};
use stem_stake::Member;

use super::{Fixture, Load, statuses};
use crate::{Retention, Store, comet::write_spec};

#[derive(Deserialize)]
struct Peer {
    key: String,
    address: SocketAddr,
}

#[derive(Deserialize)]
pub(super) struct Topology {
    peers: Vec<Peer>,
    proposal_interval_ms: u64,
    vm_workers: Option<usize>,
}

impl Topology {
    pub(super) fn load(path: &Path, fixture: &mut Fixture, load: Load) -> Result<Self> {
        let config: Self = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            config.peers.len() == load.replicas,
            "replica count differs from topology"
        );
        ensure!(
            config.proposal_interval_ms == load.cadence_ms
                && config.vm_workers.unwrap_or(2) == load.vm_workers,
            "cadence or VM worker setting differs from topology"
        );
        let mut addresses = HashSet::new();
        let mut committee = Vec::new();
        for peer in &config.peers {
            let bytes = hex::decode(peer.key.trim_start_matches("0x"))?;
            let key: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("expected a 32-byte validator key"))?;
            ensure!(
                committee.last().is_none_or(|(last, _)| last < &key)
                    && addresses.insert(peer.address),
                "expected distinct addresses and strictly ordered validator keys"
            );
            committee.push((key, 1));
        }
        fixture.spec.development_validators = committee;
        Ok(config)
    }

    pub(super) fn prepare(&self, fixture: &Fixture, directory: PathBuf) -> Result<()> {
        fs::create_dir(&directory).context("create a new genesis output directory")?;
        write_spec(&directory.join("genesis.json"), &fixture.spec)?;
        let genesis = local_genesis(fixture)?;
        fs::write(
            directory.join("genesis-id.json"),
            serde_json::to_vec_pretty(&json!({
                "network": hex::encode(fixture.spec.network.as_slice()), "genesis_id": hex::encode(genesis.as_slice())
            }))?,
        )?;
        println!("Prepared funded payment genesis in {}", directory.display());
        Ok(())
    }

    pub(super) fn connect(&self, fixture: &Fixture) -> Result<Vec<Client>> {
        let genesis = local_genesis(fixture)?;
        let verifier = finality_verifier(fixture, genesis.clone())?;
        let clients: Vec<_> = self
            .peers
            .iter()
            .map(|p| Client::new(p.address, Duration::from_secs(4)))
            .collect();
        let states = statuses(&clients)?;
        let height = states
            .iter()
            .map(|s| s.height)
            .min()
            .context("empty committee")?;
        ensure!(
            height >= 3,
            "wait for three committed heights before the payment trial"
        );
        ensure!(
            states.last().is_some_and(|s| s.history_window.is_none()),
            "last endpoint must be an archive"
        );
        let archive = clients.last().context("no archive")?;
        ensure!(
            archive.header(0)?.id(&fixture.spec.network) == genesis,
            "unexpected payment fixture genesis"
        );
        let agreed = archive.header(height)?;
        ensure!(
            agreed.context.height == height,
            "archive sent a different preflight height"
        );
        for (index, (client, status)) in clients.iter().zip(states).enumerate() {
            ensure!(
                status.member == Some(index.try_into()?)
                    && !status.observer_only
                    && !status.catching_up,
                "endpoint {index} is not the expected active committee member"
            );
            let certificate = client.finality(height)?;
            let header = client.header(height)?;
            ensure!(
                header == agreed && verifier.verify(&certificate.proof, false)?.header() == &agreed,
                "endpoint {index} returned a different finalized Header"
            );
        }
        Ok(clients)
    }

    pub(super) fn addresses(&self) -> Vec<SocketAddr> {
        self.peers.iter().map(|peer| peer.address).collect()
    }
}

fn local_genesis(fixture: &Fixture) -> Result<Byte32> {
    let db = tempfile::tempdir()?;
    Ok(Store::open(db.path(), &fixture.spec, Retention::Archive)?
        .genesis_id()
        .clone())
}

fn finality_verifier(fixture: &Fixture, genesis: Byte32) -> Result<FinalityVerifier> {
    let members = fixture
        .spec
        .development_validators
        .iter()
        .map(|(key, power)| Member {
            key: *key,
            power: *power as u64,
        })
        .collect::<Vec<_>>();
    FinalityVerifier::new(fixture.spec.network.clone(), genesis, &members)
}

pub(super) fn check_block(
    network: &Byte32,
    header: &Header,
    height: u64,
    parent: &Byte32,
    ids: &[String],
    jobs: &HashMap<&str, &Transaction>,
) -> Result<()> {
    ensure!(
        header.context.height == height && &header.context.parent == parent,
        "archive Header does not extend the measured history at height {height}"
    );
    let txs = ids
        .iter()
        .map(|id| {
            jobs.get(id.as_str())
                .copied()
                .cloned()
                .context("archive contains a transaction outside the payment workload")
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        header.context.transaction_root == Tree::transactions(network.clone(), &txs)?.root(),
        "signed payments differ from the archive transaction commitment at height {height}"
    );
    Ok(())
}

#[derive(Deserialize)]
struct Height {
    height: u64,
}

#[derive(Deserialize)]
struct Snapshot {
    #[serde(default)]
    at_us: u64,
    statuses: Vec<Height>,
}

impl Snapshot {
    fn common_height(&self, replicas: usize) -> Result<u64> {
        ensure!(
            self.statuses.len() == replicas,
            "incomplete replica height observation"
        );
        self.statuses
            .iter()
            .map(|s| s.height)
            .min()
            .context("no observed validators")
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct SampleExecution {
    transaction: String,
    receipt: String,
    proof: String,
}

impl SampleExecution {
    pub(super) fn verify(&self, network: &Byte32, header: &Header, ids: &[String]) -> Result<()> {
        let tx = Transaction::from_slice(&hex::decode(&self.transaction)?)?;
        let receipt = Receipt::decode(&hex::decode(&self.receipt)?)?;
        let raw = hex::decode(self.proof.strip_prefix("0x").unwrap_or(&self.proof))?;
        let (payment, execution) = InclusionProof::decode_execution_pair(&raw)?;
        ensure!(
            payment.count as usize == ids.len()
                && ids.get(payment.position as usize)
                    == Some(&hex::encode(tx.id(network).as_slice()))
                && payment.verify_execution(
                    network,
                    &header.execution(),
                    &tx,
                    &execution,
                    &receipt
                ),
            "sampled execution differs from the certified block"
        );
        Ok(())
    }
}

#[derive(Deserialize)]
struct Block {
    seen_us: u64,
    header: String,
    ids: Option<Vec<String>>,
    positions: Option<Vec<usize>>,
    #[serde(default)]
    receipts: Vec<SampleExecution>,
}

impl Block {
    fn payment_ids(&self, jobs: &[(String, Transaction)]) -> Result<Vec<String>> {
        match (&self.ids, &self.positions) {
            (Some(ids), None) => Ok(ids.clone()),
            (None, Some(positions)) => positions
                .iter()
                .map(|&i| {
                    Ok(jobs
                        .get(i)
                        .context("payment position exceeds workload")?
                        .0
                        .clone())
                })
                .collect(),
            _ => anyhow::bail!("expected one representation of the block payment list"),
        }
    }
}

#[derive(Deserialize)]
struct Summary {
    planned: usize,
    reused_connections: Option<usize>,
    offered: usize,
    admitted: usize,
    offered_during_window: usize,
    admitted_during_offer: usize,
    visible_without_ack: usize,
    visibility_tps: f64,
    visible: usize,
    visible_during_offer: usize,
}

#[derive(Deserialize)]
struct Delivery {
    id: String,
    offered_us: Option<u64>,
    acknowledged_us: Option<u64>,
    endpoint: Option<usize>,
}

#[derive(Deserialize)]
struct DispatchSettings {
    rate: usize,
    seconds: u64,
    drain: u64,
    senders: usize,
    #[serde(default)]
    reuse_submissions: bool,
    #[serde(default)]
    ready_senders: bool,
    #[serde(default)]
    shard_senders: bool,
    #[serde(default)]
    pause_on_full: bool,
    #[serde(default)]
    payment_type_groups: usize,
}

#[derive(Deserialize)]
struct Evidence {
    deliveries: Option<Vec<Delivery>>,
    delivery_times: Option<Vec<[Option<u64>; 2]>>,
    delivery_routes: Option<Vec<Option<usize>>>,
    submission_connections: Option<Vec<Option<bool>>>,
    before: Snapshot,
    samples: Vec<Snapshot>,
    blocks: Vec<Block>,
    summary: Summary,
    load: DispatchSettings,
    network: String,
    workload_id: String,
    start_header: String,
    header: String,
    agreed_height: u64,
    finality: String,
}

pub(super) fn verify_report(
    path: &Path,
    fixture: &Fixture,
    jobs: &[(String, Transaction)],
    workload: &str,
    load: Load,
) -> Result<()> {
    let report: Evidence = serde_json::from_reader(std::io::BufReader::new(fs::File::open(path)?))?;
    let settings = &report.load;
    ensure!(
        settings.rate == load.rate
            && settings.seconds == load.seconds
            && settings.drain == load.drain
            && settings.senders == load.senders
            && settings.reuse_submissions == load.reuse_submissions
            && settings.ready_senders == load.ready_senders
            && settings.shard_senders == load.shard_senders
            && settings.pause_on_full == load.pause_on_full
            && settings.payment_type_groups == load.payment_type_groups,
        "report dispatch settings differ from prepared trial"
    );
    let network = &fixture.spec.network;
    ensure!(
        report.workload_id == workload && hex::decode(&report.network)? == network.as_slice(),
        "report describes a different network or signed workload"
    );
    let decode = |raw: &str| Header::decode(&hex::decode(raw)?);
    let mut head = decode(&report.start_header)?;
    ensure!(
        head.context.height == report.before.common_height(load.replicas)?,
        "wrong first observation"
    );
    let by_id = jobs.iter().map(|(id, tx)| (id.as_str(), tx)).collect();
    let mut blocks = report.blocks.iter();
    let mut seen = HashSet::new();
    let mut window = 0;
    let mut elapsed = 0;
    for sample in &report.samples {
        let common = sample.common_height(load.replicas)?;
        ensure!(
            sample.at_us >= elapsed && common >= head.context.height,
            "nonmonotonic observation"
        );
        elapsed = sample.at_us;
        while head.context.height < common {
            let block = blocks.next().context("missing measured block")?;
            ensure!(
                block.seen_us == elapsed,
                "block time differs from common-height observation"
            );
            let next = decode(&block.header)?;
            let ids = block.payment_ids(jobs)?;
            check_block(
                network,
                &next,
                head.context.height + 1,
                &head.id(network),
                &ids,
                &by_id,
            )?;
            for receipt in &block.receipts {
                receipt.verify(network, &next, &ids)?;
            }
            if elapsed <= load.seconds * 1_000_000 {
                window += ids.len();
            }
            for id in ids {
                ensure!(seen.insert(id), "repeated confirmed payment");
            }
            head = next;
        }
    }
    ensure!(
        blocks.next().is_none()
            && !seen.is_empty()
            && head == decode(&report.header)?
            && head.context.height == report.agreed_height,
        "incomplete chain to certified finality"
    );
    let verifier = finality_verifier(fixture, local_genesis(fixture)?)?;
    ensure!(
        verifier
            .verify(&hex::decode(&report.finality)?, false)?
            .header()
            == &head,
        "finality proof differs from measured chain"
    );
    ensure!(
        seen.len() == report.summary.visible && window == report.summary.visible_during_offer,
        "reported confirmation counts differ from observed blocks"
    );
    let check_route = |endpoint: Option<usize>, write: Option<u64>| {
        ensure!(
            endpoint.is_some() == (load.ready_senders && write.is_some())
                && endpoint.is_none_or(|node| node < load.replicas),
            "missing or invalid payment endpoint"
        );
        Ok(())
    };
    let times: Vec<_> = match (report.deliveries, report.delivery_times) {
        (Some(rows), None) => {
            ensure!(
                report.delivery_routes.is_none(),
                "duplicate payment endpoint trace"
            );
            ensure!(rows.len() == jobs.len(), "incomplete dispatch trace");
            rows.into_iter()
                .zip(jobs)
                .map(|(r, (id, _))| {
                    ensure!(&r.id == id, "dispatch order differs from signed workload");
                    check_route(r.endpoint, r.offered_us)?;
                    Ok([r.offered_us, r.acknowledged_us])
                })
                .collect::<Result<_>>()?
        }
        (None, Some(times)) => {
            ensure!(
                report.delivery_routes.is_some() == load.ready_senders,
                "wrong endpoint trace for dispatch mode"
            );
            if let Some(routes) = report.delivery_routes {
                ensure!(
                    routes.len() == times.len(),
                    "incomplete payment endpoint trace"
                );
                for (endpoint, [write, _]) in routes.into_iter().zip(&times) {
                    check_route(endpoint, *write)?;
                }
            }
            times
        }
        _ => anyhow::bail!("expected one dispatch trace"),
    };
    ensure!(
        times.len() == jobs.len() && report.summary.planned == jobs.len(),
        "incomplete dispatch trace"
    );
    if let Some(connections) = report.submission_connections {
        ensure!(
            connections.len() == times.len(),
            "incomplete submission connection trace"
        );
        let mut reused = 0;
        for (connection, [write, _]) in connections.into_iter().zip(&times) {
            ensure!(
                (write.is_none() || connection.is_some())
                    && (connection != Some(true) || load.reuse_submissions),
                "invalid submission connection"
            );
            reused += usize::from(connection == Some(true));
        }
        ensure!(
            Some(reused) == report.summary.reused_connections,
            "connection trace differs from reported reuse count"
        );
    }
    let mut offered = 0;
    let mut admitted = 0;
    let mut writes_in_window = 0;
    let mut acks_in_window = 0;
    let mut visible_without_ack = 0;
    for ((id, _), [write, ack]) in jobs.iter().zip(times) {
        ensure!(
            ack.is_none_or(|at| write.is_some_and(|w| w <= at)),
            "acknowledgement precedes dispatch"
        );
        let confirmed = seen.contains(id);
        ensure!(
            !confirmed || write.is_some(),
            "confirmed payment lacks a recorded write"
        );
        offered += usize::from(write.is_some());
        admitted += usize::from(ack.is_some());
        writes_in_window += usize::from(write.is_some_and(|t| t <= load.seconds * 1_000_000));
        acks_in_window += usize::from(ack.is_some_and(|t| t <= load.seconds * 1_000_000));
        visible_without_ack += usize::from(confirmed && ack.is_none());
    }
    let s = report.summary;
    ensure!(
        (
            offered,
            admitted,
            writes_in_window,
            acks_in_window,
            visible_without_ack
        ) == (
            s.offered,
            s.admitted,
            s.offered_during_window,
            s.admitted_during_offer,
            s.visible_without_ack
        ),
        "submission counts differ from recorded writes and acknowledgements"
    );
    let tps = window as f64 / load.seconds as f64;
    ensure!(
        (s.visibility_tps - tps).abs() < 1e-8,
        "reported window TPS differs from verified count"
    );
    println!(
        "Verified {offered} writes, {admitted} acknowledgements, {} finalized payments, {window} in the offer window ({tps:.2} TPS), at certified height {}",
        seen.len(),
        head.context.height
    );
    Ok(())
}
