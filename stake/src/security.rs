//! Application service bonds and attestations independent of consensus stake.
use alloc::vec::Vec;
use ed25519_dalek::Signature;

use crate::{
    Authority, Error, Hash32, MAX_TOTAL_VOTING_POWER, Member, Result, compact::SignerBitmap,
    digest, public_key,
};

pub const MAX_MEMBERS: usize = 16;
pub type Payee = [u8; 20];
pub const SIGHASH_TYPE: Hash32 = [
    0x9b, 0xd7, 0xe0, 0x6f, 0x3e, 0xcf, 0x4b, 0xe0, 0xf2, 0xfc, 0xd2, 0x18, 0x8b, 0x23, 0xf1, 0xb9,
    0xfc, 0xc8, 0x8e, 0x5d, 0x4b, 0x65, 0xa8, 0x63, 0x7b, 0x17, 0x72, 0x3b, 0xbd, 0xa3, 0xcc, 0xe8,
];
pub const MIN_PAYMENT: u64 = 61 * 100_000_000;
pub const MIN_RECEIPT: u64 = 194 * 100_000_000;
pub const OUT_POINT: usize = 36;
pub const HEIGHT_MASK: u64 = (1 << 56) - 1;
pub const RELATIVE_HEIGHT: u64 = 1 << 63;

pub const SERVE: u8 = 0;
pub const RETIRE: u8 = 1;
pub const SLASH: u8 = 2;
pub const CLOSE: u8 = 3;
pub const REFUND: u8 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Policy {
    pub type_id: Hash32,
    pub ckb_genesis: Hash32,
    pub network: Hash32,
    pub operator: Payee,
    pub penalty: Payee,
    pub quorum_bps: u16,
    pub challenge_blocks: u64,
}

impl Policy {
    pub const SIZE: usize = 146;

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::SIZE {
            return Err(Error::Encoding);
        }
        let type_id = bytes[..32].try_into().unwrap();
        let ckb_genesis = bytes[32..64].try_into().unwrap();
        let network = bytes[64..96].try_into().unwrap();
        let operator = bytes[96..116].try_into().unwrap();
        let penalty = bytes[116..136].try_into().unwrap();
        let quorum_bps = u16::from_le_bytes(bytes[136..138].try_into().unwrap());
        let challenge_blocks = u64::from_le_bytes(bytes[138..].try_into().unwrap());
        if [type_id, ckb_genesis, network].contains(&[0; 32])
            || ckb_genesis == network
            || operator == [0; 20]
            || penalty == [0; 20]
            || !(6667..=10_000).contains(&quorum_bps)
            || !(1..=HEIGHT_MASK).contains(&challenge_blocks)
        {
            return Err(Error::Configuration);
        }
        Ok(Self {
            type_id,
            ckb_genesis,
            network,
            operator,
            penalty,
            quorum_bps,
            challenge_blocks,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        [
            &self.type_id[..],
            &self.ckb_genesis,
            &self.network,
            &self.operator,
            &self.penalty,
            &self.quorum_bps.to_le_bytes(),
            &self.challenge_blocks.to_le_bytes(),
        ]
        .concat()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Request {
    pub domain: Hash32,
    pub buyer: Payee,
    pub subject: [u8; OUT_POINT],
    pub refund_height: u64,
    pub receipt_capacity: u64,
}

impl Request {
    pub const SIZE: usize = 104;

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::SIZE {
            return Err(Error::Encoding);
        }
        let domain = bytes[..32].try_into().unwrap();
        let buyer = bytes[32..52].try_into().unwrap();
        let subject = bytes[52..88].try_into().unwrap();
        let refund_height = u64::from_le_bytes(bytes[88..96].try_into().unwrap());
        let receipt_capacity = u64::from_le_bytes(bytes[96..104].try_into().unwrap());
        if !valid_point(&subject)
            || domain == [0; 32]
            || buyer == [0; 20]
            || receipt_capacity < MIN_RECEIPT
            || !(1..=HEIGHT_MASK).contains(&refund_height)
        {
            return Err(Error::Configuration);
        }
        Ok(Self {
            domain,
            buyer,
            subject,
            refund_height,
            receipt_capacity,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        [
            &self.domain[..],
            &self.buyer,
            &self.subject,
            &self.refund_height.to_le_bytes(),
            &self.receipt_capacity.to_le_bytes(),
        ]
        .concat()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position {
    pub key: Hash32,
    pub beneficiary: Payee,
    pub bond: u64,
    pub slashed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Domain {
    pub retired: bool,
    pub positions: Vec<Position>,
}

impl Domain {
    const POSITION_BYTES: usize = 61;

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (prefix, positions) = bytes.split_at_checked(2).ok_or(Error::Encoding)?;
        let (positions, remainder) = positions.as_chunks::<{ Self::POSITION_BYTES }>();
        if !remainder.is_empty()
            || !matches!(prefix[0], 0 | 1)
            || prefix[1] as usize != positions.len()
            || !(1..=MAX_MEMBERS).contains(&positions.len())
        {
            return Err(Error::Encoding);
        }
        let mut entries: Vec<Position> = Vec::with_capacity(positions.len());
        let mut total = 0u64;
        for raw in positions {
            let position = Position {
                key: raw[..32].try_into().unwrap(),
                beneficiary: raw[32..52].try_into().unwrap(),
                bond: u64::from_le_bytes(raw[52..60].try_into().unwrap()),
                slashed: raw[60] == 1,
            };
            public_key(&position.key)?;
            if raw[60] > 1
                || position.bond < MIN_PAYMENT
                || position.beneficiary == [0; 20]
                || entries.last().is_some_and(|last| last.key >= position.key)
            {
                return Err(Error::Configuration);
            }
            total = total.checked_add(position.bond).ok_or(Error::Overflow)?;
            entries.push(position);
        }
        if total > MAX_TOTAL_VOTING_POWER {
            return Err(Error::Overflow);
        }
        Ok(Self {
            retired: prefix[0] == 1,
            positions: entries,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + self.positions.len() * Self::POSITION_BYTES);
        out.extend([u8::from(self.retired), self.positions.len() as u8]);
        for position in &self.positions {
            out.extend(position.key);
            out.extend(position.beneficiary);
            out.extend(position.bond.to_le_bytes());
            out.push(u8::from(position.slashed));
        }
        out
    }

    fn initial_bonded(&self) -> Result<u64> {
        self.positions.iter().try_fold(0u64, |sum, position| {
            sum.checked_add(position.bond).ok_or(Error::Overflow)
        })
    }

    pub fn bonded(&self) -> Result<u64> {
        self.positions
            .iter()
            .filter(|position| !position.slashed)
            .try_fold(0u64, |sum, position| {
                sum.checked_add(position.bond).ok_or(Error::Overflow)
            })
    }

    /// Verifies the original roster indices and returns the original signers and their payout amounts.
    pub fn verify_service(
        &self,
        policy: &Policy,
        domain: &Hash32,
        request: &[u8; OUT_POINT],
        result: &Hash32,
        reward: u64,
        bitmap: &[u8],
        signatures: &[[u8; 64]],
    ) -> Result<Vec<(usize, u64)>> {
        if self.retired || reward == 0 || !valid_point(request) || *result == [0; 32] {
            return Err(Error::Configuration);
        }
        let selected = SignerBitmap::new(self.positions.len(), bitmap)?;
        if selected.signature_count() != signatures.len() {
            return Err(Error::Encoding);
        }
        let mut power = 0u64;
        let mut translated = [0u8; MAX_MEMBERS.div_ceil(8)];
        let mut active = Vec::new();
        let mut indices = Vec::new();
        for (i, position) in self.positions.iter().enumerate() {
            let selected = bitmap[i / 8] & (1 << (i % 8)) != 0;
            if position.slashed {
                if selected {
                    return Err(Error::Authority);
                }
                continue;
            }
            if selected {
                translated[active.len() / 8] |= 1 << (active.len() % 8);
                power = power.checked_add(position.bond).ok_or(Error::Overflow)?;
                indices.push(i);
            }
            active.push(Member {
                key: position.key,
                power: position.bond,
            });
        }
        if power == 0
            || 10_000 * u128::from(power)
                < u128::from(policy.quorum_bps) * u128::from(self.initial_bonded()?)
        {
            return Err(Error::Quorum);
        }
        let authority = Authority::new(*domain, &active)?;
        authority.verify_compact(
            &signing_digest(domain, request, result),
            &translated[..active.len().div_ceil(8)],
            signatures,
        )?;
        let chosen = indices.into_iter().map(|i| {
            (
                i,
                (u128::from(reward) * u128::from(self.positions[i].bond) / u128::from(power))
                    as u64,
            )
        });
        let mut payouts: Vec<_> = chosen.collect();
        let assigned = payouts.iter().try_fold(0u64, |sum, (_, amount)| {
            sum.checked_add(*amount).ok_or(Error::Overflow)
        })?;
        let remainder = reward.checked_sub(assigned).ok_or(Error::Overflow)?;
        payouts[0].1 = payouts[0].1.checked_add(remainder).ok_or(Error::Overflow)?;
        if payouts.iter().any(|(_, amount)| *amount < MIN_PAYMENT) {
            return Err(Error::Configuration);
        }
        Ok(payouts)
    }

    pub fn verify_double_report(&self, domain: &Hash32, raw: &[u8]) -> Result<usize> {
        if raw.len() != 229 {
            return Err(Error::Encoding);
        }
        let index = raw[0] as usize;
        let position = self.positions.get(index).ok_or(Error::Evidence)?;
        let request = raw[1..37].try_into().unwrap();
        let first: &Hash32 = raw[37..69].try_into().unwrap();
        let second: &Hash32 = raw[133..165].try_into().unwrap();
        if position.slashed || !valid_point(request) || *first == [0; 32] || first >= second {
            return Err(Error::Evidence);
        }
        let key = public_key(&position.key)?;
        for (result, signature) in [(first, &raw[69..133]), (second, &raw[165..229])] {
            key.verify_strict(
                &signing_digest(domain, request, result),
                &Signature::from_bytes(signature.try_into().unwrap()),
            )
            .map_err(|_| Error::Signature)?;
        }
        Ok(index)
    }
}

pub struct Certificate<'a> {
    pub result: Hash32,
    pub bitmap: &'a [u8],
    pub signatures: &'a [[u8; 64]],
}

impl<'a> Certificate<'a> {
    pub fn decode(bytes: &'a [u8], members: usize) -> Result<Self> {
        if !(1..=MAX_MEMBERS).contains(&members) {
            return Err(Error::Configuration);
        }
        let width = members.div_ceil(8);
        let (fixed, signatures) = bytes.split_at_checked(32 + width).ok_or(Error::Encoding)?;
        let (signatures, remainder) = signatures.as_chunks::<64>();
        let bitmap = &fixed[32..];
        if !remainder.is_empty()
            || SignerBitmap::new(members, bitmap)?.signature_count() != signatures.len()
        {
            return Err(Error::Encoding);
        }
        Ok(Self {
            result: fixed[..32].try_into().unwrap(),
            bitmap,
            signatures,
        })
    }
}

pub fn signing_digest(domain: &Hash32, request: &[u8; OUT_POINT], result: &Hash32) -> Hash32 {
    digest(
        b"Stem CKB security domain live Cell attestation\0",
        &[domain, request, result],
    )
}

pub fn live_value(subject: &[u8; OUT_POINT], output: &[u8], data_hash: &Hash32) -> Hash32 {
    digest(
        b"Stem CKB security domain live Cell value\0",
        &[subject, output, data_hash],
    )
}

pub fn valid_point(point: &[u8; OUT_POINT]) -> bool {
    point[..32] != [0; 32] && point[32..] != u32::MAX.to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    #[test]
    fn removed_capital_cannot_lower_the_registered_quorum() -> Result<()> {
        let mut keys = [1, 2, 3, 4].map(|seed| SigningKey::from_bytes(&[seed; 32]));
        keys.sort_by_key(|key| key.verifying_key().to_bytes());
        let mut domain = Domain {
            retired: false,
            positions: keys
                .iter()
                .enumerate()
                .map(|(i, key)| Position {
                    key: key.verifying_key().to_bytes(),
                    beneficiary: [i as u8 + 1; 20],
                    bond: MIN_PAYMENT,
                    slashed: false,
                })
                .collect(),
        };
        let policy = Policy {
            type_id: [1; 32],
            ckb_genesis: [2; 32],
            network: [3; 32],
            operator: [4; 20],
            penalty: [5; 20],
            quorum_bps: 7500,
            challenge_blocks: 12,
        };
        let id = [6; 32];
        let request = [7; OUT_POINT];
        let value = [8; 32];
        let digest = signing_digest(&id, &request, &value);
        let signatures = keys.map(|key| key.sign(&digest).to_bytes());
        let reward = 3 * MIN_PAYMENT + 1;
        assert_eq!(
            domain.verify_service(
                &policy,
                &id,
                &request,
                &value,
                reward,
                &[7],
                &signatures[..3]
            )?,
            [(0, MIN_PAYMENT + 1), (1, MIN_PAYMENT), (2, MIN_PAYMENT)]
        );
        assert_eq!(Domain::decode(&domain.encode())?, domain);
        domain.positions[3].slashed = true;
        assert_eq!(
            domain
                .verify_service(
                    &policy,
                    &id,
                    &request,
                    &value,
                    reward,
                    &[7],
                    &signatures[..3]
                )?
                .len(),
            3
        );
        domain.positions[2].slashed = true;
        assert_eq!(
            domain.verify_service(
                &policy,
                &id,
                &request,
                &value,
                reward,
                &[3],
                &signatures[..2]
            ),
            Err(Error::Quorum)
        );
        assert_eq!(
            domain.verify_service(
                &policy,
                &id,
                &request,
                &value,
                reward,
                &[7],
                &signatures[..3]
            ),
            Err(Error::Authority)
        );
        assert_eq!(domain.bonded()?, 2 * MIN_PAYMENT);
        Ok(())
    }
}
