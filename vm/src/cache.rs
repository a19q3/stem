//! Local caches for completed VM verifications and Script Group scheduling costs.

use std::{
    cell::Cell,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::LocalKey,
    time::Instant,
};

use anyhow::{Result, ensure};
use ckb_types::{packed::Byte32, prelude::*};
use lru::LruCache;
use stem_ledger::ResolvedTransaction;

use super::{charge, verify_groups};

pub const MAX_VERIFICATION_CACHE_SHARDS: usize = 64;
const LOCK_SAMPLE_INTERVAL: u64 = u8::MAX as u64 + 1;

thread_local! {
    static RESULT_SAMPLE: Cell<u32> = const { Cell::new(0x2631_09f7) };
    static ESTIMATE_SAMPLE: Cell<u32> = const { Cell::new(0xc73a_50b9) };
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct CacheLockStats {
    pub samples: u64,
    pub wait_ns: u64,
    pub hold_ns: u64,
    pub max_wait_ns: u64,
    pub max_hold_ns: u64,
}

impl CacheLockStats {
    fn add(&mut self, other: Self) {
        self.samples = self.samples.saturating_add(other.samples);
        self.wait_ns = self.wait_ns.saturating_add(other.wait_ns);
        self.hold_ns = self.hold_ns.saturating_add(other.hold_ns);
        self.max_wait_ns = self.max_wait_ns.max(other.max_wait_ns);
        self.max_hold_ns = self.max_hold_ns.max(other.max_hold_ns);
    }
}

#[derive(Default)]
struct LockSamples {
    samples: AtomicU64,
    wait_ns: AtomicU64,
    hold_ns: AtomicU64,
    max_wait_ns: AtomicU64,
    max_hold_ns: AtomicU64,
}

impl LockSamples {
    fn record(&self, wait_ns: u64, hold_ns: u64) {
        self.wait_ns.fetch_add(wait_ns, Ordering::Relaxed);
        self.hold_ns.fetch_add(hold_ns, Ordering::Relaxed);
        self.max_wait_ns.fetch_max(wait_ns, Ordering::Relaxed);
        self.max_hold_ns.fetch_max(hold_ns, Ordering::Relaxed);
        self.samples.fetch_add(1, Ordering::Relaxed);
    }

    fn stats(&self) -> CacheLockStats {
        CacheLockStats {
            samples: self.samples.load(Ordering::Relaxed),
            wait_ns: self.wait_ns.load(Ordering::Relaxed),
            hold_ns: self.hold_ns.load(Ordering::Relaxed),
            max_wait_ns: self.max_wait_ns.load(Ordering::Relaxed),
            max_hold_ns: self.max_hold_ns.load(Ordering::Relaxed),
        }
    }
}

struct SampledMutex<T> {
    value: Mutex<T>,
    samples: LockSamples,
}

impl<T> SampledMutex<T> {
    fn new(value: T) -> Self {
        Self {
            value: Mutex::new(value),
            samples: LockSamples::default(),
        }
    }

    #[inline]
    fn access<R>(&self, sample: bool, task: impl FnOnce(&mut T) -> R) -> R {
        if !sample {
            return task(&mut self.value.lock().expect("VM cache lock"));
        }
        let start = Instant::now();
        let mut value = self.value.lock().expect("VM cache lock");
        let acquired = Instant::now();
        let result = task(&mut value);
        let released = Instant::now();
        drop(value);
        self.samples.record(
            acquired
                .duration_since(start)
                .as_nanos()
                .min(u64::MAX as u128) as u64,
            released
                .duration_since(acquired)
                .as_nanos()
                .min(u64::MAX as u128) as u64,
        );
        result
    }
}

/// Caches successful complete transaction checks and recent Script Group costs.
pub struct VerificationCache {
    crypto_v1: bool,
    verified: Box<[ResultShard]>,
    resize: Mutex<()>,
    estimates: SampledMutex<LruCache<(u8, Byte32), u64>>,
    sample_locks: bool,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct VerificationCacheStats {
    pub capacity: usize,
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub wrong_parent: u64,
    #[serde(default)]
    pub shards: usize,
    #[serde(default)]
    pub lock_sample_interval: u64,
    #[serde(default)]
    pub result_lock: CacheLockStats,
    #[serde(default)]
    pub estimate_lock: CacheLockStats,
}

#[repr(align(64))]
struct ResultShard(SampledMutex<VerifiedTransactions>);

struct VerifiedTransactions {
    entries: LruCache<Byte32, CachedVerification>,
    hits: u64,
    misses: u64,
    wrong_parent: u64,
}

impl VerifiedTransactions {
    fn new(capacity: usize) -> Self {
        Self {
            entries: LruCache::new(capacity),
            hits: 0,
            misses: 0,
            wrong_parent: 0,
        }
    }

    fn lookup(&mut self, key: &Byte32, parent: &Byte32) -> Option<u64> {
        if let Some(entry) = self.entries.get(key) {
            if entry
                .parent
                .as_ref()
                .is_none_or(|required| required == parent)
            {
                self.hits = self.hits.saturating_add(1);
                return Some(entry.cycles);
            }
            self.wrong_parent = self.wrong_parent.saturating_add(1);
        }
        self.misses = self.misses.saturating_add(1);
        None
    }

    fn remember(&mut self, key: Byte32, verification: CachedVerification) {
        // Resizing can leave zero-capacity shards when the total is small.
        if self.entries.cap() != 0 {
            self.entries.put(key, verification);
        }
    }
}

struct CachedVerification {
    cycles: u64,
    parent: Option<Byte32>,
}

fn shard_capacity(total: usize, shards: usize, index: usize) -> usize {
    total / shards + usize::from(index < total % shards)
}

impl VerificationCache {
    pub fn new(capacity: usize) -> Self {
        Self::for_genesis(capacity, false)
    }

    /// A cache belongs to one immutable set of genesis VM rules.
    pub fn for_genesis(capacity: usize, crypto_v1: bool) -> Self {
        Self::for_genesis_with_layout(capacity, crypto_v1, 1, false).expect("one cache shard")
    }

    /// Split a fixed total entry budget among power-of-two local LRU shards.
    /// Lock timing is optional and samples independently for each lock family.
    pub fn for_genesis_with_layout(
        capacity: usize,
        crypto_v1: bool,
        shards: usize,
        sample_locks: bool,
    ) -> Result<Self> {
        ensure!(
            shards.is_power_of_two() && shards <= MAX_VERIFICATION_CACHE_SHARDS,
            "invalid verification cache shard count"
        );
        Ok(Self {
            crypto_v1,
            verified: (0..shards)
                .map(|index| {
                    ResultShard(SampledMutex::new(VerifiedTransactions::new(
                        shard_capacity(capacity, shards, index),
                    )))
                })
                .collect(),
            resize: Mutex::new(()),
            estimates: SampledMutex::new(LruCache::new(capacity)),
            sample_locks,
        })
    }

    pub fn crypto_v1_enabled(&self) -> bool {
        self.crypto_v1
    }

    /// Resize the success cache independently of the per-group cost estimates.
    pub fn resize_verified(&self, capacity: usize) {
        let _resize = self.resize.lock().expect("VM cache resize lock");
        for (index, shard) in self.verified.iter().enumerate() {
            let cap = shard_capacity(capacity, self.verified.len(), index);
            shard.0.access(false, |cache| cache.entries.resize(cap));
        }
    }

    pub fn stats(&self) -> VerificationCacheStats {
        let _resize = self.resize.lock().expect("VM cache resize lock");
        let mut stats = VerificationCacheStats {
            capacity: 0,
            entries: 0,
            hits: 0,
            misses: 0,
            wrong_parent: 0,
            shards: self.verified.len(),
            lock_sample_interval: if self.sample_locks {
                LOCK_SAMPLE_INTERVAL
            } else {
                0
            },
            result_lock: CacheLockStats::default(),
            estimate_lock: self.estimates.samples.stats(),
        };
        for shard in &self.verified {
            shard.0.access(false, |cache| {
                stats.capacity += cache.entries.cap();
                stats.entries += cache.entries.len();
                stats.hits = stats.hits.saturating_add(cache.hits);
                stats.misses = stats.misses.saturating_add(cache.misses);
                stats.wrong_parent = stats.wrong_parent.saturating_add(cache.wrong_parent);
            });
            stats.result_lock.add(shard.0.samples.stats());
        }
        stats
    }

    #[inline]
    fn shard(&self, key: &Byte32) -> &SampledMutex<VerifiedTransactions> {
        let index = usize::from(key.as_slice()[0]) & (self.verified.len() - 1);
        &self.verified[index].0
    }

    #[inline]
    fn sample(&self, counter: &'static LocalKey<Cell<u32>>) -> bool {
        self.sample_locks
            && counter.with(|count| {
                // Mix the sample positions so alternating lookup and publish
                // operations do not always sample the same side.
                let mut next = count.get();
                next ^= next << 13;
                next ^= next >> 17;
                next ^= next << 5;
                count.set(next);
                next & u32::from(u8::MAX) == 0
            })
    }

    pub fn verify(
        &self,
        tx: Arc<ResolvedTransaction>,
        max_cycles: u64,
        parallel: bool,
    ) -> Result<u64> {
        let key = tx.script_cache_key();
        let parent = tx.parent_id();
        let shard = self.shard(key);
        let found = shard.access(self.sample(&RESULT_SAMPLE), |cache| {
            cache.lookup(key, parent)
        });
        if let Some(cycles) = found {
            return charge(0, cycles, max_cycles);
        }

        let key = key.clone();
        let parent = parent.clone();
        let parent_read = Arc::new(AtomicBool::new(false));
        let cycles = if tx.groups().len() < 2 {
            verify_groups(
                tx,
                max_cycles,
                parallel,
                self.crypto_v1,
                &parent_read,
                &[],
                |_, _| {},
            )?
        } else {
            let groups: Vec<_> = tx
                .groups()
                .iter()
                .map(|group| (group.kind as u8, group.script.calc_script_hash()))
                .collect();
            let estimated = if parallel {
                self.estimates
                    .access(self.sample(&ESTIMATE_SAMPLE), |cache| {
                        groups.iter().map(|id| cache.get(id).copied()).collect()
                    })
            } else {
                Vec::new()
            };
            let mut measured = Vec::with_capacity(groups.len());
            let cycles = verify_groups(
                tx,
                max_cycles,
                parallel,
                self.crypto_v1,
                &parent_read,
                &estimated,
                |index, cost| measured.push((index, cost)),
            )?;
            self.estimates
                .access(self.sample(&ESTIMATE_SAMPLE), |cache| {
                    if cache.cap() != 0 {
                        for (index, cost) in measured {
                            cache.put(groups[index].clone(), cost);
                        }
                    }
                });
            cycles
        };
        shard.access(self.sample(&RESULT_SAMPLE), |cache| {
            cache.remember(
                key,
                CachedVerification {
                    cycles,
                    parent: parent_read.load(Ordering::Relaxed).then_some(parent),
                },
            );
        });
        Ok(cycles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharded_cache_preserves_total_capacity_under_churn_and_resize() -> Result<()> {
        for invalid in [0, 3, MAX_VERIFICATION_CACHE_SHARDS * 2] {
            assert!(VerificationCache::for_genesis_with_layout(8, false, invalid, false).is_err());
        }
        let cache = VerificationCache::for_genesis_with_layout(7, false, 4, false)?;
        let fill = || {
            for byte in 0..32u8 {
                let key: Byte32 = [byte; 32].pack();
                cache.shard(&key).access(false, |shard| {
                    shard.remember(
                        key,
                        CachedVerification {
                            cycles: 1,
                            parent: None,
                        },
                    );
                });
            }
        };
        for capacity in [7, 1, 0, 11, 3] {
            cache.resize_verified(capacity);
            fill();
            let stats = cache.stats();
            assert_eq!(
                (stats.entries, stats.capacity, stats.shards),
                (capacity, capacity, 4)
            );
            assert_eq!(stats.result_lock.samples, 0);
        }
        Ok(())
    }

    #[test]
    fn lock_stats_count_only_timed_accesses() {
        let lock = SampledMutex::new(0u64);
        lock.access(false, |value| *value += 1);
        assert_eq!(lock.samples.stats().samples, 0);
        assert_eq!(lock.access(true, |value| *value), 1);
        assert_eq!(lock.samples.stats().samples, 1);
    }
}
