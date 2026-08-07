//! Owned single-result workers shared by long-running control jobs.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

/// Non-blocking state returned by [`OwnedWorker::poll`].
pub(crate) enum WorkerPoll<T> {
    /// The worker has not published its terminal value.
    Pending,
    /// The worker published its one complete terminal value and joined cleanly.
    Complete(T),
}

/// Terminal worker failure with no partial result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerFailure {
    /// The result channel closed without a value.
    Disconnected,
    /// The worker thread panicked.
    Panicked,
}

/// One join-owned worker that publishes exactly one complete value.
pub(crate) struct OwnedWorker<T> {
    receiver: Receiver<T>,
    worker: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> OwnedWorker<T> {
    /// Spawns a named worker and a zero-capacity publication channel.
    pub(crate) fn spawn(
        name: String,
        run: impl FnOnce() -> T + Send + 'static,
    ) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new().name(name).spawn(move || {
            let result = run();
            let _ = sender.send(result);
        })?;
        Ok(Self {
            receiver,
            worker: Some(worker),
        })
    }

    /// Polls without blocking and joins before returning a complete value.
    pub(crate) fn poll(&mut self) -> Result<WorkerPoll<T>, WorkerFailure> {
        match self.receiver.try_recv() {
            Ok(value) => {
                self.finish()?;
                Ok(WorkerPoll::Complete(value))
            }
            Err(TryRecvError::Empty) => Ok(WorkerPoll::Pending),
            Err(TryRecvError::Disconnected) => {
                if self.finish().is_err() {
                    Err(WorkerFailure::Panicked)
                } else {
                    Err(WorkerFailure::Disconnected)
                }
            }
        }
    }

    /// Joins the worker once. Repeated calls are inert.
    pub(crate) fn finish(&mut self) -> Result<(), WorkerFailure> {
        match self.worker.take() {
            Some(worker) => worker.join().map_err(|_| WorkerFailure::Panicked),
            None => Ok(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn completed(value: T) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send(value).expect("ready worker receiver is live");
        Self {
            receiver,
            worker: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_value_is_joined_and_returned_once() {
        let mut worker = OwnedWorker::spawn("owned-worker-value".to_owned(), || 42).unwrap();
        loop {
            match worker.poll().unwrap() {
                WorkerPoll::Pending => std::thread::yield_now(),
                WorkerPoll::Complete(value) => {
                    assert_eq!(value, 42);
                    break;
                }
            }
        }
        worker.finish().unwrap();
    }

    #[test]
    fn panic_is_typed() {
        let mut worker =
            OwnedWorker::<()>::spawn("owned-worker-panic".to_owned(), || panic!("fixture panic"))
                .unwrap();
        loop {
            match worker.poll() {
                Ok(WorkerPoll::Pending) => std::thread::yield_now(),
                Err(failure) => {
                    assert_eq!(failure, WorkerFailure::Panicked);
                    break;
                }
                Ok(WorkerPoll::Complete(())) => panic!("panicking worker completed"),
            }
        }
    }
}
