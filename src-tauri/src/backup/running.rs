//! Whether a backup or a restore is running right now.
//!
//! Read by the Windows updater, which closes Jodd: a restore cut off halfway
//! leaves an account half-restored (spec 2026-11-11-app-updater-design.md §6).

use std::sync::atomic::{AtomicUsize, Ordering};

static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Held for the whole of a backup or restore command.
pub(crate) struct Guard(());

impl Guard {
    pub(crate) fn new() -> Self {
        RUNNING.fetch_add(1, Ordering::SeqCst);
        Guard(())
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) fn any() -> bool {
    RUNNING.load(Ordering::SeqCst) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    // The only test that raises the process-wide flag; quiesce's own tests
    // pass the flag in instead, so they never see this one.
    #[test]
    fn a_guard_marks_a_backup_running_until_dropped() {
        let g = Guard::new();
        assert!(any());
        drop(g);
        assert!(!any());
    }
}
