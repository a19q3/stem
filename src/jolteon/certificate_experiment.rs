//! Opt-in research candidate. All entries are owned by one replica and epoch.
use super::{Committee, Network, Point, TimeoutCertificate};
use bytes::Bytes;
use serde::Serialize;
use std::sync::Arc;

#[derive(Default, Serialize)]
pub(super) struct FrameCache {
    #[serde(skip)]
    latest: Option<(Bytes, Arc<Vec<u8>>)>,
    hits: u64,
    encodes: u64,
}

impl FrameCache {
    // Each slot has a fixed sender/kind and is cleared on committee replacement.
    pub(super) fn send(&mut self, net: &Network, kind: u32, body: &[u8]) {
        if let Some((old, frame)) = &self.latest
            && old.as_ref() == body
        {
            self.hits += 1;
            net.broadcast_frame(kind, frame.clone());
            return;
        }
        let bytes = Bytes::copy_from_slice(body);
        let frame = net.frame(net.epoch, kind, bytes.clone());
        self.latest = Some((bytes, frame.clone()));
        self.encodes += 1;
        net.broadcast_frame(kind, frame);
    }
}

#[derive(Default, Serialize)]
pub(super) struct Cache {
    #[serde(skip)]
    parent: Option<(u64, Point, Vec<u8>)>,
    parent_hits: u64,
    parent_tc_checks: u64,
    pub(super) quorum_frame: FrameCache,
    pub(super) timeout_frame: FrameCache,
}

impl Cache {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    // Only the committee's pure parent-view predicate is memoized. Every input
    // is decoded and proposal, execution and durable voting checks still run.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jolteon::wire;
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

    #[tokio::test]
    async fn repeated_frames_match_canonical_encoding() {
        let (net, _inbox) = Network::listen(0, &["127.0.0.1:0".parse().unwrap()], 3)
            .await
            .unwrap();
        let mut cache = FrameCache::default();
        let mut previous = None;
        for bytes in [vec![2; 500], vec![2; 500], vec![3; 500]] {
            cache.send(&net, wire::QUORUM, &bytes);
            let (body, frame) = cache.latest.as_ref().unwrap();
            assert_eq!(
                frame.as_slice(),
                net.frame(3, wire::QUORUM, bytes.clone()).as_slice()
            );
            if let Some((old, old_frame)) = previous {
                assert_eq!(Arc::ptr_eq(frame, &old_frame), old == bytes);
            }
            previous = Some((body.to_vec(), frame.clone()));
        }
        assert_eq!((cache.hits, cache.encodes), (1, 2));
    }

    #[test]
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
