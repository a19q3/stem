//! Authenticated peer frames and short-lived endpoint announcements.

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use stem_stake::{Hash32, Member, jolteon::MAX_MEMBERS};

use super::{
    transport::Network,
    wire::{self, Frame},
};

pub(super) const AUTHENTICATED_PEER: u32 = u32::MAX - 1;
const REFRESH: Duration = Duration::from_secs(2);
const ENDPOINT_LIFETIME_MS: u64 = 30_000;
const ENDPOINT_CLOCK_ALLOWANCE_MS: u64 = 10_000;
const SIGNED_PREFIX: usize = 32 + 64;
const MAX_ADDRESS_BYTES: usize = 64;
pub(super) const MAX_SIGNED_ENDPOINT_BYTES: usize = SIGNED_PREFIX + 8 + MAX_ADDRESS_BYTES;
pub(super) const CHALLENGE_BYTES: usize = 32;
pub(super) const SIGNED_CHALLENGE_BYTES: usize = SIGNED_PREFIX + CHALLENGE_BYTES;

#[derive(Clone)]
pub(super) struct Authentication {
    pub network: Hash32,
    pub genesis: Hash32,
    pub key: SigningKey,
}

impl Authentication {
    fn digest(&self, author: &Hash32, epoch: u64, kind: u32, body: &[u8]) -> Hash32 {
        let mut digest = ckb_hash::new_blake2b();
        digest.update(b"Stem Jolteon authenticated transport v1\0");
        digest.update(&self.network);
        digest.update(&self.genesis);
        digest.update(author);
        digest.update(&epoch.to_le_bytes());
        digest.update(&kind.to_le_bytes());
        digest.update(body);
        let mut out = [0; 32];
        digest.finalize(&mut out);
        out
    }

    pub(super) fn sign(&self, epoch: u64, kind: u32, body: Bytes) -> Bytes {
        let author = self.key.verifying_key().to_bytes();
        let signature = self.key.sign(&self.digest(&author, epoch, kind, &body));
        let mut signed = Vec::with_capacity(SIGNED_PREFIX + body.len());
        signed.extend(author);
        signed.extend(signature.to_bytes());
        signed.extend_from_slice(&body);
        signed.into()
    }

    pub(super) fn verify(&self, frame: &Frame) -> Result<(Hash32, Bytes)> {
        ensure!(frame.sender == AUTHENTICATED_PEER, "peer transport authentication required");
        let (proof, _) = frame.body.split_at_checked(SIGNED_PREFIX).context("short peer signature")?;
        let (author, signature) = proof.split_at(32);
        let author = author.try_into()?;
        let key = VerifyingKey::from_bytes(&author)?;
        let signature = Signature::from_bytes(signature.try_into()?);
        let body = frame.body.slice(SIGNED_PREFIX..);
        key.verify_strict(&self.digest(&author, frame.epoch, frame.kind, &body), &signature)?;
        Ok((author, body))
    }
}

pub(super) struct Endpoint {
    pub address: SocketAddr,
    pub expires_ms: u64,
}

impl Endpoint {
    pub(super) fn announce(address: SocketAddr, now_ms: u64) -> Vec<u8> {
        let expires = now_ms.saturating_add(ENDPOINT_LIFETIME_MS).to_le_bytes();
        [expires.as_slice(), address.to_string().as_bytes()].concat()
    }

    pub(super) fn decode(body: &[u8], now_ms: u64) -> Result<Self> {
        let (expires, address) = body.split_first_chunk().context("short endpoint announcement")?;
        let expires_ms = u64::from_le_bytes(*expires);
        ensure!(
            expires_ms > now_ms && expires_ms - now_ms <= ENDPOINT_LIFETIME_MS + ENDPOINT_CLOCK_ALLOWANCE_MS,
            "endpoint announcement expired or too far ahead"
        );
        ensure!(address.len() <= MAX_ADDRESS_BYTES, "oversize endpoint address");
        let text = std::str::from_utf8(address)?;
        let address: SocketAddr = text.parse()?;
        ensure!(
            address.to_string() == text && address.port() != 0 && !address.ip().is_unspecified() && !address.ip().is_multicast(),
            "invalid endpoint address"
        );
        Ok(Self { address, expires_ms })
    }
}

struct LearnedEndpoint {
    peer: u16,
    dial: Option<SocketAddr>,
    expires_ms: u64,
}

pub(super) struct Discovery {
    publish: Option<SocketAddr>,
    endpoints: BTreeMap<Hash32, LearnedEndpoint>,
    pub active: Vec<Member>,
    previous: Vec<Member>,
    next_slot: usize,
    free_slots: Vec<u16>,
    last_refresh: Option<Instant>,
}

impl Discovery {
    pub(super) fn new(publish: Option<SocketAddr>, initial_slots: usize, active: Vec<Member>) -> Self {
        Self {
            publish,
            endpoints: BTreeMap::new(),
            active,
            previous: Vec::new(),
            next_slot: initial_slots,
            free_slots: Vec::new(),
            last_refresh: None,
        }
    }

    pub(super) fn id(&self, key: &Hash32) -> Option<u16> {
        self.endpoints.get(key).map(|route| route.peer)
    }

    pub(super) fn slots(&self) -> usize {
        self.next_slot
    }

    pub(super) fn eligible(&self, key: &Hash32, pending: &[Member]) -> bool {
        [&self.active[..], &self.previous[..], pending]
            .iter()
            .any(|members| members.binary_search_by_key(key, |member| member.key).is_ok())
    }

    pub(super) fn learn(&mut self, key: Hash32, endpoint: Endpoint, network: &mut Network) -> Result<bool> {
        use std::collections::btree_map::Entry;
        let assigned = self.endpoints.len();
        let route = match self.endpoints.entry(key) {
            Entry::Occupied(route) => route.into_mut(),
            Entry::Vacant(slot) => {
                ensure!(assigned < 3 * MAX_MEMBERS, "dynamic peer routes are full");
                let peer = if let Some(peer) = self.free_slots.pop() {
                    peer
                } else {
                    let peer = u16::try_from(self.next_slot)?;
                    self.next_slot += 1;
                    peer
                };
                slot.insert(LearnedEndpoint { peer, dial: None, expires_ms: 0 })
            }
        };
        if route.expires_ms >= endpoint.expires_ms {
            return Ok(false);
        }
        if route.dial != Some(endpoint.address) {
            route.dial = Some(endpoint.address);
            network.update_endpoint(route.peer, Some((endpoint.address, key)));
        }
        route.expires_ms = endpoint.expires_ms;
        Ok(true)
    }

    pub(super) fn handoff(&mut self, active: &[Member], network: &mut Network) {
        self.previous = std::mem::replace(&mut self.active, active.to_vec());
        self.endpoints.retain(|key, route| {
            if [&self.active, &self.previous].iter().any(|members| members.binary_search_by_key(key, |member| member.key).is_ok()) {
                return true;
            }
            network.update_endpoint(route.peer, None);
            self.free_slots.push(route.peer);
            false
        });
    }

    pub(super) fn tick(&mut self, now_ms: u64, network: &mut Network) {
        if self.last_refresh.is_some_and(|last| last.elapsed() < REFRESH) {
            return;
        }
        self.last_refresh = Some(Instant::now());
        for route in self.endpoints.values_mut() {
            if route.expires_ms <= now_ms && route.dial.take().is_some() {
                network.update_endpoint(route.peer, None);
            }
        }
        if let Some(address) = self.publish {
            network.broadcast(wire::PEER_ENDPOINT, Endpoint::announce(address, now_ms));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_identity_covers_network_genesis_epoch_kind_and_body() -> Result<()> {
        let auth = Authentication { network: [1; 32], genesis: [2; 32], key: SigningKey::from_bytes(&[3; 32]) };
        let endpoint: Bytes = Endpoint::announce("127.0.0.1:9876".parse()?, 500).into();
        let mut frame = Frame {
            sender: AUTHENTICATED_PEER,
            epoch: 10,
            kind: wire::PEER_ENDPOINT,
            body: auth.sign(10, wire::PEER_ENDPOINT, endpoint.clone()),
        };
        assert_eq!(auth.verify(&frame)?, (auth.key.verifying_key().to_bytes(), endpoint));
        let mut other = auth.clone();
        other.network = [9; 32];
        assert!(other.verify(&frame).is_err());
        other.network = auth.network;
        other.genesis = [8; 32];
        assert!(other.verify(&frame).is_err());
        frame.epoch += 1;
        assert!(auth.verify(&frame).is_err());
        frame.epoch -= 1;
        frame.kind = wire::GET;
        assert!(auth.verify(&frame).is_err());
        frame.kind = wire::PEER_ENDPOINT;
        let mut body = frame.body.to_vec();
        *body.last_mut().unwrap() ^= 1;
        frame.body = body.into();
        assert!(auth.verify(&frame).is_err());
        Ok(())
    }

    #[test]
    fn endpoints_require_a_bounded_live_announcement() -> Result<()> {
        let address = "[::1]:8899".parse()?;
        let body = Endpoint::announce(address, 100);
        let endpoint = Endpoint::decode(&body, 200)?;
        assert_eq!(endpoint.address, address);
        assert_eq!(endpoint.expires_ms, 100 + ENDPOINT_LIFETIME_MS);
        assert!(Endpoint::decode(&body, endpoint.expires_ms).is_err());
        assert!(Endpoint::decode(&Endpoint::announce(address, 60_000), 200).is_err());
        assert!(Endpoint::decode(&Endpoint::announce("0.0.0.0:7".parse()?, 100), 200).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn route_slots_expire_and_are_reused_only_after_committee_handoff() -> Result<()> {
        let key = [2; 32];
        let incoming = Member { key, power: 3 };
        let addresses = ["127.0.0.1:0".parse()?];
        let (mut network, _inbox) = Network::listen(0, &addresses, 0, None, None).await?;
        let mut discovery = Discovery::new(None, addresses.len(), Vec::new());
        assert!(!discovery.eligible(&key, &[]));
        assert!(discovery.eligible(&key, std::slice::from_ref(&incoming)));
        let endpoint_bytes = Endpoint::announce("127.0.0.1:9444".parse()?, 400);
        let endpoint = Endpoint::decode(&endpoint_bytes, 400)?;
        let expires = endpoint.expires_ms;
        assert!(discovery.learn(key, endpoint, &mut network)?);
        assert!(!discovery.learn(key, Endpoint::decode(&endpoint_bytes, 400)?, &mut network)?);
        let slot = discovery.id(&key).unwrap();
        assert!(network.has_route(slot));
        discovery.handoff(&[incoming], &mut network);
        assert!(discovery.eligible(&key, &[]));
        discovery.tick(expires, &mut network);
        assert!(!network.has_route(slot));
        assert_eq!(discovery.id(&key), Some(slot));
        discovery.handoff(&[], &mut network);
        assert!(discovery.eligible(&key, &[]));
        discovery.handoff(&[], &mut network);
        assert!(!discovery.eligible(&key, &[]));
        assert_eq!(discovery.id(&key), None);
        assert!(discovery.learn([3; 32], Endpoint::decode(&endpoint_bytes, 400)?, &mut network)?);
        assert_eq!(discovery.id(&[3; 32]), Some(slot));
        Ok(())
    }
}
