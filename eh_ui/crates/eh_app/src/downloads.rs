//! Download queue + worker thread (C eh_downloads.c).
//!
//! The e2e suite requires downloads to run OFF the UI thread: a slow file
//! must not freeze the event loop (the top-bar sync glyph keeps animating,
//! the framebuffer changes mid-fetch), and the progress popup must be modal.
//!
//! A single worker thread consumes the queue and writes each file to disk,
//! reporting completion over an mpsc channel the app drains on its next
//! event.  The thread reconstructs its own [`ApiClient`] from the job's
//! base/token so the worker never shares the app's client.
//!
//! Durability model (C dl_fetch / sweep_stale_parts / g_dl_gen):
//! every fetch lands in `<path>.part` and is renamed into place only on
//! success, so a crash or cancel never leaves a truncated final file;
//! boot sweeps orphan `.part` fragments; each job carries a generation
//! token so a canceled job that outlives its queue entry can never settle
//! (let alone fail) a re-enqueued book.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use eh_hal::Framebuffer;

use crate::app::{App, Overlay};
use crate::store::Book;

/// One queued file fetch.
struct Job {
    base: String,
    token: String,
    id: String,
    path: String,
    /// Generation token of the queue entry this job serves (C BsDlJob.gen).
    gen: u32,
}

/// One completed (or failed) fetch.
pub struct Done {
    pub id: String,
    pub path: String,
    pub ok: bool,
    /// Generation of the job that produced this completion.
    pub gen: u32,
}

/// Worker body: fetch one file and land it on disk.  Returns true when
/// the final path holds the complete file.  `epoch` is the cancel
/// generation: jobs whose gen is <= it were voided by [`Downloader::
/// cancel_all`] and must not touch the final path.
type FetchFn = Arc<dyn Fn(&Job, &AtomicU32) -> bool + Send + Sync>;

/// Write `bytes` to `<path>.part`, then rename into place (C dl_fetch
/// tail).  The `cancelled` probe runs before the part write; any failure
/// or cancel unlinks the fragment so neither a truncated final file nor
/// a stray `.part` ever survives the call.
pub fn write_part_atomic(path: &str, bytes: &[u8], cancelled: impl Fn() -> bool) -> bool {
    if cancelled() {
        crate::log(&format!("[bookshelf] download_book_file CANCELED path={path}"));
        return false;
    }
    let tmp = format!("{path}.part");
    let ok = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp); // never leave the .part behind
        crate::log(&format!("[bookshelf] download_book_file write/rename FAILED path={path}"));
    }
    ok
}

/// The real worker body: fetch over HTTP, then the atomic part+rename.
fn http_fetch(job: &Job, epoch: &AtomicU32) -> bool {
    let client = crate::client::ApiClient::new(&job.base, &job.token);
    match client.file(&job.id) {
        Ok(bytes) => {
            crate::log(&format!(
                "[eh_app] dl worker: got {} bytes for {}",
                bytes.len(),
                job.id
            ));
            let cancelled = || epoch.load(Ordering::SeqCst) >= job.gen;
            write_part_atomic(&job.path, &bytes, cancelled)
        }
        Err(e) => {
            crate::log(&format!("[eh_app] download worker FAILED id={}: {e}", job.id));
            false
        }
    }
}

/// The app-side handle: enqueue jobs, drain completions, cancel.
pub struct Downloader {
    tx: mpsc::Sender<Job>,
    rx: mpsc::Receiver<Done>,
    /// Books still queued or in flight.
    pub pending: usize,
    /// id → generation for every unsettled entry (queued, in flight, or
    /// done-but-undrained): the dedup set (C eh_find_download) AND the
    /// stale-settle filter (C dl_job_done's id+gen match).
    live: HashMap<String, u32>,
    /// Monotonic generation counter, bumped per enqueue (C g_dl_gen).
    gen: Arc<AtomicU32>,
    /// Cancel generation: every job with gen <= epoch is voided (its
    /// fetch may still be blocked in flight — the worker drops it before
    /// the rename and before sending Done).
    epoch: Arc<AtomicU32>,
}

impl Downloader {
    pub fn new() -> Self {
        Self::with_fetch(Arc::new(http_fetch))
    }

    /// Test seam: same queue mechanics over an injectable worker body.
    fn with_fetch(fetch: FetchFn) -> Self {
        let (tx, jrx) = mpsc::channel::<Job>();
        let (dtx, rx) = mpsc::channel::<Done>();
        let gen = Arc::new(AtomicU32::new(0));
        let epoch = Arc::new(AtomicU32::new(0));
        let wfetch = Arc::clone(&fetch);
        let wepoch = Arc::clone(&epoch);
        std::thread::spawn(move || {
            crate::log("[eh_app] dl worker: started");
            for job in jrx {
                if job.gen <= wepoch.load(Ordering::SeqCst) {
                    crate::log(&format!("[eh_app] dl worker: job voided id={}", job.id));
                    continue;
                }
                crate::log(&format!("[eh_app] dl worker: fetch id={}", job.id));
                let ok = wfetch(&job, &wepoch);
                // Re-check after the (possibly long) fetch: a cancel that
                // landed mid-flight voids both the rename and the settle.
                if job.gen <= wepoch.load(Ordering::SeqCst) {
                    continue;
                }
                crate::log(&format!("[eh_app] dl worker: write ok={ok}"));
                let _ = dtx.send(Done { id: job.id, path: job.path, ok, gen: job.gen });
            }
            crate::log("[eh_app] dl worker: exiting");
        });
        Self {
            tx,
            rx,
            pending: 0,
            live: HashMap::new(),
            gen,
            epoch,
        }
    }

    /// Queue a book file fetch.  No-op when the id already owns an
    /// unsettled entry (C eh_find_download guard: queued, in flight, or
    /// done-but-undrained).  The client is captured by value (base +
    /// token) so the worker owns a private client for its lifetime.
    /// Returns false when the enqueue was absorbed as a duplicate.
    pub fn enqueue(&mut self, base: &str, token: &str, id: &str, path: &str) -> bool {
        if self.live.contains_key(id) {
            crate::log(&format!("[bookshelf] enqueue dedup id={id}"));
            return false;
        }
        let gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        if self
            .tx
            .send(Job {
                base: base.to_string(),
                token: token.to_string(),
                id: id.to_string(),
                path: path.to_string(),
                gen,
            })
            .is_err()
        {
            return false;
        }
        self.live.insert(id.to_string(), gen);
        self.pending += 1;
        true
    }

    /// Abort every open download (C eh_cancel_downloads worker half):
    /// void every issued job — the queued ones are skipped outright, the
    /// in-flight one finishes its transfer into the void (no rename, no
    /// Done) — drop the dedup set and swallow completions that already
    /// landed.  Files that renamed into place before the cancel stay on
    /// disk; boot's flag reconciliation catches up on the next launch.
    pub fn cancel_all(&mut self) {
        self.epoch.store(self.gen.load(Ordering::SeqCst), Ordering::SeqCst);
        self.pending = 0;
        self.live.clear();
        while self.rx.try_recv().is_ok() {}
    }

    /// Drain one completed fetch, if any.  A completion whose id/generation
    /// no longer matches an unsettled entry is dropped here (C dl_job_done's
    /// stale-settle branch): it must never mark a re-enqueued book failed.
    pub fn try_next(&mut self) -> Option<Done> {
        let d = self.rx.try_recv().ok()?;
        match self.live.get(&d.id) {
            Some(&g) if g == d.gen => {
                self.live.remove(&d.id);
                Some(d)
            }
            _ => {
                crate::log(&format!(
                    "[bookshelf] stale download job settle dropped id={} gen={}",
                    d.id, d.gen
                ));
                None
            }
        }
    }

    /// Ids still owning an unsettled queue entry (test/diagnostic view).
    pub fn live_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.live.keys().cloned().collect();
        v.sort();
        v
    }

    /// A downloader with NO worker thread behind it: sends buffer up and
    /// nothing ever settles (unit-test seam for App-level batch logic).
    #[cfg(test)]
    pub fn inert() -> Self {
        let (tx, jrx) = mpsc::channel::<Job>();
        let (_dtx, rx) = mpsc::channel::<Done>();
        // Keep the job receiver alive (leaked, test-only) so sends buffer.
        std::mem::forget(jrx);
        Self {
            tx,
            rx,
            pending: 0,
            live: HashMap::new(),
            gen: Arc::new(AtomicU32::new(0)),
            epoch: Arc::new(AtomicU32::new(0)),
        }
    }
}

impl Default for Downloader {
    fn default() -> Self {
        Self::new()
    }
}

// ── boot-time reconciliation ────────────────────────────────────────────

/// Unlink stale `<file>.part` fragments left in the downloads dir by a
/// crash mid-fetch (C sweep_stale_parts): one bounded single pass;
/// errors are ignored — the worst case is a fragment surviving until
/// the next startup.  Returns the number removed.
pub fn sweep_stale_parts(dir: &str) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut seen = 0usize;
    let mut removed = 0usize;
    for e in rd.flatten() {
        seen += 1;
        if seen > 8192 {
            break;
        }
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.len() <= 5 || !name.ends_with(".part") {
            continue;
        }
        if std::fs::remove_file(e.path()).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 || seen > 0 {
        crate::log(&format!("[bookshelf] stale .part sweep removed={removed}"));
    }
    removed
}

/// Re-probe every book's on-device file and resync its downloaded flag
/// (C eh_refresh_downloaded_flags; the C boot path slices this across
/// timer ticks — Rust boots differently, so one bounded pass inline).
/// A book counts as downloaded when its expected downloads-dir filename
/// OR its stored local_path still exists; stale flags are cleared and
/// fresh ones gain their path.  Returns the number of flags flipped.
pub fn refresh_downloaded_flags(store: &crate::store::Store, dir: &str) -> usize {
    sweep_stale_parts(dir);
    // Snapshot the dir ONCE; the per-book test is a hash lookup.
    let names: std::collections::HashSet<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .take(8192)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => std::collections::HashSet::new(), // stale flags beat a crash
    };
    const PAGE: usize = 256;
    let mut changed = 0usize;
    let mut offset = 0usize;
    loop {
        let books = match store.list_books(PAGE, offset) {
            Ok(b) => b,
            Err(_) => break,
        };
        let got = books.len();
        for b in &books {
            let path = book_local_path(b, dir);
            let mut dl =
                names.contains(path.file_name().unwrap_or_default().to_string_lossy().as_ref())
                    && path.is_file();
            // The path a fresh flag records: the downloads-dir location,
            // or the stored location when only that still exists (C
            // book_existing_path keeps the stored path there).
            let mut where_ = path.to_string_lossy().into_owned();
            if !dl && !b.local_path.is_empty() && Path::new(&b.local_path).is_file() {
                // File still at its stored location although the downloads
                // folder has moved.
                dl = true;
                where_ = b.local_path.clone();
            }
            if dl != b.downloaded {
                let stored = if dl { where_ } else { String::new() };
                if store.set_downloaded(&b.id, dl, &stored).is_ok() {
                    changed += 1;
                }
            }
        }
        if got < PAGE {
            break;
        }
        offset += got;
    }
    if changed > 0 {
        crate::log(&format!("[bookshelf] refresh_downloaded_flags changed={changed}"));
    }
    changed
}

/// The local path a book downloads to (C eh_book_local_path verbatim): the
/// provider's filename sanitized to a bare basename (slashes → `_`,
/// control chars dropped), else `<id>.<ext>` (or bare `<id>` with no
/// extension).
pub fn book_local_path(book: &Book, downloads_dir: &str) -> PathBuf {
    let dir = Path::new(downloads_dir);
    if !book.filename.is_empty() && book.filename != "." && book.filename != ".." {
        let sanitized: String = book
            .filename
            .chars()
            .map(|c| if c == '/' { '_' } else { c })
            .filter(|c| *c as u32 >= 0x20 && *c != '\x7f')
            .collect();
        let sanitized = sanitized.trim();
        if !sanitized.is_empty() {
            return dir.join(sanitized);
        }
    }
    if !book.ext.is_empty() {
        dir.join(format!("{}.{}", book.id, book.ext))
    } else {
        dir.join(&book.id)
    }
}

/// UI-side state of the active download batch (the C g_dl_* globals):
/// what the progress sheet shows and what happens when the queue drains.
///
/// Three batch shapes share it, and every shape STARTS WHOLESALE — the
/// constructors replace the whole state, so a previous batch's tally can
/// never bleed into the next popup (a plain flag-poke once left
/// `total`/`done` stale and mislabeled a fresh download as "N complete"):
///
/// * single-book press ([`BatchUi::start_single`]) — the reader
///   auto-opens when the queue drains;
/// * plain popup batch ([`BatchUi::reset`] + enqueues: context Download /
///   series download / cancel) — the modal popup stays until dismissed;
/// * download-all ([`BatchUi::start_all`]) — bounded top-up queue +
///   remembered failures + the settle marker on drain.
#[derive(Default, Clone)]
pub struct BatchUi {
    /// Single-book press: auto-open the reader on drain.
    pub single: bool,
    /// (path, title) to auto-open once a single-book download drains
    /// (C: single press → download → launch reader).
    pub autopen: Option<(String, String)>,
    /// Download-all batch: logs the `download-all batch complete` settle
    /// marker on drain and drives the bounded top-up.
    pub batch_all: bool,
    /// Batch tally (done/failed/total) for the finished-popup label.
    pub done: usize,
    pub failed: usize,
    pub total: usize,
    /// Download-all top-up queue: undownloaded books staged but not yet
    /// enqueued (C batch_enqueue_slice's bounded-slice cursor).
    pub queue: std::collections::VecDeque<Book>,
    /// Ids the current download-all batch already tried and failed
    /// (C g_dl_batch_failed_ids): keeps the top-up from re-enqueueing
    /// failing books forever.
    pub failed_ids: std::collections::HashSet<String>,
}

impl BatchUi {
    /// A fresh single-book press: the reader opens when the queue drains.
    pub fn start_single(&mut self, autopen: (String, String)) {
        *self = BatchUi { single: true, autopen: Some(autopen), ..Default::default() };
    }

    /// Drop every batch trace (cancel / plain popup batch): the next
    /// popup draws from a clean slate.
    pub fn reset(&mut self) {
        *self = BatchUi::default();
    }

    /// Stage a download-all batch: fresh tally, no remembered failures,
    /// every undownloaded target queued for the bounded top-up.
    pub fn start_all(targets: Vec<Book>) -> BatchUi {
        BatchUi {
            batch_all: true,
            total: targets.len(),
            queue: targets.into_iter().collect(),
            ..Default::default()
        }
    }

    /// What the modal sheet's status line shows for the current state
    /// (the branch C draw_dl_popup makes).  `pending` is the worker's
    /// queued + in-flight job count.
    ///
    /// A LIVE download-all counts down what is left; once it drains
    /// (`batch_all` latched off, tally kept for the still-open modal) the
    /// line becomes the finished tally.  Single/series batches carry no
    /// tally (they start wholesale with `total == 0`), so they always
    /// count down — a previous batch can never bleed its numbers in.
    pub fn sheet_status(&self, pending: usize) -> SheetStatus {
        if !self.batch_all && self.total > 0 {
            SheetStatus::Tally { done: self.done, failed: self.failed }
        } else {
            SheetStatus::Remaining { count: pending }
        }
    }
}

/// The modal download sheet's status line, decided by
/// [`BatchUi::sheet_status`] (i18n rendering stays at the draw site).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SheetStatus {
    /// A finished batch's tally: "X downloaded, Y failed".
    Tally { done: usize, failed: usize },
    /// Jobs still to go: "N remaining".
    Remaining { count: usize },
}

impl<B: Framebuffer> App<B> {
    /// Abort every open download (C eh_cancel_downloads): void the
    /// in-flight fetch (its .part is never renamed), drop the queue +
    /// batch state, and close the popup.
    pub fn cancel_downloads(&mut self) {
        crate::logger::log("[bookshelf] cancel_downloads");
        self.downloader.cancel_all();
        self.dl.reset();
        self.set_overlay(Overlay::None);
    }


    /// Queue one book file on the worker + open the modal download popup
    /// (logging `draw_dl_popup` once per popup).
    pub(crate) fn enqueue_download(&mut self, id: &str, path: &Path) {
        let base = self.config.api_url.clone();
        let token = self.config.api_token.clone();
        self.downloader.enqueue(&base, &token, id, &path.to_string_lossy());
        if self.overlay != Overlay::Download {
            crate::logger::log("[bookshelf] draw_dl_popup");
        }
        self.set_overlay(Overlay::Download);
    }
    /// Drain completed downloads into the store, and when the queue empties
    /// close the popup + auto-open the reader for a single-book press.
    pub(crate) fn drain_downloads(&mut self) {
        loop {
            let Some(d) = self.downloader.try_next() else { break };
            self.downloader.pending = self.downloader.pending.saturating_sub(1);
            // The popup shows the remaining count: repaint it.
            self.dirty = true;
            if d.ok {
                self.dl.done += 1;
                if let Err(e) = self.store.set_downloaded(&d.id, true, &d.path) {
                    crate::log(&format!("[eh_app] set_downloaded: {e}"));
                }
                crate::logger::log(&format!("[bookshelf] download_book_file OK id={} path={}", d.id, d.path));
            } else {
                self.dl.failed += 1;
                crate::logger::log(&format!("[bookshelf] download_book_file FAILED id={}", d.id));
            }
            if self.dl.batch_all {
                crate::logger::log(&format!(
                    "[bookshelf] dl_progress done={} failed={} total={} active={}",
                    self.dl.done, self.dl.failed, self.dl.total, self.downloader.pending
                ));
                if !d.ok {
                    // C batch_note_failed: the top-up never re-enqueues a
                    // book this batch already tried and failed.
                    self.dl.failed_ids.insert(d.id.clone());
                }
                // Top the bounded queue up as jobs finish (C dl_advance).
                self.top_up_batch();
            }
        }
        if self.downloader.pending == 0 && self.dl.queue.is_empty() && self.overlay == Overlay::Download {
            if self.dl.single {
                // Single-book press: close the popup + auto-open the reader.
                self.set_overlay(Overlay::None);
                if let Some((path, title)) = self.dl.autopen.take() {
                    let path = PathBuf::from(path);
                    self.open_reader(&path, &title);
                }
                self.dl.single = false;
            } else {
                // Download-all / context Download: the popup stays open
                // (modal) until an outside tap dismisses it (C behavior).
                if self.dl.batch_all {
                    crate::logger::log("[bookshelf] download-all batch complete");
                    // The finished-tally popup redraw (the harness proves
                    // the popup survived the mid-drain tap via this token).
                    crate::logger::log("[bookshelf] draw_dl_popup");
                    self.dl.batch_all = false;
                    self.dirty = true;
                }
            }
        }
    }

    /// Download every not-yet-downloaded book (C More → Download all /
    /// eh_download_all_start): only downloaded=0 rows join the batch, the
    /// queue stays bounded and tops up as jobs finish, failures are
    /// remembered so they can't loop, and nothing opens when there is
    /// nothing to fetch.
    pub(crate) fn download_all(&mut self) {
        let n = self.store.count().unwrap_or(0) as usize;
        let targets: Vec<Book> = self
            .store
            .list_books(n, 0)
            .unwrap_or_default()
            .into_iter()
            .filter(|b| !b.downloaded)
            .collect();
        if targets.is_empty() {
            crate::logger::log("[bookshelf] download-all nothing to download");
            return;
        }
        self.dl = BatchUi::start_all(targets);
        self.top_up_batch();
        crate::logger::log(&format!("[bookshelf] download-all queued={}", self.dl.total));
        crate::logger::log("[bookshelf] draw_dl_popup");
        self.set_overlay(Overlay::Download);
    }

    /// In-flight window of the download-all batch (C keeps its whole
    /// queue bounded by EH_MAX_DOWNLOADS; the Rust worker channel is
    /// unbounded, so the window lives here).
    pub(crate) const DL_BATCH_WINDOW: usize = 8;

    /// Bounded download-all top-up (C dl_advance_batch →
    /// batch_enqueue_slice): keep DL_BATCH_WINDOW jobs queued/in flight,
    /// pulling staged undownloaded books and skipping ids this batch
    /// already failed (C batch_note_failed / batch_failed_id).
    pub(crate) fn top_up_batch(&mut self) {
        let dl = self.downloads_dir();
        while self.downloader.pending < Self::DL_BATCH_WINDOW {
            match self.dl.queue.pop_front() {
                Some(b) => {
                    if self.dl.failed_ids.contains(&b.id) {
                        continue;
                    }
                    let cur = book_local_path(&b, &dl);
                    let base = self.config.api_url.clone();
                    let token = self.config.api_token.clone();
                    self.downloader.enqueue(&base, &token, &b.id, &cur.to_string_lossy());
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("eh_dl_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn part_rename_on_success() {
        let dir = scratch("rename");
        let dst = dir.join("book.epub");
        assert!(write_part_atomic(dst.to_str().unwrap(), b"hello", || false));
        assert_eq!(std::fs::read(&dst).unwrap(), b"hello");
        assert!(!dir.join("book.epub.part").exists(), "fragment must be renamed away");
    }

    #[test]
    fn cancelled_write_never_touches_final_file() {
        let dir = scratch("cancel");
        let dst = dir.join("book.epub");
        std::fs::write(&dst, b"old").unwrap();
        assert!(!write_part_atomic(dst.to_str().unwrap(), b"new", || true));
        assert_eq!(std::fs::read(&dst).unwrap(), b"old", "cancel must not clobber the final file");
        assert!(!dir.join("book.epub.part").exists());
    }

    #[test]
    fn failed_write_leaves_no_final_nor_fragment() {
        let dir = scratch("fail");
        // Final path is a directory: fs::write succeeds on "<dir>.part"
        // but the rename fails — the fragment must be cleaned up.
        let dst = dir.join("book.epub");
        std::fs::create_dir(&dst).unwrap();
        assert!(!write_part_atomic(dst.to_str().unwrap(), b"data", || false));
        assert!(!dir.join("book.epub.part").exists());
        // Unwritable parent: the part write itself fails.
        assert!(!write_part_atomic(
            dir.join("nope").join("x.epub").to_str().unwrap(),
            b"data",
            || false
        ));
        assert!(!dir.join("nope").join("x.epub").exists());
    }

    #[test]
    fn dedup_enqueue_is_a_no_op() {
        let mut dl = Downloader::inert();
        assert!(dl.enqueue("b", "t", "a", "/tmp/a.epub"));
        assert!(!dl.enqueue("b", "t", "a", "/tmp/a.epub"), "duplicate id must be absorbed");
        assert!(dl.enqueue("b", "t", "c", "/tmp/c.epub"));
        assert_eq!(dl.pending, 2);
        assert_eq!(dl.live_ids(), vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn settled_id_may_be_reenqueued() {
        let mut dl = Downloader::inert();
        assert!(dl.enqueue("b", "t", "a", "/p"));
        dl.live.remove("a"); // settle (drain) removes the entry
        assert!(dl.enqueue("b", "t", "a", "/p"));
        assert_eq!(dl.pending, 2);
    }

    #[test]
    fn cancel_voids_inflight_and_queued_jobs() {
        let calls = Arc::new(AtomicU32::new(0));
        let release = Arc::new(AtomicU32::new(0));
        let calls2 = Arc::clone(&calls);
        let release2 = Arc::clone(&release);
        let fetch: FetchFn = Arc::new(move |_job, _e| {
            calls2.fetch_add(1, Ordering::SeqCst);
            // First call blocks until released (simulates a slow transfer);
            // later calls return fast.
            while calls2.load(Ordering::SeqCst) == 1 && release2.load(Ordering::SeqCst) == 0 {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            true
        });
        let mut dl = Downloader::with_fetch(fetch);
        assert!(dl.enqueue("b", "t", "slow", "/p/slow"));
        assert!(dl.enqueue("b", "t", "queued", "/p/queued"));
        while calls.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        dl.cancel_all();
        assert_eq!(dl.pending, 0);
        assert!(dl.live_ids().is_empty());
        release.store(1, Ordering::SeqCst);
        // Give the worker time to (not) deliver anything.
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(dl.try_next().is_none(), "voided jobs must never settle");
        assert!(dl.try_next().is_none());
    }

    #[test]
    fn reenqueue_after_cancel_gets_a_fresh_generation() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls2 = Arc::clone(&calls);
        let fetch: FetchFn = Arc::new(move |_job, _e| {
            let n = calls2.fetch_add(1, Ordering::SeqCst) + 1;
            if n == 1 {
                // First (soon-canceled) transfer lingers.
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            true
        });
        let mut dl = Downloader::with_fetch(fetch);
        assert!(dl.enqueue("b", "t", "a", "/p/a"));
        while calls.load(Ordering::SeqCst) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Cancel + immediate re-enqueue while the old transfer is still
        // in flight: the new job carries gen 2 > the cancel epoch.
        dl.cancel_all();
        assert!(dl.enqueue("b", "t", "a", "/p/a"));
        assert_eq!(dl.pending, 1);
        // Exactly one settle arrives, and it belongs to the NEW generation:
        // the stale job can neither deliver nor mis-mark the fresh entry.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let done = loop {
            if let Some(d) = dl.try_next() {
                break d;
            }
            assert!(std::time::Instant::now() < deadline, "fresh job never settled");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(done.ok);
        assert_eq!(done.gen, 2);
        assert!(dl.try_next().is_none(), "the stale gen-1 job must stay silent");
        // Exactly ONE settle was delivered; the app-side tally decrements
        // once for it (drain_downloads) and never sees the stale job.
        dl.pending -= 1;
        assert_eq!(dl.pending, 0);
    }

    #[test]
    fn stale_settle_with_live_newer_entry_is_dropped() {
        // Direct exercise of the try_next filter (C dl_job_done id+gen):
        // a completion carrying a dead generation must not settle the
        // newer entry for the same id.
        let (_dtx, drx) = mpsc::channel::<Done>();
        let mut dl = Downloader {
            tx: mpsc::channel().0,
            rx: drx,
            pending: 1,
            live: [("a".to_string(), 1u32)].into_iter().collect(),
            gen: Arc::new(AtomicU32::new(1)),
            epoch: Arc::new(AtomicU32::new(0)),
        };
        // Stale completion first: swallowed, the live entry survives.
        _dtx.send(Done { id: "a".into(), path: "/p".into(), ok: false, gen: 9_999 }).unwrap();
        assert!(dl.try_next().is_none());
        assert_eq!(dl.live_ids(), vec!["a".to_string()], "stale settle must not evict the entry");
        // The matching generation settles normally.
        _dtx.send(Done { id: "a".into(), path: "/p".into(), ok: false, gen: 1 }).unwrap();
        let d = dl.try_next().unwrap();
        assert!(!d.ok);
        assert!(dl.live_ids().is_empty());
    }

    // ── BatchUi contracts ───────────────────────────────────────────

    fn batch_book(id: &str) -> Book {
        Book { id: id.into(), ..Default::default() }
    }

    #[test]
    fn batch_starts_replace_the_whole_state() {
        // Regression for the stale-tally popup bug: every start wipes
        // the previous batch's tally, so a fresh popup can never show
        // an old batch's "N downloaded" numbers.
        let mut b = BatchUi::start_all(vec![batch_book("a"), batch_book("b")]);
        b.done = 5;
        b.failed = 1;
        b.failed_ids.insert("x".into());

        b.start_single(("/books/a.epub".into(), "A".into()));
        assert!(b.single);
        assert_eq!(b.autopen.as_ref().map(|(p, t)| (p.as_str(), t.as_str())), Some(("/books/a.epub", "A")));
        assert!(!b.batch_all);
        assert_eq!((b.done, b.failed, b.total), (0, 0, 0));
        assert!(b.queue.is_empty());
        assert!(b.failed_ids.is_empty());

        b.reset();
        assert_eq!(b.done, 0);
        assert!(b.autopen.is_none());
    }

    #[test]
    fn start_all_stages_queue_with_fresh_failures() {
        let b = BatchUi::start_all(vec![batch_book("a"), batch_book("b"), batch_book("c")]);
        assert!(b.batch_all);
        assert_eq!(b.total, 3);
        assert_eq!(b.queue.len(), 3);
        // A new batch forgets the previous one's failures.
        assert!(b.failed_ids.is_empty());
    }

    #[test]
    fn sheet_status_tally_only_after_a_finished_batch() {
        // Live download-all: the line counts down what is left.
        let live = BatchUi { batch_all: true, total: 12, done: 5, failed: 1, ..Default::default() };
        assert_eq!(live.sheet_status(4), SheetStatus::Remaining { count: 4 });
        // Drained download-all (flag latched off, tally kept on the
        // still-open modal): the finished tally.
        let drained = BatchUi { batch_all: false, total: 12, done: 11, failed: 1, ..Default::default() };
        assert_eq!(drained.sheet_status(0), SheetStatus::Tally { done: 11, failed: 1 });
        // Fresh single/series batches start wholesale with total == 0:
        // even right after a finished batch they count down and never
        // inherit the stale tally (the bug wholesale starts killed).
        let mut fresh = drained.clone();
        fresh.start_single(("/p".into(), "T".into()));
        assert_eq!(fresh.sheet_status(1), SheetStatus::Remaining { count: 1 });
    }
}
