#![no_std]

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
