//! A weighted Jolteon replica using the native execution and storage paths.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    net::SocketAddr,
    ops::Range,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ckb_types::prelude::*;
use ed25519_dalek::{Signer, SigningKey};
use prost::Message;
use serde::Deserialize;
use serde_json::json;
use stem_ledger::{Transaction, block::Header};
use stem_stake::{
    Hash32, Member,
    jolteon::{
        Committee, CommitteeChange, FinalityProof, HandoffProof, MAX_CLOCK_LEAD_MS, MAX_MEMBERS,
        MAX_PROPOSAL_AGE_MS, Point, Quorum, Share, TimeoutCertificate, TimeoutVote,
    },
};

use crate::native_store::{Prepared, Store};

#[cfg(feature = "jolteon-admission-pipeline")]
mod admission_pipeline;
#[cfg(feature = "jolteon-candidate-pipeline")]
mod candidate_pipeline;
#[cfg(feature = "jolteon-certificate-experiment")]
mod certificate_experiment;
mod client;
#[cfg(feature = "jolteon-commit-pipeline")]
mod commit_pipeline;
mod mempool;
mod peer_discovery;
#[cfg(feature = "jolteon-proposal-pipeline")]
mod proposal_pipeline;
mod recovery;
pub mod snapshot;
mod transport;
mod wire;

use mempool::Mempool;
use recovery::{BranchHorizon, Recovery};
use transport::{Incoming, Network};

pub use client::LatencyHistogram as AdmissionLatency;
pub use client::{
    AdmissionMetrics, AdmissionPipelineMetrics, Certificate, CertificateExperimentMetrics, Client,
    ExecutionMetrics, FrameReuseMetrics, LatencyHistogram, QueryResult, Status, SubmissionProgress,
};

const STATE: &[u8] = b"state";
const BODY: &[u8] = b"body";
const MAX_BRANCH: usize = 192;
const MAX_PENDING: usize = 512;
const MAX_MEMPOOL_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROPOSAL_BYTES: u64 = 16 * 1024 * 1024;
const RELAY_RETRIES_PER_TICK: usize = 8;
const MAX_VERIFICATION_CACHE_ENTRIES: usize = 131_072;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Static peer directory in ascending Ed25519 public-key order.
    pub peers: Vec<Peer>,
    /// Enable signed peer transport and announce this node's listening endpoint.
    #[serde(default)]
    pub discovery_address: Option<SocketAddr>,
    pub view_timeout_ms: u64,
    pub proposal_interval_ms: u64,
    #[serde(default = "worker_count")]
    pub vm_workers: usize,
    /// Local successful script verifications retained by the LRU.
    #[serde(default)]
    pub verification_cache_entries: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    key: String,
    address: SocketAddr,
}

struct Directory {
    keys: Vec<Hash32>,
    own: u16,
    local_key: Hash32,
    discovery: Option<peer_discovery::Discovery>,
}

impl Directory {
    fn id(&self, key: &Hash32) -> Option<u16> {
        if key == &self.local_key {
            return Some(self.own);
        }
        self.keys
            .binary_search(key)
            .ok()
            .map(|index| index as u16)
            .or_else(|| self.discovery.as_ref()?.id(key))
    }

    fn routes(&self, members: &[Member]) -> Vec<Option<u16>> {
        members.iter().map(|member| self.id(&member.key)).collect()
    }

    fn member(&self, members: &[Member]) -> Option<u16> {
        members
            .binary_search_by_key(&self.local_key, |member| member.key)
            .ok()
            .map(|index| index as u16)
    }

    fn slots(&self) -> usize {
        self.discovery
            .as_ref()
            .map_or(self.keys.len(), peer_discovery::Discovery::slots)
    }

    fn sender(&self, event: &Incoming) -> Option<u16> {
        if let Some(key) = &event.peer_key {
            return self.id(key).filter(|id| *id != self.own);
        }
        let index = u16::try_from(event.frame.sender).ok()?;
        (usize::from(index) < self.keys.len()).then_some(index)
    }
}

fn worker_count() -> usize {
    2
}

#[derive(Debug)]
struct StorageFault;

impl std::fmt::Display for StorageFault {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.write_str("Jolteon consensus persistence failed")
    }
}

impl std::error::Error for StorageFault {}

struct Checked {
    wire: wire::Proposal,
    header: Header,
    parent: Quorum,
}

struct Candidate {
    proposal: Checked,
    execution: Option<Arc<Prepared>>,
    #[cfg(feature = "jolteon-candidate-pipeline")]
    execution_error: Option<String>,
    source: u16,
}

struct BranchExecution {
    batches: Vec<Arc<Prepared>>,
    pending: Option<CommitteeChange>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Query {
    path: String,
    #[serde(default)]
    height: u64,
    #[serde(default)]
    prove: bool,
    data: Option<ckb_jsonrpc_types::JsonBytes>,
}

struct Replica {
    store: Store,
    committee: Committee,
    member: Option<u16>,
    directory: Directory,
    routes: Vec<Option<u16>>,
    key: SigningKey,
    net: Network,
    #[cfg(not(all(
        feature = "jolteon-candidate-pipeline",
        feature = "jolteon-proposal-pipeline"
    )))]
    workers: Arc<rayon::ThreadPool>,
    #[cfg(feature = "jolteon-candidate-pipeline")]
    candidate_pipeline: candidate_pipeline::Pipeline,
    #[cfg(feature = "jolteon-commit-pipeline")]
    commit_writer: commit_pipeline::Writer,
    #[cfg(feature = "jolteon-proposal-pipeline")]
    proposal_pipeline: proposal_pipeline::Pipeline,
    journal: wire::Journal,
    high: Quorum,
    certificate: Option<TimeoutCertificate>,
    finalized: Point,
    view: u64,
    since: Instant,
    timeout: Duration,
    proposal_interval: Duration,
    backoff: u32,
    announced: Instant,
    handoff_requested: Option<Instant>,
    handoff_tip: Option<Quorum>,
    pending: Option<CommitteeChange>,
    candidates: BTreeMap<Point, Candidate>,
    preferred: BTreeMap<u64, Point>,
    votes: BTreeMap<Point, BTreeMap<u16, [u8; 64]>>,
    observed_votes: HashMap<(u64, u16), (Point, Share)>,
    timeouts: BTreeMap<u64, BTreeMap<u16, (Quorum, Share)>>,
    certificates: HashSet<Hash32>,
    #[cfg(feature = "jolteon-certificate-experiment")]
    certificate_experiment: certificate_experiment::Cache,
    requests: HashMap<Point, Instant>,
    mempool: Mempool,
    admission: AdmissionMetrics,
    #[cfg(feature = "jolteon-admission-pipeline")]
    pipeline: admission_pipeline::Pipeline,
    execution: ExecutionMetrics,
    rejected: u64,
    timeout_views: VecDeque<u64>,
    last_untimely_view: u64,
    recovery: Recovery,
}

pub fn run(store: Store, key: SigningKey, config: Config, initialize: bool) -> Result<()> {
    if let Some(entries) = config.verification_cache_entries {
        ensure!(
            (1..=MAX_VERIFICATION_CACHE_ENTRIES).contains(&entries),
            "invalid verification cache entry limit"
        );
        store.resize_verification_cache(entries);
    }
    let genesis_members = members(&store)?;
    let genesis_committee = Committee::new(
        store.network().unpack(),
        store.genesis_id().unpack(),
        0,
        &genesis_members,
    )?;
    ensure!(
        store
            .stake_committee_limit()
            .is_none_or(|limit| limit <= MAX_MEMBERS),
        "Stake Registry committee limit exceeds Jolteon capacity"
    );
    let public = key.verifying_key().to_bytes();
    let history_window = store.history_window().unwrap_or_default();
    let keys = config
        .peers
        .iter()
        .map(|peer| -> Result<Hash32> {
            let key = hex::decode(peer.key.strip_prefix("0x").unwrap_or(&peer.key))?;
            key.try_into()
                .map_err(|_| anyhow::anyhow!("peer key must be 32 bytes"))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut addresses: Vec<_> = config.peers.iter().map(|peer| peer.address).collect();
    ensure!(
        (1..=512).contains(&keys.len())
            && keys.windows(2).all(|pair| pair[0] < pair[1])
            && addresses.iter().collect::<HashSet<_>>().len() == addresses.len(),
        "peer directory must contain distinct keys and addresses in key order"
    );
    let own = match (keys.binary_search(&public), config.discovery_address) {
        (Ok(index), discover) => {
            ensure!(
                discover.is_none_or(|address| address == addresses[index]),
                "configured local peer and discovery address differ"
            );
            index
        }
        (Err(_), Some(address)) => {
            ensure!(!addresses.contains(&address), "local discovery address duplicates a seed");
            let index = addresses.len();
            addresses.push(address);
            index
        }
        (Err(_), None) => anyhow::bail!("local key is outside the peer directory"),
    } as u16;
    let mut directory = Directory {
        keys,
        own,
        local_key: public,
        discovery: None,
    };
    ensure!(
        genesis_members
            .iter()
            .all(|member| directory.id(&member.key).is_some()),
        "peer directory is missing a genesis voter"
    );
    ensure!(
        (200..=60_000).contains(&config.view_timeout_ms)
            && config.proposal_interval_ms < config.view_timeout_ms / 2
            && (1..=64).contains(&config.vm_workers),
        "invalid replica timing or VM worker configuration"
    );

    let identity = journal_identity(&genesis_committee, public);
    let journal = match store.consensus_record(STATE)? {
        Some(raw) => {
            ensure!(!initialize, "validator journal already initialized");
            let journal: wire::Journal = wire::canonical(&raw)?;
            ensure!(
                journal.identity == identity,
                "validator journal identity mismatch"
            );
            ensure!(
                journal.history_window == history_window,
                "history retention differs from the initialized replica"
            );
            journal
        }
        None => {
            ensure!(
                initialize && store.tip().context.height == 0,
                "missing validator journal"
            );
            let genesis = genesis_committee.genesis_quorum();
            let journal = wire::Journal {
                identity: identity.to_vec(),
                high: genesis.encode(),
                finalized: genesis.point.encode().to_vec(),
                history_window,
                ..Default::default()
            };
            store.persist_consensus(&[(STATE.to_vec(), journal.encode_to_vec())])?;
            journal
        }
    };
    let (committee, active) = if journal.committee.is_empty() {
        ensure!(journal.anchor.is_empty(), "unexpected genesis epoch anchor");
        (genesis_committee, genesis_members)
    } else {
        let change = CommitteeChange::decode(&journal.committee)?;
        ensure!(change.epoch > 0, "unexpected persisted genesis committee");
        let anchor = Header::decode(&journal.anchor)?;
        ensure!(
            anchor.context.height.checked_add(1) == Some(change.activation_height)
                && anchor.context.height <= store.tip().context.height,
            "persisted epoch anchor has the wrong height"
        );
        if let Some(boundary) = store.header(change.activation_height - 1)? {
            ensure!(
                boundary == anchor,
                "persisted epoch anchor disagrees with execution"
            );
        }
        let committee = Committee::at_anchor(
            store.network().unpack(),
            store.genesis_id().unpack(),
            change.epoch,
            anchor.id(store.network()).unpack(),
            &change.members,
        )?;
        (committee, change.members)
    };
    let handoff_tip = store
        .consensus_record(&known_handoff_key(committee.epoch()))?
        .map(|raw| committee.verify_handoff(&raw).map(|(_, _, seal)| seal))
        .transpose()?;
    let routes = directory.routes(&active);
    let member = (!journal.observer_only)
        .then(|| directory.member(&active))
        .flatten();
    let pending = store
        .announced_committee()?
        .filter(|change| change.epoch > committee.epoch());
    let auth = if let Some(address) = config.discovery_address {
        let now = now_ms()?;
        peer_discovery::Endpoint::decode(&peer_discovery::Endpoint::announce(address, now), now)?;
        let publish = (usize::from(own) >= directory.keys.len()).then_some(address);
        directory.discovery = Some(peer_discovery::Discovery::new(publish, addresses.len(), active));
        Some(peer_discovery::Authentication {
            network: store.network().unpack(),
            genesis: store.genesis_id().unpack(),
            key: key.clone(),
        })
    } else {
        None
    };
    let high = Quorum::decode(&journal.high, committee.size())?;
    committee.verify_quorum(&high)?;
    let finalized = Point::decode(&journal.finalized)?;
    ensure!(
        finalized.header == store.tip().id(store.network()).as_slice()
            && journal.lock == finalized.view
            && journal.lock <= high.point.view,
        "validator journal and committed execution disagree"
    );
    let certificate = if journal.certificate.is_empty() {
        None
    } else {
        let tc = TimeoutCertificate::decode(&journal.certificate, committee.size())?;
        committee.verify_timeout_certificate(&tc)?;
        Some(tc)
    };
    if !journal.vote.is_empty() {
        Point::decode(&journal.vote)?;
    }
    if !journal.timeout.is_empty() {
        let timeout: wire::Timeout = wire::canonical(&journal.timeout)?;
        let qc = Quorum::decode(&timeout.high, committee.size())?;
        committee.verify_quorum(&qc)?;
        let vote = Share::decode(&timeout.vote)?;
        ensure!(
            Some(vote.member) == member,
            "wrong persisted timeout signer"
        );
        committee.verify_share(&committee.timeout_digest(timeout.view, qc.point), vote)?;
    }

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(async {
            let (net, mut incoming) =
                Network::listen(directory.own, &addresses, committee.epoch(), auth).await?;
            #[cfg(feature = "jolteon-admission-pipeline")]
            let (pipeline, mut admission_done) = admission_pipeline::Pipeline::start()?;
            let workers = Arc::new(rayon::ThreadPoolBuilder::new()
                .num_threads(config.vm_workers)
                .build()?);
            #[cfg(feature = "jolteon-candidate-pipeline")]
            let (candidate_pipeline, mut candidate_done) =
                candidate_pipeline::Pipeline::start(workers.clone())?;
            #[cfg(not(feature = "jolteon-candidate-pipeline"))]
            let mut candidate_done = std::future::pending::<Option<()>>();
            #[cfg(feature = "jolteon-commit-pipeline")]
            let commit_writer = commit_pipeline::Writer::start()?;
            #[cfg(feature = "jolteon-proposal-pipeline")]
            let (proposal_pipeline, mut proposal_done) =
                proposal_pipeline::Pipeline::start(workers.clone())?;
            #[cfg(not(feature = "jolteon-proposal-pipeline"))]
            let mut proposal_done = std::future::pending::<Option<()>>();
            let mut replica = Replica {
                store,
                committee,
                member,
                directory,
                routes,
                key,
                net,
                journal,
                high,
                certificate,
                finalized,
                view: 0,
                since: Instant::now(),
                announced: Instant::now(),
                handoff_requested: None,
                handoff_tip,
                pending,
                timeout: Duration::from_millis(config.view_timeout_ms),
                proposal_interval: Duration::from_millis(config.proposal_interval_ms),
                backoff: 0,
                #[cfg(not(all(
                    feature = "jolteon-candidate-pipeline",
                    feature = "jolteon-proposal-pipeline"
                )))]
                workers,
                #[cfg(feature = "jolteon-candidate-pipeline")]
                candidate_pipeline,
                #[cfg(feature = "jolteon-commit-pipeline")]
                commit_writer,
                #[cfg(feature = "jolteon-proposal-pipeline")]
                proposal_pipeline,
                candidates: BTreeMap::new(),
                preferred: BTreeMap::new(),
                votes: BTreeMap::new(),
                observed_votes: HashMap::new(),
                timeouts: BTreeMap::new(),
                certificates: HashSet::new(),
                #[cfg(feature = "jolteon-certificate-experiment")]
                certificate_experiment: Default::default(),
                requests: HashMap::new(),
                mempool: Mempool::default(),
                admission: AdmissionMetrics::default(),
                #[cfg(feature = "jolteon-admission-pipeline")]
                pipeline,
                execution: ExecutionMetrics::default(),
                rejected: 0,
                timeout_views: VecDeque::new(),
                last_untimely_view: 0,
                recovery: Recovery::default(),
            };
            replica.enter();
            let mut ticker = tokio::time::interval(Duration::from_millis(10));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The stock build has no background worker or completion channel.
            #[cfg(not(feature = "jolteon-admission-pipeline"))]
            let mut admission_done = std::future::pending::<Option<()>>();
            loop {
                tokio::select! {
                    result = async {
                        #[cfg(feature = "jolteon-admission-pipeline")]
                        { admission_done.recv().await }
                        #[cfg(not(feature = "jolteon-admission-pipeline"))]
                        { (&mut admission_done).await }
                    } => {
                        #[cfg(feature = "jolteon-admission-pipeline")]
                        replica.complete_client_admission(result.context("admission worker stopped")?)?;
                        #[cfg(not(feature = "jolteon-admission-pipeline"))]
                        let _ = result;
                    }
                    result = async {
                        #[cfg(feature = "jolteon-candidate-pipeline")]
                        { candidate_done.recv().await }
                        #[cfg(not(feature = "jolteon-candidate-pipeline"))]
                        { (&mut candidate_done).await }
                    } => {
                        #[cfg(feature = "jolteon-candidate-pipeline")]
                        replica.complete_preparation(result.context("candidate preparation worker stopped")?)?;
                        #[cfg(not(feature = "jolteon-candidate-pipeline"))]
                        let _ = result;
                    }
                    result = async {
                        #[cfg(feature = "jolteon-proposal-pipeline")]
                        { proposal_done.recv().await }
                        #[cfg(not(feature = "jolteon-proposal-pipeline"))]
                        { (&mut proposal_done).await }
                    } => {
                        #[cfg(feature = "jolteon-proposal-pipeline")]
                        replica.complete_proposal(result.context("proposal worker stopped")?)?;
                        #[cfg(not(feature = "jolteon-proposal-pipeline"))]
                        let _ = result;
                    }
                    _ = ticker.tick() => replica.tick(&mut incoming).await?,
                    event = incoming.recv() => {
                        let event = event.context("replica transport stopped")?;
                        if let Err(error) = replica.handle(event) {
                            if error.is::<StorageFault>() {
                                return Err(error);
                            }
                            replica.rejected = replica.rejected.saturating_add(1);
                        }
                    }
                }
            }
        })
}

impl Replica {
    fn save(&self, proposal: Option<(Point, &wire::Proposal)>) -> Result<()> {
        let mut records = vec![(STATE.to_vec(), self.journal.encode_to_vec())];
        if let Some((point, proposal)) = proposal {
            records.push((
                proposal_key(self.committee.epoch(), point),
                proposal.encode_to_vec(),
            ));
        }
        self.store.persist_consensus(&records).context(StorageFault)
    }

    fn enter(&mut self) {
        let transition = self.certificate.as_ref().map_or(0, |tc| tc.view);
        let next = self.high.point.view.max(transition).saturating_add(1);
        if next > self.view {
            self.view = next;
            self.since = Instant::now();
            if transition >= self.high.point.view && transition != 0 {
                self.backoff = (self.backoff + 1).min(3);
            } else {
                self.backoff = 0;
            }
        }
    }

    fn qc(&mut self, raw: &[u8]) -> Result<Quorum> {
        let quorum = Quorum::decode(raw, self.committee.size())?;
        let id = ckb_hash::blake2b_256(raw);
        if !self.certificates.contains(&id) {
            self.committee.verify_quorum(&quorum)?;
            if self.certificates.len() >= 2048 {
                self.certificates.clear();
            }
            self.certificates.insert(id);
        }
        Ok(quorum)
    }

    fn learn_quorum(&mut self, quorum: Quorum) -> Result<()> {
        if quorum.point.view == self.high.point.view {
            ensure!(
                quorum.point == self.high.point,
                "conflicting certificates for one view"
            );
        } else if quorum.point.view > self.high.point.view {
            self.journal.high = quorum.encode();
            self.high = quorum;
            self.save(None)?;
            self.enter();
        }
        Ok(())
    }

    fn learn_tc(&mut self, tc: TimeoutCertificate) -> Result<()> {
        if self
            .certificate
            .as_ref()
            .is_some_and(|prior| prior.view >= tc.view)
        {
            return Ok(());
        }
        self.learn_quorum(tc.high.clone())?;
        self.journal.certificate = tc.encode();
        if self.timeout_views.len() == 256 {
            self.timeout_views.pop_front();
        }
        self.timeout_views.push_back(tc.view);
        self.certificate = Some(tc);
        self.save(None)?;
        self.enter();
        Ok(())
    }

    fn check_proposal(
        &mut self,
        proposal: wire::Proposal,
    ) -> Result<(Point, Checked, Option<TimeoutCertificate>)> {
        let header = Header::decode(&proposal.header)?;
        ensure!(
            header.encode().as_ref() == proposal.header,
            "noncanonical execution Header"
        );
        let parent = self.qc(&proposal.parent)?;
        let header_id: Hash32 = header.id(self.store.network()).unpack();
        let hash = if proposal.seal {
            let seal = self.committee.seal_header(header_id);
            ensure!(
                proposal.transactions.is_empty()
                    && (parent.point.header == header_id || parent.point.header == seal),
                "epoch seal must extend its certified boundary"
            );
            seal
        } else {
            ensure!(
                header.context.parent.as_slice() == parent.point.header,
                "proposal does not extend its certified Header"
            );
            header_id
        };
        let point = Point {
            view: proposal.view,
            parent_view: parent.point.view,
            header: hash,
        };
        Point::decode(&point.encode())?;
        #[cfg(not(feature = "jolteon-certificate-experiment"))]
        let tc = (!proposal.timeout.is_empty())
            .then(|| TimeoutCertificate::decode(&proposal.timeout, self.committee.size()))
            .transpose()?;
        #[cfg(not(feature = "jolteon-certificate-experiment"))]
        self.committee
            .verify_parent_view(proposal.view, parent.point, tc.as_ref())?;
        #[cfg(feature = "jolteon-certificate-experiment")]
        let tc = self.certificate_experiment.verify_parent(
            &self.committee,
            proposal.view,
            parent.point,
            &proposal.timeout,
        )?;
        let signature = proposal.signature.as_slice().try_into()?;
        self.committee.verify_share(
            &self.committee.proposal_digest(point),
            Share {
                member: self.committee.leader(point.view)?,
                signature,
            },
        )?;
        Ok((
            point,
            Checked {
                wire: proposal,
                header,
                parent,
            },
            tc,
        ))
    }

    fn admit_proposal(&mut self, proposal: wire::Proposal, peer: u16) -> Result<()> {
        let (point, checked, tc) = self.check_proposal(proposal)?;
        if point.view <= self.finalized.view {
            return Ok(());
        }
        self.learn_quorum(checked.parent.clone())?;
        if let Some(tc) = tc {
            self.learn_tc(tc)?;
        }
        self.cache_proposal(point, checked, peer)
    }

    fn cache_proposal(&mut self, point: Point, checked: Checked, peer: u16) -> Result<()> {
        if self.recovery.active
            && checked
                .header
                .context
                .height
                .saturating_sub(self.store.tip().context.height)
                > MAX_BRANCH as u64
            && point != self.high.point
            && self
                .handoff_tip
                .as_ref()
                .is_none_or(|seal| seal.point != point)
        {
            return Ok(());
        }
        if !self.candidates.contains_key(&point) {
            if self.preferred.contains_key(&point.view)
                && !self.requests.contains_key(&point)
                && self.high.point != point
            {
                return Ok(());
            }
            ensure!(
                self.candidates.len() < MAX_PENDING,
                "candidate queue is full"
            );
            self.preferred.entry(point.view).or_insert(point);
            self.candidates.insert(
                point,
                Candidate {
                    proposal: checked,
                    execution: None,
                    #[cfg(feature = "jolteon-candidate-pipeline")]
                    execution_error: None,
                    source: peer,
                },
            );
        }
        Ok(())
    }

    fn request(&mut self, point: Point, peer: Option<u16>) {
        if self
            .requests
            .get(&point)
            .is_some_and(|asked| asked.elapsed() < Duration::from_millis(180))
        {
            return;
        }
        if self.requests.len() >= MAX_PENDING {
            self.requests.clear();
        }
        let repeated = self.requests.insert(point, Instant::now()).is_some();
        if let Some(peer) = peer.filter(|&peer| peer != self.directory.own && !repeated) {
            self.net.send(peer, wire::GET, point.encode().to_vec());
        } else {
            self.net.broadcast(wire::GET, point.encode().to_vec());
        }
    }

    fn branch(&mut self, mut point: Point) -> Result<Option<Vec<Point>>> {
        let mut branch = Vec::new();
        let mut source = None;
        while point != self.finalized {
            ensure!(
                point.view > self.finalized.view,
                "candidate diverges from local finality"
            );
            if branch.len() == MAX_BRANCH {
                return Err(BranchHorizon.into());
            }
            if !self.candidates.contains_key(&point) {
                if let Some(raw) = self
                    .store
                    .consensus_record(&proposal_key(self.committee.epoch(), point))
                    .context(StorageFault)?
                {
                    let proposal = wire::canonical_bytes(raw.into())?;
                    let (actual, checked, _) = self.check_proposal(proposal)?;
                    ensure!(actual == point, "wrong persisted candidate");
                    self.candidates.insert(
                        point,
                        Candidate {
                            proposal: checked,
                            execution: None,
                            #[cfg(feature = "jolteon-candidate-pipeline")]
                            execution_error: None,
                            source: self.directory.own,
                        },
                    );
                } else {
                    self.request(point, source);
                    return Ok(None);
                }
            }
            let candidate = &self.candidates[&point];
            source = Some(candidate.source);
            branch.push(point);
            point = candidate.proposal.parent.point;
        }
        branch.reverse();
        Ok(Some(branch))
    }

    fn execute(&mut self, branch: &[Point]) -> Result<Option<BranchExecution>> {
        let mut batches: Vec<Arc<Prepared>> = Vec::with_capacity(branch.len());
        let mut pending = self.pending.clone();
        for (prefix_end, point) in branch.iter().enumerate() {
            #[cfg(not(feature = "jolteon-candidate-pipeline"))]
            let _ = prefix_end;
            let candidate = self
                .candidates
                .get_mut(point)
                .context("candidate missing from branch")?;
            let proposal = &candidate.proposal;
            if proposal.wire.seal {
                let tip = batches
                    .last()
                    .map_or(self.store.tip(), |batch| batch.header());
                ensure!(
                    pending
                        .as_ref()
                        .is_some_and(|change| tip.context.height.checked_add(1)
                            == Some(change.activation_height))
                        && &proposal.header == tip,
                    "epoch seal does not close an announced boundary"
                );
                continue;
            }
            ensure!(
                pending
                    .as_ref()
                    .is_none_or(|change| proposal.header.context.height < change.activation_height),
                "outgoing committee cannot certify execution after its boundary"
            );
            let parent = batches
                .last()
                .map_or(self.store.tip(), |batch| batch.header());
            ensure!(
                proposal.header.context.timestamp_ms > parent.context.timestamp_ms,
                "execution timestamp must advance"
            );
            #[cfg(feature = "jolteon-candidate-pipeline")]
            if candidate.execution.is_none() {
                if let Some(error) = &candidate.execution_error {
                    anyhow::bail!("{error}");
                }
                let started = self.candidate_pipeline.schedule(
                    &self.store,
                    self.committee.epoch(),
                    self.finalized,
                    &branch[..=prefix_end],
                    proposal,
                    batches,
                )?;
                let counter = if started {
                    &mut self.execution.candidate_jobs.started
                } else {
                    &mut self.execution.candidate_jobs.already_running
                };
                *counter = counter.saturating_add(1);
                return Ok(None);
            }
            #[cfg(not(feature = "jolteon-candidate-pipeline"))]
            if candidate.execution.is_none() {
                let started = Instant::now();
                let transactions = proposal
                    .wire
                    .transactions
                    .iter()
                    .map(|bytes| Transaction::from_bytes(bytes.clone()))
                    .collect::<Result<Vec<_>>>()?;
                let prepared = self.workers.install(|| {
                    self.store.on_branch(&batches, |state| {
                        state.prepare(&transactions, proposal.header.context.timestamp_ms, true)
                    })
                })?;
                self.execution.candidate_verify.observe(started.elapsed());
                self.execution
                    .candidate_commitments
                    .record(prepared.commitment_timing());
                self.execution
                    .candidate_lineage
                    .record(prepared.lineage_schedule());
                ensure!(
                    prepared.header() == &proposal.header,
                    "candidate execution differs from the signed Header"
                );
                candidate.execution = Some(Arc::new(prepared));
            }
            let prepared = candidate.execution.clone().expect("prepared candidate");
            if let Some(change) = prepared.committee_change() {
                ensure!(
                    pending.is_none() && change.epoch > self.committee.epoch(),
                    "overlapping committee announcements"
                );
                pending = Some(change.clone());
            }
            batches.push(prepared);
        }
        Ok(Some(BranchExecution { batches, pending }))
    }

    async fn finalize(&mut self, inbox: &mut transport::Inbox) -> Result<()> {
        #[cfg(not(feature = "jolteon-commit-pipeline"))]
        let _ = inbox;
        let Some(high) = self.finalization_head() else {
            return Ok(());
        };
        if high.point.view <= self.finalized.view {
            return Ok(());
        }
        let branch = match self.branch(high.point) {
            Ok(Some(branch)) => branch,
            Ok(None) => return Ok(()),
            Err(error) if error.is::<BranchHorizon>() => {
                self.request_replay();
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let child = &self.candidates[&high.point].proposal;
        let parent = child.parent.point;
        if !high.point.directly_certifies(parent) || parent == self.finalized {
            return Ok(());
        }
        ensure!(
            branch.len() >= 2 && branch[branch.len() - 2] == parent,
            "invalid two-chain certificate"
        );
        let Some(BranchExecution { batches, pending }) = self.execute(&branch)? else {
            return Ok(());
        };
        let child = &self.candidates[&high.point].proposal;
        let old_epoch = self.committee.epoch();
        let mut successor = None;
        let mut durable = Vec::new();
        let commit_count = batches.len() - usize::from(!child.wire.seal);
        #[cfg(feature = "jolteon-commit-pipeline")]
        let overlap = commit_count == 1
            && !child.wire.seal
            && !self.recovery.active
            && self.handoff_tip.is_none()
            && self.high.point == high.point
            && pending.as_ref().is_none_or(|change| {
                child.header.context.height.saturating_add(1) < change.activation_height
            });
        let proof = if child.wire.seal {
            let change = pending.context("terminal execution has no successor")?;
            let (announcement, path) = self
                .store
                .on_branch(&batches, Store::prove_announced_committee)?;
            let handoff = HandoffProof {
                boundary_header: child.wire.header.clone(),
                parent_qc: child.parent.encode(),
                seal_qc: high.encode(),
                announcement,
                path: path.encode()?.to_vec(),
            }
            .encode_to_vec();
            let (verified, _, _) = self.committee.verify_handoff(&handoff)?;
            ensure!(
                verified == change && commit_count != 0,
                "handoff disagrees with execution"
            );
            let committee = Committee::at_anchor(
                self.store.network().unpack(),
                self.store.genesis_id().unpack(),
                change.epoch,
                child.header.id(self.store.network()).unpack(),
                &change.members,
            )?;
            successor = Some((change, committee));
            // A lagging replica can fetch every certified terminal proposal after the switch.
            for point in &branch[commit_count..] {
                durable.push((
                    proposal_key(old_epoch, *point),
                    self.candidates[point].proposal.wire.encode_to_vec(),
                ));
            }
            handoff
        } else {
            durable.push((
                proposal_key(old_epoch, high.point),
                child.wire.encode_to_vec(),
            ));
            FinalityProof {
                parent_header: self.candidates[&parent].proposal.wire.header.clone(),
                parent_qc: child.parent.encode(),
                child_header: child.wire.header.clone(),
                child_qc: high.encode(),
            }
            .encode_to_vec()
        };
        // Execution batches contain only real blocks. The outgoing seal produces no ledger changes.
        for (index, (point, prepared)) in branch.iter().zip(&batches).take(commit_count).enumerate()
        {
            let height = prepared.header().context.height;
            let proposal = &self.candidates[point].proposal.wire;
            let mut journal = self.journal.clone();
            let mut retired = self
                .expire_history(height, &mut journal.proposal_floor)
                .context(StorageFault)?;
            journal.finalized = point.encode().to_vec();
            journal.lock = point.view;
            journal.committed_transactions = journal
                .committed_transactions
                .checked_add(proposal.transactions.len() as u64)
                .context("transaction counter overflow")?;
            let mut records = vec![
                (proposal_key(old_epoch, *point), proposal.encode_to_vec()),
                (
                    finality_key(height),
                    wire::Finalized {
                        proof: proof.clone(),
                        epoch: old_epoch,
                        handoff: successor.is_some(),
                        view: point.view,
                    }
                    .encode_to_vec(),
                ),
            ];
            records.append(&mut durable);
            if let Some((change, next)) = successor.as_ref().filter(|_| index + 1 == commit_count) {
                let anchor = next.genesis_quorum();
                journal = wire::Journal {
                    identity: journal.identity,
                    high: anchor.encode(),
                    finalized: anchor.point.encode().to_vec(),
                    committed_transactions: journal.committed_transactions,
                    committee: change.encode(),
                    anchor: prepared.header().encode().to_vec(),
                    observer_only: journal.observer_only,
                    history_window: journal.history_window,
                    proposal_floor: journal.proposal_floor,
                    ..Default::default()
                };
                records.push((handoff_key(old_epoch), proof.clone()));
                let announced = known_handoff_key(old_epoch);
                let mut end = announced.clone();
                end.push(0);
                retired.push(announced..end);
            }
            records.push((STATE.to_vec(), journal.encode_to_vec()));
            let started = Instant::now();
            #[cfg(feature = "jolteon-commit-pipeline")]
            self.write_certified(
                inbox,
                Arc::clone(prepared),
                records,
                retired,
                overlap.then_some((branch.as_slice(), batches.as_slice())),
            )
            .await
            .context(StorageFault)?;
            #[cfg(not(feature = "jolteon-commit-pipeline"))]
            self.store
                .commit_certified(prepared, &records, &retired)
                .context(StorageFault)?;
            self.execution.commit.observe(started.elapsed());
            self.journal = journal;
            self.finalized = *point;
            if let Some(change) = prepared.committee_change() {
                self.pending = Some(change.clone());
            }
            for tx in &self.candidates[point].proposal.wire.transactions {
                self.mempool.remove(&ckb_hash::blake2b_256(tx));
            }
            let context = &prepared.header().context;
            self.mempool.expire(context.height, context.timestamp_ms);
        }
        self.recovery.record_commit(high.point, commit_count);
        if let Some((change, committee)) = successor {
            self.net.broadcast_at(old_epoch, wire::HANDOFF, proof);
            if let Some(discovery) = &mut self.directory.discovery {
                discovery.handoff(&change.members, &mut self.net);
            }
            self.routes = self.directory.routes(&change.members);
            self.member = (!self.journal.observer_only)
                .then(|| self.directory.member(&change.members))
                .flatten();
            self.net.epoch = change.epoch;
            self.high = committee.genesis_quorum();
            self.finalized = self.high.point;
            self.committee = committee;
            #[cfg(feature = "jolteon-certificate-experiment")]
            self.certificate_experiment.reset();
            self.pending = None;
            self.handoff_tip = None;
            self.handoff_requested = None;
            self.certificate = None;
            self.view = 0;
            self.backoff = 0;
            self.candidates.clear();
            self.preferred.clear();
            self.votes.clear();
            self.timeouts.clear();
            self.observed_votes.clear();
            self.requests.clear();
            self.certificates.clear();
            self.timeout_views.clear();
            self.last_untimely_view = 0;
            self.recovery.reset();
            self.enter();
        } else {
            let view = self.finalized.view;
            self.candidates.retain(|point, _| point.view > view);
            self.preferred.retain(|slot, _| *slot > view);
            self.votes.retain(|point, _| point.view > view);
            self.timeouts.retain(|slot, _| *slot > view);
            self.observed_votes
                .retain(|(slot, _), _| slot.saturating_add(256) >= view);
        }
        Ok(())
    }

    fn expire_history(
        &self,
        height: u64,
        proposal_floor: &mut Vec<u8>,
    ) -> Result<Vec<Range<Vec<u8>>>> {
        let Some(expired) = self
            .store
            .history_window()
            .and_then(|window| height.checked_sub(window))
        else {
            return Ok(Vec::new());
        };
        let key = finality_key(expired);
        let Some(bytes) = self.store.consensus_record(&key)? else {
            // Genesis and heights preceding a restored image have no local certificate.
            return Ok(Vec::new());
        };
        let record: wire::Finalized = wire::canonical(&bytes)?;
        ensure!(
            record.view > 0,
            "retained finality record lacks its proposal position"
        );
        let boundary = match record.view.checked_add(1) {
            Some(view) => proposal_prefix(record.epoch, view),
            None => proposal_prefix(record.epoch.checked_add(1).context("epoch exhausted")?, 0),
        };
        let mut lower = std::mem::replace(proposal_floor, boundary.clone());
        if lower.is_empty() {
            lower = BODY.to_vec();
        }
        ensure!(
            lower < boundary,
            "consensus history floor failed to advance"
        );
        Ok(vec![lower..boundary, key..finality_key(expired + 1)])
    }

    fn vote_for(&mut self, point: Point) -> Result<()> {
        if self.member.is_none()
            || self.handoff_tip.is_some()
            || point.view != self.view
            || point.view <= self.high.point.view
            || point.parent_view < self.journal.lock
        {
            return Ok(());
        }
        if !self.journal.timeout.is_empty() {
            let timeout: wire::Timeout = wire::canonical(&self.journal.timeout)?;
            if timeout.view >= point.view {
                return Ok(());
            }
        }
        if !self.journal.vote.is_empty() {
            let prior = Point::decode(&self.journal.vote)?;
            if prior.view >= point.view && prior != point {
                return Ok(());
            }
            if prior == point {
                return Ok(());
            }
        }
        let Some(branch) = self.branch(point)? else {
            return Ok(());
        };
        let proposal = &self.candidates[&point].proposal;
        let time = proposal.header.context.timestamp_ms;
        let seal = proposal.wire.seal;
        if !seal && !self.observe_time(point.view, time) {
            return Ok(());
        }
        if self.execute(&branch)?.is_none() {
            return Ok(());
        }
        if !seal && !self.observe_time(point.view, time) {
            return Ok(());
        }
        self.journal.vote = point.encode().to_vec();
        self.save(Some((point, &self.candidates[&point].proposal.wire)))?;
        self.dispatch_vote(point)
    }

    fn observe_time(&mut self, view: u64, time: u64) -> bool {
        let allowed = can_vote_at(time);
        if !allowed {
            self.last_untimely_view = view;
        }
        allowed
    }

    fn dispatch_vote(&mut self, point: Point) -> Result<()> {
        let Some(member) = self.member else {
            return Ok(());
        };
        let share = Share {
            member,
            signature: self.key.sign(&self.committee.vote_digest(point)).to_bytes(),
        };
        let body = [point.encode().as_slice(), &share.encode()].concat();
        let collector = self.committee.leader(point.view)?;
        let next = self.committee.leader(point.view.saturating_add(1))?;
        if let Some(peer) = self.routes[usize::from(collector)] {
            self.net.send(peer, wire::VOTE, body.clone());
        }
        if next != collector
            && let Some(peer) = self.routes[usize::from(next)]
        {
            self.net.send(peer, wire::VOTE, body);
        }
        if member == collector || member == next {
            self.accept_vote(point, share)?;
        }
        Ok(())
    }

    fn accept_vote(&mut self, point: Point, share: Share) -> Result<()> {
        if point.view == 0
            || point.view.saturating_add(256) < self.view
            || point.view > self.view.saturating_add(4)
        {
            return Ok(());
        }
        self.committee
            .verify_share(&self.committee.vote_digest(point), share)?;
        let slot = (point.view, share.member);
        if let Some((previous, first)) = self.observed_votes.get(&slot) {
            if previous == &point {
                return Ok(());
            }
            let evidence = self
                .committee
                .verify_double_vote((*previous, *first), (point, share))?;
            let key = [b"double".as_slice(), &evidence].concat();
            if self
                .store
                .consensus_record(&key)
                .context(StorageFault)?
                .is_none()
            {
                let raw = [
                    previous.encode().as_slice(),
                    &first.encode(),
                    &point.encode(),
                    &share.encode(),
                ]
                .concat();
                self.store
                    .persist_consensus(&[(key, raw)])
                    .context(StorageFault)?;
            }
            return Ok(());
        } else {
            self.observed_votes.insert(slot, (point, share));
        }
        if point.view <= self.high.point.view {
            return Ok(());
        }
        let votes = self.votes.entry(point).or_default();
        votes.insert(share.member, share.signature);
        if let Ok(qc) = self.committee.assemble_quorum(point, votes) {
            self.net.broadcast(wire::QUORUM, qc.encode());
            self.learn_quorum(qc)?;
        }
        Ok(())
    }

    fn accept_timeout(&mut self, raw: &[u8]) -> Result<()> {
        let report: wire::Timeout = wire::canonical(raw)?;
        if report.view < self.view || report.view > self.view.saturating_add(8) {
            return Ok(());
        }
        let high = self.qc(&report.high)?;
        ensure!(
            high.point.view <= report.view && report.view != 0,
            "invalid timeout view"
        );
        let share = Share::decode(&report.vote)?;
        self.committee.verify_share(
            &self.committee.timeout_digest(report.view, high.point),
            share,
        )?;
        let votes = self.timeouts.entry(report.view).or_default();
        votes.entry(share.member).or_insert((high, share));
        let power = votes.keys().try_fold(0u64, |total, &member| {
            Ok::<_, stem_stake::Error>(total + self.committee.power(member)?)
        })?;
        if self.committee.has_quorum(power) {
            let high = votes
                .values()
                .map(|(high, _)| high)
                .max_by_key(|high| high.point.view)
                .unwrap()
                .clone();
            let tc = TimeoutCertificate {
                view: report.view,
                high,
                votes: votes
                    .values()
                    .map(|(high, share)| TimeoutVote {
                        high: high.point,
                        share: *share,
                    })
                    .collect(),
            };
            self.committee.verify_timeout_certificate(&tc)?;
            self.net.broadcast(wire::TIMEOUT_CERTIFICATE, tc.encode());
            self.learn_tc(tc)?;
        }
        Ok(())
    }

    fn local_timeout(&mut self) -> Result<()> {
        let Some(member) = self.member else {
            return Ok(());
        };
        if !self.journal.timeout.is_empty() {
            let prior: wire::Timeout = wire::canonical(&self.journal.timeout)?;
            if prior.view >= self.view {
                return Ok(());
            }
        }
        let high = self.high.point;
        let share = Share {
            member,
            signature: self
                .key
                .sign(&self.committee.timeout_digest(self.view, high))
                .to_bytes(),
        };
        let raw = wire::Timeout {
            view: self.view,
            high: self.high.encode(),
            vote: share.encode().to_vec(),
        }
        .encode_to_vec();
        self.journal.timeout = raw.clone();
        self.save(None)?;
        self.execution.local_timeouts = self.execution.local_timeouts.saturating_add(1);
        self.net.broadcast(wire::TIMEOUT, raw.clone());
        self.accept_timeout(&raw)
    }

    fn proposal_parent(&self) -> Result<Option<(Quorum, Vec<u8>)>> {
        if self.handoff_tip.is_some()
            || self.recovery.active
            || Some(self.committee.leader(self.view)?) != self.member
            || self.preferred.contains_key(&self.view)
            || self.high.point.view >= self.view
            || self.high.point.view < self.journal.lock
        {
            return Ok(None);
        }
        if !self.journal.vote.is_empty() {
            let prior = Point::decode(&self.journal.vote)?;
            if prior.view >= self.view {
                return Ok(None);
            }
        }
        if !self.journal.timeout.is_empty() {
            let prior: wire::Timeout = wire::canonical(&self.journal.timeout)?;
            if prior.view >= self.view {
                return Ok(None);
            }
        }
        let parent = self.high.clone();
        let tc = if parent.point.view.checked_add(1) == Some(self.view) {
            Vec::new()
        } else {
            let Some(tc) = self
                .certificate
                .as_ref()
                .filter(|tc| tc.view.checked_add(1) == Some(self.view))
            else {
                return Ok(None);
            };
            tc.encode()
        };
        Ok(Some((parent, tc)))
    }

    fn propose(&mut self) -> Result<()> {
        #[cfg(feature = "jolteon-proposal-pipeline")]
        if self.proposal_pipeline.running() {
            return Ok(());
        }
        let Some((parent, tc)) = self.proposal_parent()? else {
            return Ok(());
        };
        let Some(branch) = self.branch(parent.point)? else {
            return Ok(());
        };
        let Some(BranchExecution { batches, pending }) = self.execute(&branch)? else {
            return Ok(());
        };
        let parent_header = batches
            .last()
            .map_or(self.store.tip(), |batch| batch.header());
        let seal = pending.as_ref().is_some_and(|change| {
            parent_header.context.height.checked_add(1) == Some(change.activation_height)
        });
        if seal {
            return self.publish_proposal(parent, tc, parent_header.clone(), Vec::new(), None);
        }
        let Ok(at) = now_ms() else {
            return Ok(());
        };
        if at <= parent_header.context.timestamp_ms {
            return Ok(());
        }
        #[cfg(feature = "jolteon-proposal-pipeline")]
        {
            let parent_id = parent_header.id(self.store.network()).unpack();
            self.schedule_proposal(branch, batches, parent_id, at)
        }
        #[cfg(not(feature = "jolteon-proposal-pipeline"))]
        {
            let started = Instant::now();
            let mut scan = Default::default();
            let result = self.workers.install(|| {
                self.store.on_branch(&batches, |state| {
                    state.build_proposal_measured(
                        self.mempool.transactions(),
                        at,
                        MAX_PROPOSAL_BYTES,
                        &mut scan,
                        crate::native_store::JOLTEON_CYCLE_SCAN_LIMIT,
                    )
                })
            });
            self.execution.proposal_scan.record(scan);
            let (transactions, execution) = result?;
            self.execution.proposal.observe(started.elapsed());
            self.execution
                .proposal_commitments
                .record(execution.commitment_timing());
            if !can_vote_at(at) {
                return Ok(());
            }
            self.publish_proposal(
                parent,
                tc,
                execution.header().clone(),
                transactions,
                Some(Arc::new(execution)),
            )
        }
    }

    fn publish_proposal(
        &mut self,
        parent: Quorum,
        tc: Vec<u8>,
        header: Header,
        transactions: Vec<Transaction>,
        execution: Option<Arc<Prepared>>,
    ) -> Result<()> {
        let seal = execution.is_none();
        let header_id: Hash32 = header.id(self.store.network()).unpack();
        let point = Point {
            view: self.view,
            parent_view: parent.point.view,
            header: if seal {
                self.committee.seal_header(header_id)
            } else {
                header_id
            },
        };
        let proposal = wire::Proposal {
            view: self.view,
            header: header.encode().to_vec(),
            parent: parent.encode(),
            timeout: tc,
            transactions: transactions.iter().map(Transaction::as_bytes).collect(),
            signature: self
                .key
                .sign(&self.committee.proposal_digest(point))
                .to_bytes()
                .to_vec(),
            seal,
        };
        self.admit_proposal(proposal.clone(), self.directory.own)?;
        self.candidates
            .get_mut(&point)
            .context("local proposal was not admitted")?
            .execution = execution;
        self.journal.vote = point.encode().to_vec();
        self.save(Some((point, &proposal)))?;
        self.net.broadcast(wire::PROPOSAL, proposal.encode_to_vec());
        self.dispatch_vote(point)
    }

    async fn tick(&mut self, inbox: &mut transport::Inbox) -> Result<()> {
        self.finalize(inbox).await?;
        if self.handoff_tip.is_some() || self.recovery.active {
            return Ok(());
        }
        if let Some(&point) = self.preferred.get(&self.view) {
            // Invalid candidate execution is a view failure. The pacemaker still runs.
            if let Err(error) = self.vote_for(point) {
                if error.is::<StorageFault>() {
                    return Err(error);
                }
            }
        }
        if self.since.elapsed() >= self.timeout * (1 << self.backoff) {
            self.local_timeout()?;
        } else if self.since.elapsed() >= self.proposal_interval {
            self.propose()?;
        }
        if self.announced.elapsed() >= Duration::from_millis(350) {
            self.announced = Instant::now();
            #[cfg(feature = "jolteon-certificate-experiment")]
            self.certificate_experiment.quorum_frame.send(
                &self.net,
                wire::QUORUM,
                &self.journal.high,
            );
            #[cfg(not(feature = "jolteon-certificate-experiment"))]
            self.net.broadcast(wire::QUORUM, self.high.encode());
            if let Some(_tc) = self
                .certificate
                .as_ref()
                .filter(|tc| tc.view >= self.high.point.view)
            {
                #[cfg(feature = "jolteon-certificate-experiment")]
                self.certificate_experiment.timeout_frame.send(
                    &self.net,
                    wire::TIMEOUT_CERTIFICATE,
                    &self.journal.certificate,
                );
                #[cfg(not(feature = "jolteon-certificate-experiment"))]
                self.net.broadcast(wire::TIMEOUT_CERTIFICATE, _tc.encode());
            }
            if !self.journal.timeout.is_empty() {
                let timeout: wire::Timeout = wire::canonical(&self.journal.timeout)?;
                if timeout.view == self.view {
                    self.net.broadcast(wire::TIMEOUT, timeout.encode_to_vec());
                }
            }
            if !self.journal.vote.is_empty() {
                let vote = Point::decode(&self.journal.vote)?;
                if vote.view == self.view {
                    if let Some(raw) = self
                        .store
                        .consensus_record(&proposal_key(self.committee.epoch(), vote))
                        .context(StorageFault)?
                        && Some(self.committee.leader(vote.view)?) == self.member
                    {
                        self.net.broadcast(wire::PROPOSAL, raw);
                    }
                    self.dispatch_vote(vote)?;
                }
            }
        }
        if let Some(discovery) = &mut self.directory.discovery
            && let Ok(now) = now_ms()
        {
            discovery.tick(now, &mut self.net);
        }
        self.retry_relays();
        Ok(())
    }

    fn relay_leaders(&self) -> [Option<u16>; 2] {
        [Some(self.view), self.view.checked_add(1)].map(|view| {
            let member = self.committee.leader(view?).ok()?;
            self.routes[usize::from(member)]
        })
    }

    fn retry_relays(&mut self) {
        let now = Instant::now();
        let [current, next] = self.relay_leaders();
        let epoch = self.committee.epoch();
        for _ in 0..RELAY_RETRIES_PER_TICK {
            let Some((tx, cursor)) = self.mempool.retry(now, epoch, self.routes.len()) else {
                break;
            };
            let alternate = self.routes[cursor];
            let routes = if cursor == 0 {
                // Start with the current view after allowing an initial
                // proposal to finalize. Later retries step through members.
                [current, next, alternate]
            } else {
                [alternate, None, None]
            };
            self.net.relay(&routes, tx.as_bytes());
            self.admission.relay_retry_attempts =
                self.admission.relay_retry_attempts.saturating_add(1);
        }
    }

    fn learn_endpoint(&mut self, event: Incoming) -> Result<()> {
        let key = event.peer_key.context("missing endpoint signer")?;
        if self.directory.keys.binary_search(&key).is_ok() || key == self.directory.local_key {
            return Ok(());
        }
        let discovery = self
            .directory
            .discovery
            .as_mut()
            .context("peer discovery is disabled")?;
        let pending = self
            .pending
            .as_ref()
            .map_or(&[][..], |change| change.members.as_slice());
        if !discovery.eligible(&key, pending) {
            return Ok(());
        }
        let endpoint = peer_discovery::Endpoint::decode(&event.frame.body, now_ms()?)?;
        if discovery.learn(key, endpoint, &mut self.net)? {
            let active = &self.directory.discovery.as_ref().expect("peer discovery").active;
            self.routes = self.directory.routes(active);
            let signed = event.signed_endpoint.context("missing signed endpoint frame")?;
            self.net.broadcast_frame(wire::PEER_ENDPOINT, signed);
        }
        Ok(())
    }

    fn handle(&mut self, event: Incoming) -> Result<()> {
        if let Some(received) = event.control_received_at {
            self.execution.control_queue.observe(received.elapsed());
        }
        if let Some(reply) = event.reply {
            let frame = event.frame;
            #[cfg(feature = "jolteon-admission-pipeline")]
            if frame.kind == wire::SUBMIT {
                return self.queue_client_admission(
                    frame.body,
                    event.submit_received_at.unwrap_or_else(Instant::now),
                    reply,
                );
            }
            let started = event.submit_received_at.map(|received| {
                let started = Instant::now();
                self.admission
                    .client_queue
                    .observe(started.duration_since(received));
                started
            });
            let result = self
                .client(frame.kind, frame.body)
                .unwrap_or_else(|error| json!({"error": format!("{error:#}")}));
            let bytes = serde_json::to_vec(&result)?;
            if let Some(started) = started {
                self.admission.client_service.observe(started.elapsed());
            }
            let _ = reply.send(bytes);
            return Ok(());
        }
        if event.frame.kind == wire::PEER_ENDPOINT {
            return self.learn_endpoint(event);
        }
        let peer = self.directory.sender(&event).context("unknown peer route")?;
        let frame = event.frame;
        let epoch = self.committee.epoch();
        if frame.kind == wire::GET {
            let point = Point::decode(&frame.body)?;
            if let Some(raw) = self
                .store
                .consensus_record(&proposal_key(frame.epoch, point))
                .context(StorageFault)?
            {
                self.net.send_at(frame.epoch, peer, wire::PROPOSAL, raw);
            }
            return Ok(());
        }
        if frame.kind == wire::GET_HANDOFF {
            if let Some(raw) = self
                .store
                .consensus_record(&handoff_key(frame.epoch))
                .context(StorageFault)?
            {
                self.net.send_at(frame.epoch, peer, wire::HANDOFF, raw);
            }
            return Ok(());
        }
        if frame.kind == wire::GET_REPLAY {
            return self.serve_replay(frame.epoch, peer, &frame.body);
        }
        if frame.epoch != epoch {
            if frame.epoch > epoch
                && self
                    .handoff_requested
                    .is_none_or(|time| time.elapsed() >= Duration::from_millis(500))
            {
                self.handoff_requested = Some(Instant::now());
                self.net.broadcast(wire::GET_HANDOFF, Vec::new());
            }
            return Ok(());
        }
        match frame.kind {
            wire::HANDOFF => {
                if self.handoff_tip.is_none() {
                    let (_, _, seal) = self.committee.verify_handoff(&frame.body)?;
                    self.store
                        .persist_consensus(&[(known_handoff_key(epoch), frame.body.to_vec())])
                        .context(StorageFault)?;
                    self.handoff_tip = Some(seal);
                }
            }
            wire::PROPOSAL => self.admit_proposal(wire::canonical_bytes(frame.body)?, peer)?,
            wire::REPLAY => self.accept_replay(peer, &frame.body)?,
            wire::VOTE => {
                let (point, share) = frame
                    .body
                    .split_at_checked(Point::SIZE)
                    .context("truncated vote")?;
                self.accept_vote(Point::decode(point)?, Share::decode(share)?)?;
            }
            wire::QUORUM => {
                let qc = self.qc(&frame.body)?;
                let point = qc.point;
                self.learn_quorum(qc)?;
                if point.view > self.finalized.view && !self.candidates.contains_key(&point) {
                    self.request(point, Some(peer));
                }
            }
            wire::TIMEOUT => self.accept_timeout(&frame.body)?,
            wire::TIMEOUT_CERTIFICATE => {
                let tc = TimeoutCertificate::decode(&frame.body, self.committee.size())?;
                if self
                    .certificate
                    .as_ref()
                    .is_none_or(|old| old.view < tc.view)
                {
                    self.committee.verify_timeout_certificate(&tc)?;
                    self.learn_tc(tc)?;
                }
            }
            wire::RELAY_TX => {
                self.insert_tx(&frame.body, false)?;
            }
            _ => anyhow::bail!("unknown replica message"),
        }
        Ok(())
    }

    fn insert_tx(&mut self, raw: &Bytes, client: bool) -> Result<Hash32> {
        let requests = if client {
            &mut self.admission.client_requests
        } else {
            &mut self.admission.relayed_requests
        };
        *requests = requests.saturating_add(1);
        let id = ckb_hash::blake2b_256(raw);
        if self.mempool.contains(&id) {
            return Ok(id);
        }
        self.admission_capacity(raw.len())?;
        let tx = self.check_admission(raw)?;
        self.mempool.insert(id, tx)?;
        Ok(id)
    }

    fn admission_capacity(&self, bytes: usize) -> Result<()> {
        ensure!(
            bytes <= stem_ledger::MAX_TRANSACTION_BYTES
                && self.mempool.bytes().saturating_add(bytes) <= MAX_MEMPOOL_BYTES,
            "mempool capacity exceeded"
        );
        Ok(())
    }

    fn check_admission(&mut self, raw: &Bytes) -> Result<Transaction> {
        let tx = Transaction::from_bytes(raw.clone())?;
        let started = Instant::now();
        let checked = self.store.check(&tx);
        self.record_admission_check(started.elapsed().as_micros().try_into().unwrap_or(u64::MAX));
        checked?;
        Ok(tx)
    }

    fn record_admission_check(&mut self, verification_us: u64) {
        self.admission.checks = self.admission.checks.saturating_add(1);
        self.admission.verification_us = self
            .admission
            .verification_us
            .saturating_add(verification_us);
    }

    fn admitted_client(&mut self, id: Hash32, body: Bytes) -> serde_json::Value {
        if self
            .mempool
            .track_local(id, Instant::now(), self.committee.epoch())
        {
            self.net.relay(&self.relay_leaders(), body);
        }
        json!({"admitted": hex::encode(id)})
    }

    fn client(&mut self, kind: u32, body: Bytes) -> Result<serde_json::Value> {
        let tip = self.store.tip();
        match kind {
            wire::STATUS => {
                let status = json!({
                    "height": tip.context.height, "epoch": self.committee.epoch(), "view": self.view,
                    "high_view": self.high.point.view, "member": self.member,
                    "voting_power": self.member.map(|member| self.committee.power(member)).transpose()?,
                    "app_hash": hex::encode(tip.app_hash(self.store.network()).as_slice()),
                    "committed_transactions": self.journal.committed_transactions,
                    "timeout_view": self.certificate.as_ref().map_or(0, |tc| tc.view),
                    "timeout_views": self.timeout_views,
                    "last_untimely_view": self.last_untimely_view,
                    "catching_up": self.recovery.active,
                    "replayed_blocks": self.recovery.blocks,
                    "observer_only": self.journal.observer_only,
                    "history_window": self.store.history_window(),
                    "sent_bytes": self.net.traffic.sent.load(Ordering::Relaxed),
                    "received_bytes": self.net.traffic.received.load(Ordering::Relaxed),
                    "transport_drops": self.net.traffic.dropped.load(Ordering::Relaxed),
                    "invalid_messages": self.rejected,
                    "mempool_transactions": self.mempool.len(),
                    "mempool_bytes": self.mempool.bytes(),
                    "admission": self.admission,
                    "execution": self.execution,
                    "verification_cache": self.store.verification_cache_stats(),
                });
                #[cfg(feature = "jolteon-certificate-experiment")]
                let status = {
                    let mut value = status;
                    value["certificate_experiment"] = json!(&self.certificate_experiment);
                    value
                };
                #[cfg(feature = "jolteon-admission-pipeline")]
                let status = {
                    let mut value = status;
                    value["admission_pipeline"] = json!(self.pipeline.status());
                    value
                };
                Ok(status)
            }
            wire::SUBMIT => {
                let id = self.insert_tx(&body, true)?;
                Ok(self.admitted_client(id, body))
            }
            wire::HEADER => {
                let height = u64::from_le_bytes(body.as_ref().try_into()?);
                let header = self
                    .store
                    .header(height)?
                    .context("no finalized Header at that height")?;
                Ok(json!({
                    "height": height,
                    "header": hex::encode(header.encode()),
                    "app_hash": hex::encode(header.app_hash(self.store.network()).as_slice()),
                }))
            }
            wire::FINALITY => {
                let height = u64::from_le_bytes(body.as_ref().try_into()?);
                let proof = self
                    .store
                    .consensus_record(&finality_key(height))?
                    .context("no finalized proof at that height")?;
                let proof: wire::Finalized = wire::canonical(&proof)?;
                Ok(
                    json!({"proof": hex::encode(proof.proof), "epoch": proof.epoch, "handoff": proof.handoff}),
                )
            }
            wire::HANDOFF_PROOF => {
                let epoch = u64::from_le_bytes(body.as_ref().try_into()?);
                let proof = self
                    .store
                    .consensus_record(&handoff_key(epoch))?
                    .context("no handoff from that epoch")?;
                Ok(json!({"proof": hex::encode(proof)}))
            }
            wire::QUERY => {
                ensure!(body.len() <= 1024, "oversized native query");
                let query: Query = serde_json::from_slice(&body)?;
                let data = query.data.as_ref().map_or(&[][..], |data| data.as_bytes());
                let (height, value) = crate::native_query::query(
                    &self.store,
                    &query.path,
                    data,
                    query.height,
                    query.prove,
                )?;
                Ok(json!({"height": height, "value": hex::encode(value)}))
            }
            _ => anyhow::bail!("unknown client request"),
        }
    }
}

fn members(store: &Store) -> Result<Vec<Member>> {
    store
        .genesis_validators()
        .iter()
        .map(|(key, power)| {
            Ok(Member {
                key: *key,
                power: u64::try_from(*power)?,
            })
        })
        .collect()
}

fn journal_identity(genesis: &Committee, public: Hash32) -> Hash32 {
    let identity = genesis.proposal_digest(Point {
        view: 0,
        parent_view: 0,
        header: public,
    });
    ckb_hash::blake2b_256(
        [
            b"Stem Jolteon ordered history journal\0".as_slice(),
            &identity,
        ]
        .concat(),
    )
}

pub fn verify_finality(
    store: &Store,
    handoffs: &[Vec<u8>],
    raw: &[u8],
    terminal: bool,
) -> Result<Header> {
    let mut verifier = stem_ledger::light::FinalityVerifier::new(
        store.network().clone(),
        store.genesis_id().clone(),
        &members(store)?,
    )?;
    for handoff in handoffs {
        verifier.accept_handoff(handoff)?;
    }
    Ok(verifier.verify(raw, terminal)?.header().clone())
}

fn proposal_key(epoch: u64, point: Point) -> Vec<u8> {
    let mut key = proposal_prefix(epoch, point.view);
    key.extend_from_slice(&point.encode()[8..]);
    key
}

fn proposal_prefix(epoch: u64, view: u64) -> Vec<u8> {
    [BODY, &epoch.to_be_bytes(), &view.to_be_bytes()].concat()
}

fn handoff_key(epoch: u64) -> Vec<u8> {
    [b"handoff".as_slice(), &epoch.to_be_bytes()].concat()
}

fn known_handoff_key(epoch: u64) -> Vec<u8> {
    [b"known-handoff".as_slice(), &epoch.to_be_bytes()].concat()
}

fn finality_key(height: u64) -> Vec<u8> {
    [b"finality".as_slice(), &height.to_be_bytes()].concat()
}

fn can_vote_at(timestamp_ms: u64) -> bool {
    now_ms().is_ok_and(|now| timely(timestamp_ms, now))
}

fn timely(timestamp_ms: u64, now: u64) -> bool {
    (now.saturating_sub(MAX_PROPOSAL_AGE_MS)..=now.saturating_add(MAX_CLOCK_LEAD_MS))
        .contains(&timestamp_ms)
}

fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}

#[cfg(test)]
mod time_tests {
    use super::timely;

    #[test]
    fn fresh_vote_window_is_bounded_in_both_directions() {
        let now = 150_000;
        assert!(timely(120_000, now));
        assert!(timely(now, now));
        assert!(timely(151_000, now));
        assert!(!timely(119_999, now));
        assert!(!timely(151_001, now));
        assert!(timely(0, 0));
        assert!(timely(u64::MAX, u64::MAX));
    }
}
