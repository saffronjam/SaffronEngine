//! Cooperative cancellation and deadline aborts shared by evaluator jobs.

use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
use crate::{Error, Result};

/// Cooperative cancellation token. A cancelled job never publishes a partial result.
#[derive(Clone, Debug)]
pub struct GraphCancellationToken {
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    remaining_checks: Arc<AtomicU64>,
    #[cfg(test)]
    pub(super) observed_checks: Arc<AtomicU64>,
    #[cfg(test)]
    abort_checkpoint: Arc<AtomicU64>,
    #[cfg(test)]
    abort_kind: Arc<AtomicU64>,
}

impl Default for GraphCancellationToken {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            remaining_checks: Arc::new(AtomicU64::new(u64::MAX)),
            #[cfg(test)]
            observed_checks: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            abort_checkpoint: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            abort_kind: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl GraphCancellationToken {
    /// Cancels every evaluator sharing this token.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Whether cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return true;
        }
        #[cfg(test)]
        {
            self.observed_checks.fetch_add(1, Ordering::Relaxed);
            match self.remaining_checks.fetch_update(
                Ordering::AcqRel,
                Ordering::Acquire,
                |remaining| match remaining {
                    u64::MAX | 0 => None,
                    _ => Some(remaining - 1),
                },
            ) {
                Ok(_) | Err(u64::MAX) => false,
                Err(0) => true,
                Err(_) => false,
            }
        }
        #[cfg(not(test))]
        false
    }

    #[cfg(test)]
    pub(super) fn cancel_after_checks(&self, checks: u64) {
        self.remaining_checks.store(checks, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn observed_checks(&self) -> u64 {
        self.observed_checks.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(super) fn abort_at_checkpoint(
        &self,
        checkpoint: TestEvaluationCheckpoint,
        kind: TestAbortKind,
    ) {
        self.abort_kind.store(kind as u64, Ordering::Release);
        self.abort_checkpoint
            .store(checkpoint as u64, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn check_test_checkpoint(
        &self,
        checkpoint: TestEvaluationCheckpoint,
        time_limit_ms: u64,
    ) -> Result<()> {
        if self.abort_checkpoint.load(Ordering::Acquire) != checkpoint as u64 {
            return Ok(());
        }
        match self.abort_kind.load(Ordering::Acquire) {
            value if value == TestAbortKind::Cancelled as u64 => Err(Error::GraphCancelled),
            value if value == TestAbortKind::Deadline as u64 => Err(Error::GraphLimit {
                resource: "time milliseconds",
                requested: time_limit_ms.saturating_add(1),
                limit: time_limit_ms,
            }),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) enum TestEvaluationCheckpoint {
    AfterPreflight = 1,
    AfterPreparation = 2,
    AfterTraversal = 3,
    BeforeFinalValidation = 4,
    BeforePublication = 5,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(super) enum TestAbortKind {
    Cancelled = 1,
    Deadline = 2,
}
