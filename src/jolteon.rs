//! A weighted Jolteon replica using the native execution and storage paths.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use ckb_types::prelude::*;
use ed25519_dalek::{Signer, SigningKey};
use prost::Message;
use serde::Deserialize;
use serde_json::json;
use stem_ledger::{Transaction, block::Header};
use stem_stake::{
    Hash32, Member,
    jolteon::{Committee, FinalityProof, Point, Quorum, Share, TimeoutCertificate, TimeoutVote},
};

use crate::native_store::{Prepared, Store};

mod transport;
mod wire;

use transport::{Incoming, Network};

const STATE: &[u8] = b"state";
const MAX_BRANCH: usize = 192;
const MAX_PENDING: usize = 512;
const MAX_MEMPOOL_BYTES: usize = 16 * 1024 * 1024;
const MAX_PROPOSAL_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Peer addresses in the public-key order of the genesis committee.
    pub peers: Vec<SocketAddr>,
    pub view_timeout_ms: u64,
    pub proposal_interval_ms: u64,
    #[serde(default = "worker_count")]
    pub vm_workers: usize,
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
    source: u16,
}

struct Replica {
    store: Store,
    committee: Committee,
    member: u16,
    key: SigningKey,
    net: Network,
    workers: rayon::ThreadPool,
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
    candidates: BTreeMap<Point, Candidate>,
    preferred: BTreeMap<u64, Point>,
    votes: BTreeMap<Point, BTreeMap<u16, [u8; 64]>>,
    observed_votes: HashMap<(u64, u16), (Point, Share)>,
    timeouts: BTreeMap<u64, BTreeMap<u16, (Quorum, Share)>>,
    certificates: HashSet<Hash32>,
    requests: HashMap<Point, Instant>,
    mempool: BTreeMap<Hash32, Transaction>,
    mempool_bytes: usize,
    rejected: u64,
    timeout_views: VecDeque<u64>,
}

pub fn run(store: Store, key: SigningKey, config: Config, initialize: bool) -> Result<()> {
    let members = members(&store)?;
    let committee = Committee::new(
        store.network().unpack(),
        store.genesis_id().unpack(),
        0,
        &members,
    )?;
    let public = key.verifying_key().to_bytes();
    let member = u16::try_from(
        members
            .binary_search_by_key(&public, |m| m.key)
            .ok()
            .context("signing key is outside the genesis committee")?,
    )?;
    ensure!(
        config.peers.len() == committee.size()
            && config.peers.iter().collect::<HashSet<_>>().len() == committee.size(),
        "peer list must name every validator exactly once"
    );
    ensure!(
        (200..=60_000).contains(&config.view_timeout_ms)
            && config.proposal_interval_ms < config.view_timeout_ms / 2
            && (1..=64).contains(&config.vm_workers),
        "invalid replica timing or VM worker configuration"
    );

    let identity = committee.proposal_digest(Point {
        view: 0,
        parent_view: 0,
        header: public,
    });
    let journal = match store.consensus_record(STATE)? {
        Some(raw) => {
            ensure!(!initialize, "validator journal already initialized");
            let journal: wire::Journal = wire::canonical(&raw)?;
            ensure!(
                journal.identity == identity,
                "validator journal identity mismatch"
            );
            journal
        }
        None => {
            ensure!(
                initialize && store.tip().context.height == 0,
                "missing validator journal"
            );
            let genesis = committee.genesis_quorum();
            let journal = wire::Journal {
                identity: identity.to_vec(),
                high: genesis.encode(),
                finalized: genesis.point.encode().to_vec(),
                ..Default::default()
            };
            store.persist_consensus(&[(STATE.to_vec(), journal.encode_to_vec())])?;
            journal
        }
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
        ensure!(vote.member == member, "wrong persisted timeout signer");
        committee.verify_share(&committee.timeout_digest(timeout.view, qc.point), vote)?;
    }

    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(async {
            let (net, mut incoming) = Network::listen(member, &config.peers).await?;
            let mut replica = Replica {
                store,
                committee,
                member,
                key,
                net,
                journal,
                high,
                certificate,
                finalized,
                view: 0,
                since: Instant::now(),
                announced: Instant::now(),
                timeout: Duration::from_millis(config.view_timeout_ms),
                proposal_interval: Duration::from_millis(config.proposal_interval_ms),
                backoff: 0,
                workers: rayon::ThreadPoolBuilder::new()
                    .num_threads(config.vm_workers)
                    .build()?,
                candidates: BTreeMap::new(),
                preferred: BTreeMap::new(),
                votes: BTreeMap::new(),
                observed_votes: HashMap::new(),
                timeouts: BTreeMap::new(),
                certificates: HashSet::new(),
                requests: HashMap::new(),
                mempool: BTreeMap::new(),
                mempool_bytes: 0,
                rejected: 0,
                timeout_views: VecDeque::new(),
            };
            replica.enter();
            let mut ticker = tokio::time::interval(Duration::from_millis(10));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => replica.tick()?,
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
            records.push((proposal_key(point), proposal.encode_to_vec()));
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
        ensure!(
            header.context.parent.as_slice() == parent.point.header,
            "proposal does not extend its certified Header"
        );
        let point = Point {
            view: proposal.view,
            parent_view: parent.point.view,
            header: header.id(self.store.network()).unpack(),
        };
        Point::decode(&point.encode())?;
        let tc = (!proposal.timeout.is_empty())
            .then(|| TimeoutCertificate::decode(&proposal.timeout, self.committee.size()))
            .transpose()?;
        self.committee
            .verify_parent_view(proposal.view, parent.point, tc.as_ref())?;
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
        if let Some(peer) = peer.filter(|&peer| peer != self.member && !repeated) {
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
                point.view > self.finalized.view && branch.len() < MAX_BRANCH,
                "candidate does not extend the committed branch horizon"
            );
            if !self.candidates.contains_key(&point) {
                if let Some(raw) = self
                    .store
                    .consensus_record(&proposal_key(point))
                    .context(StorageFault)?
                {
                    let proposal = wire::canonical(&raw)?;
                    let (actual, checked, _) = self.check_proposal(proposal)?;
                    ensure!(actual == point, "wrong persisted candidate");
                    self.candidates.insert(
                        point,
                        Candidate {
                            proposal: checked,
                            execution: None,
                            source: self.member,
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

    fn execute(&mut self, branch: &[Point]) -> Result<Vec<Arc<Prepared>>> {
        let mut batches = Vec::with_capacity(branch.len());
        for point in branch {
            let candidate = self
                .candidates
                .get_mut(point)
                .context("candidate missing from branch")?;
            let proposal = &candidate.proposal;
            if candidate.execution.is_none() {
                let transactions = proposal
                    .wire
                    .transactions
                    .iter()
                    .map(|bytes| Transaction::from_slice(bytes))
                    .collect::<Result<Vec<_>>>()?;
                let prepared = self.workers.install(|| {
                    self.store.on_branch(&batches, |state| {
                        state.prepare(&transactions, proposal.header.context.timestamp_ms, true)
                    })
                })?;
                ensure!(
                    prepared.header() == &proposal.header,
                    "candidate execution differs from the signed Header"
                );
                ensure!(
                    prepared.validator_updates().is_empty(),
                    "committee handoff is not enabled in this replica"
                );
                candidate.execution = Some(Arc::new(prepared));
            }
            batches.push(candidate.execution.clone().expect("prepared candidate"));
        }
        Ok(batches)
    }

    fn finalize(&mut self) -> Result<()> {
        let high = self.high.clone();
        if high.point.view <= self.finalized.view {
            return Ok(());
        }
        let Some(branch) = self.branch(high.point)? else {
            return Ok(());
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
        let proof = FinalityProof {
            parent_header: self.candidates[&parent].proposal.wire.header.clone(),
            parent_qc: child.parent.encode(),
            child_header: child.wire.header.clone(),
            child_qc: high.encode(),
        }
        .encode_to_vec();
        let batches = self.execute(&branch)?;
        // Keep the child available before committing any certified ancestors.
        self.save(Some((
            high.point,
            &self.candidates[&high.point].proposal.wire,
        )))?;
        for (point, prepared) in branch.iter().zip(batches).take(branch.len() - 1) {
            let proposal = &self.candidates[point].proposal.wire;
            self.journal.finalized = point.encode().to_vec();
            self.journal.lock = point.view;
            self.journal.committed_transactions = self
                .journal
                .committed_transactions
                .checked_add(proposal.transactions.len() as u64)
                .context("transaction counter overflow")?;
            self.store
                .commit_certified(
                    &prepared,
                    &[
                        (STATE.to_vec(), self.journal.encode_to_vec()),
                        (proposal_key(*point), proposal.encode_to_vec()),
                        (
                            finality_key(prepared.header().context.height),
                            proof.clone(),
                        ),
                    ],
                )
                .context(StorageFault)?;
            self.finalized = *point;
            for tx in &proposal.transactions {
                let id = ckb_hash::blake2b_256(tx);
                if let Some(removed) = self.mempool.remove(&id) {
                    self.mempool_bytes -= removed.as_slice().len();
                }
            }
        }
        let view = self.finalized.view;
        self.candidates.retain(|point, _| point.view > view);
        self.preferred.retain(|slot, _| *slot > view);
        self.votes.retain(|point, _| point.view > view);
        self.timeouts.retain(|slot, _| *slot > view);
        self.observed_votes
            .retain(|(slot, _), _| slot.saturating_add(256) >= view);
        Ok(())
    }

    fn vote_for(&mut self, point: Point) -> Result<()> {
        if point.view != self.view
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
        ensure!(
            self.candidates[&point].proposal.header.context.timestamp_ms
                <= now_ms()?.saturating_add(30_000),
            "proposal timestamp is too far in the future"
        );
        self.execute(&branch)?;
        self.journal.vote = point.encode().to_vec();
        self.save(Some((point, &self.candidates[&point].proposal.wire)))?;
        self.dispatch_vote(point)
    }

    fn dispatch_vote(&mut self, point: Point) -> Result<()> {
        let share = Share {
            member: self.member,
            signature: self.key.sign(&self.committee.vote_digest(point)).to_bytes(),
        };
        let body = [point.encode().as_slice(), &share.encode()].concat();
        let collector = self.committee.leader(point.view)?;
        let next = self.committee.leader(point.view.saturating_add(1))?;
        self.net.send(collector, wire::VOTE, body.clone());
        if next != collector {
            self.net.send(next, wire::VOTE, body);
        }
        if self.member == collector || self.member == next {
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
        if !self.journal.timeout.is_empty() {
            let prior: wire::Timeout = wire::canonical(&self.journal.timeout)?;
            if prior.view >= self.view {
                return Ok(());
            }
        }
        let high = self.high.point;
        let share = Share {
            member: self.member,
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
        self.net.broadcast(wire::TIMEOUT, raw.clone());
        self.accept_timeout(&raw)
    }

    fn propose(&mut self) -> Result<()> {
        if self.committee.leader(self.view)? != self.member
            || self.preferred.contains_key(&self.view)
        {
            return Ok(());
        }
        if !self.journal.vote.is_empty() {
            let prior = Point::decode(&self.journal.vote)?;
            if prior.view >= self.view {
                return Ok(());
            }
        }
        if !self.journal.timeout.is_empty() {
            let prior: wire::Timeout = wire::canonical(&self.journal.timeout)?;
            if prior.view >= self.view {
                return Ok(());
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
                return Ok(());
            };
            tc.encode()
        };
        let Some(branch) = self.branch(parent.point)? else {
            return Ok(());
        };
        let batches = self.execute(&branch)?;
        let (transactions, execution) = self.workers.install(|| {
            self.store.on_branch(&batches, |state| {
                let at = now_ms()?.max(state.tip().context.timestamp_ms);
                let selected = state.select(
                    self.mempool.values().cloned().collect(),
                    at,
                    MAX_PROPOSAL_BYTES,
                )?;
                let prepared = state.prepare(&selected, at, true)?;
                Ok((selected, prepared))
            })
        })?;
        ensure!(
            execution.validator_updates().is_empty(),
            "committee handoff is not enabled in this replica"
        );
        let point = Point {
            view: self.view,
            parent_view: parent.point.view,
            header: execution.header().id(self.store.network()).unpack(),
        };
        let proposal = wire::Proposal {
            view: self.view,
            header: execution.header().encode().to_vec(),
            parent: parent.encode(),
            timeout: tc,
            transactions: transactions
                .iter()
                .map(|tx| tx.as_slice().to_vec())
                .collect(),
            signature: self
                .key
                .sign(&self.committee.proposal_digest(point))
                .to_bytes()
                .to_vec(),
        };
        self.admit_proposal(proposal.clone(), self.member)?;
        self.candidates.get_mut(&point).unwrap().execution = Some(Arc::new(execution));
        self.journal.vote = point.encode().to_vec();
        self.save(Some((point, &proposal)))?;
        self.net.broadcast(wire::PROPOSAL, proposal.encode_to_vec());
        self.dispatch_vote(point)
    }

    fn tick(&mut self) -> Result<()> {
        self.finalize()?;
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
            self.net.broadcast(wire::QUORUM, self.high.encode());
            if let Some(tc) = self
                .certificate
                .as_ref()
                .filter(|tc| tc.view >= self.high.point.view)
            {
                self.net.broadcast(wire::TIMEOUT_CERTIFICATE, tc.encode());
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
                        .consensus_record(&proposal_key(vote))
                        .context(StorageFault)?
                        && self.committee.leader(vote.view)? == self.member
                    {
                        self.net.broadcast(wire::PROPOSAL, raw);
                    }
                    self.dispatch_vote(vote)?;
                }
            }
        }
        Ok(())
    }

    fn handle(&mut self, event: Incoming) -> Result<()> {
        let frame = event.frame;
        if let Some(reply) = event.reply {
            let result = self
                .client(frame.kind, frame.body)
                .unwrap_or_else(|error| json!({"error": format!("{error:#}")}));
            let _ = reply.send(serde_json::to_vec(&result)?);
            return Ok(());
        }
        let peer = u16::try_from(frame.sender)?;
        self.committee.power(peer)?;
        match frame.kind {
            wire::PROPOSAL => self.admit_proposal(wire::canonical(&frame.body)?, peer)?,
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
            wire::GET => {
                let point = Point::decode(&frame.body)?;
                if let Some(raw) = self
                    .store
                    .consensus_record(&proposal_key(point))
                    .context(StorageFault)?
                {
                    self.net.send(peer, wire::PROPOSAL, raw);
                }
            }
            wire::RELAY_TX => {
                self.insert_tx(frame.body)?;
            }
            _ => anyhow::bail!("unknown replica message"),
        }
        Ok(())
    }

    fn insert_tx(&mut self, raw: Vec<u8>) -> Result<bool> {
        let id = ckb_hash::blake2b_256(&raw);
        if self.mempool.contains_key(&id) {
            return Ok(false);
        }
        ensure!(
            raw.len() <= stem_ledger::MAX_TRANSACTION_BYTES
                && self.mempool_bytes.saturating_add(raw.len()) <= MAX_MEMPOOL_BYTES,
            "mempool capacity exceeded"
        );
        let tx = Transaction::from_slice(&raw)?;
        self.workers.install(|| self.store.check(&tx))?;
        self.mempool_bytes += raw.len();
        self.mempool.insert(id, tx);
        Ok(true)
    }

    fn client(&mut self, kind: u32, body: Vec<u8>) -> Result<serde_json::Value> {
        let tip = self.store.tip();
        match kind {
            wire::STATUS => Ok(json!({
                "height": tip.context.height, "view": self.view, "high_view": self.high.point.view, "member": self.member,
                "app_hash": hex::encode(tip.app_hash(self.store.network()).as_slice()),
                "committed_transactions": self.journal.committed_transactions,
                "timeout_view": self.certificate.as_ref().map_or(0, |tc| tc.view),
                "timeout_views": self.timeout_views,
                "sent_bytes": self.net.traffic.sent.load(Ordering::Relaxed),
                "received_bytes": self.net.traffic.received.load(Ordering::Relaxed),
                "transport_drops": self.net.traffic.dropped.load(Ordering::Relaxed),
                "invalid_messages": self.rejected,
            })),
            wire::SUBMIT => {
                let id = hex::encode(ckb_hash::blake2b_256(&body));
                if self.insert_tx(body.clone())? {
                    self.net.broadcast(wire::RELAY_TX, body);
                }
                Ok(json!({"admitted": id}))
            }
            wire::HEADER => {
                let height = u64::from_le_bytes(body.as_slice().try_into()?);
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
                let height = u64::from_le_bytes(body.as_slice().try_into()?);
                let proof = self
                    .store
                    .consensus_record(&finality_key(height))?
                    .context("no finalized proof at that height")?;
                Ok(json!({"proof": hex::encode(proof)}))
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

pub fn verify_finality(store: &Store, raw: &[u8]) -> Result<Header> {
    let committee = Committee::new(
        store.network().unpack(),
        store.genesis_id().unpack(),
        0,
        &members(store)?,
    )?;
    Header::decode(committee.verify_finality(raw)?.as_bytes())
}

fn proposal_key(point: Point) -> Vec<u8> {
    [b"body".as_slice(), &point.encode()].concat()
}

fn finality_key(height: u64) -> Vec<u8> {
    [b"finality".as_slice(), &height.to_be_bytes()].concat()
}

fn now_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .try_into()?)
}
