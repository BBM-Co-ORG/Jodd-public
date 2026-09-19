//! Account admission for periodic and explicit sync work. No transport or DB policy.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};

pub struct Scheduler {
    lanes: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
    capacity: Arc<Semaphore>,
    local_capacity: Arc<Semaphore>,
}
pub struct Lease {
    _account: OwnedMutexGuard<()>,
    _capacity: OwnedSemaphorePermit,
}
impl Default for Scheduler {
    fn default() -> Self {
        Self::new(2)
    }
}
impl Scheduler {
    pub fn new(limit: usize) -> Self {
        assert!(limit > 0);
        Self {
            lanes: Mutex::new(HashMap::new()),
            capacity: Arc::new(Semaphore::new(limit)),
            local_capacity: Arc::new(Semaphore::new(1)),
        }
    }
    fn lane(&self, id: &str) -> Arc<AsyncMutex<()>> {
        let mut lanes = self.lanes.lock().unwrap();
        lanes.retain(|_, v| v.strong_count() > 0);
        lanes
            .entry(id.to_owned())
            .or_default()
            .upgrade()
            .unwrap_or_else(|| {
                let lane = Arc::new(AsyncMutex::new(()));
                lanes.insert(id.to_owned(), Arc::downgrade(&lane));
                lane
            })
    }
    /// Teardown must wait for this account's in-flight work, without consuming
    /// an unrelated account's transport slot.
    pub async fn exclusive(&self, id: &str) -> OwnedMutexGuard<()> {
        self.lane(id).lock_owned().await
    }
    /// LocalFS has a separate one-operation pool: ordinary local editing cannot
    /// wait for a remote account to free a network slot.
    pub async fn enter_local(&self, id: &str, periodic: bool) -> Option<Lease> {
        self.admit(id, periodic, &self.local_capacity).await
    }

    /// Acquire the account BEFORE capacity: waiters for A must not take B's slot.
    /// Periodic requests coalesce while a lane is queued/running. Flush waits,
    /// then rereads SQLite under the lease instead of replaying an old snapshot.
    pub async fn enter(&self, id: &str, periodic: bool) -> Option<Lease> {
        self.admit(id, periodic, &self.capacity).await
    }
    async fn admit(&self, id: &str, periodic: bool, capacity: &Arc<Semaphore>) -> Option<Lease> {
        let lane = self.lane(id);
        let account = if periodic {
            lane.try_lock_owned().ok()?
        } else {
            lane.lock_owned().await
        };
        let capacity = capacity.clone().acquire_owned().await.ok()?;
        Some(Lease {
            _account: account,
            _capacity: capacity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll, Waker},
    };
    fn poll<F: Future>(f: Pin<&mut F>) -> Poll<F::Output> {
        f.poll(&mut Context::from_waker(Waker::noop()))
    }
    #[test]
    fn delayed_account_does_not_block_another_and_capacity_is_bounded() {
        let s = Scheduler::default();
        let mut a = Box::pin(s.enter("A", false));
        let Poll::Ready(Some(a)) = poll(a.as_mut()) else {
            panic!("A admitted")
        };
        let mut b = Box::pin(s.enter("B", false));
        let Poll::Ready(Some(b)) = poll(b.as_mut()) else {
            panic!("B must progress while A is delayed")
        };
        let mut c = Box::pin(s.enter("C", false));
        assert!(poll(c.as_mut()).is_pending());
        drop(b);
        assert!(matches!(poll(c.as_mut()), Poll::Ready(Some(_))));
        drop(a);
    }
    #[test]
    fn waiting_same_account_does_not_take_capacity_and_local_writes_do_not_wait_for_network() {
        let s = Scheduler::default();
        let mut a = Box::pin(s.enter("A", false));
        let Poll::Ready(Some(a)) = poll(a.as_mut()) else {
            panic!()
        };
        let mut another_a = Box::pin(s.enter("A", false));
        assert!(poll(another_a.as_mut()).is_pending());
        let mut b = Box::pin(s.enter("B", false));
        let Poll::Ready(Some(b)) = poll(b.as_mut()) else {
            panic!("A waiter consumed B capacity")
        };
        let mut local = Box::pin(s.enter_local("local", false));
        let Poll::Ready(Some(local)) = poll(local.as_mut()) else {
            panic!("local edit waited for remote I/O")
        };
        let mut local_tick = Box::pin(s.enter_local("local", true));
        assert!(matches!(poll(local_tick.as_mut()), Poll::Ready(None)));
        let mut other_local = Box::pin(s.enter_local("other_local", false));
        assert!(poll(other_local.as_mut()).is_pending());
        drop(local);
        assert!(matches!(poll(other_local.as_mut()), Poll::Ready(Some(_))));
        drop(a);
        drop(b);
        assert!(matches!(poll(another_a.as_mut()), Poll::Ready(Some(_))));
    }
    #[test]
    fn ready_accounts_progress_fifo_while_one_account_stays_delayed() {
        let s = Scheduler::default();
        let mut a = Box::pin(s.enter("A", false));
        let Poll::Ready(Some(_a)) = poll(a.as_mut()) else {
            panic!()
        };
        let mut b = Box::pin(s.enter("B", false));
        let Poll::Ready(Some(b)) = poll(b.as_mut()) else {
            panic!()
        };
        let mut c = Box::pin(s.enter("C", false));
        let mut d = Box::pin(s.enter("D", false));
        assert!(poll(c.as_mut()).is_pending());
        assert!(poll(d.as_mut()).is_pending());
        drop(b);
        assert!(poll(d.as_mut()).is_pending(), "D cannot overtake queued C");
        let Poll::Ready(Some(c)) = poll(c.as_mut()) else {
            panic!()
        };
        // Even another periodic request for C coalesces rather than growing the queue.
        let mut duplicate = Box::pin(s.enter("C", true));
        assert!(matches!(poll(duplicate.as_mut()), Poll::Ready(None)));
        drop(c);
        assert!(matches!(poll(d.as_mut()), Poll::Ready(Some(_))));
    }
    #[test]
    fn cancellation_while_waiting_for_capacity_releases_account_lane() {
        let s = Scheduler::new(1);
        let mut a = Box::pin(s.enter("A", false));
        let Poll::Ready(Some(a)) = poll(a.as_mut()) else {
            panic!()
        };
        let mut b = Box::pin(s.enter("B", false));
        assert!(poll(b.as_mut()).is_pending());
        drop(b);
        drop(a);
        let mut b = Box::pin(s.enter("B", true));
        assert!(matches!(poll(b.as_mut()), Poll::Ready(Some(_))));
        drop(b);
        let _other = s.lane("C");
        assert_eq!(
            s.lanes.lock().unwrap().len(),
            1,
            "idle identities are not retained"
        );
    }
    #[test]
    fn flush_waits_periodic_coalesces_and_cancel_releases_queue() {
        let s = Scheduler::default();
        let mut first = Box::pin(s.enter("A", true));
        let Poll::Ready(Some(first)) = poll(first.as_mut()) else {
            panic!()
        };
        let mut periodic = Box::pin(s.enter("A", true));
        assert!(matches!(poll(periodic.as_mut()), Poll::Ready(None)));
        let mut flush = Box::pin(s.enter("A", false));
        assert!(poll(flush.as_mut()).is_pending());
        drop(flush);
        drop(first);
        let mut next = Box::pin(s.enter("A", false));
        assert!(matches!(poll(next.as_mut()), Poll::Ready(Some(_))));
    }
}

#[cfg(test)]
mod workload_tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        future::Future,
        pin::Pin,
        task::{Context, Poll, Waker},
    };
    use tokio::sync::oneshot;
    fn poll<F: Future>(f: Pin<&mut F>) -> Poll<F::Output> {
        f.poll(&mut Context::from_waker(Waker::noop()))
    }

    // Logical milliseconds, advanced only by the driver; no sleeps/network.
    // The capacity-one case characterizes the old worker's global exclusion.
    async fn delayed_backend(
        s: &Scheduler,
        account: &str,
        clock: &Cell<u64>,
        complete: oneshot::Receiver<()>,
        metrics: &RefCell<Vec<(String, u64, u64, u64)>>,
    ) {
        let queued = clock.get();
        let _lease = s.enter(account, false).await.unwrap();
        let started = clock.get();
        complete.await.unwrap();
        metrics.borrow_mut().push((
            account.into(),
            started - queued,
            clock.get() - started,
            clock.get() - queued,
        ));
    }
    #[test]
    fn measured_before_after_delayed_backend_workload() {
        for capacity in [1, 2] {
            let s = Scheduler::new(capacity);
            let clock = Cell::new(0);
            let metrics = RefCell::new(vec![]);
            let (a_done, a_rx) = oneshot::channel();
            let (b_done, b_rx) = oneshot::channel();
            let mut a = Box::pin(delayed_backend(&s, "A", &clock, a_rx, &metrics));
            let mut b = Box::pin(delayed_backend(&s, "B", &clock, b_rx, &metrics));
            assert!(poll(a.as_mut()).is_pending());
            assert!(poll(b.as_mut()).is_pending());
            if capacity == 2 {
                clock.set(1);
                b_done.send(()).unwrap();
                assert!(poll(b.as_mut()).is_ready());
                clock.set(100);
                a_done.send(()).unwrap();
                assert!(poll(a.as_mut()).is_ready());
                assert_eq!(
                    *metrics.borrow(),
                    vec![("B".into(), 0, 1, 1), ("A".into(), 0, 100, 100)]
                );
            } else {
                clock.set(100);
                a_done.send(()).unwrap();
                assert!(poll(a.as_mut()).is_ready());
                assert!(poll(b.as_mut()).is_pending());
                clock.set(101);
                b_done.send(()).unwrap();
                assert!(poll(b.as_mut()).is_ready());
                assert_eq!(
                    *metrics.borrow(),
                    vec![("A".into(), 0, 100, 100), ("B".into(), 100, 1, 101)]
                );
            }
            println!(
                "capacity={capacity}, (account, queue_ms, operation_ms, end_to_end_ms)={:?}",
                metrics.borrow()
            );
        }
    }

    #[test]
    fn overlapping_tick_flush_rereads_after_create_rekey_edit_and_delete() {
        use crate::{
            accounts::BackendKind, backend::SavedNote, db, push_one_dirty_db, save_note_db,
        };
        let dir = tempfile::tempdir().unwrap();
        let db = db::Db::open_unencrypted(&dir.path().to_path_buf()).unwrap();
        let s = Scheduler::default();
        let first = save_note_db(
            &db,
            "A",
            BackendKind::Microsoft,
            Some("placeholder"),
            "T",
            "v1",
            "Notes",
            None,
            None,
        )
        .unwrap();
        let calls = RefCell::new(vec![]);
        let (done, rx) = oneshot::channel();
        let mut tick = Box::pin(async {
            let _lease = s.enter("A", true).await.unwrap();
            let snapshot = db.list_dirty().unwrap().remove(0);
            calls.borrow_mut().push("create");
            rx.await.unwrap();
            let saved = SavedNote {
                local_version: snapshot.local_version,
                id: "remote".into(),
                uuid: "assigned".into(),
                version: "r1".into(),
                date: snapshot.date.clone(),
                body_html: snapshot.body_html.clone(),
            };
            push_one_dirty_db(&db, &snapshot, &saved, "assigned", true, true).unwrap();
        });
        assert!(poll(tick.as_mut()).is_pending());
        let edited = save_note_db(
            &db,
            "A",
            BackendKind::Microsoft,
            Some(&first.uuid),
            "T",
            "v2",
            "Notes",
            None,
            Some(first.local_version),
        )
        .unwrap();
        let mut flush = Box::pin(async {
            let _lease = s.enter("A", false).await.unwrap();
            let snapshot = db.list_dirty().unwrap().remove(0);
            assert_eq!(snapshot.uuid, "assigned");
            assert_eq!(snapshot.body_html, edited.body_html);
            calls.borrow_mut().push("update");
            let saved = SavedNote {
                local_version: snapshot.local_version,
                id: "remote".into(),
                uuid: "assigned".into(),
                version: "r2".into(),
                date: snapshot.date.clone(),
                body_html: snapshot.body_html.clone(),
            };
            push_one_dirty_db(&db, &snapshot, &saved, "assigned", false, false).unwrap();
        });
        assert!(poll(flush.as_mut()).is_pending());
        done.send(()).unwrap();
        assert!(poll(tick.as_mut()).is_ready());
        assert_eq!(
            db.get("assigned", "A").unwrap().unwrap().sync_state,
            db::SyncState::Dirty
        );
        assert!(poll(flush.as_mut()).is_ready());
        let mut again = Box::pin(s.enter("A", false));
        let Poll::Ready(Some(_guard)) = poll(again.as_mut()) else {
            panic!()
        };
        assert!(
            db.list_dirty().unwrap().is_empty(),
            "overlap must not repeat accepted push"
        );
        db.mark_deleted("assigned", "A").unwrap();
        let deletion = db.list_deleted_pending().unwrap().remove(0);
        assert_eq!(deletion.id, "remote");
        calls.borrow_mut().push("delete");
        assert_eq!(*calls.borrow(), vec!["create", "update", "delete"]);
    }
}
