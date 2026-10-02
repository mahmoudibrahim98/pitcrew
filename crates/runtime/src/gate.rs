//! A serialising lock with a deadline.

use std::sync::{Condvar, Mutex};
use std::time::Instant;

use crate::lock;

/// One holder at a time; others wait until a deadline.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    busy: Mutex<bool>,
    free: Condvar,
}

/// Holding the gate; dropping it lets the next one in.
pub(crate) struct Pass<'a>(&'a Gate);

impl Gate {
    /// Waits for the gate until `deadline`; `None` if it is still held then.
    pub(crate) fn enter(&self, deadline: Instant) -> Option<Pass<'_>> {
        let mut busy = lock(&self.busy);
        while *busy {
            let left = deadline.checked_duration_since(Instant::now())?;
            busy = self
                .free
                .wait_timeout(busy, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        *busy = true;
        Some(Pass(self))
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        *lock(&self.0.busy) = false;
        self.0.free.notify_one();
    }
}
