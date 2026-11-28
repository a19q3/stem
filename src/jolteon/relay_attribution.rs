//! Bounded local measurement of signed-wire identities. It never gates admission.
use std::collections::HashMap;

use stem_stake::Hash32;

const MAX_IDENTITIES: usize = 524_288;

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct Metrics {
    pub tracked_identities: usize,
    pub untracked_relays: u64,
    pub untracked_finalizations: u64,
    pub first: RelayMetrics,
    pub repeated: RelayMetrics,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct RelayMetrics {
    pub frames: u64,
    pub pool_hits: u64,
    pub capacity_rejections: u64,
    pub decode_errors: u64,
    pub after_finality: u64,
    pub after_finality_checks: u64,
    pub first_check: CheckMetrics,
    pub repeated_same_height: CheckMetrics,
    pub repeated_new_height: CheckMetrics,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct CheckMetrics {
    pub count: u64,
    pub elapsed_us: u64,
    pub passed: u64,
}

impl CheckMetrics {
    fn record(&mut self, elapsed_us: u64, passed: bool) {
        self.count = self.count.saturating_add(1);
        self.elapsed_us = self.elapsed_us.saturating_add(elapsed_us);
        self.passed = self.passed.saturating_add(u64::from(passed));
    }
}

pub(super) enum Outcome {
    PoolHit,
    Capacity,
    DecodeError,
    Checked {
        height: u64,
        elapsed_us: u64,
        passed: bool,
    },
}

#[derive(Default)]
struct Identity {
    relayed: bool,
    finalized: bool,
    checked_height: Option<u64>,
}

#[derive(Default)]
pub(super) struct Attribution {
    seen: HashMap<Hash32, Identity>,
    metrics: Metrics,
}

impl Attribution {
    pub(super) fn finalized(&mut self, id: Hash32) {
        if let Some(identity) = Self::identity(&mut self.seen, id) {
            identity.finalized = true;
        } else {
            self.metrics.untracked_finalizations =
                self.metrics.untracked_finalizations.saturating_add(1);
        }
    }

    pub(super) fn record(&mut self, id: Hash32, outcome: Outcome) {
        let Some(identity) = Self::identity(&mut self.seen, id) else {
            self.metrics.untracked_relays = self.metrics.untracked_relays.saturating_add(1);
            return;
        };
        let metrics = if identity.relayed {
            &mut self.metrics.repeated
        } else {
            identity.relayed = true;
            &mut self.metrics.first
        };
        metrics.frames = metrics.frames.saturating_add(1);
        metrics.after_finality = metrics
            .after_finality
            .saturating_add(u64::from(identity.finalized));
        match outcome {
            Outcome::PoolHit => metrics.pool_hits = metrics.pool_hits.saturating_add(1),
            Outcome::Capacity => {
                metrics.capacity_rejections = metrics.capacity_rejections.saturating_add(1)
            }
            Outcome::DecodeError => metrics.decode_errors = metrics.decode_errors.saturating_add(1),
            Outcome::Checked {
                height,
                elapsed_us,
                passed,
            } => {
                metrics.after_finality_checks = metrics
                    .after_finality_checks
                    .saturating_add(u64::from(identity.finalized));
                let counts = match identity.checked_height.replace(height) {
                    None => &mut metrics.first_check,
                    Some(old) if old == height => &mut metrics.repeated_same_height,
                    Some(_) => &mut metrics.repeated_new_height,
                };
                counts.record(elapsed_us, passed);
            }
        }
    }

    fn identity(seen: &mut HashMap<Hash32, Identity>, id: Hash32) -> Option<&mut Identity> {
        use std::collections::hash_map::Entry;
        let full = seen.len() == MAX_IDENTITIES;
        match seen.entry(id) {
            Entry::Occupied(entry) => Some(entry.into_mut()),
            Entry::Vacant(entry) if !full => Some(entry.insert(Identity::default())),
            Entry::Vacant(_) => None,
        }
    }

    pub(super) fn metrics(&self) -> Metrics {
        Metrics {
            tracked_identities: self.seen.len(),
            ..self.metrics
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_wire_repeats_from_repeat_validation_and_finalized_work() {
        let mut attribution = Attribution::default();
        let checked = |height| Outcome::Checked {
            height,
            elapsed_us: 9,
            passed: height == 3,
        };
        let id = [1; 32];
        attribution.record(id, Outcome::Capacity);
        attribution.record(id, checked(3));
        attribution.record(id, Outcome::PoolHit);
        attribution.record(id, checked(3));
        attribution.finalized(id);
        attribution.record(id, checked(4));
        attribution.finalized([2; 32]);
        attribution.record([2; 32], checked(4));
        let metrics = attribution.metrics();
        assert_eq!(metrics.tracked_identities, 2);
        assert_eq!(metrics.first.frames, 2);
        assert_eq!(metrics.first.capacity_rejections, 1);
        assert_eq!(metrics.first.after_finality_checks, 1);
        assert_eq!(metrics.repeated.frames, 4);
        assert_eq!(metrics.repeated.pool_hits, 1);
        assert_eq!(metrics.repeated.first_check.passed, 1);
        assert_eq!(metrics.repeated.repeated_same_height.elapsed_us, 9);
        assert_eq!(metrics.repeated.repeated_new_height.passed, 0);
        assert_eq!(metrics.repeated.after_finality_checks, 1);
    }
}
