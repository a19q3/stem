//! Bound speculative Script Group work while settling in canonical order.

use std::{
    panic::{self, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    },
    thread,
    time::Duration,
};

use anyhow::Result;

use super::{charge, group_limit, is_cycle_limit};

type Completion = (usize, u64, thread::Result<Result<u64>>);

pub(super) fn verify(
    groups: usize,
    max_cycles: u64,
    workers: usize,
    estimates: &[Option<u64>],
    run: impl Fn(usize, u64) -> Result<u64> + Sync,
    mut observe: impl FnMut(usize, u64),
) -> Result<u64> {
    let window = workers.saturating_mul(2).min(groups);
    let mut ready: Vec<Option<Completion>> = (0..window).map(|_| None).collect();
    let (sender, receiver) = mpsc::channel();
    let stop = &AtomicBool::new(false);
    let run = &run;
    rayon::in_place_scope_fifo(|scope| {
        let result = (|| {
            let mut submitted = 0;
            let mut executing = 0;
            let mut next = 0;
            let mut total = 0;
            while next < groups {
                let remaining = max_cycles - total;
                let Some((index, limit, result)) = ready[next % window].take() else {
                    let share = remaining / workers.min(groups - next) as u64;
                    while submitted < groups && executing < workers && submitted - next < window {
                        let index = submitted;
                        let limit = group_limit(estimates, index, share, remaining);
                        let sender = sender.clone();
                        scope.spawn_fifo(move |_| {
                            if !stop.load(Ordering::Relaxed) {
                                let outcome =
                                    panic::catch_unwind(AssertUnwindSafe(|| run(index, limit)));
                                let _ = sender.send((index, limit, outcome));
                            }
                        });
                        submitted += 1;
                        executing += 1;
                    }
                    let completion = next_completion(&receiver);
                    let slot = completion.0 % window;
                    debug_assert!(ready[slot].is_none());
                    ready[slot] = Some(completion);
                    executing -= 1;
                    continue;
                };
                debug_assert_eq!(index, next);
                let result = result.unwrap_or_else(|panic| panic::resume_unwind(panic));
                let cycles = match result {
                    // A smaller speculative allowance may have cut short a
                    // valid script. A larger one may reveal a script error
                    // that the ordered cycle budget would preempt.
                    Err(error)
                        if (is_cycle_limit(&error) && limit < remaining)
                            || (!is_cycle_limit(&error) && limit > remaining) =>
                    {
                        run(index, remaining)?
                    }
                    other => other?,
                };
                total = charge(total, cycles, max_cycles)?;
                observe(index, cycles);
                next += 1;
            }
            Ok(total)
        })();
        stop.store(true, Ordering::Relaxed);
        result
    })
}

fn next_completion(receiver: &Receiver<Completion>) -> Completion {
    loop {
        match receiver.try_recv() {
            Ok(completion) => return completion,
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                unreachable!("Script Group workers lost their scope")
            }
        }
        // A transaction worker may also be a Rayon worker. Let it execute
        // pending jobs so a pool full of nested verifiers can still progress.
        if rayon::yield_now() != Some(rayon::Yield::Executed) {
            match receiver.recv_timeout(Duration::from_micros(100)) {
                Ok(completion) => return completion,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    unreachable!("Script Group workers lost their scope")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use anyhow::ensure;
    use ckb_vm::Error;

    use super::*;

    #[test]
    fn retries_undersized_speculation_and_reports_canonical_charges() -> Result<()> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let costs = [3, 11, 2, 1];
        let visits = Mutex::new(Vec::new());
        let mut observed = Vec::new();
        let cycles = pool.install(|| {
            verify(
                costs.len(),
                20,
                2,
                &[],
                |index, limit| {
                    visits.lock().unwrap().push((index, limit));
                    if costs[index] > limit {
                        return Err(Error::CyclesExceeded.into());
                    }
                    Ok(costs[index])
                },
                |index, cycles| observed.push((index, cycles)),
            )
        })?;
        assert_eq!(cycles, 17);
        assert_eq!(observed, [(0, 3), (1, 11), (2, 2), (3, 1)]);
        let visits = visits.into_inner().unwrap();
        assert!(visits.contains(&(1, 10)) && visits.contains(&(1, 17)));
        Ok(())
    }

    #[test]
    fn speculative_script_failure_respects_the_ordered_budget() -> Result<()> {
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        let costs = [16, 8, 1];
        let mut observed = Vec::new();
        let failure = pool
            .install(|| {
                verify(
                    costs.len(),
                    20,
                    2,
                    &[Some(16), Some(8)],
                    |index, limit| {
                        if costs[index] > limit {
                            return Err(Error::CyclesExceeded.into());
                        }
                        ensure!(index != 1, "Script rejected within speculative budget");
                        Ok(costs[index])
                    },
                    |index, cycles| observed.push((index, cycles)),
                )
            })
            .unwrap_err();
        assert!(is_cycle_limit(&failure));
        assert_eq!(observed, [(0, 16)]);
        Ok(())
    }
}
