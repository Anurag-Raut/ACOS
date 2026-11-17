//! A small counting gate with queueing and cancellation.
//!
//! The server bounds how many backend calls run at once. Callers queue for a
//! permit; a queued caller can be cancelled, and waiting past the queue timeout
//! returns an error the server maps to a structured `RESOURCE_LIMIT` envelope.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::process::CancelToken;

/// Why a permit could not be acquired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateError {
    /// The caller was cancelled while waiting.
    Cancelled,
    /// The queue wait exceeded its deadline.
    Timeout,
}

/// A counting gate. `limit` concurrent permits.
#[derive(Debug)]
pub struct Gate {
    limit: usize,
    in_use: Mutex<usize>,
    cv: Condvar,
}

impl Gate {
    /// Create a gate admitting `limit` concurrent callers (minimum 1).
    pub fn new(limit: usize) -> Self {
        Gate {
            limit: limit.max(1),
            in_use: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    /// The configured concurrency limit.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Acquire a permit, waiting at most `timeout` while polling `cancel`.
    pub fn acquire(
        &self,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<Permit<'_>, GateError> {
        let deadline = Instant::now() + timeout;
        let mut in_use = self.in_use.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if *in_use < self.limit {
                *in_use += 1;
                return Ok(Permit { gate: self });
            }
            if cancel.is_cancelled() {
                return Err(GateError::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(GateError::Timeout);
            }
            let wait = Duration::from_millis(5).min(deadline.saturating_duration_since(now));
            let (guard, _) = self
                .cv
                .wait_timeout(in_use, wait)
                .unwrap_or_else(|e| e.into_inner());
            in_use = guard;
        }
    }
}

/// A held permit; dropping it releases the slot and wakes one waiter.
#[derive(Debug)]
pub struct Permit<'a> {
    gate: &'a Gate,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut in_use = self.gate.in_use.lock().unwrap_or_else(|e| e.into_inner());
        *in_use = in_use.saturating_sub(1);
        self.gate.cv.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn permits_are_bounded_and_released() {
        let gate = Arc::new(Gate::new(1));
        let cancel = CancelToken::new();
        let first = gate.acquire(Duration::from_millis(50), &cancel).unwrap();
        let gate2 = Arc::clone(&gate);
        let cancel2 = cancel.clone();
        let waiter =
            std::thread::spawn(move || gate2.acquire(Duration::from_secs(5), &cancel2).map(|_| ()));
        std::thread::sleep(Duration::from_millis(30));
        drop(first);
        assert!(waiter.join().unwrap().is_ok());
    }

    #[test]
    fn cancelled_waiter_stops() {
        let gate = Gate::new(1);
        let cancel = CancelToken::new();
        let _held = gate.acquire(Duration::from_millis(50), &cancel).unwrap();
        cancel.cancel();
        assert_eq!(
            gate.acquire(Duration::from_secs(5), &cancel).err(),
            Some(GateError::Cancelled)
        );
    }
}
