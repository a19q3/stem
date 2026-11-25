use std::{path::PathBuf, time::Instant};

use serde_json::json;
use stem_ledger::lineage::Outcome;

use super::{
    Fixture, Result, Retention, Store, TIME, Transaction,
    lineage::{DynamicLineage, FixtureLineage},
};

#[test]
#[ignore = "paired proposer selection measurement with a full logical read budget"]
fn measure_exhausted_lineage_selection() -> Result<()> {
    const QUEUED: usize = 4096;
    const FITS: usize = 24;
    const SAMPLES: usize = 11;
    let mut fixture = Fixture::new()?;
    fixture.spec.limits.cell_reads = 46 * FITS as u64;
    fixture.spec.limits.transaction_count = 64;
    fixture.spec.limits.transaction_cycles = 20_000_000;
    fixture.spec.limits.block_cycles = 2_000_000_000;
    fixture.spec.limits.cell_read_bytes = 128 * 1024 * 1024;
    fixture.spec.limits.cell_write_bytes = 4 * 1024 * 1024;
    let registry = FixtureLineage::fund(&mut fixture)?;
    let dynamic = DynamicLineage::fund_using(&mut fixture, &registry, QUEUED, 8, None)?;
    let registrations = (0..8)
        .map(|i| dynamic.register(&fixture, i))
        .collect::<Result<Vec<_>>>()?;
    let targets: Vec<_> = (0..8).collect();
    let pool: Vec<_> = (0..QUEUED)
        .map(|i| dynamic.signed(&fixture, i, &targets, (0, QUEUED as u64), 1))
        .collect::<Result<_>>()?;
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(directory.path(), &fixture.spec, Retention::Archive)?;
    store.commit(store.prepare(&registrations, TIME, false)?)?;
    assert_eq!(
        Outcome::decode(&store.check(&pool[QUEUED - 1])?.protocol_result)?,
        Outcome::Applied
    );
    let reference = store.prepare(&pool[..FITS], TIME, false)?;
    let mut samples = Vec::new();
    for _ in 0..SAMPLES {
        // Construct the input before starting the timer in both revisions.
        let candidates = pool.clone();
        let started = Instant::now();
        let selected = store.select(candidates, TIME, u64::MAX)?;
        samples.push(started.elapsed().as_micros());
        assert_eq!(selected.len(), FITS);
        assert!(
            selected
                .iter()
                .zip(&pool)
                .all(|(actual, expected)| { actual.as_slice() == expected.as_slice() })
        );
        assert_eq!(
            store.prepare(&selected, TIME, false)?.header(),
            reference.header()
        );
    }
    let workload = pool
        .iter()
        .flat_map(Transaction::as_slice)
        .copied()
        .collect::<Vec<_>>();
    let evidence = json!({
        "queued": QUEUED, "selected": FITS, "reads": fixture.spec.limits.cell_reads,
        "signed_workload_id": hex::encode(ckb_hash::blake2b_256(workload)),
        "header": hex::encode(reference.header().encode()), "selection_us": samples,
    });
    if let Some(path) = std::env::var_os("STEM_SELECTION_REPORT") {
        std::fs::write(PathBuf::from(path), serde_json::to_vec(&evidence)?)?;
    }
    println!("{evidence}");
    Ok(())
}
