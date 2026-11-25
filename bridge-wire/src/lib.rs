#![no_std]
pub mod asset;
pub mod intercell;
pub mod spore;

pub const HASH_SIZE: usize = 32;
pub const MAX_RECIPIENT_SIZE: usize = 4096;
pub const AMOUNT_SIZE: usize = 16;
pub const DEPOSIT: u8 = 0;
pub const ESCROW: u8 = 1;
pub const NATIVE_VAULT_SIZE: usize = 146;
pub const XUDT_VAULT_SIZE: usize = 226;
pub const PAYOUT_UNITS_OFFSET: usize = 210;

#[derive(PartialEq, Eq)]
pub struct Asset {
    pub gate_code: [u8; HASH_SIZE],
    pub type_hash: [u8; HASH_SIZE],
}

pub fn vault_asset(data: &[u8]) -> Option<Asset> {
    if data.len() != XUDT_VAULT_SIZE || data[0] != 2 {
        return None;
    }
    let gate_code = data[NATIVE_VAULT_SIZE..178].try_into().ok()?;
    let type_hash = data[178..PAYOUT_UNITS_OFFSET].try_into().ok()?;
    if gate_code == [0; HASH_SIZE] || type_hash == [0; HASH_SIZE] {
        return None;
    }
    Some(Asset {
        gate_code,
        type_hash,
    })
}

#[derive(PartialEq, Eq)]
pub enum Gate<'a> {
    Deposit {
        chain: &'a [u8; HASH_SIZE],
        recipient: &'a [u8],
    },
    Escrow,
}

pub fn gate(args: &[u8]) -> Option<(&[u8; HASH_SIZE], Gate<'_>)> {
    let (vault, body) = args.split_at_checked(HASH_SIZE)?;
    let (&mode, body) = body.split_first()?;
    let role = match mode {
        DEPOSIT => {
            let (chain, recipient) = body.split_at_checked(HASH_SIZE)?;
            if recipient.is_empty() || recipient.len() > MAX_RECIPIENT_SIZE {
                return None;
            }
            Gate::Deposit {
                chain: chain.try_into().ok()?,
                recipient,
            }
        }
        ESCROW if body.is_empty() => Gate::Escrow,
        _ => return None,
    };
    Some((vault.try_into().ok()?, role))
}

pub fn amount(data: &[u8]) -> Option<u128> {
    Some(u128::from_le_bytes(data.try_into().ok()?))
}

pub const WITHDRAWAL_KEY_SIZE: usize = 15 + HASH_SIZE + 8;
const WITHDRAWAL_PREFIX: &[u8; 15] = b"bridge/pending/";
const WITHDRAWAL_FIXED: usize = 100;
pub const MAX_WITHDRAWAL_RECORD: usize = HASH_SIZE + WITHDRAWAL_FIXED + MAX_RECIPIENT_SIZE;

pub fn pending_key(lane: &[u8; HASH_SIZE], nonce: u64) -> [u8; WITHDRAWAL_KEY_SIZE] {
    let mut result = [0; WITHDRAWAL_KEY_SIZE];
    result[..15].copy_from_slice(WITHDRAWAL_PREFIX);
    result[15..47].copy_from_slice(lane);
    result[47..].copy_from_slice(&nonce.to_le_bytes());
    result
}

pub fn lane_id(
    genesis: &[u8; HASH_SIZE],
    vault: &[u8; HASH_SIZE],
    asset: Option<&[u8; HASH_SIZE]>,
) -> [u8; HASH_SIZE] {
    let tag = [u8::from(asset.is_some())];
    hash(
        b"Stem bridge lane\0",
        &[genesis, vault, &tag, asset.map_or(&[], |value| &value[..])],
    )
}

pub fn withdrawal_receipt_id(network: &[u8; HASH_SIZE], receipt: &[u8]) -> [u8; HASH_SIZE] {
    hash(b"Stem withdrawal receipt\0", &[network, receipt])
}

pub struct Withdrawal<'a> {
    pub id: [u8; HASH_SIZE],
    pub lane: [u8; HASH_SIZE],
    pub nonce: u64,
    pub capacity: u64,
    pub units: u128,
    pub recipient: &'a [u8],
}

impl<'a> Withdrawal<'a> {
    pub fn decode(network: &[u8; HASH_SIZE], record: &'a [u8]) -> Option<Self> {
        let (id, receipt) = record.split_at_checked(HASH_SIZE)?;
        let (fields, recipient) = receipt.split_at_checked(WITHDRAWAL_FIXED)?;
        if recipient.is_empty()
            || recipient.len() > MAX_RECIPIENT_SIZE
            || id != withdrawal_receipt_id(network, receipt)
        {
            return None;
        }
        let capacity = u64::from_le_bytes(fields[76..84].try_into().ok()?);
        if capacity == 0 {
            return None;
        }
        Some(Self {
            id: id.try_into().ok()?,
            lane: fields[..32].try_into().ok()?,
            nonce: u64::from_le_bytes(fields[32..40].try_into().ok()?),
            capacity,
            units: amount(&fields[84..100])?,
            recipient,
        })
    }
}

fn hash(domain: &[u8], parts: &[&[u8]]) -> [u8; HASH_SIZE] {
    let mut hasher = ckb_hash::new_blake2b();
    hasher.update(domain);
    for part in parts {
        hasher.update(part);
    }
    let mut result = [0; HASH_SIZE];
    hasher.finalize(&mut result);
    result
}
