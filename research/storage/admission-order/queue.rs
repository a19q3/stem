//! Copy into tests/mempool_observer.rs for the standalone Linux measurement.
#[path = "../src/jolteon/mempool.rs"]
mod mempool;

use anyhow::{Result, ensure};
use mempool::Mempool;
use std::{
    fs::{self, File},
    hint::black_box,
    io::{BufReader, Read},
    time::Instant,
};
use stem_ledger::Transaction;

fn memory_kib() -> Result<(usize, usize)> {
    let status = fs::read_to_string("/proc/self/status")?;
    let read = |prefix| -> Result<usize> {
        Ok(status.lines().find_map(|line| line.strip_prefix(prefix))
            .unwrap().split_whitespace().next().unwrap().parse()?)
    };
    Ok((read("VmRSS:")?, read("VmHWM:")?))
}

#[test]
fn fill_and_drain() -> Result<()> {
    const COUNT: usize = 65_536;
    const BLOCK: usize = 2048;
    let mut input = BufReader::new(File::open(std::env::var("STEM_SIGNED_PAYMENTS")?)?);
    let mut pool = Mempool::default();
    let mut ids = Vec::with_capacity(COUNT);
    println!("POOL start {:?}", memory_kib()?);
    for round in 0..2 {
        let start = Instant::now();
        let mut wire_bytes = 0usize;
        for _ in 0..COUNT {
            let mut size = [0; 4];
            input.read_exact(&mut size)?;
            let size = u32::from_le_bytes(size) as usize;
            ensure!(size == 242, "expected an ordinary signed payment");
            let mut raw = vec![0; size];
            input.read_exact(&mut raw)?;
            let id = ckb_hash::blake2b_256(&raw);
            ensure!(!pool.contains(&id));
            pool.insert(id, Transaction::from_bytes(raw.into())?)?;
            ids.push(id);
            wire_bytes += size;
        }
        let fill_us = start.elapsed().as_micros();
        ensure!(pool.len() == COUNT && pool.bytes() == wire_bytes && wire_bytes <= 16 * 1024 * 1024);
        let (rss, peak) = memory_kib()?;
        let start = Instant::now();
        // A proposer normally stops at a block budget. Touch the candidate bytes
        // without executing the payment VM or measuring the authenticated store.
        for _ in 0..128 {
            let bytes: usize = pool.transactions().take(BLOCK)
                .map(|tx| black_box(tx.as_slice()).len()).sum();
            ensure!(bytes == BLOCK * 242);
        }
        let scan_us = start.elapsed().as_micros();
        let start = Instant::now();
        for offset in [1, 0] {
            for id in ids.iter().skip(offset).step_by(2) {
                pool.remove(id);
            }
        }
        let drain_us = start.elapsed().as_micros();
        ensure!(pool.len() == 0 && pool.bytes() == 0);
        ids.clear();
        let (after_rss, after_peak) = memory_kib()?;
        println!("POOL round {round} {wire_bytes} {fill_us} {scan_us} {drain_us} {rss} {peak} {after_rss} {after_peak}");
    }
    Ok(())
}
