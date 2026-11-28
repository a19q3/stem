use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Shared by the Replica and one speculative worker.
#[derive(Clone, Default)]
pub(crate) struct PreparationCancellation(Arc<AtomicBool>);

impl PreparationCancellation {
    pub(crate) fn cancel(&self) -> bool {
        !self.0.swap(true, Ordering::Relaxed)
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Abandoned local work. This does not classify the candidate as invalid.
#[derive(Debug)]
pub(crate) struct PreparationCancelled;

impl fmt::Display for PreparationCancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("speculative preparation cancelled")
    }
}

impl std::error::Error for PreparationCancelled {}
