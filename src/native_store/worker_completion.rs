//! Wait for a scoped task while helping its Rayon pool.

use std::{
    sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError},
    time::Duration,
};

pub(super) fn next<T>(receiver: &Receiver<T>) -> T {
    loop {
        match receiver.try_recv() {
            Ok(completion) => return completion,
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => unreachable!("execution workers lost their scope"),
        }
        // The coordinator can be on the same pool as the workers. Run pending
        // tasks before parking, and periodically retry if several blocks share
        // the pool and work becomes available after the probe.
        if rayon::yield_now() != Some(rayon::Yield::Executed) {
            match receiver.recv_timeout(Duration::from_micros(100)) {
                Ok(completion) => return completion,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    unreachable!("execution workers lost their scope")
                }
            }
        }
    }
}
