//! Blocking client for a native Stem replica's request endpoint.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use prost::Message;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use stem_ledger::{Transaction, block::Header};
use stem_stake::Hash32;

use super::wire;
use crate::native_store::{CellStreamStats, CommitmentTiming, LineageSchedule, ProposalScan};

const IDLE_SUBMISSION_CONNECTIONS: usize = 4;

pub struct Client {
    address: SocketAddr,
    timeout: Duration,
    submissions: Option<Mutex<Vec<SubmissionSocket>>>,
}

struct SubmissionSocket {
    socket: TcpStream,
    sent: usize,
    last_reply: Instant,
}

/// Local transport milestones for one submission, including failed requests.
#[derive(Debug, Default)]
pub struct SubmissionProgress {
    /// Whether the request used a socket with a previous reply.
    pub reused_connection: bool,
    /// When a socket was ready to write, including a reused socket.
    pub connected_at: Option<Instant>,
    pub frame_written_at: Option<Instant>,
    pub response_read_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct LatencyHistogram {
    pub elapsed_us: u64,
    pub buckets: [u64; 6],
}

impl LatencyHistogram {
    pub const UPPER_BOUNDS_US: [u64; 5] = [1_000, 10_000, 100_000, 500_000, 2_000_000];

    pub(super) fn observe(&mut self, elapsed: Duration) {
        let micros = elapsed.as_micros().try_into().unwrap_or(u64::MAX);
        let bucket = Self::UPPER_BOUNDS_US.partition_point(|bound| *bound < micros);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.elapsed_us = self.elapsed_us.saturating_add(micros);
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct AdmissionMetrics {
    pub client_requests: u64,
    pub relayed_requests: u64,
    pub checks: u64,
    pub verification_us: u64,
    #[serde(default)]
    pub relay_retry_attempts: u64,
    #[serde(default)]
    pub client_queue: LatencyHistogram,
    #[serde(default)]
    pub client_service: LatencyHistogram,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct ExecutionMetrics {
    pub proposal: LatencyHistogram,
    #[serde(default)]
    pub proposal_jobs: ProposalMetrics,
    #[serde(default)]
    pub proposal_scan: ProposalScan,
    #[serde(default)]
    pub proposal_commitments: CommitmentMetrics,
    pub candidate_verify: LatencyHistogram,
    #[serde(default)]
    pub candidate_commitments: CommitmentMetrics,
    #[serde(default)]
    pub candidate_lineage: LineageSchedule,
    #[serde(default)]
    pub candidate_cell_stream: CellStreamStats,
    #[serde(default)]
    pub control_queue: LatencyHistogram,
    #[serde(default)]
    pub local_timeouts: u64,
    #[serde(default)]
    pub candidate_jobs: CandidateMetrics,
    pub commit: LatencyHistogram,
    #[serde(default)]
    pub commit_jobs: CommitMetrics,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct CommitmentMetrics {
    pub total: LatencyHistogram,
    pub cells: LatencyHistogram,
    #[serde(default)]
    pub cell_keys: LatencyHistogram,
    #[serde(default)]
    pub cell_tree: LatencyHistogram,
    #[serde(default)]
    pub cell_batch: LatencyHistogram,
    pub system: LatencyHistogram,
    pub execution: LatencyHistogram,
}

impl CommitmentMetrics {
    pub(super) fn record(&mut self, timing: &CommitmentTiming) {
        self.total.observe(timing.total);
        self.cells.observe(timing.cells);
        self.cell_keys.observe(timing.cell_phases.keys);
        self.cell_tree.observe(timing.cell_phases.tree);
        self.cell_batch.observe(timing.cell_phases.batch);
        self.system.observe(timing.system);
        self.execution.observe(timing.execution);
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct CommitMetrics {
    pub acknowledged: u64,
    #[serde(default)]
    pub overlap_eligible_writes: u64,
    pub descendants: u64,
    pub status_replies: u64,
    pub deferred_events: u64,
    #[serde(default)]
    pub persistence: LatencyHistogram,
    #[serde(default)]
    pub state_prebuilds: u64,
    #[serde(default)]
    pub reused_state_build: LatencyHistogram,
    #[serde(default)]
    pub reused_write_buffers: u64,
    #[serde(default)]
    pub cell_node_publication: LatencyHistogram,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct ProposalMetrics {
    pub started: u64,
    #[serde(default)]
    pub snapshot_capture: LatencyHistogram,
    #[serde(default)]
    pub snapshot_entries: u64,
    pub published: u64,
    pub stale: u64,
    #[serde(default)]
    pub stale_us: u64,
    #[serde(default)]
    pub cancel_requested: u64,
    #[serde(default)]
    pub cancelled: u64,
    #[serde(default)]
    pub cancelled_us: u64,
    #[serde(default)]
    pub commit_overlap_started: u64,
    #[serde(default)]
    pub commit_overlap_published: u64,
    #[serde(default)]
    pub commit_overlap_us: u64,
    #[serde(default)]
    pub pre_quorum_started: u64,
    #[serde(default)]
    pub pre_quorum_published: u64,
    #[serde(default)]
    pub pre_quorum_overlap_us: u64,
    #[serde(default)]
    pub rebased: u64,
    #[serde(default)]
    pub rebased_blocks: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct CandidateMetrics {
    pub started: u64,
    pub applied: u64,
    pub stale: u64,
    #[serde(default)]
    pub stale_us: u64,
    #[serde(default)]
    pub cancel_requested: u64,
    #[serde(default)]
    pub cancelled: u64,
    #[serde(default)]
    pub cancelled_us: u64,
    pub invalid: u64,
    pub already_running: u64,
    #[serde(default)]
    pub rebased: u64,
    #[serde(default)]
    pub rebased_blocks: u64,
    #[serde(default)]
    pub commit_overlap_applied: u64,
    #[serde(default)]
    pub commit_overlap_us: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct AdmissionPipelineMetrics {
    pub dispatched: u64,
    pub coalesced: u64,
    pub full: u64,
    pub snapshot_misses: u64,
    pub rechecked: u64,
    pub pending_replies: usize,
    pub pending_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct FrameReuseMetrics {
    pub hits: u64,
    pub encodes: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct CertificateExperimentMetrics {
    pub parent_hits: u64,
    pub parent_tc_checks: u64,
    pub quorum_frame: FrameReuseMetrics,
    pub timeout_frame: FrameReuseMetrics,
    #[serde(default)]
    pub current_qc_hits: u64,
    #[serde(default)]
    pub current_tc_hits: u64,
    #[serde(default)]
    pub recent_qc_hits: u64,
    #[serde(default)]
    pub recent_qc_inserts: u64,
    #[serde(default)]
    pub vote_frame_encodes_saved: u64,
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct Status {
    pub height: u64,
    pub epoch: u64,
    pub view: u64,
    pub high_view: u64,
    pub timeout_views: Vec<u64>,
    pub last_untimely_view: u64,
    pub app_hash: String,
    pub member: Option<u16>,
    pub voting_power: Option<u64>,
    pub committed_transactions: u64,
    pub catching_up: bool,
    pub observer_only: bool,
    pub history_window: Option<u64>,
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub transport_drops: u64,
    pub invalid_messages: u64,
    pub mempool_transactions: usize,
    pub mempool_bytes: usize,
    pub admission: Option<AdmissionMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_pipeline: Option<AdmissionPipelineMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_experiment: Option<CertificateExperimentMetrics>,
    #[serde(default)]
    pub execution: ExecutionMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_cache: Option<stem_vm::VerificationCacheStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_cell_index: Option<crate::native_store::LiveCellIndexStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_cell_cache: Option<crate::native_store::PinnedCellCacheStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_node_index: Option<crate::native_store::CellNodeIndexStatus>,
}

#[derive(Debug, Deserialize)]
pub struct Certificate {
    pub epoch: u64,
    pub handoff: bool,
    #[serde(deserialize_with = "unhex")]
    pub proof: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub struct QueryResult {
    pub height: u64,
    #[serde(deserialize_with = "unhex")]
    pub value: Vec<u8>,
}

#[derive(Deserialize)]
struct Admission {
    admitted: String,
}

#[derive(Deserialize)]
struct HeaderResult {
    height: u64,
    #[serde(deserialize_with = "unhex")]
    header: Vec<u8>,
}

#[derive(Deserialize)]
struct Handoff {
    #[serde(deserialize_with = "unhex")]
    proof: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Response<T> {
    Failure { error: String },
    Success(T),
}

impl Client {
    pub fn new(address: SocketAddr, timeout: Duration) -> Self {
        Self {
            address,
            timeout,
            submissions: None,
        }
    }

    /// Reuse up to four idle submission sockets to a current Stem replica.
    /// Transport errors return to the caller without an automatic resubmission.
    pub fn with_submission_reuse(mut self) -> Self {
        self.submissions = Some(Mutex::default());
        self
    }

    pub fn status(&self) -> Result<Status> {
        self.request(wire::STATUS, Vec::new())
    }

    /// A successful reply confirms admission. Fetch a finality proof for confirmation.
    pub fn submit(&self, transaction: &Transaction) -> Result<Hash32> {
        self.submit_inner(transaction, None)
    }

    /// Submit once and record the transport milestones reached by the request.
    pub fn submit_with_progress(
        &self,
        transaction: &Transaction,
        progress: &mut SubmissionProgress,
    ) -> Result<Hash32> {
        *progress = SubmissionProgress::default();
        self.submit_inner(transaction, Some(progress))
    }

    fn submit_inner(
        &self,
        transaction: &Transaction,
        progress: Option<&mut SubmissionProgress>,
    ) -> Result<Hash32> {
        let Admission { admitted } =
            self.request_with_progress(wire::SUBMIT, transaction.as_bytes(), progress)?;
        let id = ckb_hash::blake2b_256(transaction.as_slice());
        ensure!(
            hex::decode(admitted)? == id,
            "replica returned a different transaction digest"
        );
        Ok(id)
    }

    pub fn header(&self, height: u64) -> Result<Header> {
        let reply: HeaderResult = self.request(wire::HEADER, height.to_le_bytes().to_vec())?;
        let header = Header::decode(&reply.header)?;
        ensure!(
            reply.height == height && header.context.height == height,
            "replica returned a different Header height"
        );
        Ok(header)
    }

    pub fn finality(&self, height: u64) -> Result<Certificate> {
        self.request(wire::FINALITY, height.to_le_bytes().to_vec())
    }

    pub fn handoff(&self, epoch: u64) -> Result<Vec<u8>> {
        let Handoff { proof } = self.request(wire::HANDOFF_PROOF, epoch.to_le_bytes().to_vec())?;
        Ok(proof)
    }

    pub fn query(&self, path: &str, height: u64, data: &[u8], prove: bool) -> Result<QueryResult> {
        self.request(wire::QUERY, serde_json::to_vec(&json!({
            "path": path, "height": height, "data": format!("0x{}", hex::encode(data)), "prove": prove,
        }))?)
    }

    fn request<T: DeserializeOwned>(&self, kind: u32, body: Vec<u8>) -> Result<T> {
        self.request_with_progress(kind, body.into(), None)
    }

    fn request_with_progress<T: DeserializeOwned>(
        &self,
        kind: u32,
        body: Bytes,
        mut progress: Option<&mut SubmissionProgress>,
    ) -> Result<T> {
        let frame = wire::Frame {
            sender: wire::CLIENT,
            kind,
            body,
            epoch: 0,
        };
        let length = frame.encoded_len();
        ensure!(length <= wire::MAX_FRAME, "oversized request");
        let mut packet = Vec::with_capacity(4 + length);
        packet.extend_from_slice(&(length as u32).to_be_bytes());
        frame.encode(&mut packet)?;
        let pool = self.submissions.as_ref().filter(|_| kind == wire::SUBMIT);
        let previous = pool.and_then(|pool| {
            let mut idle = pool.lock().expect("submission pool");
            idle.retain(|connection| connection.last_reply.elapsed() < wire::SUBMIT_IDLE / 2);
            idle.pop()
        });
        let (mut socket, sent) = match previous {
            Some(connection) => (connection.socket, connection.sent),
            None => {
                let socket = TcpStream::connect_timeout(&self.address, self.timeout)?;
                socket.set_write_timeout(Some(self.timeout))?;
                socket.set_read_timeout(Some(self.timeout))?;
                if pool.is_some() {
                    socket.set_nodelay(true)?;
                }
                (socket, 0)
            }
        };
        if let Some(progress) = progress.as_deref_mut() {
            progress.connected_at = Some(Instant::now());
            progress.reused_connection = sent != 0;
        }
        let response = exchange(&mut socket, &packet, progress)?;
        if let Some(pool) = pool
            && sent + 1 < wire::SUBMIT_SESSION_REQUESTS
        {
            let mut idle = pool.lock().expect("submission pool");
            if idle.len() < IDLE_SUBMISSION_CONNECTIONS {
                idle.push(SubmissionSocket {
                    socket,
                    sent: sent + 1,
                    last_reply: Instant::now(),
                });
            }
        }
        match response {
            Response::Failure { error } => bail!("{error}"),
            Response::Success(response) => Ok(response),
        }
    }
}

fn exchange<T: DeserializeOwned>(
    socket: &mut TcpStream,
    packet: &[u8],
    mut progress: Option<&mut SubmissionProgress>,
) -> Result<Response<T>> {
    socket.write_all(packet)?;
    if let Some(progress) = progress.as_deref_mut() {
        progress.frame_written_at = Some(Instant::now());
    }
    let mut length = [0; 4];
    socket.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    ensure!(
        (1..=wire::MAX_FRAME).contains(&length),
        "invalid response length"
    );
    let mut bytes = vec![0; length];
    socket.read_exact(&mut bytes)?;
    if let Some(progress) = progress {
        progress.response_read_at = Some(Instant::now());
    }
    serde_json::from_slice(&bytes).context("invalid replica response")
}

fn unhex<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Vec<u8>, D::Error> {
    let text = String::deserialize(decoder)?;
    hex::decode(text).map_err(serde::de::Error::custom)
}
