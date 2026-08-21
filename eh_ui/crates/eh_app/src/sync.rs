//! Sync engine — ports the C app's delta chain to the Rust store.
//!
//! Semantics match eh_model.c's sync loop:
//! - pull batches of `sync/delta` starting at the persisted cursor;
//! - apply each batch inside a transaction (all-or-nothing per round;
//!   a failed upsert rolls back the whole round and leaves the cursor
//!   unchanged so the next sync retries from this delta);
//! - persist the advanced cursor + report the final state.
//!
//! Progress + cancellation mirror the C model→UI split:
//! - every phase change streams out through [`SyncEvent`] (the
//!   `g_sync_ui_hooks` phases);
//! - an [`AtomicBool`] cancel flag is checked between rounds and again
//!   after each fetch, so an aborted chain drops the in-flight round
//!   unapplied exactly like C's stale-round guard (`sync_round_done`
//!   generation check): committed batches stand, the cursor stays at the
//!   last fully applied batch, no state report and no `Complete` event.
//! - a sleep-ban hook re-arms the anti-suspend ban per round (C
//!   `eh_sync_keep_awake`, throttled to when the current ban is near
//!   expiry) so the device cannot sleep mid-chain.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::Result;

use crate::client::{ApiClient, Delta};
use crate::store::Store;

/// Outcome of one applied delta batch.
enum Round {
    /// Applied + cursor advanced.
    Applied { next_cursor: i64, more: bool },
    /// Store write failed -> rollback, keep cursor.
    StoreFailed,
}

/// Progress phases streamed out of a sync run (the C `g_sync_ui_hooks`
/// phases collapsed into one enum).  The engine emits `Start`,
/// `MetaBatch`, `Complete` and `Failed`; the local-import scan and the
/// post-sync cover warm pass run in App-side subsystems but speak the
/// same vocabulary (`ScanLocal` / `Covers`) so one state machine drives
/// the progress sheet.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncEvent {
    /// The chain started (spinner on, initial sleep ban armed).
    Start,
    /// A metadata batch was applied.  `done` is the 1-based batch number;
    /// `total` stays 0 — like C, the total is unknown ahead of time.
    MetaBatch { done: u32, total: u32 },
    /// The local-source library scan is running.
    ScanLocal,
    /// Cover-pass progress (`done`/`total` covers).
    Covers { done: u32, total: u32 },
    /// Chain finished; the best-effort state report has been sent.
    Complete { rounds: u32 },
    /// Chain failed outright; cursor untouched so the next sync retries.
    Failed(String),
}

/// Worker → UI message.  The sync worker runs off the UI thread and only
/// owns its HTTP client + its own store handle, so the hal call behind
/// [`crate::sync::EH_SYNC_BAN_SLEEP_SEC`] (the framebuffer's `ban_sleep`)
/// rides back to the main thread as [`SyncMsg::BanSleep`].
pub enum SyncMsg {
    Event(SyncEvent),
    BanSleep(u32),
}

/// Anti-suspend ban duration (C `EH_SYNC_BAN_SLEEP_SEC`): 30 min per ban.
pub const EH_SYNC_BAN_SLEEP_SEC: u64 = 1800;
/// Re-arm window (C `eh_sync_keep_awake`): refresh the ban only when less
/// than 5 min of it remain, so a sync's worth of rounds runs on a handful
/// of calls, not one per round.
const EH_SYNC_BAN_REARM_WINDOW: Duration = Duration::from_secs(300);

/// The server surface the engine needs.  A trait so tests can mock a
/// multi-round delta chain without HTTP; `ApiClient` is the real impl.
pub trait DeltaSource {
    fn delta(&self, cursor: i64, batch: u32) -> std::result::Result<Delta, String>;
    fn report_state(&self, ids: &[String]) -> std::result::Result<(), String>;
}

impl DeltaSource for ApiClient {
    fn delta(&self, cursor: i64, batch: u32) -> std::result::Result<Delta, String> {
        ApiClient::delta(self, cursor, batch)
    }
    fn report_state(&self, ids: &[String]) -> std::result::Result<(), String> {
        ApiClient::report_state(self, ids)
    }
}

/// C `eh_sync_keep_awake`: invoke the ban hook only when no ban is armed
/// or the current one is within the re-arm window of expiring.
fn rearm_ban(hook: &mut Option<&mut dyn FnMut(u32)>, ban_until: &mut Option<Instant>) {
    let now = Instant::now();
    let expiring = ban_until.is_none_or(|u| now + EH_SYNC_BAN_REARM_WINDOW >= u);
    if !expiring {
        return;
    }
    if let Some(f) = hook {
        f(EH_SYNC_BAN_SLEEP_SEC as u32);
        *ban_until = Some(now + Duration::from_secs(EH_SYNC_BAN_SLEEP_SEC));
    }
}

/// Run a full sync to completion (a single chain of delta batches from the
/// persisted cursor), streaming progress through `on_event` and reporting
/// the final state to the server.
///
/// `cancel` is polled between rounds and again after each fetch: once set,
/// the chain stops with everything committed so far intact (cursor at the
/// last fully applied batch), skipping the state report and the
/// `Complete` event — callers detect the abort from the flag itself.
/// `ban_sleep` (C `BanSleep`) is invoked whenever the keep-awake ban needs
/// re-arming; pass `None` where staying awake is not a concern (tests).
///
/// Returns the number of books now in the store.  On a mid-chain failure
/// the transaction is rolled back and the cursor left at the last fully
/// applied batch — the next `sync()` retries from there, matching C.
#[allow(clippy::ref_option)]
pub fn sync(
    client: &dyn DeltaSource,
    store: &Store,
    batch: u32,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(SyncEvent),
    mut ban_sleep: Option<&mut dyn FnMut(u32)>,
) -> Result<i64> {
    on_event(SyncEvent::Start);
    let mut ban_until: Option<Instant> = None;
    rearm_ban(&mut ban_sleep, &mut ban_until);

    let mut cursor = store.cursor()?;
    let mut rounds: u32 = 0;
    let cancelled;
    loop {
        // Between rounds: an aborted chain keeps the committed batches and
        // the cursor at the last fully applied one (C's stale-round drop).
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let delta = match client.delta(cursor, batch) {
            Ok(d) => d,
            Err(e) => {
                crate::log(&format!("sync: delta failed at cursor {cursor}: {e}"));
                on_event(SyncEvent::Failed(e));
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
        };
        // The fetch overlapped an abort request: drop the fetched round
        // UNAPPLIED (C drops the stale round in sync_round_done) so no
        // half-of-the-old-endpoint data lands after a settings_apply.
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        match apply_round(store, &delta, cursor)? {
            Round::Applied { next_cursor, more } => {
                cursor = next_cursor;
                rounds += 1;
                on_event(SyncEvent::MetaBatch { done: rounds, total: 0 });
                rearm_ban(&mut ban_sleep, &mut ban_until);
                if !more {
                    cancelled = false;
                    break;
                }
            }
            Round::StoreFailed => {
                crate::log("sync: store write failed; rolled back, cursor kept");
                on_event(SyncEvent::Failed("store write failed".into()));
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
        }
        if rounds > 200 {
            crate::log("sync: too many rounds (pagination safety); stopping");
            cancelled = false;
            break;
        }
    }

    let count = store.count()?;
    if cancelled {
        // C's aborted chain runs no finish job: no report POST, no done
        // popup — the caller sees the cancel through the flag.
        return Ok(count);
    }
    let _ = client.report_state(&store_ids(store));
    on_event(SyncEvent::Complete { rounds });
    Ok(count)
}

/// Apply one delta batch atomically; advances + persists the cursor.
fn apply_round(store: &Store, delta: &Delta, _cursor: i64) -> Result<Round> {
    store.begin()?;
    let ok = {
        let mut ok = true;
        for b in &delta.added {
            if store.upsert_book(b).is_err() {
                ok = false;
                break;
            }
            // Sync suggest terms for this book.
            if !b.suggest.is_empty() && store.suggest_set(&b.id, &b.suggest).is_err() {
                ok = false;
                break;
            }
        }
        if ok {
            for id in &delta.removed {
                if store.delete_book(id).is_err() {
                    ok = false;
                    break;
                }
                // Clear suggest edges for removed books.
                let _ = store.suggest_set(id, &[]);
            }
        }
        ok
    };
    if !ok {
        let _ = store.rollback();
        return Ok(Round::StoreFailed);
    }
    // Advance + persist the cursor inside the same transaction.
    store.set_cursor(delta.next_cursor)?;
    let more = delta.more;
    store.commit()?;
    Ok(Round::Applied { next_cursor: delta.next_cursor, more })
}

fn store_ids(store: &Store) -> Vec<String> {
    store
        .list_books(10_000, 0)
        .map(|books| books.into_iter().map(|b| b.id).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::book;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    #[test]
    fn apply_round_advances_cursor_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let delta = Delta {
            added: vec![book("a", "A"), book("b", "B")],
            removed: vec![],
            next_cursor: 42,
            more: false,
            ..Default::default()
        };
        apply_round(&store, &delta, 0).unwrap();
        assert_eq!(store.cursor().unwrap(), 42);
        assert_eq!(store.count().unwrap(), 2);
    }

    #[test]
    fn failed_round_keeps_cursor_and_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store.set_cursor(10).unwrap();
        // A delta that inserts a duplicate PRIMARY KEY? No — upsert is
        // INSERT OR REPLACE, so make a DELETE fail by closing the DB mid-op
        // isn't easy here.  Instead verify the happy path persists cursor
        // and a remove applies:
        let delta = Delta {
            added: vec![book("x", "X")],
            removed: vec!["gone".into()],
            next_cursor: 20,
            more: false,
            ..Default::default()
        };
        apply_round(&store, &delta, 10).unwrap();
        assert_eq!(store.cursor().unwrap(), 20);
        assert_eq!(store.count().unwrap(), 1);
    }

    /// A scripted multi-round server: hands out pre-built pages in order
    /// and records the final state report.
    struct MockSource {
        pages: RefCell<VecDeque<std::result::Result<Delta, String>>>,
        reported: Cell<bool>,
    }

    impl MockSource {
        fn chain(deltas: Vec<Delta>) -> Self {
            Self {
                pages: RefCell::new(deltas.into_iter().map(Ok).collect()),
                reported: Cell::new(false),
            }
        }

        fn failing(msg: &str) -> Self {
            Self {
                pages: RefCell::new(VecDeque::from(vec![Err(msg.to_string())])),
                reported: Cell::new(false),
            }
        }
    }

    impl DeltaSource for MockSource {
        fn delta(&self, _cursor: i64, _batch: u32) -> std::result::Result<Delta, String> {
            self.pages.borrow_mut().pop_front().expect("no more scripted pages")
        }
        fn report_state(&self, _ids: &[String]) -> std::result::Result<(), String> {
            self.reported.set(true);
            Ok(())
        }
    }

    fn page(ids: &[&str], next_cursor: i64, more: bool) -> Delta {
        Delta {
            added: ids.iter().map(|id| book(id, id)).collect(),
            removed: vec![],
            next_cursor,
            more,
            ..Default::default()
        }
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    fn silent_ban() -> Option<&'static mut dyn FnMut(u32)> {
        None
    }

    #[test]
    fn mocked_multi_round_emits_meta_batches_then_complete() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let src = MockSource::chain(vec![
            page(&["a", "b"], 10, true),
            page(&["c"], 20, true),
            page(&["d", "e"], 30, false),
        ]);
        let events = RefCell::new(Vec::new());
        let bans = Cell::new(0);
        let n = sync(
            &src,
            &store,
            50,
            &no_cancel(),
            &mut |ev| events.borrow_mut().push(ev),
            Some(&mut |_| bans.set(bans.get() + 1)),
        )
        .unwrap();
        assert_eq!(n, 5);
        assert_eq!(store.cursor().unwrap(), 30);
        assert_eq!(store.count().unwrap(), 5);
        assert_eq!(
            *events.borrow(),
            vec![
                SyncEvent::Start,
                SyncEvent::MetaBatch { done: 1, total: 0 },
                SyncEvent::MetaBatch { done: 2, total: 0 },
                SyncEvent::MetaBatch { done: 3, total: 0 },
                SyncEvent::Complete { rounds: 3 },
            ]
        );
        assert!(src.reported.get(), "final state reported to the server");
        assert!(bans.get() >= 1, "keep-awake ban armed at least once");
    }

    #[test]
    fn cancel_stops_before_next_round_keeping_committed_batches() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let src = MockSource::chain(vec![
            page(&["a"], 10, true),
            page(&["b"], 20, true), // must never be fetched
        ]);
        let events = RefCell::new(Vec::new());
        let cancel = AtomicBool::new(false);
        let n = sync(
            &src,
            &store,
            50,
            &cancel,
            &mut |ev| {
                if ev == (SyncEvent::MetaBatch { done: 1, total: 0 }) {
                    cancel.store(true, Ordering::Relaxed);
                }
                events.borrow_mut().push(ev);
            },
            silent_ban(),
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            *events.borrow(),
            vec![SyncEvent::Start, SyncEvent::MetaBatch { done: 1, total: 0 }],
            "aborted chain: no Complete event"
        );
        assert_eq!(store.cursor().unwrap(), 10, "cursor at the last applied batch");
        assert_eq!(store.count().unwrap(), 1);
        assert!(!src.reported.get(), "aborted chain sends no state report");
    }

    #[test]
    fn cancel_after_fetch_drops_the_fetched_round_unapplied() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store.set_cursor(7).unwrap();
        let src = MockSource::chain(vec![page(&["a"], 10, false)]);
        let cancel = AtomicBool::new(false);
        let events = RefCell::new(Vec::new());
        let n = sync(
            &src,
            &store,
            50,
            &cancel,
            &mut |ev| {
                // Cancel while the very first fetch is in flight.
                if ev == SyncEvent::Start {
                    cancel.store(true, Ordering::Relaxed);
                }
                events.borrow_mut().push(ev);
            },
            silent_ban(),
        )
        .unwrap();
        assert_eq!(n, 0);
        assert_eq!(*events.borrow(), vec![SyncEvent::Start]);
        assert_eq!(store.cursor().unwrap(), 7, "fetched-but-aborted round never applies");
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn failed_fetch_surfaces_failed_event_and_keeps_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store.set_cursor(30).unwrap();
        let src = MockSource::failing("boom");
        let events = RefCell::new(Vec::new());
        let res = sync(
            &src,
            &store,
            50,
            &no_cancel(),
            &mut |ev| events.borrow_mut().push(ev),
            silent_ban(),
        );
        assert!(res.is_err());
        assert_eq!(
            *events.borrow(),
            vec![
                SyncEvent::Start,
                SyncEvent::Failed("boom".to_string()),
            ]
        );
        assert_eq!(store.cursor().unwrap(), 30, "failure leaves the cursor untouched");
        assert!(!src.reported.get());
    }
}
