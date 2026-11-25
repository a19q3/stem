//! Canonical signed terms shared by Stem's solver, wallet, and RISC-V Lock.
#![no_std]

use ckb_hash::new_blake2b;

pub const TRADE_BYTES: usize = 49;
pub const SPONSOR_BYTES: usize = 125;
pub const LOCK_ARGS_BYTES: usize = 52;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Terms {
    pub valid_before: u64,
    pub max_fee: u64,
    pub delivery: [u8; 32],
    pub sponsorship: Option<Sponsorship>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Sponsorship {
    pub max_debit: u64,
    pub subject: [u8; 36],
    pub delivery: [u8; 32],
}

pub struct Authorization {
    pub network: [u8; 32],
    pub lock: [u8; 32],
    pub out_point: [u8; 36],
    pub input_cell: [u8; 32],
    pub dependencies: [u8; 32],
}

impl Terms {
    pub fn encode(&self, buffer: &mut [u8; SPONSOR_BYTES]) -> usize {
        buffer[0] = u8::from(self.sponsorship.is_some());
        buffer[1..9].copy_from_slice(&self.valid_before.to_le_bytes());
        buffer[9..17].copy_from_slice(&self.max_fee.to_le_bytes());
        buffer[17..TRADE_BYTES].copy_from_slice(&self.delivery);
        if let Some(sponsor) = &self.sponsorship {
            buffer[49..57].copy_from_slice(&sponsor.max_debit.to_le_bytes());
            buffer[57..93].copy_from_slice(&sponsor.subject);
            buffer[93..].copy_from_slice(&sponsor.delivery);
            SPONSOR_BYTES
        } else {
            TRADE_BYTES
        }
    }

    pub fn decode(encoded: &[u8]) -> Option<Self> {
        let sponsorship = match (encoded.first(), encoded.len()) {
            (Some(0), TRADE_BYTES) => None,
            (Some(1), SPONSOR_BYTES) => Some(Sponsorship {
                max_debit: u64::from_le_bytes(encoded[49..57].try_into().ok()?),
                subject: encoded[57..93].try_into().ok()?,
                delivery: encoded[93..].try_into().ok()?,
            }),
            _ => return None,
        };
        let terms = Self {
            valid_before: u64::from_le_bytes(encoded[1..9].try_into().ok()?),
            max_fee: u64::from_le_bytes(encoded[9..17].try_into().ok()?),
            delivery: encoded[17..TRADE_BYTES].try_into().ok()?,
            sponsorship,
        };
        terms.is_valid().then_some(terms)
    }

    pub fn is_valid(&self) -> bool {
        self.valid_before != 0
            && self
                .sponsorship
                .is_none_or(|s| (1..=self.max_fee).contains(&s.max_debit))
    }

    pub fn signing_digest(&self, context: &Authorization) -> [u8; 32] {
        let mut encoded = [0; SPONSOR_BYTES];
        let len = self.encode(&mut encoded);
        let mut hash = new_blake2b();
        hash.update(b"Stem paired Cell authorization\0");
        hash.update(&context.network);
        hash.update(&context.lock);
        hash.update(&context.out_point);
        hash.update(&context.input_cell);
        hash.update(&context.dependencies);
        hash.update(&encoded[..len]);
        let mut bytes = [0; 32];
        hash.finalize(&mut bytes);
        bytes
    }
}
