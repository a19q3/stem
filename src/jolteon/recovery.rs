//! Recover a long gap by authenticating and executing bounded historical branches.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use stem_stake::jolteon::{FinalityProof, Point, Quorum};

use super::{MAX_BRANCH, Replica, StorageFault, VerifiedQuorum, finality_key, wire};

const RETRY: Duration = Duration::from_millis(180);
const PROOF_DEADLINE: Duration = Duration::from_secs(4);
const RESUME_GAP: u64 = 24;

#[derive(Debug)]
pub(super) struct BranchHorizon;

impl std::fmt::Display for BranchHorizon {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.write_str("candidate exceeds the bounded replay horizon")
    }
}

impl std::error::Error for BranchHorizon {}

#[derive(Default)]
pub(super) struct Recovery {
    pub(super) active: bool,
    pub(super) blocks: u64,
    request_at: Option<Instant>,
    next_peer: usize,
    target: Option<(VerifiedQuorum, Instant)>,
    progressed: bool,
}

impl Recovery {
    pub(super) fn reset(&mut self) {
        self.active = false;
        self.request_at = None;
        self.target = None;
        self.progressed = false;
    }

    #[cfg(feature = "jolteon-candidate-pipeline")]
    pub(super) fn preparation_target(&self) -> Option<Point> {
        self.target
            .as_ref()
            .filter(|(_, received)| received.elapsed() < PROOF_DEADLINE)
            .map(|(quorum, _)| quorum.point)
    }

    pub(super) fn record_commit(&mut self, point: Point, blocks: usize) {
        if self
            .target
            .as_ref()
            .is_some_and(|(qc, _)| qc.point == point)
        {
            self.target = None;
            self.request_at = None;
            self.blocks = self.blocks.saturating_add(blocks as u64);
            self.progressed = true;
        }
    }
}

impl Replica {
    pub(super) fn finalization_head(&mut self) -> Option<VerifiedQuorum> {
        let latest = self.handoff_tip.as_ref().unwrap_or(&self.high);
        let tip = self.store.tip().context.height;
        let gap = self
            .candidates
            .get(&latest.point)
            .map(|candidate| candidate.proposal.header.context.height.saturating_sub(tip));
        if !self.recovery.active && gap.is_none_or(|gap| gap <= MAX_BRANCH as u64) {
            return Some(latest.clone());
        }
        if let Some((target, received)) = &self.recovery.target
            && received.elapsed() < PROOF_DEADLINE
        {
            return Some(target.clone());
        }
        self.recovery.target = None;
        if self.recovery.progressed && gap.is_some_and(|gap| gap <= RESUME_GAP) {
            let latest = latest.clone();
            self.recovery.reset();
            return Some(latest);
        }
        self.request_replay();
        None
    }

    pub(super) fn request_replay(&mut self) {
        self.recovery.active = true;
        let Some(height) = self.store.tip().context.height.checked_add(1) else {
            return;
        };
        if self
            .recovery
            .request_at
            .is_some_and(|at| at.elapsed() < RETRY)
        {
            return;
        }
        let count = self.directory.slots();
        if count < 2 {
            return;
        }
        let Some(peer) = (0..count)
            .map(|offset| ((self.recovery.next_peer + offset) % count) as u16)
            .find(|&peer| self.net.has_route(peer))
        else {
            return;
        };
        self.recovery.next_peer = (usize::from(peer) + 1) % count;
        self.recovery.request_at = Some(Instant::now());
        self.net
            .send(peer, wire::GET_REPLAY, height.to_le_bytes().to_vec());
    }

    pub(super) fn serve_replay(&self, epoch: u64, peer: u16, raw: &[u8]) -> Result<()> {
        let height = u64::from_le_bytes(raw.try_into()?);
        if let Some(proof) = self
            .store
            .consensus_record(&finality_key(height))
            .context(StorageFault)?
            && wire::canonical::<wire::Finalized>(&proof)?.epoch == epoch
        {
            self.net.send_at(epoch, peer, wire::REPLAY, proof);
        }
        Ok(())
    }

    pub(super) fn accept_replay(&mut self, peer: u16, raw: &[u8]) -> Result<()> {
        if !self.recovery.active || self.recovery.target.is_some() {
            return Ok(());
        }
        ensure!(
            raw.len() <= 256 * 1024,
            "oversized historical finality proof"
        );
        let record: wire::Finalized = wire::canonical(raw)?;
        ensure!(
            record.epoch == self.committee.epoch(),
            "wrong recovery epoch"
        );
        let (header, child) = if record.handoff {
            let (_, header, seal) = self.committee.verify_handoff(&record.proof)?;
            (header, seal)
        } else {
            let header = self.committee.verify_finality(&record.proof)?;
            let proof = <FinalityProof as prost::Message>::decode(record.proof.as_slice())?;
            (
                header,
                Quorum::decode(&proof.child_qc, self.committee.size())?,
            )
        };
        // A responder may send a later proof if it finalized several ancestors together.
        // Its locally replayed parents must still descend from our exact finalized point.
        ensure!(
            child.point.view > self.finalized.view
                && header
                    .height()
                    .checked_sub(self.store.tip().context.height)
                    .is_some_and(|gap| (1..MAX_BRANCH as u64).contains(&gap)),
            "historical proof falls outside the requested replay window"
        );
        let child: VerifiedQuorum = child.into();
        self.learn_quorum(child.clone())?;
        self.request(child.point, Some(peer));
        self.recovery.target = Some((child, Instant::now()));
        // Prefer a peer that has just demonstrated access to the finalized history.
        self.recovery.next_peer = usize::from(peer);
        Ok(())
    }
}
