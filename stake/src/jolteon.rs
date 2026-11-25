//! Canonical certificates and voting domains for Stem's weighted two-chain protocol.

use alloc::{collections::BTreeMap, vec, vec::Vec};
use ed25519_dalek::Signature;
use prost::Message;

use crate::{
    Authority, Error, Hash32, Member, Result,
    compact::SignerBitmap,
    digest,
    finality::{NATIVE_HEADER_BYTES, native_height, native_id},
    history::Frontier,
};

pub const MAX_MEMBERS: usize = 128;
const LEADER_SLOTS: usize = 10_000;
const MAX_PROOF_BYTES: usize = 32 * 1024;

/// Two certified Headers. The child certifies the parent at the next view.
#[derive(Clone, Message)]
pub struct FinalityProof {
    #[prost(bytes, tag = "1")]
    pub parent_header: Vec<u8>,
    #[prost(bytes, tag = "2")]
    pub parent_qc: Vec<u8>,
    #[prost(bytes, tag = "3")]
    pub child_header: Vec<u8>,
    #[prost(bytes, tag = "4")]
    pub child_qc: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalizedHeader {
    network: Hash32,
    header: [u8; NATIVE_HEADER_BYTES],
}

impl FinalizedHeader {
    pub fn as_bytes(&self) -> &[u8; NATIVE_HEADER_BYTES] {
        &self.header
    }

    pub fn height(&self) -> u64 {
        native_height(&self.header)
    }

    /// Checks an MMR extension from a previously authenticated checkpoint.
    pub fn verify_extension(
        &self,
        checkpoint: &[u8; NATIVE_HEADER_BYTES],
        checkpoint_frontier: &[u8],
        suffix: &[Hash32],
    ) -> Result<()> {
        let height = native_height(checkpoint);
        let mut frontier = Frontier::decode(self.network, checkpoint_frontier)?;
        if height >= self.height()
            || frontier.count() != height
            || frontier.root() != *history_root(checkpoint)
        {
            return Err(Error::Anchor);
        }
        frontier.append(height, &native_id(&self.network, checkpoint))?;
        if !frontier.verify_extension(self.height(), history_root(&self.header), suffix) {
            return Err(Error::Anchor);
        }
        Ok(())
    }
}

fn history_root(header: &[u8; NATIVE_HEADER_BYTES]) -> &Hash32 {
    header[144..176].try_into().expect("fixed history root")
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Point {
    pub view: u64,
    pub parent_view: u64,
    pub header: Hash32,
}

impl Point {
    pub const SIZE: usize = 48;

    pub fn encode(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[..8].copy_from_slice(&self.view.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.parent_view.to_le_bytes());
        bytes[16..].copy_from_slice(&self.header);
        bytes
    }

    pub fn decode(raw: &[u8]) -> Result<Self> {
        let bytes: &[u8; Self::SIZE] = raw.try_into().map_err(|_| Error::Encoding)?;
        let point = Self {
            view: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            parent_view: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            header: bytes[16..].try_into().unwrap(),
        };
        if !point.valid() {
            return Err(Error::Encoding);
        }
        Ok(point)
    }

    fn valid(self) -> bool {
        self.header != [0; 32]
            && if self.view == 0 {
                self.parent_view == 0
            } else {
                self.parent_view < self.view
            }
    }

    pub fn directly_certifies(self, parent: Self) -> bool {
        parent.view != 0
            && parent.view.checked_add(1) == Some(self.view)
            && self.parent_view == parent.view
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Share {
    pub member: u16,
    pub signature: [u8; 64],
}

impl Share {
    pub const SIZE: usize = 66;

    pub fn encode(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[..2].copy_from_slice(&self.member.to_le_bytes());
        bytes[2..].copy_from_slice(&self.signature);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let raw: &[u8; Self::SIZE] = bytes.try_into().map_err(|_| Error::Encoding)?;
        Ok(Self {
            member: u16::from_le_bytes(raw[..2].try_into().unwrap()),
            signature: raw[2..].try_into().unwrap(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Quorum {
    pub point: Point,
    bitmap: Vec<u8>,
    signatures: Vec<[u8; 64]>,
}

impl Quorum {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.point.encode().to_vec();
        bytes.extend_from_slice(&self.bitmap);
        for signature in &self.signatures {
            bytes.extend_from_slice(signature);
        }
        bytes
    }

    pub fn decode(raw: &[u8], members: usize) -> Result<Self> {
        let (point, tail) = raw.split_at_checked(Point::SIZE).ok_or(Error::Encoding)?;
        let point = Point::decode(point)?;
        if point.view == 0 {
            if !tail.is_empty() {
                return Err(Error::Encoding);
            }
            return Ok(Self {
                point,
                bitmap: Vec::new(),
                signatures: Vec::new(),
            });
        }
        if !(1..=MAX_MEMBERS).contains(&members) {
            return Err(Error::Committee);
        }
        let (bitmap, signatures) = tail
            .split_at_checked(members.div_ceil(8))
            .ok_or(Error::Encoding)?;
        let count = SignerBitmap::new(members, bitmap)?.signature_count();
        if signatures.len() != count * 64 {
            return Err(Error::Encoding);
        }
        Ok(Self {
            point,
            bitmap: bitmap.to_vec(),
            signatures: signatures
                .chunks_exact(64)
                .map(|sig| sig.try_into().unwrap())
                .collect(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeoutVote {
    pub high: Point,
    pub share: Share,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeoutCertificate {
    pub view: u64,
    pub high: Quorum,
    pub votes: Vec<TimeoutVote>,
}

impl TimeoutCertificate {
    pub fn encode(&self) -> Vec<u8> {
        let high = self.high.encode();
        let mut bytes = self.view.to_le_bytes().to_vec();
        bytes.extend_from_slice(&(high.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&high);
        for vote in &self.votes {
            bytes.extend_from_slice(&vote.high.encode());
            bytes.extend_from_slice(&vote.share.encode());
        }
        bytes
    }

    pub fn decode(bytes: &[u8], members: usize) -> Result<Self> {
        let (prefix, tail) = bytes.split_at_checked(10).ok_or(Error::Encoding)?;
        let view = u64::from_le_bytes(prefix[..8].try_into().unwrap());
        let size = u16::from_le_bytes(prefix[8..10].try_into().unwrap()) as usize;
        let (high, votes) = tail.split_at_checked(size).ok_or(Error::Encoding)?;
        let high = Quorum::decode(high, members)?;
        let width = Point::SIZE + Share::SIZE;
        if votes.is_empty() || !votes.len().is_multiple_of(width) || votes.len() / width > members {
            return Err(Error::Encoding);
        }
        let votes = votes
            .chunks_exact(width)
            .map(|raw| {
                Ok(TimeoutVote {
                    high: Point::decode(&raw[..Point::SIZE])?,
                    share: Share::decode(&raw[Point::SIZE..])?,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self { view, high, votes })
    }
}

pub struct Committee {
    network: Hash32,
    genesis: Hash32,
    epoch: u64,
    generation: Hash32,
    authority: Authority,
    schedule: Vec<u16>,
}

impl Committee {
    pub fn new(network: Hash32, genesis: Hash32, epoch: u64, members: &[Member]) -> Result<Self> {
        if network == [0; 32] || genesis == [0; 32] || !(1..=MAX_MEMBERS).contains(&members.len()) {
            return Err(Error::Configuration);
        }
        let mut raw = Vec::with_capacity(40 * members.len());
        for member in members {
            raw.extend_from_slice(&member.key);
            raw.extend_from_slice(&member.power.to_le_bytes());
        }
        let generation = digest(b"Stem Jolteon committee\0", &[&raw]);
        let authority = Authority::new(generation, members)?;
        let schedule = leader_schedule(members, authority.total);
        Ok(Self {
            network,
            genesis,
            epoch,
            generation,
            authority,
            schedule,
        })
    }

    pub fn size(&self) -> usize {
        self.authority.guardians.len()
    }

    pub fn genesis_quorum(&self) -> Quorum {
        Quorum {
            point: Point {
                view: 0,
                parent_view: 0,
                header: self.genesis,
            },
            bitmap: Vec::new(),
            signatures: Vec::new(),
        }
    }

    pub fn leader(&self, view: u64) -> Result<u16> {
        let slot = view.checked_sub(1).ok_or(Error::Sequence)? % self.schedule.len() as u64;
        Ok(self.schedule[slot as usize])
    }

    pub fn power(&self, member: u16) -> Result<u64> {
        self.authority
            .guardians
            .get(usize::from(member))
            .map(|(_, power)| *power)
            .ok_or(Error::Committee)
    }

    pub fn has_quorum(&self, power: u64) -> bool {
        3 * u128::from(power) > 2 * u128::from(self.authority.total)
    }

    fn sign_digest(&self, domain: &[u8], fields: &[u8]) -> Hash32 {
        digest(
            domain,
            &[
                &self.network,
                &self.genesis,
                &self.epoch.to_le_bytes(),
                &self.generation,
                fields,
            ],
        )
    }

    pub fn proposal_digest(&self, point: Point) -> Hash32 {
        self.sign_digest(b"Stem Jolteon proposal\0", &point.encode())
    }

    pub fn vote_digest(&self, point: Point) -> Hash32 {
        self.sign_digest(b"Stem Jolteon vote\0", &point.encode())
    }

    pub fn timeout_digest(&self, view: u64, high: Point) -> Hash32 {
        let mut fields = [0; 8 + Point::SIZE];
        fields[..8].copy_from_slice(&view.to_le_bytes());
        fields[8..].copy_from_slice(&high.encode());
        self.sign_digest(b"Stem Jolteon timeout\0", &fields)
    }

    pub fn verify_share(&self, digest: &Hash32, share: Share) -> Result<u64> {
        let (key, power) = self
            .authority
            .guardians
            .get(usize::from(share.member))
            .ok_or(Error::Committee)?;
        key.verify_strict(digest, &Signature::from_bytes(&share.signature))
            .map_err(|_| Error::Signature)?;
        Ok(*power)
    }

    pub fn verify_double_vote(
        &self,
        first: (Point, Share),
        second: (Point, Share),
    ) -> Result<Hash32> {
        let (left, a) = first;
        let (right, b) = second;
        if !left.valid()
            || !right.valid()
            || left.view == 0
            || left.view != right.view
            || left == right
            || a.member != b.member
        {
            return Err(Error::Evidence);
        }
        self.verify_share(&self.vote_digest(left), a)?;
        self.verify_share(&self.vote_digest(right), b)?;
        let mut position = [0; 10];
        position[..8].copy_from_slice(&left.view.to_le_bytes());
        position[8..].copy_from_slice(&a.member.to_le_bytes());
        Ok(self.sign_digest(b"Stem Jolteon double vote\0", &position))
    }

    pub fn verify_quorum(&self, quorum: &Quorum) -> Result<()> {
        if quorum.point == self.genesis_quorum().point
            && quorum.bitmap.is_empty()
            && quorum.signatures.is_empty()
        {
            return Ok(());
        }
        if quorum.point.view == 0 || !quorum.point.valid() {
            return Err(Error::Sequence);
        }
        self.authority.verify_compact(
            &self.vote_digest(quorum.point),
            &quorum.bitmap,
            &quorum.signatures,
        )
    }

    pub fn verify_finality(&self, bytes: &[u8]) -> Result<FinalizedHeader> {
        if bytes.len() > MAX_PROOF_BYTES {
            return Err(Error::Encoding);
        }
        let proof = FinalityProof::decode(bytes).map_err(|_| Error::Encoding)?;
        if proof.encode_to_vec() != bytes {
            return Err(Error::Encoding);
        }
        let parent = Quorum::decode(&proof.parent_qc, self.size())?;
        let child = Quorum::decode(&proof.child_qc, self.size())?;
        let parent_header = proof
            .parent_header
            .as_slice()
            .try_into()
            .map_err(|_| Error::Encoding)?;
        let child_header = proof
            .child_header
            .as_slice()
            .try_into()
            .map_err(|_| Error::Encoding)?;
        if !child.point.directly_certifies(parent.point)
            || native_id(&self.network, parent_header) != parent.point.header
            || native_id(&self.network, child_header) != child.point.header
            || child_header[16..48] != parent.point.header
            || native_height(parent_header).checked_add(1) != Some(native_height(child_header))
            || timestamp(parent_header) > timestamp(child_header)
        {
            return Err(Error::Sequence);
        }
        self.verify_quorum(&parent)?;
        self.verify_quorum(&child)?;
        Ok(FinalizedHeader {
            network: self.network,
            header: *parent_header,
        })
    }

    /// The network collector has already verified these individual votes.
    pub fn assemble_quorum(&self, point: Point, votes: &BTreeMap<u16, [u8; 64]>) -> Result<Quorum> {
        let mut bitmap = vec![0u8; self.size().div_ceil(8)];
        let mut signatures = Vec::with_capacity(votes.len());
        let mut power = 0u64;
        for (&member, signature) in votes {
            power += self.power(member)?;
            let member = usize::from(member);
            bitmap[member / 8] |= 1 << (member % 8);
            signatures.push(*signature);
        }
        if !self.has_quorum(power) {
            return Err(Error::Quorum);
        }
        Ok(Quorum {
            point,
            bitmap,
            signatures,
        })
    }

    pub fn verify_timeout_certificate(&self, tc: &TimeoutCertificate) -> Result<()> {
        self.verify_quorum(&tc.high)?;
        let highest = tc.high.point;
        if tc.view == 0 || highest.view > tc.view || tc.votes.len() > self.size() {
            return Err(Error::Sequence);
        }
        let mut last = None;
        let mut witnessed = false;
        let mut power = 0;
        for vote in &tc.votes {
            if last.is_some_and(|member| vote.share.member <= member)
                || vote.high.view > highest.view
                || (vote.high.view == highest.view && vote.high != highest)
            {
                return Err(Error::Sequence);
            }
            power += self.verify_share(&self.timeout_digest(tc.view, vote.high), vote.share)?;
            witnessed |= vote.high == highest;
            last = Some(vote.share.member);
        }
        if !witnessed || !self.has_quorum(power) {
            return Err(Error::Quorum);
        }
        Ok(())
    }

    pub fn verify_parent_view(
        &self,
        view: u64,
        parent: Point,
        tc: Option<&TimeoutCertificate>,
    ) -> Result<()> {
        let adjacent = parent.view.checked_add(1) == Some(view);
        if adjacent && tc.is_none() {
            return Ok(());
        }
        let tc = tc.ok_or(Error::Sequence)?;
        self.verify_timeout_certificate(tc)?;
        if tc.view.checked_add(1) != Some(view)
            || parent.view >= view
            || (parent != tc.high.point && parent.view <= tc.high.point.view)
        {
            return Err(Error::Sequence);
        }
        Ok(())
    }
}

fn timestamp(header: &[u8; NATIVE_HEADER_BYTES]) -> u64 {
    u64::from_le_bytes(header[8..16].try_into().expect("fixed timestamp"))
}

// Smooth weighted round robin, with integer apportionment for large raw powers.
// Reference implementation: weighted order leader schedule at revision 90e0b1c49.
fn leader_schedule(members: &[Member], total: u64) -> Vec<u16> {
    let scale = total.min(LEADER_SLOTS as u64) as usize;
    let weights = if total <= LEADER_SLOTS as u64 {
        members
            .iter()
            .map(|member| member.power as i64)
            .collect::<Vec<_>>()
    } else {
        let available = (LEADER_SLOTS - members.len()) as u128;
        let total = u128::from(total);
        let mut weights = Vec::with_capacity(members.len());
        let mut remainders = Vec::with_capacity(members.len());
        for (index, member) in members.iter().enumerate() {
            let share = available * u128::from(member.power);
            weights.push(1 + (share / total) as i64);
            remainders.push((core::cmp::Reverse(share % total), index));
        }
        remainders.sort_unstable();
        let additional = scale - weights.iter().sum::<i64>() as usize;
        for (_, index) in remainders.into_iter().take(additional) {
            weights[index] += 1;
        }
        weights
    };
    let mut credit = vec![0i64; members.len()];
    let mut schedule = Vec::with_capacity(scale);
    for _ in 0..scale {
        let mut selected = 0;
        for (index, weight) in weights.iter().enumerate() {
            credit[index] += weight;
            if credit[index] > credit[selected] {
                selected = index;
            }
        }
        credit[selected] -= scale as i64;
        schedule.push(selected as u16);
    }
    schedule
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    fn fixture() -> (Vec<SigningKey>, Vec<Member>) {
        let mut keys: Vec<_> = (1..=4)
            .map(|seed| SigningKey::from_bytes(&[seed; 32]))
            .collect();
        keys.sort_by_key(|key| key.verifying_key().to_bytes());
        let members = keys
            .iter()
            .zip([4, 2, 2, 1])
            .map(|(key, power)| Member {
                key: key.verifying_key().to_bytes(),
                power,
            })
            .collect();
        (keys, members)
    }

    fn share(keys: &[SigningKey], index: u16, digest: Hash32) -> Share {
        Share {
            member: index,
            signature: keys[index as usize].sign(&digest).to_bytes(),
        }
    }

    #[test]
    fn compact_quorum_binds_weight_epoch_and_exact_signer_set() {
        let (keys, members) = fixture();
        let committee = Committee::new([8; 32], [9; 32], 0, &members).unwrap();
        let point = Point {
            view: 1,
            parent_view: 0,
            header: [1; 32],
        };
        let digest = committee.vote_digest(point);
        let mut votes = BTreeMap::from([
            (0, share(&keys, 0, digest).signature),
            (1, share(&keys, 1, digest).signature),
        ]);
        assert_eq!(committee.assemble_quorum(point, &votes), Err(Error::Quorum));
        votes.insert(3, share(&keys, 3, digest).signature);
        let quorum = committee.assemble_quorum(point, &votes).unwrap();
        let mut encoded = quorum.encode();
        assert_eq!(Quorum::decode(&encoded, members.len()).unwrap(), quorum);
        committee.verify_quorum(&quorum).unwrap();
        let next_epoch = Committee::new([8; 32], [9; 32], 1, &members).unwrap();
        assert!(next_epoch.verify_quorum(&quorum).is_err());
        encoded[Point::SIZE] |= 1 << 7;
        assert!(Quorum::decode(&encoded, members.len()).is_err());
    }

    #[test]
    fn certified_header_accepts_only_an_extension_of_the_anchored_history() {
        let (keys, members) = fixture();
        let network = [8; 32];
        let mut frontier = Frontier::empty(network);
        let mut index = BTreeMap::new();
        let mut checkpoint = [0; NATIVE_HEADER_BYTES];
        let mut checkpoint_frontier = Vec::new();
        let mut headers = Vec::new();
        let mut last = [0; 32];
        for height in 0u64..=8 {
            let mut header = [0; NATIVE_HEADER_BYTES];
            header[..8].copy_from_slice(&height.to_le_bytes());
            header[16..48].copy_from_slice(&last);
            header[144..176].copy_from_slice(&frontier.root());
            if height == 2 {
                checkpoint = header;
                checkpoint_frontier = frontier.encode();
            }
            last = native_id(&network, &header);
            frontier
                .append_indexed(height, &last, |start, level, node| {
                    index.insert((start, level), node);
                })
                .unwrap();
            headers.push(header);
        }
        let committee =
            Committee::new(network, native_id(&network, &headers[0]), 0, &members).unwrap();
        let certify = |view: u64| {
            let point = Point {
                view,
                parent_view: view - 1,
                header: native_id(&network, &headers[view as usize]),
            };
            let digest = committee.vote_digest(point);
            let votes = [0, 1, 3]
                .into_iter()
                .map(|i| (i, share(&keys, i, digest).signature))
                .collect();
            committee.assemble_quorum(point, &votes).unwrap().encode()
        };
        let proof = FinalityProof {
            parent_header: headers[7].to_vec(),
            parent_qc: certify(7),
            child_header: headers[8].to_vec(),
            child_qc: certify(8),
        }
        .encode_to_vec();
        let certified = committee.verify_finality(&proof).unwrap();
        let suffix = crate::history::extension(3, 7, |start, level| {
            index.get(&(start, level)).copied().ok_or(Error::Anchor)
        })
        .unwrap();
        assert_eq!(certified.as_bytes(), &headers[7]);
        certified
            .verify_extension(&checkpoint, &checkpoint_frontier, &suffix)
            .unwrap();
        checkpoint[80] ^= 1;
        assert_eq!(
            certified.verify_extension(&checkpoint, &checkpoint_frontier, &suffix),
            Err(Error::Anchor)
        );
    }

    #[test]
    fn timeout_binds_each_signed_high_point_and_equivocation_has_one_identity() {
        let (keys, members) = fixture();
        let committee = Committee::new([8; 32], [9; 32], 0, &members).unwrap();
        let genesis = committee.genesis_quorum();
        let parent = Point {
            view: 1,
            parent_view: 0,
            header: [1; 32],
        };
        let sibling = Point {
            header: [2; 32],
            ..parent
        };
        let a = (parent, share(&keys, 0, committee.vote_digest(parent)));
        let b = (sibling, share(&keys, 0, committee.vote_digest(sibling)));
        let evidence = committee.verify_double_vote(a, b).unwrap();
        assert_eq!(committee.verify_double_vote(b, a).unwrap(), evidence);
        assert!(committee.verify_double_vote(a, a).is_err());

        let high = committee
            .assemble_quorum(
                parent,
                &BTreeMap::from([
                    (0, a.1.signature),
                    (1, share(&keys, 1, committee.vote_digest(parent)).signature),
                    (3, share(&keys, 3, committee.vote_digest(parent)).signature),
                ]),
            )
            .unwrap();
        let mut certificate = TimeoutCertificate {
            view: 2,
            high,
            votes: [0, 1, 3]
                .into_iter()
                .map(|index| {
                    let high = if index == 0 { parent } else { genesis.point };
                    TimeoutVote {
                        high,
                        share: share(&keys, index, committee.timeout_digest(2, high)),
                    }
                })
                .collect(),
        };
        committee
            .verify_parent_view(3, parent, Some(&certificate))
            .unwrap();
        assert!(
            committee
                .verify_parent_view(3, genesis.point, Some(&certificate))
                .is_err()
        );
        certificate.votes[1].high = parent;
        assert_eq!(
            committee.verify_timeout_certificate(&certificate),
            Err(Error::Signature)
        );
    }
}
