//! Standalone comparison of the state-change sort used by the native Store.
use std::{env, hint::black_box, time::Instant};
type Change = ([u8; 32], Option<[u8; 32]>);

#[derive(Copy, Clone)]
enum Algorithm {
    Oracle,
    Network,
    Insertion,
    OmitLast,
}

fn sort(values: &mut [Change], method: Algorithm) {
    if values.len() > 8 || matches!(method, Algorithm::Oracle) {
        values.sort_unstable_by_key(|(id, _)| *id);
        return;
    }
    let n = values.len();
    match method {
        Algorithm::Insertion => {
            for i in 1..n {
                let mut j = i;
                while j > 0 && values[j].0 < values[j - 1].0 {
                    values.swap(j, j - 1);
                    j -= 1;
                }
            }
        }
        Algorithm::Network | Algorithm::OmitLast => {
            // Odd-even transposition network, adjacent exchanges keep ties stable.
            let width = n.saturating_sub(usize::from(matches!(method, Algorithm::OmitLast)));
            for phase in 0..width {
                for j in ((phase % 2 + 1)..width).step_by(2) {
                    if values[j].0 < values[j - 1].0 {
                        values.swap(j, j - 1);
                    }
                }
            }
        }
        Algorithm::Oracle => unreachable!(),
    }
}

fn rng(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn fixture(pattern: &[u8], prefix: bool) -> Vec<Change> {
    pattern
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let mut key = [0xff; 32];
            key[if prefix { 31 } else { 0 }] = v;
            (key, (i % 2 == 0).then_some([i as u8; 32]))
        })
        .collect()
}

fn next_permutation(values: &mut [u8]) -> bool {
    let Some(i) = (1..values.len()).rev().find(|&i| values[i - 1] < values[i]) else {
        return false;
    };
    let j = (i..values.len())
        .rev()
        .find(|&j| values[i - 1] < values[j])
        .unwrap();
    values.swap(i - 1, j);
    values[i..].reverse();
    true
}

fn check(input: &[Change]) {
    let mut expected = input.to_vec();
    sort(&mut expected, Algorithm::Oracle);
    for alg in [Algorithm::Network, Algorithm::Insertion] {
        let mut copy = input.to_vec();
        sort(&mut copy, alg);
        assert_eq!(expected, copy);
    }
}

fn gate() {
    let mut checks = 0;
    for n in 0..=8 {
        let mut order = (0..n as u8).collect::<Vec<_>>();
        loop {
            check(&fixture(&order, true));
            checks += 1;
            if !next_permutation(&mut order) {
                break;
            }
        }
        for mask in 0..(1 << n) {
            let bits = (0..n).map(|i| ((mask >> i) & 1) as u8).collect::<Vec<_>>();
            check(&fixture(&bits, false));
            checks += 1;
        }
    }
    let mut bad = fixture(&[1, 0], true);
    sort(&mut bad, Algorithm::OmitLast);
    assert!(bad[0].0 > bad[1].0);
    println!("{{\"gate_cases\":{checks},\"rejected_mutant_input\":[1,0]}}");
}

fn main() {
    let args = env::args().collect::<Vec<_>>();
    if args[1] == "gate" {
        gate();
        return;
    }
    let n = args[1].parse::<usize>().unwrap();
    let seed = args[2].parse::<u64>().unwrap();
    let mut state = seed;
    let mut corpus = Vec::new();
    for index in 0..2048 {
        let mut row = [([0; 32], None); 8];
        for item in &mut row[..n] {
            for chunk in item.0.chunks_exact_mut(8) {
                chunk.copy_from_slice(&rng(&mut state).to_le_bytes());
            }
            if index % 3 == 0 {
                item.0[..30].fill(0xff);
            }
            if state & 1 == 1 {
                item.1 = Some([state as u8; 32]);
            }
        }
        if index % 4 == 0 {
            sort(&mut row[..n], Algorithm::Oracle);
        }
        if index % 4 == 1 {
            sort(&mut row[..n], Algorithm::Oracle);
            row[..n].reverse();
        }
        check(&row[..n]);
        corpus.push(row);
    }
    let alg = match args[3].as_str() {
        "oracle" => Algorithm::Oracle,
        "network" => Algorithm::Network,
        "insertion" => Algorithm::Insertion,
        _ => unreachable!(),
    };
    let mut elapsed = Vec::new();
    let mut checksum = 0u64;
    for trial in 0..10 {
        let mut work = corpus.clone();
        let now = Instant::now();
        for row in &mut work {
            sort(black_box(&mut row[..n]), alg);
        }
        let ns = now.elapsed().as_nanos();
        black_box(&work);
        if trial > 0 {
            elapsed.push(ns);
        }
        if trial == 0 {
            // Portable digest of keys, optional tags and actual moved payload bytes.
            for (key, value) in work.iter().flat_map(|r| &r[..n]) {
                for b in key
                    .iter()
                    .copied()
                    .chain([u8::from(value.is_some())])
                    .chain(value.iter().flatten().copied())
                {
                    checksum = (checksum ^ u64::from(b)).wrapping_mul(1099511628211);
                }
            }
        }
    }
    println!("{{\"batches_ns\":{elapsed:?},\"rows\":2048,\"checksum\":\"{checksum:016x}\"}}");
}
