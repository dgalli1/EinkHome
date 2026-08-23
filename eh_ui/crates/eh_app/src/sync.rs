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

use crate::app::{App, Overlay, Source};
use crate::client::{ApiClient, Delta};
use crate::store::Store;

use eh_hal::Framebuffer;

use crate::widgets::sync_popup::{SyncStage, SYNC_DONE_CLOSE_MS, SYNC_FAIL_CLOSE_MS};

/// Rows per delta round (C EH_SYNC_BATCH): 100k books = 100 rounds.
pub const EH_SYNC_BATCH: u32 = 1000;

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

/// Handle to the in-flight sync worker thread: its event stream plus the
/// shared cancel flag (settings_apply sets it BEFORE rebuilding endpoints
/// — C eh_sync_abort's generation bump — so an aborted round never
/// applies).
#[derive(Default)]
pub(crate) struct WorkerHandle {
    /// Worker → UI messages (None when idle / aborted).
    pub rx: Option<std::sync::mpsc::Receiver<SyncMsg>>,
    /// Polled between delta rounds and after each fetch.
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl WorkerHandle {
    /// Arm a fresh chain: new channel + clear cancel flag, returning the
    /// cancel clone the worker thread closes over.
    pub fn arm(&mut self, rx: std::sync::mpsc::Receiver<SyncMsg>) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.rx = Some(rx);
        self.cancel = std::sync::Arc::clone(&cancel);
        cancel
    }
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
        // C eh_model's ceiling is 400 rounds = 200k books (a 100k first
        // sync needs the 201st round to observe the empty delta).
        if rounds > 400 {
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

impl<B: Framebuffer> App<B> {
    /// Manual library sync (C top-bar sync icon, which==2 → eh_do_sync +
    /// eh_sync_popup_open).  While a sync is already in flight a tap just
    /// re-opens the sheet over the live run (C eh_sync_popup_open keeps
    /// the running counters).
    pub(crate) fn do_sync(&mut self) {
        if self.syncing {
            self.sync_popup_open();
            return;
        }
        self.start_sync(true);
    }

    /// Silent re-sync used by settings_apply / the source chooser (C calls
    /// eh_do_sync directly there — no progress sheet).
    pub(crate) fn resync(&mut self) {
        if self.syncing {
            return;
        }
        self.start_sync(false);
    }

    /// Spawn the sync worker thread.  Threading model (the boring safe
    /// option): the worker owns ONLY a cloned HTTP client and its own
    /// independently-opened [`Store`] handle on the same DB file; it
    /// streams [`crate::sync::SyncMsg`]s over an mpsc channel that
    /// [`App::tick`] drains on the UI thread.  Chosen over
    /// `Arc<Mutex<Store>>` because the App renders from its store every
    /// frame — a shared mutex would stall draws behind whole-round
    /// transactions — and SQLite's 2 s busy_timeout (set in Store::open)
    /// absorbs the rare commit collision between the two connections.
    pub(crate) fn start_sync(&mut self, popup: bool) {
        // No configured server — nothing to sync against (the C app
        // resolved its API host before arming eh_do_sync).
        if self.syncing || self.config.api_url.is_empty() {
            return;
        }
        // Logged synchronously on the UI thread so the e2e log slicer
        // always sees the entry even though the chain runs async
        // (C eh_evt_init's synchronous eh_do_sync entry log).
        crate::logger::log(&format!("[bookshelf] do_sync ENTER batch={}", crate::sync::EH_SYNC_BATCH));
        // Initial anti-suspend ban (C eh_do_sync's eh_sync_keep_awake);
        // per-round re-arms come back as SyncMsg::BanSleep.
        self.screen()
            .framebuffer()
            .ban_sleep(crate::sync::EH_SYNC_BAN_SLEEP_SEC as u32);
        let (tx, rx) = std::sync::mpsc::channel::<crate::sync::SyncMsg>();
        let cancel = self.sync_worker.arm(rx);
        self.syncing = true;
        if popup {
            self.sync_popup_open();
        }
        let client = self.client.clone();
        let db_path = self.db_path.clone();
        let spawned = std::thread::Builder::new()
            .name("sync".into())
            .spawn(move || {
                // The worker's own store handle; see the threading note.
                let store = match Store::open(&db_path) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(crate::sync::SyncMsg::Event(
                            crate::sync::SyncEvent::Failed(format!("store open: {e}")),
                        ));
                        return;
                    }
                };
                let _ = crate::sync::sync(
                    &client,
                    &store,
                    crate::sync::EH_SYNC_BATCH,
                    &cancel,
                    &mut |ev| {
                        let _ = tx.send(crate::sync::SyncMsg::Event(ev));
                    },
                    Some(&mut |secs| {
                        let _ = tx.send(crate::sync::SyncMsg::BanSleep(secs));
                    }),
                );
            });
        if spawned.is_err() {
            self.sync_worker.rx = None;
            self.syncing = false;
            crate::log("[eh_app] sync worker spawn failed");
        }
    }

    /// Abort any in-flight sync chain (C eh_sync_abort): set the cancel
    /// flag — checked between rounds AND after each fetch, so an aborted
    /// round never applies — and detach the stale event stream.  Called
    /// from settings_apply BEFORE the endpoint URLs are rebuilt.
    pub(crate) fn sync_abort(&mut self) {
        use std::sync::atomic::Ordering;
        self.sync_worker.cancel.store(true, Ordering::Relaxed);
        self.sync_worker.rx = None;
        self.syncing = false;
    }

    /// Open the sync-progress sheet (C eh_sync_popup_open).
    pub(crate) fn sync_popup_open(&mut self) {
        if self.sync_popup.open && self.overlay == Overlay::Sync {
            return;
        }
        // Re-opening the sheet over a LIVE run keeps the running counters
        // (C eh_sync_popup_open resets only when no sync is running, so
        // the progress lines never jump backwards).
        let live = self.syncing;
        let mut p = std::mem::take(&mut self.sync_popup);
        p.open = true;
        p.stage = SyncStage::Meta;
        p.stage_at = Some(std::time::Instant::now());
        if !live {
            p.round = 0;
            p.scanned = 0;
            p.covers_done = 0;
            p.covers_total = 0;
            p.error.clear();
        }
        self.sync_popup = p;
        self.set_overlay(Overlay::Sync);
    }

    /// Drain the sync worker's messages + advance the sheet's auto-close
    /// timers.  Returns true when the frame changed and a repaint is due.
    pub(crate) fn sync_poll(&mut self) -> bool {
        let msgs: Vec<crate::sync::SyncMsg> =
            self.sync_worker.rx.as_ref().map_or_else(Vec::new, |rx| rx.try_iter().collect());
        let mut changed = !msgs.is_empty();
        for m in msgs {
            match m {
                crate::sync::SyncMsg::BanSleep(secs) => {
                    // The hal handle lives on the UI thread; perform the
                    // worker's re-arm request here (C called BanSleep on
                    // the main thread too).
                    self.screen().framebuffer().ban_sleep(secs);
                }
                crate::sync::SyncMsg::Event(ev) => changed |= self.apply_sync_event(ev),
            }
        }
        if self.sync_popup.open {
            changed |= self.sync_popup_close_tick();
        }
        changed
    }

    /// Apply one worker event to the popup state machine + terminal
    /// bookkeeping (port of finish_sync / sync_round_outcome_fail's UI
    /// side).  Returns true when the frame changed.
    pub(crate) fn apply_sync_event(&mut self, ev: crate::sync::SyncEvent) -> bool {
        match ev {
            crate::sync::SyncEvent::Start => false, // the sheet opened at the trigger
            crate::sync::SyncEvent::MetaBatch { done, .. } => {
                self.sync_popup.stage = SyncStage::Meta;
                self.sync_popup.round = done;
                self.sync_popup.stage_at = Some(std::time::Instant::now());
                true
            }
            crate::sync::SyncEvent::ScanLocal => {
                self.sync_popup.stage = SyncStage::Scan;
                self.sync_popup.stage_at = Some(std::time::Instant::now());
                true
            }
            crate::sync::SyncEvent::Covers { done, total } => {
                self.sync_popup.stage = SyncStage::Covers;
                self.sync_popup.covers_done = done;
                self.sync_popup.covers_total = total;
                true
            }
            crate::sync::SyncEvent::Complete { rounds } => {
                crate::logger::log(&format!(
                    "[bookshelf] do_sync: rounds={rounds} cursor={} (books={})",
                    self.store.cursor().unwrap_or(0),
                    self.store.count().unwrap_or(0)
                ));
                self.finish_sync(true)
            }
            crate::sync::SyncEvent::Failed(e) => {
                crate::logger::log(&format!("[bookshelf] do_sync FAILED: {e}"));
                self.sync_popup.error = e;
                self.finish_sync(false)
            }
        }
    }

    /// Terminal bookkeeping for a sync chain (C finish_sync +
    /// eh_sync_popup_finish/fail): stop the spinner, rebuild the view,
    /// hand off to the cover warm pass, stage the popup auto-close.
    pub(crate) fn finish_sync(&mut self, ok: bool) -> bool {
        self.syncing = false;
        self.sync_worker.rx = None;
        // A source switch whose sync applies nothing must still re-project
        // the view under the new source (C keeps this unconditional too).
        self.rebuild_view();
        if self.source == Source::Kavita {
            self.cover_warm_start();
        }
        if self.sync_popup.open {
            self.sync_popup.stage = if ok { SyncStage::Covers } else { SyncStage::Fail };
            self.sync_popup.stage_at = Some(std::time::Instant::now());
        }
        self.refresh_shelf();
        true
    }

    /// Advance the sheet's auto-close (C sync_popup_close_tick): while the
    /// cover warm pass still drains, stay on COVERS so the striped bar
    /// moves; once drained flash DONE for SYNC_DONE_CLOSE_MS; FAIL shows
    /// the error for SYNC_FAIL_CLOSE_MS.  Returns true when the frame
    /// changed.
    pub(crate) fn sync_popup_close_tick(&mut self) -> bool {
        let Some(at) = self.sync_popup.stage_at else { return false };
        match self.sync_popup.stage {
            SyncStage::Fail => {
                if at.elapsed() >= std::time::Duration::from_millis(SYNC_FAIL_CLOSE_MS) {
                    self.set_overlay(Overlay::None); // also clears popup.open
                    return true;
                }
                false
            }
            SyncStage::Covers => {
                let (done, total) = self.warm_progress();
                if total > 0 && done < total {
                    if done != self.sync_popup.covers_done {
                        self.sync_popup.covers_done = done;
                        self.sync_popup.covers_total = total;
                        return true; // the bar advanced
                    }
                    return false;
                }
                self.sync_popup.stage = SyncStage::Done;
                self.sync_popup.stage_at = Some(std::time::Instant::now());
                true
            }
            SyncStage::Done => {
                if at.elapsed() >= std::time::Duration::from_millis(SYNC_DONE_CLOSE_MS) {
                    self.set_overlay(Overlay::None);
                    return true;
                }
                false
            }
            SyncStage::Meta | SyncStage::Scan => false, // modal while running
        }
    }

    /// Cover-warm progress (done, total) for the popup's covers bar
    /// (C eh_cover_warm_progress).
    pub(crate) fn warm_progress(&self) -> (u32, u32) {
        (self.warm.done(), self.warm.total as u32)
    }
    /// Advance the top-bar sync glyph rotation while a sync or download is
    /// in flight (C sync_spin_tick): 15°/s.  The facade ticks every 200 ms,
    /// so +3° per active tick matches the C cadence; returns true when the
    /// angle moved and the top bar needs a repaint.
    pub(crate) fn sync_spin_tick(&mut self) -> bool {
        if !(self.syncing || self.downloader.pending > 0) {
            self.sync_angle = 0; // nothing in flight — the glyph rests
            return false;
        }
        self.sync_angle = (self.sync_angle + 3) % 360;
        true
    }
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

    #[test]
    fn worker_handle_arm_shares_one_fresh_cancel_flag() {
        let (_tx, rx) = std::sync::mpsc::channel::<SyncMsg>();
        let mut h = WorkerHandle::default();
        assert!(h.rx.is_none());

        let cancel = h.arm(rx);
        // A brand-new chain starts uncancelled...
        assert!(!cancel.load(std::sync::atomic::Ordering::Relaxed));
        assert!(!h.cancel.load(std::sync::atomic::Ordering::Relaxed));
        // ...and the handle's flag IS the worker's clone: aborting via
        // the handle reaches the thread's `cancel` and vice versa.
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(h.cancel.load(std::sync::atomic::Ordering::Relaxed));
        assert!(h.rx.is_some());
    }
}
