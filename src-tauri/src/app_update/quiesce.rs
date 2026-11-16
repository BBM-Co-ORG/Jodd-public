//! Before Windows closes Jodd to install an update: wait until no account is
//! mid-push (spec 2026-11-11-app-updater-design.md §6).
//!
//! The installer ends the process with `exit(0)`. A push cut off after the
//! server took it but before `mark_pushed` leaves the row dirty, and the next
//! launch sends it again — on Microsoft a second note, since a Graph CREATE has
//! no idempotency marker. Holding every account's lane means no round is in
//! flight when the process ends, and none can start before it does.

use std::time::Duration;

use tokio::sync::OwnedMutexGuard;

use crate::sync_schedule::Scheduler;

/// How long to wait for rounds in flight before giving up on this attempt.
pub(crate) const INSTALL_QUIESCE: Duration = Duration::from_secs(30);

/// Every account's lane, held. Drop the result only when the process is about
/// to end, or to give up.
pub(crate) async fn quiesce(
    sched: &Scheduler,
    account_ids: &[String],
    deadline: Duration,
) -> Result<Vec<OwnedMutexGuard<()>>, String> {
    let held = quiesce_unless(crate::backup::running::any(), sched, account_ids, deadline).await?;
    // A backup or restore takes no sync lane, so one started during the wait
    // above would not have been waited for: look again with the lanes held.
    if crate::backup::running::any() {
        return Err(BACKUP_RUNNING.to_string());
    }
    Ok(held)
}

const BACKUP_RUNNING: &str = "a backup or restore is running; try again when it finishes";

/// `quiesce` with the backup check passed in: the flag is process-wide, so a
/// test that raised it would refuse every other test running beside it.
async fn quiesce_unless(
    backup_running: bool,
    sched: &Scheduler,
    account_ids: &[String],
    deadline: Duration,
) -> Result<Vec<OwnedMutexGuard<()>>, String> {
    if backup_running {
        return Err(BACKUP_RUNNING.to_string());
    }
    let take_all = async {
        let mut held = Vec::with_capacity(account_ids.len());
        for id in account_ids {
            held.push(sched.exclusive(id).await);
        }
        held
    };
    // On timeout the future is dropped, and every lane it took with it.
    tokio::time::timeout(deadline, take_all)
        .await
        .map_err(|_| "Jodd is still sending changes — try again in a minute".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn quiesce_takes_every_lane() {
        let sched = Scheduler::default();
        let held = quiesce_unless(false, &sched, &ids(&["a", "b"]), Duration::from_secs(2)).await.unwrap();
        assert_eq!(held.len(), 2);
        let blocked = tokio::time::timeout(Duration::from_millis(50), sched.enter("a", true)).await;
        // A periodic round on a held lane is refused at once (coalesced), never admitted.
        assert!(matches!(blocked, Ok(None) | Err(_)), "a round started while the lanes were held");
        let flush = tokio::time::timeout(Duration::from_millis(50), sched.enter("b", false)).await;
        assert!(flush.is_err(), "a flush started while the lanes were held");
    }

    #[tokio::test]
    async fn quiesce_waits_for_a_round_in_flight() {
        let sched = Arc::new(Scheduler::default());
        let lease = sched.enter("a", false).await.unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(lease);
        });
        let start = Instant::now();
        let held = quiesce_unless(false, &sched, &ids(&["a"]), Duration::from_secs(2)).await.unwrap();
        assert_eq!(held.len(), 1);
        assert!(start.elapsed() >= Duration::from_millis(90), "did not wait: {:?}", start.elapsed());
    }

    #[tokio::test]
    async fn quiesce_refuses_at_the_deadline() {
        let sched = Scheduler::default();
        let _busy = sched.exclusive("b").await;
        let err = quiesce_unless(false, &sched, &ids(&["a", "b"]), Duration::from_millis(100)).await.unwrap_err();
        assert!(err.contains("still sending changes"), "{err}");
        // Nothing leaks: the lane it had already taken is free again.
        let a = tokio::time::timeout(Duration::from_millis(50), sched.exclusive("a")).await;
        assert!(a.is_ok(), "lane a stayed held after the refusal");
    }

    #[tokio::test]
    async fn quiesce_refuses_during_backup() {
        let sched = Scheduler::default();
        let err = quiesce_unless(true, &sched, &ids(&["a"]), Duration::from_secs(1)).await.unwrap_err();
        assert!(err.contains("backup or restore"), "{err}");
        // Refused before taking anything.
        assert!(tokio::time::timeout(Duration::from_millis(50), sched.exclusive("a")).await.is_ok());
    }
}
