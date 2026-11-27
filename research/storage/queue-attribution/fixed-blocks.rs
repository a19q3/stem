use super::*;
use std::time::Instant;

#[test]
fn time_identical_funded_candidates() -> Result<()> {
    let (fixture, txs) = receipt_equivalence::funded_payments(2048)?;
    let home = tempfile::tempdir()?;
    let store = Store::open(home.path(), &fixture.spec, Retention::Archive)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut results = Vec::new();
    for count in [256, 1024, 2048] {
        let block = &txs[..count];
        let reference = store.prepare(block, TIME + 1, false)?;
        let mut samples = Vec::new();
        for run in 0..12 {
            let (candidate, total_us, finish_us) = pool.install(|| -> Result<_> {
                let start = Instant::now();
                let mut finishing = start;
                let candidate = store.prepare_measured(block, TIME + 1, true, || {
                    finishing = Instant::now();
                })?;
                let end = Instant::now();
                Ok((
                    candidate,
                    (end - start).as_micros(),
                    (end - finishing).as_micros(),
                ))
            })?;
            assert_eq!(candidate.header(), reference.header());
            assert_eq!(candidate.receipts(), reference.receipts());
            if run >= 2 {
                samples.push([total_us, finish_us]);
            }
        }
        results.push(serde_json::json!({
            "transactions": count,
            "header": hex::encode(reference.header().id(store.network()).as_slice()),
            "prepare_and_finish_us": samples
        }));
    }
    println!("FIXED_BLOCKS {}", serde_json::to_string(&results)?);
    Ok(())
}
