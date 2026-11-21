//! Phase 3 (spec 2026-11-05 §10): while the app runs, it keeps the mirror and
//! the skills current itself, so Claude Code sees an edit — the iPhone's, or
//! one made here — without waiting for its next session.
//!
//! The decision is pure and tested here; `agent_files_commands::spawn_live_sync`
//! is the loop that feeds it. What it watches is cheap on purpose: a counter
//! SQLite already keeps (`Db::change_counter`), the scope file's modified
//! time, and the accounts' active state — no table is read until something
//! moved.
//!
//! **Route to the screen (gotcha #6):** after each run the loop emits
//! [`EVENT`]; both Settings sections listen for it and re-read their status.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crate::accounts::Account;
use crate::db::Db;
use crate::mcp_scope::ScopeFile;

/// Emitted to the frontend after every background run.
pub const EVENT: &str = "agent-files-synced";
/// How often the loop looks.
pub const TICK: Duration = Duration::from_secs(10);
/// Run once things have been quiet this long…
pub const QUIET: Duration = Duration::from_secs(30);
/// …or this long after the first change, whichever comes first: the sync
/// worker writes on every poll, so "quiet" alone could wait forever.
pub const MAX_WAIT: Duration = Duration::from_secs(120);

/// Everything that, when it changes, can change a mirrored or published file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    db: (i64, i64),
    scope_modified: Option<SystemTime>,
    accounts: Vec<(String, bool)>,
}

impl Fingerprint {
    pub fn read(db: &Db, accounts: &[Account], scope_path: &Path) -> Fingerprint {
        Fingerprint {
            db: db.change_counter().unwrap_or((-1, -1)),
            scope_modified: std::fs::metadata(scope_path).and_then(|m| m.modified()).ok(),
            accounts: accounts.iter().map(|a| (a.id.clone(), a.is_active())).collect(),
        }
    }
}

/// Whether there is anything to keep current: a mirror that is on, a skill,
/// or a skill Jodd published before (so stopping the last one still removes
/// it). The loop does nothing else when this is false.
pub fn wanted(scope: &ScopeFile, skills_root: Option<&Path>) -> bool {
    scope.mirror().enabled || !scope.skills().is_empty() || skills_root.is_some_and(super::skills::has_published)
}

/// When to run, from what the loop observes.
#[derive(Debug, Default)]
pub struct Debouncer {
    seen: Option<Fingerprint>,
    synced: Option<Fingerprint>,
    first_change: Option<Instant>,
    last_change: Option<Instant>,
}

impl Debouncer {
    /// Record an observation. True when a run is due: something moved since
    /// the last run, and it has been quiet for [`QUIET`] or waiting for
    /// [`MAX_WAIT`]. The first observation after start counts as a change, so
    /// the app brings the files up to date shortly after it opens.
    pub fn observe(&mut self, fp: &Fingerprint, now: Instant) -> bool {
        if self.seen.as_ref() != Some(fp) {
            self.seen = Some(fp.clone());
            self.last_change = Some(now);
            self.first_change.get_or_insert(now);
        }
        if self.synced.as_ref() == Some(fp) {
            self.first_change = None;
            return false;
        }
        let quiet = self.last_change.is_some_and(|t| now.duration_since(t) >= QUIET);
        let waited = self.first_change.is_some_and(|t| now.duration_since(t) >= MAX_WAIT);
        quiet || waited
    }

    /// A run finished that started from `fp`. A change that landed during the
    /// run makes the next observation differ, so it is not lost.
    pub fn ran(&mut self, fp: &Fingerprint) {
        self.synced = Some(fp.clone());
        self.first_change = None;
    }
}

/// True when a run was refused only because another sync (the hook, or Sync
/// now) held the lock: try again later rather than count it as done.
pub fn was_busy(out: &super::SyncOutcome) -> bool {
    [&out.mirror_error, &out.skills_error].iter().any(|e| e.as_deref() == Some(super::BUSY))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(n: i64) -> Fingerprint {
        Fingerprint { db: (n, 0), scope_modified: None, accounts: vec![] }
    }

    #[test]
    fn the_first_look_after_start_runs_once_things_are_quiet() {
        let mut d = Debouncer::default();
        let t0 = Instant::now();
        assert!(!d.observe(&fp(1), t0));
        assert!(!d.observe(&fp(1), t0 + QUIET - Duration::from_secs(1)));
        assert!(d.observe(&fp(1), t0 + QUIET));
        d.ran(&fp(1));
        assert!(!d.observe(&fp(1), t0 + QUIET * 10), "nothing moved since the run");
    }

    #[test]
    fn a_change_waits_for_quiet_and_restarts_the_wait_while_it_keeps_changing() {
        let mut d = Debouncer::default();
        let t0 = Instant::now();
        d.ran(&fp(1));
        assert!(!d.observe(&fp(1), t0));
        assert!(!d.observe(&fp(2), t0));
        assert!(!d.observe(&fp(3), t0 + Duration::from_secs(20)), "still changing");
        assert!(!d.observe(&fp(3), t0 + Duration::from_secs(40)));
        assert!(d.observe(&fp(3), t0 + Duration::from_secs(50)), "30 s quiet");
    }

    #[test]
    fn writes_that_never_stop_still_sync_within_the_max_wait() {
        let mut d = Debouncer::default();
        let t0 = Instant::now();
        d.ran(&fp(0));
        let mut ran_at = None;
        for i in 1..=20 {
            let now = t0 + TICK * i as u32;
            if d.observe(&fp(i), now) {
                ran_at = Some(now);
                break;
            }
        }
        // The first change is at the first tick; the run is MAX_WAIT after it.
        assert_eq!(ran_at, Some(t0 + TICK + MAX_WAIT));
    }

    #[test]
    fn a_change_during_a_run_is_not_lost_and_going_back_counts_as_synced() {
        let mut d = Debouncer::default();
        let t0 = Instant::now();
        assert!(!d.observe(&fp(1), t0));
        assert!(d.observe(&fp(1), t0 + QUIET));
        d.ran(&fp(1));
        assert!(!d.observe(&fp(2), t0 + QUIET + TICK), "a new change waits");
        assert!(d.observe(&fp(2), t0 + QUIET * 2 + TICK));
        d.ran(&fp(2));
        assert!(!d.observe(&fp(2), t0 + QUIET * 5));
    }

    #[test]
    fn nothing_configured_means_nothing_to_watch() {
        let root = tempfile::tempdir().unwrap();
        assert!(!wanted(&ScopeFile::default(), Some(root.path())));
        let mut s = ScopeFile::default();
        s.agent_files.mirror.enabled = true;
        assert!(wanted(&s, None));
    }

    /// The event name is the whole contract between the loop and the screen.
    #[test]
    fn the_frontend_listens_for_this_event() {
        let ts = include_str!("../../../src/lib/agentFiles.ts");
        assert!(ts.contains(&format!("'{EVENT}'")), "agentFiles.ts must name {EVENT}");
    }

    #[test]
    fn the_counter_moves_on_a_write() {
        let db = crate::test_support::temp_db();
        let before = db.change_counter().unwrap();
        let n = crate::test_support::note("gmail:a@x.com", "u").build();
        db.upsert_from_remote(&n, crate::db::RemotePin::RemoteWins).unwrap();
        assert_ne!(db.change_counter().unwrap(), before);
    }
}
