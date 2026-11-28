//! Opt-in certificate reuse, scoped to one replica and committee.
use super::Network;
#[cfg(feature = "jolteon-shared-certificate-path")]
use super::VerifiedQuorum;
#[cfg(feature = "jolteon-certificate-experiment")]
use super::{Committee, Point, TimeoutCertificate};
use bytes::Bytes;
use serde::Serialize;
use std::sync::Arc;

#[cfg(feature = "jolteon-retain-verified-quorums")]
const RECENT_QUORUMS: usize = 32;

#[cfg(feature = "jolteon-retain-verified-quorums")]
struct RecentQuorums(lru::LruCache<Box<[u8]>, VerifiedQuorum>);

#[cfg(feature = "jolteon-retain-verified-quorums")]
impl Default for RecentQuorums {
    fn default() -> Self {
        Self(lru::LruCache::new(RECENT_QUORUMS))
    }
}

#[derive(Default, Serialize)]
pub(super) struct FrameCache {
    #[serde(skip)]
    latest: Option<CachedFrame>,
    hits: u64,
    encodes: u64,
}

struct CachedFrame {
    epoch: u64,
    kind: u32,
    body: Bytes,
    frame: Arc<Vec<u8>>,
}

impl FrameCache {
    // One sender owns this cache, and committee replacement clears its slots.
    pub(super) fn send(&mut self, net: &Network, kind: u32, body: &[u8]) {
        if let Some(cached) = &self.latest
            && (cached.epoch, cached.kind, cached.body.as_ref()) == (net.epoch, kind, body)
        {
            self.hits += 1;
            net.broadcast_frame(kind, cached.frame.clone());
            return;
        }
        let bytes = Bytes::copy_from_slice(body);
        let frame = net.frame(net.epoch, kind, bytes.clone());
        self.latest = Some(CachedFrame {
            epoch: net.epoch,
            kind,
            body: bytes,
            frame: frame.clone(),
        });
        self.encodes += 1;
        net.broadcast_frame(kind, frame);
    }
}

#[derive(Default, Serialize)]
pub(super) struct Cache {
    #[cfg(feature = "jolteon-certificate-experiment")]
    #[serde(skip)]
    parent: Option<(u64, Point, Vec<u8>)>,
    parent_hits: u64,
    parent_tc_checks: u64,
    pub(super) quorum_frame: FrameCache,
    pub(super) timeout_frame: FrameCache,
    #[cfg(feature = "jolteon-shared-certificate-path")]
    current_qc_hits: u64,
    #[cfg(feature = "jolteon-retain-verified-quorums")]
    #[serde(skip)]
    recent_quorums: RecentQuorums,
    #[cfg(feature = "jolteon-retain-verified-quorums")]
    recent_qc_hits: u64,
    #[cfg(feature = "jolteon-retain-verified-quorums")]
    recent_qc_inserts: u64,
    #[cfg(feature = "jolteon-shared-certificate-path")]
    current_tc_hits: u64,
    #[cfg(feature = "jolteon-shared-certificate-path")]
    pub(super) vote_frame_encodes_saved: u64,
}

impl Cache {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    // Only the committee's pure parent-view predicate is memoized. Every input
    // is decoded and proposal, execution and durable voting checks still run.
    #[cfg(feature = "jolteon-certificate-experiment")]
    pub(super) fn verify_parent(
        &mut self,
        committee: &Committee,
        view: u64,
        parent: Point,
        raw: &[u8],
    ) -> anyhow::Result<Option<TimeoutCertificate>> {
        if raw.is_empty() {
            committee.verify_parent_view(view, parent, None)?;
            return Ok(None);
        }
        let tc = TimeoutCertificate::decode(raw, committee.size())?;
        if let Some((old_view, old_parent, bytes)) = &self.parent
            && (*old_view, *old_parent, bytes.as_slice()) == (view, parent, raw)
        {
            self.parent_hits += 1;
            return Ok(Some(tc));
        }
        self.parent_tc_checks += 1;
        committee.verify_parent_view(view, parent, Some(&tc))?;
        self.parent = Some((view, parent, raw.to_vec()));
        Ok(Some(tc))
    }

    /// The replica maintains the decoded QC and its authenticated journal
    /// bytes together. A different encoding goes through the normal verifier.
    #[cfg(feature = "jolteon-shared-certificate-path")]
    pub(super) fn reuse_quorum(
        &mut self,
        high: &VerifiedQuorum,
        verified: &[u8],
        received: &[u8],
    ) -> Option<VerifiedQuorum> {
        if verified == received {
            self.current_qc_hits += 1;
            return Some(high.clone());
        }
        None
    }

    #[cfg(feature = "jolteon-retain-verified-quorums")]
    pub(super) fn recent_quorum(&mut self, raw: &[u8]) -> Option<VerifiedQuorum> {
        // Match the largest legal bitmap and signature array before hashing.
        const MAX_BYTES: usize =
            super::Point::SIZE + super::MAX_MEMBERS.div_ceil(8) + super::MAX_MEMBERS * 64;
        if raw.len() > MAX_BYTES {
            return None;
        }
        let quorum = self.recent_quorums.0.get(raw)?;
        self.recent_qc_hits = self.recent_qc_hits.saturating_add(1);
        Some(quorum.clone())
    }

    #[cfg(feature = "jolteon-retain-verified-quorums")]
    pub(super) fn remember_quorum(&mut self, raw: &[u8], quorum: &VerifiedQuorum) {
        // Own only the exact QC bytes. A QC can be a slice of a much larger
        // proposal frame, whose backing buffer must not be pinned here.
        self.recent_quorums.0.put(raw.into(), quorum.clone());
        self.recent_qc_inserts = self.recent_qc_inserts.saturating_add(1);
    }

    #[cfg(feature = "jolteon-shared-certificate-path")]
    pub(super) fn known_timeout(&mut self, verified: &[u8], received: &[u8]) -> bool {
        let hit = !verified.is_empty() && verified == received;
        self.current_tc_hits += u64::from(hit);
        hit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "jolteon-shared-certificate-path")]
    use crate::jolteon::Quorum;
    use crate::jolteon::wire;
    use crate::jolteon::{Committee, Point, TimeoutCertificate};
    use ed25519_dalek::{Signer, SigningKey};
    use stem_stake::{
        Member,
        jolteon::{Share, TimeoutVote},
    };

    fn fixture(n: u8) -> (Committee, Vec<Member>, TimeoutCertificate) {
        let mut keys: Vec<_> = (1..=n).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        keys.sort_by_key(|key| key.verifying_key().to_bytes());
        let members: Vec<_> = keys
            .iter()
            .enumerate()
            .map(|(i, key)| Member {
                key: key.verifying_key().to_bytes(),
                power: 1 + i as u64 % 2,
            })
            .collect();
        let committee = Committee::new([1; 32], [2; 32], 0, &members).unwrap();
        let point = Point {
            view: 1,
            parent_view: 0,
            header: [7; 32],
        };
        let votes = keys
            .iter()
            .enumerate()
            .map(|(i, key)| (i as u16, key.sign(&committee.vote_digest(point)).to_bytes()))
            .collect();
        let high = committee.assemble_quorum(point, &votes).unwrap();
        let votes = keys
            .iter()
            .enumerate()
            .map(|(i, key)| TimeoutVote {
                high: point,
                share: Share {
                    member: i as u16,
                    signature: key.sign(&committee.timeout_digest(2, point)).to_bytes(),
                },
            })
            .collect();
        (
            committee,
            members,
            TimeoutCertificate {
                view: 2,
                high,
                votes,
            },
        )
    }

    #[test]
    #[cfg(feature = "jolteon-certificate-experiment")]
    fn success_memo_matches_reference_across_inputs_and_reset() {
        let (committee, members, tc) = fixture(4);
        let parent = tc.high.point;
        let raw = tc.encode();
        let mut cache = Cache::default();
        for _ in 0..3 {
            assert_eq!(
                cache.verify_parent(&committee, 3, parent, &raw).unwrap(),
                Some(tc.clone())
            );
        }
        assert_eq!((cache.parent_hits, cache.parent_tc_checks), (2, 1));
        let mut other = tc.clone();
        other.votes[0].share.signature[0] ^= 1;
        let mut duplicate = tc.clone();
        duplicate.votes[1] = duplicate.votes[0];
        for bytes in [&raw[..9], &other.encode(), &duplicate.encode()] {
            assert!(cache.verify_parent(&committee, 3, parent, bytes).is_err());
        }
        assert!(cache.verify_parent(&committee, 4, parent, &raw).is_err());
        assert!(
            cache
                .verify_parent(&committee, 3, committee.genesis_quorum().point, &raw)
                .is_err()
        );
        assert!(
            cache
                .verify_parent(&committee, 2, parent, &[])
                .unwrap()
                .is_none()
        );
        cache.reset();
        let successor = Committee::at_anchor([1; 32], [2; 32], 1, [3; 32], &members).unwrap();
        assert!(cache.verify_parent(&successor, 3, parent, &raw).is_err());
        assert_eq!(cache.parent_hits, 0);
    }

    #[test]
    #[cfg(feature = "jolteon-shared-certificate-path")]
    fn current_certificate_reuse_requires_exact_authenticated_bytes() {
        let (committee, members, tc) = fixture(4);
        committee.verify_quorum(&tc.high).unwrap();
        committee.verify_timeout_certificate(&tc).unwrap();
        let high: VerifiedQuorum = tc.high.clone().into();
        let quorum = high.encode();
        let timeout = tc.encode();
        let mut cache = Cache::default();
        let reused = cache.reuse_quorum(&high, &quorum, &quorum).unwrap();
        assert_eq!(reused.encode(), quorum);
        assert!(cache.known_timeout(&timeout, &timeout));

        let mut altered = tc.clone();
        altered.votes[0].share.signature[0] ^= 1;
        assert!(!cache.known_timeout(&timeout, &altered.encode()));
        assert!(cache.reuse_quorum(&high, &quorum, &quorum[..9]).is_none());
        let mut changed_quorum = quorum.clone();
        *changed_quorum.last_mut().unwrap() ^= 1;
        assert!(
            cache
                .reuse_quorum(&high, &quorum, &changed_quorum)
                .is_none()
        );
        let decoded = Quorum::decode(&changed_quorum, committee.size()).unwrap();
        assert!(committee.verify_quorum(&decoded).is_err());
        assert_eq!((cache.current_qc_hits, cache.current_tc_hits), (1, 1));

        cache.reset();
        let next = Committee::at_anchor([1; 32], [2; 32], 1, [3; 32], &members).unwrap();
        let anchor: VerifiedQuorum = next.genesis_quorum().into();
        assert!(
            cache
                .reuse_quorum(&anchor, &anchor.encode(), &quorum)
                .is_none()
        );
        assert!(!cache.known_timeout(&[], &timeout));
        assert_eq!((cache.current_qc_hits, cache.current_tc_hits), (0, 0));
    }

    #[test]
    #[cfg(feature = "jolteon-retain-verified-quorums")]
    fn recent_quorums_share_exact_signed_allocations_with_bounded_retention() {
        let (committee, _, tc) = fixture(4);
        let keys: Vec<_> = {
            let mut keys: Vec<_> = (1..=4).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
            keys.sort_by_key(|key| key.verifying_key().to_bytes());
            keys
        };
        let mut cache = Cache::default();
        let high: VerifiedQuorum = tc.high.into();
        let raw = high.encode();
        committee.verify_quorum(&high).unwrap();
        assert!(cache.recent_quorum(&raw).is_none());
        cache.remember_quorum(&raw, &high);
        assert!(Arc::ptr_eq(&cache.recent_quorum(&raw).unwrap(), &high));

        let mut altered = raw.clone();
        *altered.last_mut().unwrap() ^= 1;
        assert!(cache.recent_quorum(&altered).is_none());
        assert!(cache.recent_quorum(&raw[..raw.len() - 1]).is_none());
        assert!(cache.recent_quorum(&[0; 16 * 1024]).is_none());

        let mut last = Vec::new();
        for view in 2..=RECENT_QUORUMS as u64 + 1 {
            let point = Point {
                view,
                parent_view: view - 1,
                header: [7; 32],
            };
            let votes = keys
                .iter()
                .enumerate()
                .map(|(i, key)| (i as u16, key.sign(&committee.vote_digest(point)).to_bytes()))
                .collect();
            let quorum: VerifiedQuorum = committee.assemble_quorum(point, &votes).unwrap().into();
            committee.verify_quorum(&quorum).unwrap();
            last = quorum.encode();
            cache.remember_quorum(&last, &quorum);
        }
        assert_eq!(cache.recent_quorums.0.len(), RECENT_QUORUMS);
        assert!(cache.recent_quorum(&raw).is_none());
        assert_eq!(cache.recent_quorum(&last).unwrap().encode(), last);
        cache.reset();
        assert!(cache.recent_quorum(&last).is_none());
        assert_eq!((cache.recent_qc_hits, cache.recent_qc_inserts), (0, 0));
    }

    #[tokio::test]
    async fn repeated_frames_match_canonical_encoding() {
        let (mut net, _inbox) = Network::listen(0, &["127.0.0.1:0".parse().unwrap()], 3, None)
            .await
            .unwrap();
        let mut cache = FrameCache::default();
        let mut previous = None;
        for (epoch, kind, bytes) in [
            (3, wire::QUORUM, vec![2; 500]),
            (3, wire::QUORUM, vec![2; 500]),
            (3, wire::QUORUM, vec![3; 500]),
            (4, wire::QUORUM, vec![3; 500]),
            (4, wire::TIMEOUT_CERTIFICATE, vec![3; 500]),
        ] {
            net.epoch = epoch;
            cache.send(&net, kind, &bytes);
            let cached = cache.latest.as_ref().unwrap();
            let frame = &cached.frame;
            assert_eq!(
                frame.as_slice(),
                net.frame(epoch, kind, bytes.clone()).as_slice()
            );
            if let Some((key, old_frame)) = previous {
                assert_eq!(Arc::ptr_eq(frame, &old_frame), key == (epoch, kind, bytes));
            }
            previous = Some(((epoch, kind, cached.body.to_vec()), frame.clone()));
        }
        assert_eq!((cache.hits, cache.encodes), (1, 4));
    }

    #[test]
    #[cfg(feature = "jolteon-certificate-experiment")]
    #[ignore = "matched certificate CPU experiment"]
    fn repeated_timeout_cost() {
        for n in [4, 16, 64, 128] {
            let (committee, _, tc) = fixture(n);
            let raw = tc.encode();
            for repeat in 0..6 {
                let modes = if repeat % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                };
                for cached in modes {
                    let mut cache = Cache::default();
                    let now = std::time::Instant::now();
                    for _ in 0..16 {
                        let verified = if cached {
                            cache
                                .verify_parent(&committee, 3, tc.high.point, &raw)
                                .unwrap()
                                .unwrap()
                        } else {
                            let value = TimeoutCertificate::decode(&raw, committee.size()).unwrap();
                            committee
                                .verify_parent_view(3, tc.high.point, Some(&value))
                                .unwrap();
                            value
                        };
                        std::hint::black_box(verified);
                    }
                    println!(
                        "CERT_COST {n} {repeat} {cached} {} {} {}",
                        now.elapsed().as_nanos(),
                        raw.len(),
                        cache.parent_hits
                    );
                }
            }
        }
    }
}
