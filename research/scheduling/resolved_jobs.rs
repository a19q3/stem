//! Real StemVM jobs on immutable inputs. Keep the result index in submission order.
use super::*;
use rayon::prelude::*;
use std::time::Instant;

#[test]
#[ignore = "matched independent verification scheduling experiment"]
fn measure_resolved_job_schedules() -> Result<()> {
    let mut network = Network::new()?;
    let programs = build_contracts(&mut network)?;
    let scripts = [
        script(&programs[0], b"asset"),
        script(&programs[1], b"asset"),
        script(&programs[2], b"asset"),
    ];
    let mut jobs = Vec::new();
    let mut expected = Vec::new();
    let mut estimates = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for index in 0..160 {
        let groups = if index < 32 {
            index % 4
        } else {
            [0, 0, 1, 0, 3, 0, 0, 2][index % 8]
        };
        let types = scripts[..groups].to_vec();
        let input_cell = network.asset(types.clone(), 25, 1)?;
        let input = network.add_input(input_cell);
        let signed = network.signed(vec![input], vec![network.asset(types, 25, 1)?])?;
        let resolved = network.resolve(&network.id, signed)?;
        let limit = if index >= 32 && index % 29 == 0 {
            1000
        } else {
            CYCLE_LIMIT
        };
        let now = Instant::now();
        let result = verify(resolved.clone(), limit, false).map_err(|error| error.to_string());
        if index < 32 {
            estimates[groups].push(now.elapsed().as_nanos() as u64);
        } else {
            expected.push(result);
            jobs.push((resolved, limit, groups));
        }
    }
    let cost = estimates.map(|mut values| {
        values.sort_unstable();
        values[values.len() / 2]
    });
    for workers in [2, 4, 8, 16, 32] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()?;
        for trial in 0..6 {
            let mut modes = ["waves", "steal", "longest"];
            if trial % 2 == 1 {
                modes.reverse();
            }
            for mode in modes {
                let start = Instant::now();
                let mut indices: Vec<_> = (0..jobs.len()).collect();
                if mode == "longest" {
                    indices.sort_by_key(|&i| std::cmp::Reverse(cost[jobs[i].2]));
                }
                let run = |&i: &usize| {
                    let (tx, limit, _) = &jobs[i];
                    let begin = start.elapsed().as_nanos();
                    let value =
                        verify(tx.clone(), *limit, false).map_err(|error| error.to_string());
                    let end = start.elapsed().as_nanos();
                    (i, value, begin, end - begin)
                };
                let outcomes = pool.install(|| {
                    if mode == "waves" {
                        let mut outcomes = Vec::with_capacity(indices.len());
                        for wave in indices.chunks(workers) {
                            outcomes.extend(wave.par_iter().map(run).collect::<Vec<_>>());
                        }
                        outcomes
                    } else {
                        indices.par_iter().map(run).collect::<Vec<_>>()
                    }
                });
                let mut ordered = vec![None; jobs.len()];
                let mut starts = Vec::with_capacity(jobs.len());
                let mut services = Vec::with_capacity(jobs.len());
                for (i, result, begin, service) in outcomes {
                    ordered[i] = Some(result);
                    starts.push(begin);
                    services.push(service);
                }
                let wall = start.elapsed().as_nanos();
                assert!(
                    ordered
                        .iter()
                        .zip(&expected)
                        .all(|(a, b)| a.as_ref() == Some(b))
                );
                starts.sort_unstable();
                services.sort_unstable();
                let rank = (99 * jobs.len()).div_ceil(100) - 1;
                println!(
                    "SCHEDULE {workers} {trial} {mode} {wall} {} {} {} {} {}",
                    services.iter().sum::<u128>(),
                    starts[rank],
                    services[rank],
                    expected.iter().flatten().sum::<u64>(),
                    expected.iter().filter(|o| o.is_err()).count()
                );
            }
        }
    }
    Ok(())
}
