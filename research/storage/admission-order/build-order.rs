use super::*;
use std::{sync::Arc, time::Instant};

#[test]
fn proposal_order_cost() -> Result<()> {
    const COUNT: usize = 2048;
    let (fixture, payments) = receipt_equivalence::funded_payments((2 * COUNT) as u32)?;
    let home = tempfile::tempdir()?;
    let store = Store::open(home.path(), &fixture.spec, Retention::Archive)?;
    let parent = Arc::new(store.prepare(&payments[..COUNT], TIME + 1, false)?);
    let start = parent.header().id(store.network());
    let by_hash = |i: &usize| ckb_hash::blake2b_256(payments[*i].as_slice());
    let input_order: Vec<_> = (COUNT..2 * COUNT).collect();
    let mut fixed = input_order.clone();
    fixed.sort_by_cached_key(by_hash);
    let mut rotated = fixed.clone();
    let first = rotated.partition_point(|i| by_hash(i).as_slice() < start.as_slice());
    rotated.rotate_left(first);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut jobs = Vec::new();
    for (name, order) in [
        ("wire_hash", fixed),
        ("input_index", input_order),
        ("parent_hash", rotated),
    ] {
        let selected: Vec<_> = order.iter().map(|i| payments[*i].clone()).collect();
        let reference = store.on_branch(std::slice::from_ref(&parent), |state| {
            state.prepare(&selected, TIME + 2, false)
        })?;
        for spent in [false, true] {
            let mut candidates = Vec::new();
            for &i in &order {
                if spent {
                    candidates.push(payments[i - COUNT].clone());
                }
                candidates.push(payments[i].clone());
            }
            jobs.push((
                name,
                spent,
                candidates,
                selected.clone(),
                reference.header().clone(),
                reference.receipts().to_vec(),
            ));
        }
    }
    let mut samples = Vec::new();
    // Six forward rotations followed by six reverse rotations balance position and direction.
    for repetition in 0..14 {
        for position in 0..jobs.len() {
            let m = repetition + jobs.len() - 2;
            let index = (position + m) % jobs.len();
            let index = if (m / jobs.len()) % 2 == 0 {
                index
            } else {
                jobs.len() - 1 - index
            };
            let (name, spent, candidates, selected, header, receipts) = &jobs[index];
            let (picked, built, elapsed) = pool.install(|| {
                store.on_branch(std::slice::from_ref(&parent), |state| {
                    let start = Instant::now();
                    let (picked, built) = state.build_proposal(candidates, TIME + 2, u64::MAX)?;
                    Ok((picked, built, start.elapsed().as_nanos() as u64))
                })
            })?;
            assert_eq!(picked.len(), COUNT);
            assert!(
                picked
                    .iter()
                    .zip(selected)
                    .all(|(a, b)| a.as_slice() == b.as_slice())
            );
            assert_eq!(built.header(), header);
            assert_eq!(built.receipts(), receipts);
            samples.push(
                serde_json::json!({"repetition": repetition, "position": position,
                "order": name, "spent_before_each_payment": spent, "build_ns": elapsed,
                "header": hex::encode(header.id(store.network()).as_slice())}),
            );
        }
    }
    println!("PROPOSAL_ORDER {}", serde_json::to_string(&samples)?);
    Ok(())
}
