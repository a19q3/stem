use super::*;

pub(super) fn funded_payments(count: u32) -> Result<(Fixture, Vec<Transaction>)> {
    let mut fixture = Fixture::new()?;
    let limits = &mut fixture.spec.limits;
    limits.transaction_count = 2048;
    limits.block_cycles = 4_000_000_000;
    limits.cell_reads = 8192;
    limits.cell_read_bytes = 128 * 1024 * 1024;
    limits.cell_write_bytes = 4 * 1024 * 1024;
    let mut txs = Vec::new();
    for index in 0..count {
        let point = OutPoint::new([17; 32].pack(), index);
        fixture.spec.development_cells.push((
            point.clone(),
            Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?,
        ));
        txs.push(fixture.signed(point, None, FUND - FEE, None)?);
    }
    Ok((fixture, txs))
}

#[test]
fn matching_roots_and_receipts_across_the_parallel_cutoff() -> Result<()> {
    let (fixture, txs) = funded_payments(1024)?;
    let home = tempfile::tempdir()?;
    let open = |name| Store::open(&home.path().join(name), &fixture.spec, Retention::Archive);
    let mut serial = open("serial")?;
    let mut parallel = open("parallel")?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut offset = 0;
    for (index, count) in [255, 256, 513].into_iter().enumerate() {
        let height = index as u64 + 1;
        let time = TIME + height;
        let block = &txs[offset..offset + count];
        let reference = serial.prepare(block, time, false)?;
        let follower = pool.install(|| parallel.prepare(block, time, true))?;
        let (selected, leader) = pool.install(|| parallel.build_proposal(block, time, u64::MAX))?;
        assert_eq!(selected.len(), count);
        for candidate in [&follower, &leader] {
            assert_eq!(candidate.header(), reference.header());
            assert_eq!(candidate.receipts(), reference.receipts());
        }
        serial.commit(reference)?;
        parallel.commit(follower)?;
        offset += count;
    }
    drop(parallel);
    let reopened = open("parallel")?;
    assert_eq!(serial.tip(), reopened.tip());
    for height in 1..=3 {
        let (expected_txs, expected_receipts) = serial.execution(height)?.unwrap();
        let (actual_txs, actual_receipts) = reopened.execution(height)?.unwrap();
        assert_eq!(expected_receipts, actual_receipts);
        for (expected, actual) in expected_txs.iter().zip(&actual_txs) {
            assert_eq!(expected.as_slice(), actual.as_slice());
        }
        assert_eq!(expected_txs.len(), actual_txs.len());
    }
    Ok(())
}
