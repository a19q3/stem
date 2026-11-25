//! Direct verification of consecutive CometBFT v0.38 commits for CKB.

use alloc::{collections::BTreeSet, vec::Vec};
use core::cmp::Reverse;

use ed25519_dalek::Signature;
use prost::Message;
use sha2::{Digest, Sha256};
use tendermint_proto::v0_38::{
    crypto::{PublicKey, public_key},
    types::{Header, SimpleValidator},
};

use crate::{
    BlockId, Error, Hash32, MAX_TOTAL_VOTING_POWER, Member, Result, SignedVote, VoteKind, digest,
    public_key as validator_key,
};

pub const MAX_STEPS: usize = 8;
pub const MAX_VALIDATORS: usize = 128;
pub const MAX_HEADER_BYTES: usize = 1024;
pub const NATIVE_HEADER_BYTES: usize = 208;
pub const MAX_PROOF_BYTES: usize = 1 + MAX_STEPS
    * (2 + MAX_HEADER_BYTES
        + 36
        + 4
        + NATIVE_HEADER_BYTES
        + 2
        + MAX_VALIDATORS * 40
        + 2
        + MAX_VALIDATORS * 78);
const BLOCK_PROTOCOL: u64 = 11;

#[derive(Clone, Copy)]
pub struct Trust<'a> {
    pub network: Hash32,
    pub chain_id: &'a str,
    pub initial_committee: Hash32,
    pub genesis_app_hash: Hash32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub block: BlockId,
    pub next_committee: Hash32,
    pub native_header: [u8; NATIVE_HEADER_BYTES],
}

impl Tip {
    pub const SIZE: usize = 68 + 32 + NATIVE_HEADER_BYTES;

    /// The authenticated native execution height, one below Comet's height.
    pub fn height(&self) -> u64 {
        native_height(&self.native_header)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SIZE);
        self.block.encode(&mut bytes);
        bytes.extend_from_slice(&self.next_committee);
        bytes.extend_from_slice(&self.native_header);
        bytes
    }

    pub fn decode(mut bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::SIZE {
            return Err(Error::Encoding);
        }
        let tip = Self {
            block: BlockId::decode(&mut bytes)?,
            next_committee: take(&mut bytes)?,
            native_header: take(&mut bytes)?,
        };
        if tip.height() >= i64::MAX as u64 || tip.next_committee == [0; 32] {
            return Err(Error::Encoding);
        }
        Ok(tip)
    }

    pub fn app_hash(&self, network: &Hash32) -> Hash32 {
        app_hash(network, &self.native_header)
    }
}

/// Check each intervening commit, including any committee handoffs, without
/// trusting the submitter or accepting height jumps.
pub fn verify(trust: &Trust<'_>, old: Option<&Tip>, mut bytes: &[u8]) -> Result<Tip> {
    if trust.network == [0; 32]
        || trust.initial_committee == [0; 32]
        || trust.genesis_app_hash == [0; 32]
        || !(1..=50).contains(&trust.chain_id.len())
        || bytes.len() > MAX_PROOF_BYTES
    {
        return Err(Error::Configuration);
    }
    let step_count = usize::from(u8::from_le_bytes(take(&mut bytes)?));
    if !(1..=MAX_STEPS).contains(&step_count) {
        return Err(Error::Encoding);
    }
    let mut tip = old.cloned();
    let mut previous_committee: Option<Committee> = None;
    for _ in 0..step_count {
        let size = usize::from(u16::from_le_bytes(take(&mut bytes)?));
        if size == 0 || size > MAX_HEADER_BYTES {
            return Err(Error::Encoding);
        }
        let (encoded, remaining) = bytes.split_at_checked(size).ok_or(Error::Encoding)?;
        bytes = remaining;
        let header = Header::decode(encoded).map_err(|_| Error::Encoding)?;
        if header.encode_to_vec() != encoded {
            return Err(Error::Encoding);
        }
        let native_header: [u8; NATIVE_HEADER_BYTES];
        let block = BlockId {
            hash: header_hash(&header)?,
            parts: u32::from_le_bytes(take(&mut bytes)?),
            part_set_hash: take(&mut bytes)?,
        };
        let round = i32::from_le_bytes(take(&mut bytes)?);
        native_header = take(&mut bytes)?;
        let next_committee: Hash32 = header
            .next_validators_hash
            .as_slice()
            .try_into()
            .map_err(|_| Error::Committee)?;
        let claimed = Tip {
            block,
            next_committee,
            native_header,
        };
        let expected = verify_link(trust, tip.as_ref(), &header, &claimed)?;
        if round < 0 || claimed.block.parts == 0 || claimed.block.part_set_hash == [0; 32] {
            return Err(Error::Encoding);
        }
        let members = usize::from(u16::from_le_bytes(take(&mut bytes)?));
        if members != 0 {
            if previous_committee
                .as_ref()
                .is_some_and(|committee| committee.hash == expected)
            {
                return Err(Error::Encoding);
            }
            previous_committee = Some(Committee::decode(members, &mut bytes)?);
        }
        let committee = previous_committee.as_ref().ok_or(Error::Committee)?;
        if committee.hash != expected || header.validators_hash.as_slice() != expected {
            return Err(Error::Committee);
        }
        committee.verify_votes(trust.chain_id, header.height, round, block, &mut bytes)?;
        tip = Some(claimed);
    }
    if !bytes.is_empty() {
        return Err(Error::Encoding);
    }
    tip.ok_or(Error::Sequence)
}

fn verify_link(
    trust: &Trust<'_>,
    old: Option<&Tip>,
    header: &Header,
    next: &Tip,
) -> Result<Hash32> {
    let height = next.height().checked_add(1).ok_or(Error::Overflow)?;
    if header.chain_id != trust.chain_id
        || next.next_committee == [0; 32]
        || header.app_hash.as_slice() != next.app_hash(&trust.network)
        || u64::try_from(header.height) != Ok(height)
    {
        return Err(Error::Anchor);
    }
    let previous = header.last_block_id.as_ref().ok_or(Error::Encoding)?;
    let parts = previous.part_set_header.as_ref().ok_or(Error::Encoding)?;
    match old {
        Some(old) => {
            if old.height().checked_add(1) != Some(next.height()) {
                return Err(Error::Sequence);
            }
            if previous.hash.as_slice() != old.block.hash
                || parts.total != old.block.parts
                || parts.hash.as_slice() != old.block.part_set_hash
                || next.native_header[16..48] != native_id(&trust.network, &old.native_header)
            {
                return Err(Error::Anchor);
            }
            Ok(old.next_committee)
        }
        None => {
            if next.height() != 0
                || next.app_hash(&trust.network) != trust.genesis_app_hash
                || !previous.hash.is_empty()
                || parts.total != 0
                || !parts.hash.is_empty()
            {
                return Err(Error::Anchor);
            }
            Ok(trust.initial_committee)
        }
    }
}

fn native_height(header: &[u8; NATIVE_HEADER_BYTES]) -> u64 {
    u64::from_le_bytes(header[..8].try_into().expect("fixed native height"))
}

fn native_id(network: &Hash32, header: &[u8; NATIVE_HEADER_BYTES]) -> Hash32 {
    digest(b"Stem Header\0", &[network, header])
}

fn app_hash(network: &Hash32, header: &[u8; NATIVE_HEADER_BYTES]) -> Hash32 {
    digest(
        b"Stem AppHash\0",
        &[network, &header[..8], &native_id(network, header)],
    )
}

struct Committee {
    hash: Hash32,
    members: Vec<Member>,
    power: u64,
}

impl Committee {
    fn decode(count: usize, bytes: &mut &[u8]) -> Result<Self> {
        if !(1..=MAX_VALIDATORS).contains(&count) {
            return Err(Error::Committee);
        }
        let mut addresses = BTreeSet::new();
        let mut prior = None;
        let mut power = 0u64;
        let mut members = Vec::with_capacity(count);
        let mut leaves = Vec::with_capacity(count);
        for _ in 0..count {
            let member = Member {
                key: take(bytes)?,
                power: u64::from_le_bytes(take(bytes)?),
            };
            let hash = Sha256::digest(member.key);
            let address: [u8; 20] = hash[..20].try_into().expect("fixed validator address");
            let key = (Reverse(member.power), address);
            if member.power == 0
                || prior.is_some_and(|previous| previous >= key)
                || !addresses.insert(address)
            {
                return Err(Error::Committee);
            }
            prior = Some(key);
            power = power
                .checked_add(member.power)
                .filter(|power| *power <= MAX_TOTAL_VOTING_POWER)
                .ok_or(Error::Committee)?;
            let value = SimpleValidator {
                pub_key: Some(PublicKey {
                    sum: Some(public_key::Sum::Ed25519(member.key.to_vec())),
                }),
                voting_power: member.power as i64,
            };
            leaves.push(leaf(&value.encode_to_vec()));
            members.push(member);
        }
        Ok(Self {
            members,
            power,
            hash: merkle(&leaves),
        })
    }

    fn verify_votes(
        &self,
        chain_id: &str,
        height: i64,
        round: i32,
        block: BlockId,
        bytes: &mut &[u8],
    ) -> Result<()> {
        let count = usize::from(u16::from_le_bytes(take(bytes)?));
        if count == 0 || count > self.members.len() {
            return Err(Error::Quorum);
        }
        let mut prior = None;
        let mut power = 0u64;
        for _ in 0..count {
            let index = usize::from(u16::from_le_bytes(take(bytes)?));
            if prior.is_some_and(|last| last >= index) {
                return Err(Error::Encoding);
            }
            prior = Some(index);
            let member = self.members.get(index).ok_or(Error::Committee)?;
            let vote = SignedVote {
                kind: VoteKind::Precommit,
                height,
                round,
                block_id: Some(block),
                seconds: i64::from_le_bytes(take(bytes)?),
                nanos: i32::from_le_bytes(take(bytes)?),
                signature: take(bytes)?,
            };
            validator_key(&member.key)?
                .verify_strict(
                    &vote.sign_bytes(chain_id)?,
                    &Signature::from_bytes(&vote.signature),
                )
                .map_err(|_| Error::Signature)?;
            power += member.power;
        }
        if power * 3 <= self.power * 2 {
            return Err(Error::Quorum);
        }
        Ok(())
    }
}

/// Matches Header.Hash in CometBFT v0.38, including protobuf wrapper encodings.
fn header_hash(header: &Header) -> Result<Hash32> {
    let version = header.version.ok_or(Error::Encoding)?;
    let time = header.time.ok_or(Error::Encoding)?;
    let last_block = header.last_block_id.as_ref().ok_or(Error::Encoding)?;
    if version.block != BLOCK_PROTOCOL
        || !(-62_135_596_800..=253_402_300_799).contains(&time.seconds)
        || !(0..1_000_000_000).contains(&time.nanos)
        || last_block.part_set_header.is_none()
        || header.height <= 0
        || !(1..=50).contains(&header.chain_id.len())
        || header.proposer_address.len() != 20
        || header.validators_hash.len() != 32
        || header.next_validators_hash.len() != 32
        || header.app_hash.len() != 32
    {
        return Err(Error::Encoding);
    }
    Ok(merkle(&[
        leaf(&version.encode_to_vec()),
        bytes_value(header.chain_id.as_bytes()),
        leaf(
            &Int64Value {
                value: header.height,
            }
            .encode_to_vec(),
        ),
        leaf(&time.encode_to_vec()),
        leaf(&last_block.encode_to_vec()),
        bytes_value(&header.last_commit_hash),
        bytes_value(&header.data_hash),
        bytes_value(&header.validators_hash),
        bytes_value(&header.next_validators_hash),
        bytes_value(&header.consensus_hash),
        bytes_value(&header.app_hash),
        bytes_value(&header.last_results_hash),
        bytes_value(&header.evidence_hash),
        bytes_value(&header.proposer_address),
    ]))
}

#[derive(Message)]
struct Int64Value {
    #[prost(int64, tag = "1")]
    value: i64,
}

#[derive(Message)]
struct BytesValue {
    #[prost(bytes = "vec", tag = "1")]
    value: Vec<u8>,
}

fn bytes_value(bytes: &[u8]) -> Hash32 {
    leaf(
        &BytesValue {
            value: bytes.to_vec(),
        }
        .encode_to_vec(),
    )
}

fn leaf(bytes: &[u8]) -> Hash32 {
    let mut hash = Sha256::new();
    hash.update([0]);
    hash.update(bytes);
    hash.finalize().into()
}

fn merkle(leaves: &[Hash32]) -> Hash32 {
    if leaves.len() == 1 {
        return leaves[0];
    }
    let split = leaves.len().next_power_of_two() / 2;
    let mut hash = Sha256::new();
    hash.update([1]);
    hash.update(merkle(&leaves[..split]));
    hash.update(merkle(&leaves[split..]));
    hash.finalize().into()
}

fn take<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N]> {
    let (item, remaining) = bytes.split_at_checked(N).ok_or(Error::Encoding)?;
    *bytes = remaining;
    Ok(item.try_into().expect("fixed-width field"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use ed25519_dalek::{Signer, SigningKey};
    use tendermint_proto::{
        google::protobuf::Timestamp,
        v0_38::{
            types::{BlockId as ProtoBlockId, PartSetHeader},
            version::Consensus,
        },
    };

    struct Voters {
        keys: Vec<SigningKey>,
        encoded: Vec<u8>,
        hash: Hash32,
    }

    impl Voters {
        fn new(seeds: &[u8]) -> Self {
            let mut encoded = Vec::new();
            let keys: Vec<_> = seeds
                .iter()
                .enumerate()
                .map(|(index, seed)| {
                    let key = SigningKey::from_bytes(&[*seed; 32]);
                    encoded.extend_from_slice(key.verifying_key().as_bytes());
                    encoded.extend_from_slice(&(4u64 - index as u64).to_le_bytes());
                    key
                })
                .collect();
            let hash = Committee::decode(keys.len(), &mut encoded.as_slice())
                .unwrap()
                .hash;
            Self {
                keys,
                encoded,
                hash,
            }
        }
    }

    fn step(
        trust: &Trust<'_>,
        previous: Option<&Tip>,
        voters: &Voters,
        next_committee: Hash32,
        indices: &[usize],
    ) -> (Tip, Vec<u8>) {
        let height = previous.map_or(0, |previous| previous.height() + 1);
        let mut native_header = [0; NATIVE_HEADER_BYTES];
        native_header[..8].copy_from_slice(&height.to_le_bytes());
        let mut block_id = ProtoBlockId {
            hash: vec![],
            part_set_header: Some(PartSetHeader {
                total: 0,
                hash: vec![],
            }),
        };
        if let Some(old) = previous {
            native_header[16..48].copy_from_slice(&native_id(&trust.network, &old.native_header));
            block_id = ProtoBlockId {
                hash: old.block.hash.to_vec(),
                part_set_header: Some(PartSetHeader {
                    total: old.block.parts,
                    hash: old.block.part_set_hash.to_vec(),
                }),
            };
        }
        assert!(next_committee != [0; 32]);
        let header = Header {
            version: Some(Consensus {
                block: BLOCK_PROTOCOL,
                app: 1,
            }),
            chain_id: trust.chain_id.into(),
            height: (height + 1) as i64,
            time: Some(Timestamp {
                seconds: 25,
                nanos: 123,
            }),
            last_block_id: Some(block_id),
            validators_hash: voters.hash.to_vec(),
            next_validators_hash: next_committee.to_vec(),
            app_hash: app_hash(&trust.network, &native_header).to_vec(),
            proposer_address: [10; 20].into(),
            ..Header::default()
        };
        let serialized = header.encode_to_vec();
        let block = BlockId {
            hash: header_hash(&header).unwrap(),
            parts: 1,
            part_set_hash: [11; 32],
        };
        let mut encoded = vec![1];
        encoded.extend_from_slice(&(serialized.len() as u16).to_le_bytes());
        encoded.extend_from_slice(&serialized);
        encoded.extend_from_slice(&block.parts.to_le_bytes());
        encoded.extend_from_slice(&block.part_set_hash);
        encoded.extend_from_slice(&0i32.to_le_bytes());
        encoded.extend_from_slice(&native_header);
        encoded.extend_from_slice(&(voters.keys.len() as u16).to_le_bytes());
        encoded.extend_from_slice(&voters.encoded);
        encoded.extend_from_slice(&(indices.len() as u16).to_le_bytes());
        for &index in indices {
            let vote = SignedVote {
                kind: VoteKind::Precommit,
                height: header.height,
                round: 0,
                block_id: Some(block),
                seconds: 26,
                nanos: 321,
                signature: [0; 64],
            };
            let signature = voters.keys[index].sign(&vote.sign_bytes(trust.chain_id).unwrap());
            encoded.extend_from_slice(&(index as u16).to_le_bytes());
            encoded.extend_from_slice(&vote.seconds.to_le_bytes());
            encoded.extend_from_slice(&vote.nanos.to_le_bytes());
            encoded.extend_from_slice(&signature.to_bytes());
        }
        (
            Tip {
                block,
                next_committee,
                native_header,
            },
            encoded,
        )
    }

    #[test]
    fn enforces_weighted_quorum_and_authenticates_the_committed_successor() {
        let initial = Voters::new(&[41, 42, 43]);
        let incoming = Voters::new(&[51, 52, 53]);
        let network = [8; 32];
        let trust = Trust {
            network,
            chain_id: "stem-direct-test",
            initial_committee: initial.hash,
            genesis_app_hash: app_hash(&network, &[0; NATIVE_HEADER_BYTES]),
        };

        let (_, minority) = step(&trust, None, &initial, incoming.hash, &[0, 2]);
        assert_eq!(verify(&trust, None, &minority), Err(Error::Quorum)); // Exactly 2/3.
        let (_, duplicate) = step(&trust, None, &initial, incoming.hash, &[0, 0]);
        assert_eq!(verify(&trust, None, &duplicate), Err(Error::Encoding));
        let (first, good) = step(&trust, None, &initial, incoming.hash, &[0, 1]);
        assert_eq!(verify(&trust, None, &good).unwrap(), first);

        let (_, wrong_set) = step(&trust, Some(&first), &initial, incoming.hash, &[0, 1]);
        assert_eq!(
            verify(&trust, Some(&first), &wrong_set),
            Err(Error::Committee)
        );
        let (second, handoff) = step(&trust, Some(&first), &incoming, incoming.hash, &[0, 1]);
        let mut chain = good.clone();
        chain[0] += 1;
        chain.extend_from_slice(&handoff[1..]);
        assert_eq!(verify(&trust, None, &chain).unwrap(), second);
        assert_eq!(verify(&trust, Some(&first), &handoff).unwrap(), second);
        assert_eq!(
            verify(&trust, Some(&second), &handoff),
            Err(Error::Sequence)
        );

        let mut tampered = handoff.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(
            verify(&trust, Some(&first), &tampered),
            Err(Error::Signature)
        );
        let mut bad_parent = first;
        bad_parent.block.part_set_hash[0] ^= 1;
        assert_eq!(
            verify(&trust, Some(&bad_parent), &handoff),
            Err(Error::Anchor)
        );
    }
}
