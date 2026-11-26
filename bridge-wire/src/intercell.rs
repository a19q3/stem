//! Canonical records shared by Stem and the CKB mailbox contract.

use super::{HASH_SIZE, hash};

pub type Identity = [u8; HASH_SIZE];

pub const SEND_ACTION: u16 = 0xff00;
pub const RECEIVE_ACTION: u16 = 0xff01;
pub const MAX_ACTIONS_PER_TRANSACTION: usize = 4;
pub const MAX_PAYLOAD: usize = 4096;
pub const MESSAGE_HEADER: usize = 4 * HASH_SIZE + 8;
pub const MAX_MESSAGE: usize = MESSAGE_HEADER + MAX_PAYLOAD;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Message<'a> {
    pub source: Identity,
    pub destination: Identity,
    pub sender: Identity,
    pub receiver: Identity,
    pub nonce: u64,
    pub payload: &'a [u8],
}

impl<'a> Message<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (header, payload) = raw.split_at_checked(MESSAGE_HEADER)?;
        let message = Self {
            source: header[..32].try_into().ok()?,
            destination: header[32..64].try_into().ok()?,
            sender: header[64..96].try_into().ok()?,
            receiver: header[96..128].try_into().ok()?,
            nonce: u64::from_le_bytes(header[128..].try_into().ok()?),
            payload,
        };
        (message.source != message.destination
            && [
                message.source,
                message.destination,
                message.sender,
                message.receiver,
            ]
            .iter()
            .all(|id| *id != [0; 32])
            && payload.len() <= MAX_PAYLOAD)
            .then_some(message)
    }

    pub fn header(&self) -> [u8; MESSAGE_HEADER] {
        let mut bytes = [0; MESSAGE_HEADER];
        bytes[..32].copy_from_slice(&self.source);
        bytes[32..64].copy_from_slice(&self.destination);
        bytes[64..96].copy_from_slice(&self.sender);
        bytes[96..128].copy_from_slice(&self.receiver);
        bytes[128..].copy_from_slice(&self.nonce.to_le_bytes());
        bytes
    }

    /// An authorized sender can use a nonce once per network and endpoint pair.
    /// The authenticated outbox record separately binds the payload.
    pub fn id(&self) -> Identity {
        hash(b"Stem Intercell message ID\0", &[&self.header()])
    }
}

pub fn record_key(inbound: bool, id: &Identity) -> [u8; 43] {
    let mut key = [0; 43];
    key[..10].copy_from_slice(b"intercell/");
    key[10] = u8::from(inbound);
    key[11..].copy_from_slice(id);
    key
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailboxPolicy {
    pub type_id: Identity,
    pub ckb_genesis: Identity,
    pub stem_network: Identity,
    pub owner: Identity,
    pub checkpoint_code: Identity,
    pub checkpoint_id: Identity,
}

impl MailboxPolicy {
    pub const SIZE: usize = 6 * HASH_SIZE;

    pub fn decode(raw: &[u8]) -> Option<Self> {
        let (fields, remaining) = raw.as_chunks::<HASH_SIZE>();
        let [
            type_id,
            ckb_genesis,
            stem_network,
            owner,
            checkpoint_code,
            checkpoint_id,
        ] = fields
        else {
            return None;
        };
        if !remaining.is_empty() || fields.contains(&[0; 32]) || ckb_genesis == stem_network {
            return None;
        }
        Some(Self {
            type_id: *type_id,
            ckb_genesis: *ckb_genesis,
            stem_network: *stem_network,
            owner: *owner,
            checkpoint_code: *checkpoint_code,
            checkpoint_id: *checkpoint_id,
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MailboxState<'a> {
    pub next_nonce: u64,
    pub consumed: Identity,
    /// Present only on the successor of a send. Historical inclusion proves it.
    pub outgoing: &'a [u8],
}

impl<'a> MailboxState<'a> {
    pub const HEADER: usize = 8 + HASH_SIZE;

    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (header, outgoing) = raw.split_at_checked(Self::HEADER)?;
        let next_nonce = u64::from_le_bytes(header[..8].try_into().ok()?);
        if !outgoing.is_empty() && Message::decode(outgoing)?.nonce.checked_add(1)? != next_nonce {
            return None;
        }
        Some(Self {
            next_nonce,
            consumed: header[8..].try_into().ok()?,
            outgoing,
        })
    }
}

pub const MAX_PATRICIA_PATH: usize = 64 * 1024;
pub const MAX_REPLAY_PATH: usize = 32 * 257;
// 208-byte execution Header, 8-byte leaf count, at most 64 peaks and 63 siblings.
pub const MAX_HISTORY_PROOF: usize = 208 + 8 + HASH_SIZE * (64 + 63);

pub struct Delivery<'a> {
    pub record: &'a [u8],
    pub outbox: &'a [u8],
    pub history: &'a [u8],
    pub replay: &'a [u8],
}

impl<'a> Delivery<'a> {
    /// The three little-endian lengths are followed by their fields, then the replay path.
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (lengths, raw) = raw.split_at_checked(12)?;
        let record_len = u32::from_le_bytes(lengths[..4].try_into().ok()?) as usize;
        let outbox_len = u32::from_le_bytes(lengths[4..8].try_into().ok()?) as usize;
        let history_len = u32::from_le_bytes(lengths[8..].try_into().ok()?) as usize;
        if record_len > MAX_MESSAGE
            || outbox_len > MAX_PATRICIA_PATH
            || history_len > MAX_HISTORY_PROOF
        {
            return None;
        }
        let (record, raw) = raw.split_at_checked(record_len)?;
        let (outbox, raw) = raw.split_at_checked(outbox_len)?;
        let (history, replay) = raw.split_at_checked(history_len)?;
        (replay.len() <= MAX_REPLAY_PATH).then_some(Self {
            record,
            outbox,
            history,
            replay,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_binds_networks_endpoints_and_nonce_with_one_payload_per_slot() {
        let message = Message {
            source: [1; 32],
            destination: [2; 32],
            sender: [3; 32],
            receiver: [4; 32],
            nonce: u64::MAX,
            payload: b"event",
        };
        let mut encoded = [0; MESSAGE_HEADER + 5];
        encoded[..MESSAGE_HEADER].copy_from_slice(&message.header());
        encoded[MESSAGE_HEADER..].copy_from_slice(message.payload);
        assert_eq!(Message::decode(&encoded), Some(message.clone()));
        encoded[MESSAGE_HEADER] ^= 1;
        assert_eq!(Message::decode(&encoded).unwrap().id(), message.id());
        for offset in [0, 32, 64, 96, 128] {
            encoded[offset] ^= 1;
            assert_ne!(Message::decode(&encoded).unwrap().id(), message.id());
            encoded[offset] ^= 1;
        }
        assert!(Message::decode(&encoded[..MESSAGE_HEADER - 1]).is_none());
        let mut oversized = [0; MAX_MESSAGE + 1];
        oversized[..MESSAGE_HEADER].copy_from_slice(&message.header());
        assert!(Message::decode(&oversized).is_none());
        assert_ne!(
            record_key(false, &message.id()),
            record_key(true, &message.id())
        );
    }
}
