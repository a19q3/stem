//! Authorize an objective double-vote penalty with a live native checkpoint.

use alloc::vec::Vec;
use prost::Message;

use crate::{
    Error, Hash32, Liability, Result,
    finality::{NATIVE_HEADER_BYTES, native_height, native_id},
    history::Frontier,
    patricia::{Commitment, Namespace, decode_path},
};

use super::{
    COMMITTEE_RECORD, Committee, CommitteeChange, Point, Share,
    checkpoint::{Policy, Tip, roster_id},
};

pub fn epoch_end_key(epoch: u64) -> Vec<u8> {
    [b"stake/epoch-end/".as_slice(), &epoch.to_le_bytes()].concat()
}

#[derive(Clone, Message)]
pub struct Equivocation {
    #[prost(bytes, tag = "1")]
    pub anchor: Vec<u8>,
    #[prost(bytes, tag = "2")]
    pub roster: Vec<u8>,
    #[prost(bytes, tag = "3")]
    pub roster_path: Vec<u8>,
    #[prost(bytes, tag = "4")]
    pub history_frontier: Vec<u8>,
    #[prost(bytes, tag = "5")]
    pub history_siblings: Vec<u8>,
    /// First height outside this epoch. Zero proves the epoch has no announced successor.
    #[prost(uint64, tag = "6")]
    pub end_height: u64,
    #[prost(bytes, tag = "7")]
    pub end_path: Vec<u8>,
    #[prost(bytes, tag = "8")]
    pub first: Vec<u8>,
    #[prost(bytes, tag = "9")]
    pub second: Vec<u8>,
}

struct Vote<'a> {
    point: Point,
    share: Share,
    header: &'a [u8; NATIVE_HEADER_BYTES],
}

impl<'a> Vote<'a> {
    fn decode(raw: &'a [u8]) -> Result<Self> {
        let (point, remainder) = raw.split_at_checked(Point::SIZE).ok_or(Error::Encoding)?;
        let (share, header) = remainder
            .split_at_checked(Share::SIZE)
            .ok_or(Error::Encoding)?;
        Ok(Self {
            point: Point::decode(point)?,
            share: Share::decode(share)?,
            header: header.try_into().map_err(|_| Error::Encoding)?,
        })
    }

    fn verify_header(
        &self,
        policy: &Policy,
        committee: &Committee,
        start: u64,
        end: u64,
        checkpoint_height: u64,
        liability: &Liability,
    ) -> Result<()> {
        let height = native_height(self.header);
        let height_i64 = i64::try_from(height).map_err(|_| Error::Evidence)?;
        if height < start
            || (end != 0 && height >= end)
            || height > checkpoint_height
            || liability.first_active_height <= 0
            || height_i64 < liability.first_active_height
            || liability
                .first_inactive_height
                .is_some_and(|cutoff| height_i64 >= cutoff)
        {
            return Err(Error::Evidence);
        }
        let id = native_id(&policy.network, self.header);
        if self.point.header != id
            && (height.checked_add(1) != Some(end)
                || self.point.header != committee.seal_header(id))
        {
            return Err(Error::Evidence);
        }
        Ok(())
    }
}

pub fn verify(policy: &Policy, tip: &Tip, liability: &Liability, bytes: &[u8]) -> Result<Hash32> {
    if bytes.len() > 72 * 1024 {
        return Err(Error::Encoding);
    }
    let evidence = Equivocation::decode(bytes).map_err(|_| Error::Encoding)?;
    if evidence.encode_to_vec() != bytes {
        return Err(Error::Encoding);
    }
    let roster = CommitteeChange::decode(&evidence.roster)?;
    let anchor = evidence
        .anchor
        .as_slice()
        .try_into()
        .map_err(|_| Error::Encoding)?;
    let anchor_id = native_id(&policy.network, anchor);
    let commitment = Commitment::new(policy.network, Namespace::System);
    if roster.epoch == 0 {
        if anchor_id != policy.genesis
            || native_height(anchor) != 0
            || roster_id(&evidence.roster) != policy.initial_roster
            || !evidence.roster_path.is_empty()
            || !evidence.history_frontier.is_empty()
            || !evidence.history_siblings.is_empty()
        {
            return Err(Error::Anchor);
        }
    } else {
        let height = native_height(anchor);
        let frontier = Frontier::decode(policy.network, &evidence.history_frontier)?;
        let (siblings, remaining) = evidence.history_siblings.as_chunks::<32>();
        if !remaining.is_empty()
            || height.checked_add(1) != Some(roster.activation_height)
            || frontier.count() != tip.height()
            || frontier.root().as_slice() != &tip.header[144..176]
            || !frontier.verify_inclusion(height, &anchor_id, siblings)
            || !commitment.verify(
                anchor[112..144].try_into().unwrap(),
                &commitment.record_key(COMMITTEE_RECORD),
                Some(&commitment.value_hash(&evidence.roster)),
                decode_path(&evidence.roster_path)?.into_iter(),
            )
        {
            return Err(Error::Anchor);
        }
    }
    let end = evidence.end_height;
    if end != 0 && end <= roster.activation_height {
        return Err(Error::Epoch);
    }
    let expected_end = (end != 0).then(|| commitment.value_hash(&end.to_le_bytes()));
    if !commitment.verify(
        &tip.system_root(),
        &commitment.record_key(&epoch_end_key(roster.epoch)),
        expected_end.as_ref(),
        decode_path(&evidence.end_path)?.into_iter(),
    ) {
        return Err(Error::Anchor);
    }
    let first = Vote::decode(&evidence.first)?;
    let second = Vote::decode(&evidence.second)?;
    if first.point >= second.point
        || roster
            .members
            .get(usize::from(first.share.member))
            .is_none_or(|member| member.key != liability.validator)
    {
        return Err(Error::Evidence);
    }
    let committee = Committee::at_anchor(
        policy.network,
        policy.genesis,
        roster.epoch,
        anchor_id,
        &roster.members,
    )?;
    for vote in [&first, &second] {
        vote.verify_header(
            policy,
            &committee,
            roster.activation_height,
            end,
            tip.height(),
            liability,
        )?;
    }
    committee.verify_double_vote((first.point, first.share), (second.point, second.share))
}
