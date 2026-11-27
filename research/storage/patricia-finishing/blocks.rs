use super::*;
use std::{sync::Arc, time::Instant};

#[test]
fn measure_fixed_children() -> Result<()> {
    let (fixture, payments) = receipt_equivalence::funded_payments(4096)?;
    let home = tempfile::tempdir()?;
    let open = |name| Store::open(&home.path().join(name), &fixture.spec, Retention::Archive);
    let mut committed = open("committed")?;
    let speculative = open("speculative")?;
    let parent = &payments[..2048];
    committed.commit(committed.prepare(parent, TIME + 1, false)?)?;
    let parent = Arc::new(speculative.prepare(parent, TIME + 1, false)?);
    assert_eq!(parent.header(), committed.tip());
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    for count in [256, 1024, 2048] {
        let block = &payments[2048..2048 + count];
        let reference = committed.prepare(block, TIME + 2, false)?;
        let header = hex::encode(reference.header().id(committed.network()).as_slice());
        for repetition in 0..14 {
            let order = if repetition % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for branch in order {
                let measure = |store: &Store| -> Result<_> {
                    let start = Instant::now();
                    let candidate = store.prepare(block, TIME + 2, true)?;
                    let elapsed = start.elapsed().as_nanos();
                    assert_eq!(candidate.header(), reference.header());
                    assert_eq!(candidate.receipts(), reference.receipts());
                    Ok(elapsed)
                };
                let elapsed = pool.install(|| {
                    if branch {
                        speculative.on_branch(std::slice::from_ref(&parent), measure)
                    } else {
                        measure(&committed)
                    }
                })?;
                println!("CHILD {repetition} {count} {branch} {header} {elapsed}");
            }
        }
    }
    let child = &payments[2048..];
    let candidate = speculative.on_branch(std::slice::from_ref(&parent), |state| {
        pool.install(|| state.prepare(child, TIME + 2, true))
    })?;
    let expected = committed.prepare(child, TIME + 2, false)?;
    assert_eq!(expected.header(), candidate.header());
    let mut speculative = speculative;
    speculative.commit(Arc::try_unwrap(parent).ok().expect("parent owned by test"))?;
    speculative.commit(candidate)?;
    committed.commit(expected)?;
    drop(speculative);
    let reopened = open("speculative")?;
    assert_eq!(reopened.tip(), committed.tip());
    for height in 1..=2 {
        let (txs, receipts) = reopened.execution(height)?.expect("persisted executions");
        let (baseline, expected) = committed.execution(height)?.expect("reference executions");
        assert_eq!(receipts, expected);
        assert_eq!(txs.len(), baseline.len());
        for (tx, baseline) in txs.iter().zip(baseline) {
            assert_eq!(tx.as_slice(), baseline.as_slice());
        }
    }
    Ok(())
}
