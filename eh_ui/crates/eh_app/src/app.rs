//! The EinkHome application: owns the current screen + active overlay and
//! routes taps with one geometry source (the shell's taffy rects), exactly
//! the C app's eh_hit_top_bar / eh_hit_pager / eh_hit_thumbnail +
//! eh_book_press_action model.
//!
//! The app is the ONLY owner of the [`Screen`]: navigation (page flip,
//! back) rebuilds the screen from the same framebuffer, mirroring the C
//! app's full-redraw navigation. Overlays (More menu, Settings, Launcher)
//! are drawn by the app on top of the screen's canvas and flush their own
//! region with a partial update.

use std::path::{Path, PathBuf};

use eh_hal::{Framebuffer, InputEvent, KeyCode, Rect};
use eh_shell::Screen;

use crate::appui::{PAGER_H, TOP_BAR_H};
use crate::client::ApiClient;
use crate::config::{Config, parse_kv_file};
use crate::cover;
use crate::shelf::{self, ShelfEntry};
use crate::store::{Book, Store};

/// A row of the More menu (the C app's menu drawer), in tap order.
#[derive(Clone, Copy, PartialEq)]
pub enum MenuRow {
    GroupBy,
    SortBy,
    DownloadAll,
    Settings,
    Applications,
}

/// A row/button of the Settings screen (C eh_settings rows).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SettingsRow {
    ApiHost,
    ApiKey,
    ReaderApp,
    DownloadFolder,
    SystemApp,
    Save,
    ShowLogs,
    Licenses,
}

/// A launcher entry (C BsLauncherItem): a group header (group=true) or an
/// app cell with its firmware icon path, launch path, and the optional
/// per-item launch arguments ("params"/"param", capped at
/// eh_launcher.rs's LAUNCHER_MAX_PARAMS).
#[derive(Clone, Default)]
pub struct LauncherItem {
    pub group: bool,
    pub text: String,
    pub path: String,
    pub icon: String,
    pub params: Vec<String>,
}

/// Overlay state (the C app's `overlay` family, collapsed to the screens
/// this port has).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Overlay {
    None,
    /// The "…" menu drawer (right 3/4 of the screen).
    More,
    /// The settings page (full screen).
    Settings,
    /// The launcher overlay (full screen, scrolling column).
    Launcher,
    /// The source chooser sheet (Kavita / Local / Folder).
    Source,
    /// The modal download-progress popup.
    Download,
    /// The modal sync-progress sheet (C eh_draw_sync_popup).
    Sync,
    /// The long-press context menu sheet (Open/Download/Delete or
    /// series Download-all/Delete).
    Context,
    /// The Group by chooser sheet.
    GroupChooser,
    /// The Sort by chooser sheet.
    SortChooser,
    /// The full-screen log viewer.
    LogViewer,
    /// The licenses list viewer.
    Licenses,
    /// One license's full-text page.
    LicenseDetail,
}

/// One long-press context action (C eh_ctx_*).
#[derive(Clone, Copy, PartialEq)]
pub enum ContextAction {
    Open,
    Download,
    Delete,
    DownloadAll,
    DeleteAll,
}

/// Which chooser sheet is open (group vs sort) — both share the same
/// centered-row sheet layout.
#[derive(Clone, Copy)]
pub enum ChooserKind {
    Group,
    Sort,
}

/// Stage of the sync-progress sheet (C EH_SYNC_STAGE_META/SCAN/COVERS/
/// DONE/FAIL).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SyncStage {
    /// Pulling metadata batches.
    Meta,
    /// The local-source library scan.
    Scan,
    /// The post-sync cover warm pass.
    Covers,
    /// Flashed briefly before the sheet auto-closes.
    Done,
    /// The chain failed; the error shows before the auto-close.
    Fail,
}

/// State machine for the sync-progress sheet (C eh_g_state.sync_popup +
/// sync_stage/sync_round/sync_scan + the `bsyncp` weak timer).
#[derive(Clone, Debug)]
pub struct SyncPopup {
    pub open: bool,
    pub stage: SyncStage,
    /// Metadata batch counter (C sync_round, shown as `batch N`).
    pub round: u32,
    /// Books scanned by the local import (C sync_scan).
    pub scanned: u32,
    /// Cover-pass counters for the striped bar (C eh_cover_warm_progress).
    pub covers_done: u32,
    pub covers_total: u32,
    /// The failure text for the Fail stage line.
    pub error: String,
    /// When the current stage was entered (drives the auto-close timing).
    pub stage_at: Option<std::time::Instant>,
}

impl Default for SyncPopup {
    fn default() -> Self {
        Self {
            open: false,
            stage: SyncStage::Meta,
            round: 0,
            scanned: 0,
            covers_done: 0,
            covers_total: 0,
            error: String::new(),
            stage_at: None,
        }
    }
}

/// Sync-sheet height (C popup_geom(..., 190)).
pub(crate) const SYNC_SHEET_H: u32 = 190;
/// Auto-close delays ported from eh_popups.c: the Done line flashes for
/// 900 ms before the sheet closes; the Fail line shows for 1500 ms.
const SYNC_DONE_CLOSE_MS: u64 = 900;
const SYNC_FAIL_CLOSE_MS: u64 = 1500;

/// The pager's four page actions (the C contract: -1/-3/-4/-2 →
/// prev/first/last/next).
#[derive(Clone, Copy, PartialEq)]
pub enum PageAction {
    Prev,
    First,
    Last,
    Next,
}

/// The standard firmware reader path (C eh_plat_standard_reader).
pub const STANDARD_READER: &str = "/ebrmain/bin/eink-reader.app";

/// The third-party reader path (C eh_plat_koreader_path): present only
/// when the user installed it under /mnt/ext1/applications.
pub const KOREADER_PATH: &str = "/mnt/ext1/applications/koreader.app";

/// Probe the known reader binaries (C eh_detect_readers): returns the
/// paths that are actually executable, in offer order.  The standard
/// reader is always in the firmware image; KOReader only when installed.
pub fn detect_readers() -> Vec<&'static str> {
    let executable = |p: &str| {
        std::fs::metadata(p)
            .map(|m| {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            })
            .unwrap_or(false)
    };
    let out: Vec<&'static str> = [(STANDARD_READER, "Standard"), (KOREADER_PATH, "KOReader")]
        .into_iter()
        .filter(|(p, label)| {
            let ok = executable(p);
            crate::logger::log(&format!(
                "[bookshelf] reader {}: {} ({p})",
                if ok { "detected" } else { "not found" },
                label
            ));
            ok
        })
        .map(|(p, _)| p)
        .collect();
    // Host/PC fallback (no firmware readers on the filesystem): offer the
    // standard reader so the row still cycles Auto → Standard.
    if out.is_empty() {
        vec![STANDARD_READER]
    } else {
        out
    }
}

/// Human label of a detected reader path (C eh_g_readers[].label).
pub fn reader_label_of(path: &str) -> &'static str {
    match path {
        KOREADER_PATH => "KOReader",
        _ => "Standard",
    }
}

/// Map a stored `reader=` value back to a preference index given the
/// detected readers (C eh_reader_pref_from_path): "auto"/""/NULL → 0
/// (server open-with); a path matching a detected reader → its 1-based
/// index; anything else (e.g. a reader that was uninstalled) → 0.
pub fn reader_pref_from_path(value: &str, readers: &[&str]) -> i32 {
    if value.is_empty() || value == "auto" {
        return 0;
    }
    readers
        .iter()
        .position(|p| *p == value)
        .map_or(0, |i| i as i32 + 1)
}

/// Suggestion rows shown in the live band (C EH_SUGGEST_MAX_HITS).
pub const SUGGEST_MAX_HITS: usize = 10;

/// The active library source (C `BsSourceMode`, EH_SOURCE_*).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Kavita,
    Local,
    Folder,
}

impl Source {
    /// The persisted config value is the lowercase C camel name
    /// ("kavita"/"local"/"folder"); anything else → Kavita.
    pub fn from_config(s: &Option<String>) -> Self {
        match s.as_deref() {
            Some("local") => Source::Local,
            Some("folder") => Source::Folder,
            _ => Source::Kavita,
        }
    }
    /// The config-file value (C `eh_save_config_file` writes the same).
    pub fn config_value(self) -> String {
        match self {
            Source::Kavita => "kavita".to_string(),
            Source::Local => "local".to_string(),
            Source::Folder => "folder".to_string(),
        }
    }
}

/// Shelf rendering mode (C `BsViewMode`, EH_VIEW_GRID/LIST).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Grid,
    List,
}

/// The top-level tab (C `BsMainTab`, EH_TAB_LIBRARY/SEARCH).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Library,
    Search,
}
/// The field currently edited by the on-screen keyboard (Settings).
#[derive(Clone, Copy, PartialEq)]
pub enum KbField {
    ApiHost,
    ApiKey,
    /// The search page's query edit.
    Search,
}

// The firmware's OpenKeyboard commit is async: the static handler stashes
// the field + text in this thread_local and the app drains it on its next
// event (same thread — the inkview event loop runs both).
thread_local! {
    static KB_FIELD: std::cell::RefCell<KbField> = const { std::cell::RefCell::new(KbField::ApiHost) };
    static KB_PENDING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The firmware keyboard commit callback (static fn pointer, shared by all
/// fields): stashes the edited text in the thread_local; the app drains it
/// on its next event.
pub(crate) fn kb_commit(bytes: &[u8]) {
    let s = String::from_utf8_lossy(bytes).into_owned();
    KB_PENDING.with(|p| *p.borrow_mut() = Some(s));
}

/// Arm the keyboard for `field` (call before `Framebuffer::open_keyboard`).
pub(crate) fn kb_arm(field: KbField) {
    KB_FIELD.with(|f| *f.borrow_mut() = field);
}

/// Drain a committed keyboard edit (field, text), if one is pending.
pub(crate) fn kb_take_pending() -> Option<(KbField, String)> {
    let field = KB_FIELD.with(|f| *f.borrow());
    KB_PENDING.with(|p| p.borrow_mut().take()).map(|t| (field, t))
}

/// The bookshelf app bound to one framebuffer backend.
pub struct App<B: Framebuffer> {
    screen: Option<Screen<B>>,
    /// Framebuffer facts cached so overlay draws (which run while
    /// `screen` is take()n inside present) never need `screen()` — a
    /// re-entrant `screen()` there panics.  Refreshed whenever the screen
    /// is alive (see [`App::sync_fb_cache`]).
    fb_screen_w: u32,
    fb_net_active: bool,
    fb_profile: eh_hal::DeviceProfile,
    theme_cache: std::collections::HashMap<String, Option<eh_hal::ThemeBitmap>>,
    /// The bottom of the app's content area (C `eh_content_bottom()`): the
    /// screen height minus the self-drawn status strip on devices where the
    /// firmware panel painter is inactive.
    pub content_bottom: u32,
    /// Height of the self-drawn status strip (0 when the firmware owns the
    /// panel band).
    self_panel: u32,
    /// The minute (unix/60) the self panel last stamped; the strip is
    /// redrawn only when the minute rolls over (C re-stamps on show +
    /// keyboard commit; minute granularity covers the ticking clock).
    last_panel_min: i64,
    pub client: ApiClient,
    pub store: Store,
    pub config: Config,
    pub cfg_path: Option<PathBuf>,
    pub covers_dir: PathBuf,
    /// Reading progress per local path (C g_progress): reloaded from the
    /// firmware explorer db on init and show/foreground.
    pub progress: crate::progress::ProgressMap,
    pub page: usize,
    pub pages: usize,
    /// The current page's entries; the grid widgets mirror these.
    pub entries: Vec<ShelfEntry>,
    pub overlay: Overlay,
    /// Menu-row rects (rebuilt each draw; tap geometry matches the paint).
    pub menu_rows: Vec<(Rect, MenuRow)>,
    /// Settings row/button rects (same pattern).
    pub settings_rows: Vec<(Rect, SettingsRow)>,
    pub launcher_items: Vec<LauncherItem>,
    /// Launcher item rects, parallel to `launcher_items` (layout coords,
    /// pre-scroll — taps apply the scroll offset like the C app).
    pub launcher_rects: Vec<Rect>,
    pub launcher_scroll: i32,
    /// Launcher drag tracking: the last pointer y + accumulated delta while
    /// a drag is in flight (PointerMove -> scroll, then PointerUp taps).
    drag_y: Option<i32>,
    drag_total: i32,
    pub launcher_body_h: i32,
    pub launcher_view_h: i32,
    pub source: Source,
    pub view_mode: ViewMode,
    pub tab: Tab,
    pub query: String,
    /// True between the manual-sync trigger and its completion (drives the
    /// top-bar sync glyph).
    pub syncing: bool,
    /// Rotation (deg) of the top-bar sync glyph while a sync/download is
    /// in flight (C eh_g_state.sync_angle; the tick advances it 15°/s).
    pub sync_angle: i32,
    /// Source-chooser row rects (parallel to the three rows).
    pub source_rows: Vec<Rect>,
    /// Active grouping preset + drill level (C eh_g_group / drill).
    pub group: crate::store::GroupPreset,
    pub sort: crate::store::SortMode,
    pub drill: u32,
    /// Per-level saved pages + raw scope values + display names for the
    /// nested group drill (C eh_g_saved_pages[] / eh_g_drill_values[],
    /// EH_GROUP_MAX_LEVELS): level L's page is remembered when its card is
    /// tapped, and restored on drill-back so the user lands where they
    /// left off.  The deepest non-empty name feeds the top-bar title.
    pub drill_saved_pages: [usize; 2],
    pub drill_values: [String; 2],
    pub drill_names: [String; 2],
    /// Reader preference (C eh_g_state.reader_pref): 0 = Auto, 1 = the
    /// standard eink reader.
    pub reader_pref: i32,
    pub reader_path: String,
    /// Keyboard is open editing the search input (C search_kb flag).
    pub search_kb: bool,
    /// Live suggestion terms for the current keyboard buffer.
    pub suggestions: Vec<String>,
    /// Last keyboard buffer the suggest tick acted on (C g_last_suggest_q):
    /// the 200 ms poll only re-queries the store when the buffer moved.
    pub suggest_q: String,
    /// Full-library cover-warm queue (C eh_cover_warm_start): remote ids
    /// still to fetch, one drained per tick while online.
    pub warm_queue: Vec<String>,
    /// The settings row (or search input) currently owning the on-screen
    /// keyboard — the draw inverts the editing row (C eh_g_kb_field).
    pub kb_editing: Option<KbField>,
    /// Group/sort chooser row rects (drawn in the chooser sheet overlays).
    pub chooser_rects: Vec<Rect>,
    /// Download queue + worker + completion channel.
    pub downloader: crate::downloads::Downloader,
    /// True when the active download batch came from a single-book press
    /// (auto-open the reader when it drains); false for download-all.
    pub dl_single: bool,
    /// True when the active batch is a download-all (logs the
    /// `download-all batch complete` settle marker on drain).
    pub dl_batch_all: bool,
    /// Download-all batch tally (done/failed/total) for `dl_progress`.
    pub dl_done: usize,
    pub dl_failed: usize,
    pub dl_total: usize,
    /// (path, title) to auto-open in the reader once a single-book download
    /// drains (C: single press → download → launch reader).
    pub dl_autopen: Option<(String, String)>,
    /// Download-all top-up queue: undownloaded books staged but not yet
    /// enqueued (C batch_enqueue_slice's bounded-slice cursor).
    pub dl_batch_queue: std::collections::VecDeque<Book>,
    /// Ids the current download-all batch already tried and failed
    /// (C g_dl_batch_failed_ids): keeps the top-up from re-enqueueing
    /// failing books forever.
    pub dl_batch_failed: std::collections::HashSet<String>,
    pub context_items: Vec<ContextAction>,
    pub context_rects: Vec<Rect>,
    /// Series set by long-press (for the `context menu open series=N` log).
    pub context_series: u32,
    /// The license currently shown in the detail page (licenses viewer).
    pub license_selected: Option<usize>,
    /// First visible row of the log tail (<0 = pinned to the newest end,
    /// C eh_g_state.log_scroll) / of the licenses list or detail.
    pub log_scroll: i32,
    pub lic_scroll: i32,
    /// Decoded launcher icon art by path (decoded once; the emulator PNG
    /// decode is ~100ms each, so per-frame re-decoding froze the render).
    pub icon_cache: std::collections::HashMap<String, (u32, u32, Vec<u8>)>,
    /// The book the context menu was opened for (None when dismissed).
    pub context_book: Option<Book>,
    /// The series context's scope + label (stack-card long-press).
    pub context_scope: String,
    pub context_label: String,
    pub context_count: i64,
    /// Long-press tracking: the down-tap screen position + time.
    press_pos: Option<(i32, i32)>,
    press_start: Option<std::time::Instant>,
    /// True when the frame content changed since the last present (the
    /// present skip: unchanged frames redraw nothing — the emulator's
    /// full redraw is ~1s, so skipping keeps event processing prompt).
    pub dirty: bool,
    /// The overlay the last present drew (skip detection).
    pub last_overlay: Overlay,
    /// Folder-source browser state (C BR_MODE_BROWSER: path/scroll/rows).
    pub browser: crate::local::Browser,
    /// In-flight local import scan (worker → main-thread apply), with its
    /// chain generation so a re-kick invalidates a stale result
    /// (C g_local_scan_gen).
    pub(crate) local_scan:
        Option<std::sync::mpsc::Receiver<(u32, Vec<crate::local::LocalBook>)>>,
    pub(crate) local_gen: u32,
    /// Path of the store DB — the async sync worker opens its own handle
    /// on the same file (Store::open's legacy import is once-guarded by
    /// the `.migrated` rename; the FTS backfill no-ops when populated).
    db_path: PathBuf,
    /// Event stream from the in-flight sync worker (None when idle).
    pub(crate) sync_rx: Option<std::sync::mpsc::Receiver<crate::sync::SyncMsg>>,
    /// Cancel flag shared with the worker (settings_apply sets it before
    /// rebuilding endpoints — C eh_sync_abort's generation bump).
    pub(crate) sync_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Sync-progress sheet state (visible while overlay == Overlay::Sync).
    pub sync_popup: SyncPopup,
    /// Total ids queued by the current cover-warm pass (denominator for
    /// the popup's covers bar; C eh_cover_warm_progress's total).
    pub(crate) warm_total: usize,
}

/// Books per shelf page by breakpoint (the C app's per-breakpoint grid).
pub fn per_page(bp: eh_layout::Breakpoint) -> usize {
    match bp {
        eh_layout::Breakpoint::Narrow => 6,
        eh_layout::Breakpoint::Std => 15,
        eh_layout::Breakpoint::Wide => 24,
    }
}

/// True when `path` exists (or was just created) and accepts a write
/// probe — the C `access(wanted, W_OK)` + mkdir dance.
fn ensure_writable_dir(path: &str) -> bool {
    if let Err(e) = std::fs::create_dir_all(path) {
        crate::log(&format!("[eh_app] create {path} failed: {e}"));
        return false;
    }
    let probe = format!("{path}/.eh-probe");
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

impl<B: Framebuffer> App<B> {
    /// Build the app and render the first shelf page (a fresh store syncs
    /// the full library from the API first; a warm store is instant).
    pub fn new(fb: B, config: Config, cfg_path: Option<PathBuf>, app_dir: &Path) -> Self {
        let client = ApiClient::new(&config.api_url, &config.api_token);
        let db_path = app_dir.join(Store::LIB_DB_FILENAME);
        let store = Store::open(&db_path)
            .unwrap_or_else(|e| panic!("open store at {}: {e}", db_path.display()));
        let covers_dir = cover::resolve_covers_dir(app_dir);
        let mut downloads_dir = config
            .downloads_dir
            .clone()
            .unwrap_or_else(|| "/mnt/ext1/Downloads".to_string());
        // C eh_resolve_downloads_dir: a downloads dir we cannot create or
        // write (first run on the host, non-root guest) falls through to
        // the platform scratch root so downloads still work.
        if !ensure_writable_dir(&downloads_dir) {
            crate::log(&format!(
                "[eh_app] downloads dir {downloads_dir} unusable; falling back to /tmp"
            ));
            downloads_dir = "/tmp".to_string();
            let _ = std::fs::create_dir_all(&downloads_dir);
        }
        let config = Self::ensure_config(&config, cfg_path.as_deref(), &downloads_dir);
        // Language resolution (C cfg load + eh_evt_detect_lang): the
        // config value seeds the chain; device global.cfg / $LANG may
        // still override it inside i18n::init.
        crate::i18n::init(config.language.as_deref());
        // Boot-time reconciliation (C eh_refresh_downloaded_flags_boot_start
        // + sweep_stale_parts): sweep orphan .part fragments, then resync
        // every book's downloaded flag with what is actually on disk.
        crate::downloads::refresh_downloaded_flags(&store, &downloads_dir);
        let screen = Screen::new(fb, shelf::shelf_font());
        let (content_bottom, self_panel) = {
            let s = screen.framebuffer().screen();
            // Live devices with no firmware panel painter draw their own
            // 106px status strip (C eh_plat_panel_height's *self_panel);
            // the SDL/PC build and firmware-panel platforms use the
            // content area as-is.  The BACKEND owns the decision.
            if screen.framebuffer().needs_self_panel() {
                (s.height.saturating_sub(106), 106)
            } else {
                (s.content_height(), 0)
            }
        };
        let source = Source::from_config(&config.source);
        let mut app = Self {
            screen: Some(screen),
            fb_screen_w: 0,
            fb_net_active: true,
            fb_profile: eh_hal::DeviceProfile::default(),
            theme_cache: std::collections::HashMap::new(),
            content_bottom,
            self_panel,
            last_panel_min: -1,
            client,
            store,
            config,
            cfg_path,
            covers_dir,
            source_rows: Vec::new(),
            group: crate::store::GroupPreset::None,
            sort: crate::store::SortMode::Title,
            dl_batch_failed: std::collections::HashSet::new(),
            context_items: Vec::new(),
            progress: crate::progress::reload(),
            page: 0,
            pages: 0,
            entries: Vec::new(),
            overlay: Overlay::None,
            menu_rows: Vec::new(),
            settings_rows: Vec::new(),
            launcher_items: Vec::new(),
            launcher_rects: Vec::new(),
            launcher_scroll: 0,
            launcher_body_h: 0,
            launcher_view_h: 0,
            source,
            view_mode: ViewMode::Grid,
            tab: Tab::Library,
            query: String::new(),
            syncing: false,
            drill_saved_pages: [0; 2],
            drill_values: [String::new(), String::new()],
            drill_names: [String::new(), String::new()],
            drill: 0,
            reader_pref: 0,
            reader_path: "auto".to_string(),
            search_kb: false,
            suggestions: Vec::new(),
            kb_editing: None,
            suggest_q: String::new(),
            // Full-library cover-warm pass (C eh_cover_warm_start):
            // remote book ids still needing a cover fetch, popped one
            // per app tick while online.
            warm_queue: Vec::new(),
            chooser_rects: Vec::new(),
            downloader: crate::downloads::Downloader::new(),
            dl_single: false,
            dl_batch_all: false,
            dl_done: 0,
            dl_failed: 0,
            dl_total: 0,
            dl_autopen: None,
            dl_batch_queue: std::collections::VecDeque::new(),
            sync_angle: 0,
            log_scroll: -1,
            lic_scroll: 0,
            context_rects: Vec::new(),
            context_series: 0,
            license_selected: None,
            icon_cache: std::collections::HashMap::new(),
            context_book: None,
            context_scope: String::new(),
            context_label: String::new(),
            context_count: 0,
            press_pos: None,
            press_start: None,
            drag_y: None,
            drag_total: 0,
            browser: Default::default(),
            local_scan: None,
            local_gen: 0,
            dirty: true,
            last_overlay: Overlay::None,
            db_path,
            sync_rx: None,
            sync_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sync_popup: SyncPopup::default(),
            warm_total: 0,
        };
        app.sync_fb_cache();
        app.boot();
        app
    }

    /// Open the firmware keyboard on a settings field (C
    /// eh_input.c:435/453): the commit is async — the handler stashes the
    /// text and [`App::on_event`] drains it.
    pub fn edit_field(&mut self, field: KbField) {
        use crate::app::{kb_arm, kb_take_pending};
        // The draw inverts the row owning the keyboard (C
        // eh_settings_draw_row's `editing`).
        self.kb_editing = Some(field);
        self.dirty = true;
        let initial = match field {
            KbField::ApiHost => self.config.api_url.clone(),
            KbField::ApiKey => self.config.api_token.clone(),
            KbField::Search => self.query.clone(),
        };
        // Any stale pending commit is discarded (a new edit supersedes it).
        let _ = kb_take_pending();
        kb_arm(field);
        let (title, init) = match field {
            KbField::ApiHost => (crate::i18n::tr("settings.api_host"), initial.as_str()),
            KbField::ApiKey => (crate::i18n::tr("settings.api_key"), initial.as_str()),
            KbField::Search => (crate::i18n::tr("tab.search"), initial.as_str()),
        };
        // The commit handler lives in eh_backend_inkview (static fn
        // pointer); it pushes into app's thread_local and we drain on the
        // next event.
        self.screen()
            .framebuffer_mut()
            .open_keyboard(title, init, crate::app::kb_commit);
    }

    /// Start the background full-library cover-warm pass (C
    /// eh_cover_warm_start, run after a remote sync on the Kavita
    /// source): every server book's cover lands in the on-disk cache so
    /// offline launches still show real covers — not just the pages the
    /// user happened to view.
    fn cover_warm_start(&mut self) {
        if self.source != Source::Kavita {
            return;
        }
        self.warm_queue = self
            .store
            .list_books(1_000_000, 0)
            .unwrap_or_default()
            .into_iter()
            .map(|b| b.id)
            .collect();
        self.warm_total = self.warm_queue.len();
    }

    /// Drain the warm pass: at most one fetch handed to a background
    /// thread per call (the C pass arms its bcov weak timer per fetch),
    /// skipped entirely offline.  The network call MUST NOT run on the
    /// UI thread — a blocking fetch here stalls event processing for the
    /// whole request duration and the shell feels dead after boot.
    fn cover_warm_tick(&mut self) {
        if self.warm_queue.is_empty() || !self.screen().framebuffer().net_active() {
            return;
        }
        while let Some(id) = self.warm_queue.pop() {
            if cover::load_cached(&self.covers_dir, &id).is_some() {
                continue;
            }
            let client = self.client.clone();
            let covers_dir = self.covers_dir.clone();
            let _ = std::thread::Builder::new()
                .name("cover-warm".into())
                .spawn(move || {
                    let _ = cover::fetch(&client, &covers_dir, &id);
                });
            break;
        }
    }
    /// Boot: sync the library delta (only when online or the source is
    /// local-only — C eh_evt_init), then build the first shelf page.
    fn boot(&mut self) {
        self.resolve_reader();
        let online = self.screen().framebuffer().net_active();
        match self.source {
            // The Local source kicks the async storage-root import instead
            // of a remote sync (C EVT_INIT → eh_local_import_scanner); the
            // apply lands on a later tick.
            Source::Local => crate::local::kick_import(self),
            Source::Folder => {} // the browser is the shelf body; no sync
            Source::Kavita => {
                if online {
                    // Async like C's one-shot initsync timer: the worker
                    // streams events; tick() applies the terminal one
                    // (rebuild + warm pass) once the chain lands.
                    self.start_sync(false);
                }
            }
        }
        // Materialise the default view (flat, recent order) — the shelf
        // reads from `view`, and the group/sort choosers rebuild it.
        let (g, s, d, q) = (self.group, self.sort, self.drill, self.query.clone());
        let total = {
            let scopes = self.drill_scopes();
            self.store.view_rebuild(g as i64, s as i64, d as i64, &q, &scopes).unwrap_or(0)
        };
        crate::logger::log(&format!(
            "[bookshelf] view_rebuild: view={} sort={} group={} drill={}",
            total, s as i64, g as i64, d
        ));
        self.refresh_shelf();
        // Full-library cover-warm pass (C: eh_cover_warm_start after a
        // remote sync on the Kavita source) — every server cover lands in
        // the cache in the background, drained by cover_warm_tick.
        self.cover_warm_start();
        // The visible page's covers first (the C on-page fetch path), so
        // the initial shelf shows art without waiting for the background
        // pass to reach them; skipped entirely offline.
        if online {
            let ids: Vec<String> = self.entries.iter().map(|e| e.book.id.clone()).collect();
            for id in ids {
                if cover::load_cached(&self.covers_dir, &id).is_none() {
                    let _ = cover::fetch(&self.client, &self.covers_dir, &id);
                }
            }
        }
    }

    /// Persist the resolved config (C: eh_save_config_file at boot, so the
    /// defaults become visible + editable in Settings).  The /tmp override
    /// (dead/delayed API for the e2e suite) must NOT leak into the base
    /// cfg: the save writes the base file's own api_url/api_token, while
    /// the runtime config keeps the override (re-applied on every load).
    fn ensure_config(config: &Config, cfg_path: Option<&Path>, downloads_dir: &str) -> Config {
        let mut config = config.clone();
        if config.downloads_dir.as_deref().unwrap_or("") != downloads_dir {
            config.downloads_dir = Some(downloads_dir.to_string());
        }
        if let Some(p) = cfg_path {
            let base = parse_kv_file(p).unwrap_or_default();
            let mut save = config.clone();
            if !base.api_url.is_empty() {
                save.api_url = base.api_url;
            }
            if !base.api_token.is_empty() {
                save.api_token = base.api_token;
            }
            if let Err(e) = save.save(p) {
                crate::log(&format!("[eh_app] config save failed: {e}"));
            }
        }
        config
    }


    // ── screen access ─────────────────────────────────────────────────


    /// Refresh the framebuffer caches from the live screen; call after
    /// building/moving the screen so overlay draws (which run while the
    /// screen is take()n) can use the cached values.
    fn sync_fb_cache(&mut self) {
        if let Some(s) = self.screen.as_mut() {
            let fb = s.framebuffer();
            self.fb_screen_w = fb.screen().width;
            self.fb_profile = fb.device_profile();
            self.fb_net_active = fb.net_active();
        }
    }
    /// Screen width safe to call from overlay draws (screen may be
    /// take()n during present).
    pub fn screen_width(&self) -> u32 {
        self.screen
            .as_ref()
            .map(|s| s.framebuffer().screen().width)
            .unwrap_or(self.fb_screen_w)
    }

    /// Device profile safe to call from overlay draws.
    pub(crate) fn device_profile(&mut self) -> eh_hal::DeviceProfile {
        self.sync_fb_cache();
        self.fb_profile
    }

    /// Theme-resource lookup safe to call from overlay draws: resolves
    /// through the framebuffer when it is alive, else replays the cache.
    pub(crate) fn theme_resource(&mut self, name: &str) -> Option<eh_hal::ThemeBitmap> {
        if self.screen.is_some() {
            self.sync_fb_cache();
            let t = self.screen.as_mut().unwrap().framebuffer().theme_resource(name);
            self.theme_cache.insert(name.to_string(), t.clone());
            t
        } else {
            self.theme_cache.get(name).cloned().flatten()
        }
    }
    pub fn screen(&mut self) -> &mut Screen<B> {
        self.screen.as_mut().expect("screen built")
    }

    /// Present the current frame: the screen, then the active overlay on
    /// top of the canvas.  The overlay + the self status strip flush only
    /// their own regions (partial update — the e-ink discipline).
    pub fn present(&mut self) {
        let _t0 = std::time::Instant::now();
        self.drain_keyboard();
        // Complete any worker downloads (may auto-open the reader when a
        // single-book batch drains) before we take the screen.
        self.drain_downloads();
        let ov = self.overlay;
        let changed = self.dirty || ov != self.last_overlay;
        self.dirty = false;
        self.last_overlay = ov;
        if !changed {
            // Unchanged frame: nothing to repaint (the emulator's full
            // redraw is ~1s, so skipping keeps event processing prompt —
            // and on e-ink it is the correct discipline).  Only the
            // self-panel minute rollover still needs the stamp.
            if self.self_panel > 0 {
                let min = panel_minute();
                if min != self.last_panel_min {
                    self.last_panel_min = min;
                    if let Some(s) = self.screen.as_mut() {
                        stamp_self_panel(s.framebuffer_mut(), self.content_bottom, self.self_panel);
                    }
                }
            }
            return;
        }
        let mut s = self.screen.take().expect("screen present");
        s.redraw_full();
        if ov != Overlay::None {
            let scr = s.framebuffer().screen();
            let fmt = s.framebuffer().format();
            let stride = s.framebuffer().stride();
            let mut dirty: Vec<Rect> = Vec::new();
            {
                let fb = s.framebuffer_mut();
                let mut surf = eh_render::Surface::new(fb.surface_mut(), scr.width, scr.height, stride, fmt);
                match ov {
                    Overlay::More => crate::menu::draw(&mut surf, self, &mut dirty),
                    Overlay::Settings => crate::settings::draw(&mut surf, self, &mut dirty),
                    Overlay::Launcher => crate::launcher::draw(&mut surf, self, &mut dirty),
                    Overlay::Source => crate::source::draw(&mut surf, self, &mut dirty),
                    Overlay::Download => draw_download_popup(&mut surf, self, &mut dirty),
                    Overlay::Sync => draw_sync_popup(&mut surf, self, &mut dirty),
                    Overlay::Context => draw_context_menu(&mut surf, self, &mut dirty),
                    Overlay::GroupChooser => draw_chooser_sheet(&mut surf, self, &mut dirty, ChooserKind::Group),
                    Overlay::SortChooser => draw_chooser_sheet(&mut surf, self, &mut dirty, ChooserKind::Sort),
                    Overlay::LogViewer => crate::viewer::draw_log_viewer(&mut surf, self, &mut dirty),
                    Overlay::Licenses => crate::viewer::draw_licenses(&mut surf, self, &mut dirty),
                    Overlay::LicenseDetail => crate::viewer::draw_license_detail(&mut surf, self, &mut dirty),
                    Overlay::None => {}
                }
            }
            if let Some(u) = union_rects(&dirty) {
                s.framebuffer_mut().refresh(u, eh_hal::RefreshMode::Partial);
            }
        }
        // The self-drawn status strip lives below the content area (the
        // firmware owns the band otherwise).  Re-stamp on the first
        // present and whenever the clock's minute rolls over.
        if self.self_panel > 0 {
            let min = panel_minute();
            if min != self.last_panel_min {
                self.last_panel_min = min;
                stamp_self_panel(s.framebuffer_mut(), self.content_bottom, self.self_panel);
            }
        }
        self.screen = Some(s);
    }

    // ── navigation / input ────────────────────────────────────────────

    /// Route one input event (keyboard commits first, then taps through the
    /// overlay or the shelf; Back closes overlays).  State-only: the caller
    /// presents afterwards (the C tap handlers draw + flush themselves).
    pub fn on_event(&mut self, ev: &InputEvent) {
        if self.drain_keyboard() {
            return; // a keyboard commit consumed this event (C draws in it)
        }
        match ev {
            InputEvent::KeyDown { key: KeyCode::Back } => self.back(),
            // Home is a no-op while foregrounded (C eh_evt_keypress: the
            // taskmanager handles it; closing here would read as a crash).
            InputEvent::KeyDown { key: KeyCode::Home } => {}
            // Page-turn buttons paginate the shelf; with an overlay open
            // they fall through to the Back logic (close the topmost
            // sheet), matching the stock bookshelf (C eh_evt_keypress).
            InputEvent::KeyDown { key: key @ (KeyCode::PrevPage | KeyCode::NextPage) } => {
                if self.overlay == Overlay::None {
                    // Folder source: the browser body pages its listing
                    // (C eh_evt_keypress → eh_browse_page).
                    if self.source == Source::Folder && self.browser.open {
                        let dir = match key {
                            KeyCode::NextPage => 1,
                            _ => -1,
                        };
                        crate::local::browse_page(self, dir);
                        return;
                    }
                    let target = match key {
                        KeyCode::NextPage => self.page + 1,
                        _ => self.page.saturating_sub(1),
                    };
                    if target < self.pages {
                        self.goto_page(target);
                    }
                } else {
                    self.back();
                }
            }
            InputEvent::PointerDown { x, y } => {
                self.press_pos = Some((*x, *y));
                self.press_start = Some(std::time::Instant::now());
                self.drag_y = Some(*y);
                self.drag_total = 0;
            }
            InputEvent::PointerMove { x, y } => {
                // Launcher vertical drag: move the scroll offset with the
                // finger (C eh_launcher drag), clamped to the body.
                if self.overlay == Overlay::Launcher {
                    if let (Some(prev), Some(_)) = (self.drag_y, self.press_start) {
                        let dy = prev - *y;
                        self.drag_total += dy;
                        let max = (self.launcher_body_h - self.launcher_view_h).max(0);
                        let new = (self.launcher_scroll + dy).clamp(0, max);
                        if new != self.launcher_scroll {
                            self.launcher_scroll = new;
                            self.dirty = true;
                        }
                    }
                }
                self.drag_y = Some(*y);
                let _ = x;
            }
            InputEvent::PointerUp { x, y } => {
                let (x, y) = (*x, *y);
                // Long-press on the shelf → context menu (C eh_long_press).
                let is_long = match (self.press_pos, self.press_start) {
                    (Some((px, py)), Some(t0)) => {
                        let moved = (x - px).abs() > 24 || (y - py).abs() > 24;
                        let held = t0.elapsed() >= std::time::Duration::from_millis(450);
                        !moved && held
                    }
                    _ => false,
                };
                self.press_pos = None;
                self.press_start = None;
                if self.overlay == Overlay::None && is_long && self.tab == Tab::Library && self.long_press_at(x, y) {
                    return;
                }
                // A drag (moved > 48px) is not a tap.
                let dragged = self.drag_total.abs() > 48;
                self.drag_total = 0;
                self.drag_y = None;
                if dragged {
                    return;
                }
                if self.overlay == Overlay::None {
                    self.tap_screen(x, y);
                } else {
                    self.tap_overlay(x, y);
                }
            }
            // EVT_SHOW / EVT_FOREGROUND (C eh_evt_show): a full redraw —
            // the user may have been reading with the integrated reader
            // or KOReader while we were away, so refresh their progress
            // first, then repaint everything.
            InputEvent::WidgetShown => self.reload_progress(),
            _ => {}
        }
    }

    /// Consume a committed keyboard edit.  Returns true when the event that
    /// triggered this drain came from the keyboard commit and must not also
    /// be routed (the C app's commit handler draws immediately, so the tap
    /// that closed the keyboard never reaches the screen).
    fn drain_keyboard(&mut self) -> bool {
        match kb_take_pending() {
            None => false,
            Some((KbField::ApiHost, text)) => {
                self.config.api_url = normalize_host(&text);
                self.client = ApiClient::new(&self.config.api_url, &self.config.api_token);
                self.kb_editing = None;
                self.save_config();
                self.dirty = true;
                true
            }
            Some((KbField::ApiKey, text)) => {
                self.config.api_token = text;
                self.client = ApiClient::new(&self.config.api_url, &self.config.api_token);
                self.kb_editing = None;
                self.save_config();
                self.dirty = true;
                true
            }
            Some((KbField::Search, text)) => {
                let changed = text != self.query;
                // The keyboard is closing: tear the live suggestion band
                // down (C eh_keyboard_handler: ClearTimerByName + nsuggest=0).
                self.search_kb = false;
                self.suggestions.clear();
                self.suggest_q.clear();
                if changed {
                    self.commit_search(&text);
                } else if self.tab == Tab::Search {
                    // Keyboard dismissed unchanged: redraw the bar in normal style.
                    self.refresh_shelf();
                }
                true
            }
        }
    }

    /// The 200 ms suggest tick (C suggest_debounce_tick): while the search
    /// keyboard is open, poll the live keyboard buffer — the firmware's
    /// text-change callback never fires on this build — and re-query the
    /// suggestion index only when the buffer moved.  Returns true when the
    /// band changed and a repaint is due.  The caller owns the cadence
    /// (the facade's weak timer; the C app re-arms SetWeakTimerEx here).
    pub fn tick(&mut self) -> bool {
        // Background full-library cover-warm pass (one fetch per tick).
        self.cover_warm_tick();
        // Drain a finished local-source import (C apply chain's main-thread
        // slice): replaces the 'local' source and rebuilds the view.
        crate::local::poll_import(self);
        // Drain the async sync worker (C's wkr done-callbacks + bsyncp
        // close tick): applies events to the popup state machine and
        // lands the terminal rebuild on the main thread.
        if self.sync_poll() {
            self.dirty = true; // the present() skip would swallow the update
        }
        let due = self.sync_spin_tick();
        if due {
            // The glyph rotated: the top bar needs a repaint (the facade
            // presents every tick; present() skips when not dirty).
            self.dirty = true;
        }
        if !self.search_kb || self.tab != Tab::Search {
            return due;
        }
        let Some(text) = self.screen().framebuffer().live_keyboard_text() else {
            return false;
        };
        if text == self.suggest_q {
            return false; // nothing typed since the last tick
        }
        self.suggest_q = text;
        let rows = self
            .store
            .suggest_list(&self.suggest_q, crate::app::SUGGEST_MAX_HITS)
            .unwrap_or_default();
        if rows == self.suggestions {
            return false; // buffer moved but the hits did not (C `changed` check)
        }
        self.suggestions = rows;
        // Rebuild so the band shows the new rows (or restores the history
        // list when the hits emptied); present() flushes from `dirty`.
        self.refresh_shelf();
        true
    }

    /// Advance the top-bar sync glyph rotation while a sync or download is
    /// in flight (C sync_spin_tick): 15°/s.  The facade ticks every 200 ms,
    /// so +3° per active tick matches the C cadence; returns true when the
    /// angle moved and the top bar needs a repaint.
    fn sync_spin_tick(&mut self) -> bool {
        if !(self.syncing || self.downloader.pending > 0) {
            self.sync_angle = 0; // nothing in flight — the glyph rests
            return false;
        }
        self.sync_angle = (self.sync_angle + 3) % 360;
        true
    }

    /// Back (hardware key): close the topmost overlay; on the search tab
    /// leave search keeping the active query filter (C: 'the grid stays
    /// filtered').
    fn back(&mut self) {
        if self.overlay != Overlay::None {
            self.set_overlay(Overlay::None);
            self.menu_rows.clear();
            self.settings_rows.clear();
            self.launcher_rects.clear();
            self.source_rows.clear();
            self.context_rects.clear();
            self.context_items.clear();
            self.context_book = None;
            return;
        }
        // Drilled into a group: pop the drill level first.
        if self.drill > 0 {
            self.drill_back();
            return;
        }
        // Folder source: Back ascends one level; at the browser root it
        // falls through (C eh_browse_up's "caller decides" contract).
        if self.source == Source::Folder && self.browser.open && crate::local::browse_up(self) {
            return;
        }
        if self.tab == Tab::Search {
            self.leave_search();
        }
    }

    /// Shelf tap routing (C eh_hit_top_bar / eh_hit_pager /
    /// eh_hit_thumbnail), sharing the shell's taffy geometry: widget 0 is
    /// the top bar, the last widget the pager, the rest are the covers
    /// (or, on the search tab, the input row + history rows).
    fn tap_screen(&mut self, x: i32, y: i32) {
        // System-bar tap (C eh_pu_handle_chrome_system): any tap in the
        // status-strip band below the content area hands the tap to the
        // firmware control panel.
        if y >= self.content_bottom as i32 {
            crate::logger::log("[bookshelf] system bar tapped -> control panel");
            self.screen().framebuffer().open_control_panel();
            return;
        }
        let topbar = self.screen().widget_rect(0);
        let last = self.screen().widgets.len().saturating_sub(1);
        let pager = self.screen().widget_rect(last);

        if y >= topbar.y as i32 && y < topbar.y as i32 + topbar.h as i32 {
            self.tap_top_bar(x, y);
            return;
        }
        if y >= pager.y as i32 && y < pager.y as i32 + pager.h as i32 {
            self.tap_pager(x, y, pager);
            return;
        }
        // Folder source: the browser owns the body (C eh_on_tap_browse).
        if self.source == Source::Folder && self.browser.open {
            crate::local::tap_browse(self, x, y);
            return;
        }
        if self.tab == Tab::Search {
            self.tap_search_body(x, y);
            return;
        }
        // Forward widget indices 2..last are the cover tiles (0 = top bar,
        // 1 = grid container, last = pager).  The C hit-test walks tiles
        // top-to-bottom; tap_cover maps the widget index to the entry.
        let hit = {
            let n = self.screen().widgets.len();
            let mut h: Option<usize> = None;
            for fwd in 2..n.saturating_sub(1) {
                if self.screen().widgets[fwd].hit(x, y) {
                    h = Some(fwd);
                    break;
                }
            }
            h
        };
        if let Some(i) = hit {
            self.tap_cover(i);
        }
    }

    /// Top bar zones (C eh_hit_top_bar + eh_hit_top_bar_right).  Left box:
    /// back (search / drilled) or no-op.  Source button opens the chooser.
    /// Right stack, in the C order from the corner: menu(3) / sync(2) /
    /// layout(7) / search(5).
    fn tap_top_bar(&mut self, x: i32, _y: i32) {
        use crate::appui::{BTN_PAD, BTN_SIZE, SOURCE_BTN_X, SOURCE_BTN_W};
        let r = self.screen().widget_rect(0);
        let w = r.w as i32;
        // Left button: back chevron (search / drilled) or house.
        if x >= BTN_PAD as i32 && x < (BTN_PAD + BTN_SIZE) as i32 {
            if self.tab == Tab::Search {
                self.leave_search();
            } else if self.drill > 0 {
                // While drilled the house is replaced by the back
                // chevron; tapping it pops one drill level (C eh_drill_back).
                self.drill_back();
            }
            return;
        }
        if self.tab == Tab::Search {
            return; // search bar has no other zones
        }
        // Source button.
        if (SOURCE_BTN_X..SOURCE_BTN_X + SOURCE_BTN_W).contains(&x) {
            self.set_overlay(Overlay::Source);
            return;
        }
        // Right stack (w - pad - k*btn for k=4,3,2,1 → search/layout/sync/menu).
        if x >= w - (BTN_PAD + 4 * BTN_SIZE) as i32 && x < w - (BTN_PAD + 3 * BTN_SIZE) as i32 {
            self.enter_search();
        } else if x >= w - (BTN_PAD + 3 * BTN_SIZE) as i32 && x < w - (BTN_PAD + 2 * BTN_SIZE) as i32 {
            self.toggle_layout();
        } else if x >= w - (BTN_PAD + 2 * BTN_SIZE) as i32 && x < w - (BTN_PAD + BTN_SIZE) as i32 {
            self.do_sync();
        } else if x >= w - (BTN_PAD + BTN_SIZE) as i32 && x < w - BTN_PAD as i32 {
            self.set_overlay(Overlay::More);
        }
    }

    /// Open the Search sub-page (C top-bar search-icon tap, which==5).
    fn enter_search(&mut self) {
        self.tab = Tab::Search;
        self.page = 0;
        self.refresh_shelf();
    }

    /// Leave Search back to the library shelf, keeping the query filter.
    /// A still-open keyboard is cancelled first (C eh_evt_back_search_drill:
    /// CloseKeyboard, then the tab switch; the handler tears the band down).
    fn leave_search(&mut self) {
        if self.search_kb {
            self.screen()
                .framebuffer_mut()
                .cancel_keyboard();
            // The cancelled keyboard never delivers a commit, so drain the
            // band state here (the C handler's teardown).
            self.search_kb = false;
            self.suggestions.clear();
            self.suggest_q.clear();
        }
        self.tab = Tab::Library;
        self.page = 0;
        self.refresh_shelf();
    }

    /// Toggle grid / list view (C layout icon, which==7); resets to page 0.
    fn toggle_layout(&mut self) {
        self.view_mode = if self.view_mode == ViewMode::Grid { ViewMode::List } else { ViewMode::Grid };
        self.page = 0;
        self.refresh_shelf();
    }

    /// Manual library sync (C top-bar sync icon, which==2 → eh_do_sync +
    /// eh_sync_popup_open).  While a sync is already in flight a tap just
    /// re-opens the sheet over the live run (C eh_sync_popup_open keeps
    /// the running counters).
    pub(crate) fn do_sync(&mut self) {
        if self.syncing {
            self.sync_popup_open();
            return;
        }
        crate::logger::log("[bookshelf] do_sync ENTER");
        self.start_sync(true);
    }

    /// Silent re-sync used by settings_apply / the source chooser (C calls
    /// eh_do_sync directly there — no progress sheet).
    pub(crate) fn resync(&mut self) {
        if self.syncing {
            return;
        }
        crate::logger::log("[bookshelf] do_sync ENTER");
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
    fn start_sync(&mut self, popup: bool) {
        // Initial anti-suspend ban (C eh_do_sync's eh_sync_keep_awake);
        // per-round re-arms come back as SyncMsg::BanSleep.
        self.screen()
            .framebuffer()
            .ban_sleep(crate::sync::EH_SYNC_BAN_SLEEP_SEC as u32);
        let (tx, rx) = std::sync::mpsc::channel::<crate::sync::SyncMsg>();
        self.sync_rx = Some(rx);
        self.sync_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.syncing = true;
        if popup {
            self.sync_popup_open();
        }
        let client = self.client.clone();
        let db_path = self.db_path.clone();
        let cancel = std::sync::Arc::clone(&self.sync_cancel);
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
                    50,
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
            self.sync_rx = None;
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
        self.sync_cancel.store(true, Ordering::Relaxed);
        self.sync_rx = None;
        self.syncing = false;
    }

    /// Open the sync-progress sheet (C eh_sync_popup_open).
    fn sync_popup_open(&mut self) {
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
    fn sync_poll(&mut self) -> bool {
        let msgs: Vec<crate::sync::SyncMsg> =
            self.sync_rx.as_ref().map_or_else(Vec::new, |rx| rx.try_iter().collect());
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
    fn apply_sync_event(&mut self, ev: crate::sync::SyncEvent) -> bool {
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
    fn finish_sync(&mut self, ok: bool) -> bool {
        self.syncing = false;
        self.sync_rx = None;
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
    fn sync_popup_close_tick(&mut self) -> bool {
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
    fn warm_progress(&self) -> (u32, u32) {
        let total = self.warm_total as u32;
        (total.saturating_sub(self.warm_queue.len() as u32), total)
    }

    /// Apply a committed search query (C eh_keyboard_handler non-empty
    /// branch): record it, filter the shelf, return to the library tab.
    /// Empty / unchanged text keeps the search page open (C outside-tap).
    fn commit_search(&mut self, term: &str) {
        let term = term.trim().to_string();
        if term.is_empty() || term == self.query {
            // Dismissed unedited: leave search, don't teleport home.
            return;
        }
        self.query = term.clone();
        if let Err(e) = self.store.search_add(&term) {
            crate::log(&format!("[eh_app] search_add: {e}"));
        }
        crate::logger::log(&format!("[bookshelf] search commit: query=`{term}`"));
        self.tab = Tab::Library;
        self.refresh_shelf();
    }

    /// True while the full-library warm pass still has covers to fetch
    /// (C eh_cover_warm_active); offline counts as drained — the pass is
    /// gated off offline and would otherwise pin the sheet forever.
    pub(crate) fn cover_warm_active(&mut self) -> bool {
        if self.warm_queue.is_empty() {
            return false;
        }
        // Safe from overlay draws (screen take()n during present): use
        // the live probe when available, else the cached value.
        if self.screen.is_some() {
            let net = self.screen.as_mut().unwrap().framebuffer().net_active();
            self.fb_net_active = net;
            net
        } else {
            self.fb_net_active
        }
    }

    /// Search-tab body taps: the input row opens the keyboard; a history
    /// row re-runs that stored query (C eh_hit_search_input / history tap).
    /// While the keyboard is open (C eh_pu_handle_search_kb) a suggestion
    /// or history row tap cancels the keyboard and commits the term —
    /// CloseKeyboard() delivers no commit, so the app performs it — and
    /// any other tap above the keyboard dismisses it.
    fn tap_search_body(&mut self, x: i32, y: i32) {
        let n = self.screen().widgets.len();
        let last = n.saturating_sub(1);
        // Input row is widget index 1 (bordered box inset like its draw).
        // With the keyboard already open a tap here dismisses it (C:
        // outside-band branch), it never re-opens.
        if !self.search_kb && n > 1 {
            let r = self.screen().widget_rect(1);
            if x >= r.x as i32 + 16
                && x < (r.x + r.w) as i32 - 16
                && y >= r.y as i32 + 10
                && y < (r.y + r.h) as i32 - 10
            {
                self.edit_search();
                return;
            }
        }
        // History ROWS are widget indices 3..last: index 2 is the body
        // CONTAINER (it spans the whole body, so treating it as a row
        // would swallow every tap below the input).  With the keyboard
        // open and suggestions showing, the rows parallel self.suggestions
        // (the band replaced the history list); otherwise the store's
        // newest-first history list.
        let mut hit: Option<usize> = None;
        let mut rects: Vec<Rect> = Vec::new();
        for i in 3..last {
            rects.push(self.screen().widget_rect(i));
        }
        for (i, r) in rects.iter().enumerate() {
            if r.contains(x, y) {
                hit = Some(i);
                break;
            }
        }
        if let Some(idx) = hit {
            let terms = if self.search_kb && !self.suggestions.is_empty() {
                crate::logger::log(&format!(
                    "[bookshelf] suggest tap: term=`{}`",
                    self.suggestions[idx]
                ));
                Some(self.suggestions[idx].clone())
            } else {
                self.store
                    .search_list(1000, 0)
                    .unwrap_or_default()
                    .get(idx)
                    .map(|t| {
                        crate::logger::log(&format!("[bookshelf] search history tap: query=`{t}`"));
                        t.clone()
                    })
            };
            if let Some(t) = terms {
                if self.search_kb {
                    // Cancel first: the firmware close must not deliver a
                    // commit racing ours (C CloseKeyboard + app-side commit).
                    self.search_kb = false;
                    self.suggestions.clear();
                    self.suggest_q.clear();
                    self.screen().framebuffer_mut().cancel_keyboard();
                }
                self.commit_search(&t);
            }
            return;
        }
        // Outside the rows with the keyboard open: dismiss it, staying on
        // the Search page (the bar returns to normal style).
        if self.search_kb {
            self.search_kb = false;
            self.suggestions.clear();
            self.suggest_q.clear();
            self.screen().framebuffer_mut().cancel_keyboard();
            self.refresh_shelf();
        }
    }

    /// Open the search keyboard with the current query as initial text.
    fn edit_search(&mut self) {
        use crate::app::{kb_arm, kb_commit, kb_take_pending};
        let initial = self.query.clone();
        let _ = kb_take_pending();
        kb_arm(KbField::Search);
        self.search_kb = true;
        self.suggestions.clear();
        // Reset the tick cache so the first poll acts even when the
        // pre-filled buffer matches the old query (C g_last_suggest_q[0]=0).
        self.suggest_q.clear();
        // Rebuild the search page to show the inverted input bar.
        self.refresh_shelf();
        self.screen()
            .framebuffer_mut()
            .open_keyboard("Search", &initial, kb_commit);
    }

    /// The pager's four buttons (C eh_hit_pager: -1/-3/-4/-2).  Box
    /// geometry mirrors appui::Pager's draw (x offsets from the band
    /// edges, 96×64).  Actions follow the C contract exactly:
    /// "<" prev / "<<" first / ">>" last / ">" next (eh_main.c
    /// eh_pu_handle_tail: -1/-3/-4/-2).
    fn tap_pager(&mut self, x: i32, y: i32, band: Rect) {
        let bw = 96i32;
        let bh = 64i32;
        let by = (band.y + (band.h - 64) / 2) as i32;
        let bx0 = band.x as i32;
        let bx1 = (band.x + band.w) as i32;
        let boxes = [
            (bx0 + 12, PageAction::Prev),     // "<" prev
            (bx0 + 116, PageAction::First),   // "<<" first
            (bx1 - 212, PageAction::Last),    // ">>" last
            (bx1 - 108, PageAction::Next),    // ">" next
        ];
        for (bx, action) in boxes {
            let b = Rect { x: bx as u32, y: by as u32, w: bw as u32, h: bh as u32 };
            if b.contains(x, y) {
                let target = match action {
                    PageAction::Prev => self.page.saturating_sub(1),
                    PageAction::First => 0,
                    PageAction::Last => self.pages.saturating_sub(1),
                    PageAction::Next => (self.page + 1).min(self.pages.saturating_sub(1)),
                };
                self.goto_page(target);
                return;
            }
        }
    }

    /// A cover tile tap (C eh_hit_thumbnail → eh_book_press_action).
    fn tap_cover(&mut self, idx: usize) {
        let pos = idx - 2; // [0]=top bar, [1]=grid container precede covers
        if pos < self.entries.len() {
            if self.entries[pos].stack {
                // Stack card: drill into the group (C eh_drill_card).
                let card = crate::store::ViewRow {
                    kind: 1,
                    book_id: self.entries[pos].book.id.clone(),
                    series_id: self.entries[pos].stack_scope.clone(),
                    series_name: self.entries[pos].stack_label.clone(),
                    series_count: self.entries[pos].stack_count,
                };
                self.drill_into_card(&card);
                return;
            }
            let book = self.entries[pos].book.clone();
            self.press_book(&book);
        }
    }


    /// The C app's eh_book_press_action: probe the on-disk file (both the
    /// current downloads dir AND the stored path — the folder may have
    /// moved since the fetch), persist the state, then either
    /// download-then-open or open directly.
    fn press_book(&mut self, book: &Book) {
        let downloads_dir = self
            .config
            .downloads_dir
            .clone()
            .unwrap_or_else(|| "/mnt/ext1/Downloads".to_string());
        let cur = book_local_path(book, &downloads_dir);
        let stored = PathBuf::from(&book.local_path);
        let exists = cur.is_file() || (!book.local_path.is_empty() && stored.is_file());
        if exists {
            let path = if cur.is_file() { cur } else { stored };
            if !book.downloaded || book.local_path != path.to_string_lossy() {
                if let Err(e) = self.store.set_downloaded(&book.id, true, &path.to_string_lossy()) {
                    crate::log(&format!("[eh_app] set_downloaded: {e}"));
                }
            }
            self.open_reader(&path, &book.title);
        } else {
            // Async: enqueue on the worker, show the modal popup, auto-open
            // the reader when the queue drains.
            self.dl_single = true;
            self.dl_autopen = Some((cur.to_string_lossy().into_owned(), book.title.clone()));
            self.enqueue_download(&book.id, &cur);
        }
    }

    /// The active downloads dir (C eh_resolve_downloads_dir default).
    fn downloads_dir(&self) -> String {
        self.config
            .downloads_dir
            .clone()
            .unwrap_or_else(|| "/mnt/ext1/Downloads".to_string())
    }

    /// Queue one book file on the worker + open the modal download popup
    /// (logging `draw_dl_popup` once per popup).
    fn enqueue_download(&mut self, id: &str, path: &Path) {
        let base = self.config.api_url.clone();
        let token = self.config.api_token.clone();
        self.downloader.enqueue(&base, &token, id, &path.to_string_lossy());
        if self.overlay != Overlay::Download {
            crate::logger::log("[bookshelf] draw_dl_popup");
        }
        self.set_overlay(Overlay::Download);
    }

    /// Abort every open download (C eh_cancel_downloads): void the
    /// in-flight fetch (its .part is never renamed), drop the queue +
    /// batch state, and close the popup.
    pub fn cancel_downloads(&mut self) {
        crate::logger::log("[bookshelf] cancel_downloads");
        self.downloader.cancel_all();
        self.dl_batch_queue.clear();
        self.dl_batch_failed.clear();
        self.dl_single = false;
        self.dl_batch_all = false;
        self.dl_autopen = None;
        self.set_overlay(Overlay::None);
    }

    /// Drain completed downloads into the store, and when the queue empties
    /// close the popup + auto-open the reader for a single-book press.
    fn drain_downloads(&mut self) {
        loop {
            let Some(d) = self.downloader.try_next() else { break };
            self.downloader.pending = self.downloader.pending.saturating_sub(1);
            // The popup shows the remaining count: repaint it.
            self.dirty = true;
            if d.ok {
                self.dl_done += 1;
                if let Err(e) = self.store.set_downloaded(&d.id, true, &d.path) {
                    crate::log(&format!("[eh_app] set_downloaded: {e}"));
                }
                crate::logger::log(&format!("[bookshelf] download_book_file OK id={} path={}", d.id, d.path));
            } else {
                self.dl_failed += 1;
                crate::logger::log(&format!("[bookshelf] download_book_file FAILED id={}", d.id));
            }
            if self.dl_batch_all {
                crate::logger::log(&format!(
                    "[bookshelf] dl_progress done={} failed={} total={} active={}",
                    self.dl_done, self.dl_failed, self.dl_total, self.downloader.pending
                ));
                if !d.ok {
                    // C batch_note_failed: the top-up never re-enqueues a
                    // book this batch already tried and failed.
                    self.dl_batch_failed.insert(d.id.clone());
                }
                // Top the bounded queue up as jobs finish (C dl_advance).
                self.top_up_batch();
            }
        }
        if self.downloader.pending == 0 && self.dl_batch_queue.is_empty() && self.overlay == Overlay::Download {
            if self.dl_single {
                // Single-book press: close the popup + auto-open the reader.
                self.set_overlay(Overlay::None);
                if let Some((path, title)) = self.dl_autopen.take() {
                    let path = PathBuf::from(path);
                    self.open_reader(&path, &title);
                }
                self.dl_single = false;
            } else {
                // Download-all / context Download: the popup stays open
                // (modal) until an outside tap dismisses it (C behavior).
                if self.dl_batch_all {
                    crate::logger::log("[bookshelf] download-all batch complete");
                    // The finished-tally popup redraw (the harness proves
                    // the popup survived the mid-drain tap via this token).
                    crate::logger::log("[bookshelf] draw_dl_popup");
                    self.dl_batch_all = false;
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
    fn download_all(&mut self) {
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
        self.dl_single = false;
        self.dl_batch_all = true;
        self.dl_done = 0;
        self.dl_failed = 0;
        self.dl_total = targets.len();
        self.dl_autopen = None;
        // New batch: drop the previous batch's failed-id set and stage the
        // targets for the bounded top-up (C download_all_start).
        self.dl_batch_failed.clear();
        self.dl_batch_queue = targets.into_iter().collect();
        self.top_up_batch();
        crate::logger::log(&format!("[bookshelf] download-all queued={}", self.dl_total));
        crate::logger::log("[bookshelf] draw_dl_popup");
        self.set_overlay(Overlay::Download);
    }

    /// In-flight window of the download-all batch (C keeps its whole
    /// queue bounded by EH_MAX_DOWNLOADS; the Rust worker channel is
    /// unbounded, so the window lives here).
    const DL_BATCH_WINDOW: usize = 8;

    /// Bounded download-all top-up (C dl_advance_batch →
    /// batch_enqueue_slice): keep DL_BATCH_WINDOW jobs queued/in flight,
    /// pulling staged undownloaded books and skipping ids this batch
    /// already failed (C batch_note_failed / batch_failed_id).
    fn top_up_batch(&mut self) {
        let dl = self.downloads_dir();
        while self.downloader.pending < Self::DL_BATCH_WINDOW {
            match self.dl_batch_queue.pop_front() {
                Some(b) => {
                    if self.dl_batch_failed.contains(&b.id) {
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

    /// A long-press at (x, y): if it lands on a book tile, open the context
    /// menu (C eh_long_press → eh_context).  Returns true when opened.
    fn long_press_at(&mut self, x: i32, y: i32) -> bool {
        let topbar = self.screen().widget_rect(0);
        let last = self.screen().widgets.len().saturating_sub(1);
        let pager = self.screen().widget_rect(last);
        if y < topbar.y as i32 || y >= pager.y as i32 {
            return false;
        }
        for (i, w) in self.screen().widgets.iter().enumerate().skip(1).take(last.saturating_sub(1)) {
            if w.hit(x, y) {
                let pos = i - 2; // widget 0 = topbar, 1 = grid container
                if pos < self.entries.len() {
                    if self.entries[pos].stack {
                        // A stack card long-press opens the SERIES context
                        // (Download all / Delete series).
                        let scope = self.entries[pos].stack_scope.clone();
                        let label = self.entries[pos].stack_label.clone();
                        let count = self.entries[pos].stack_count;
                        self.open_context_series(&scope, &label, count);
                        return true;
                    }
                    let book = self.entries[pos].book.clone();
                    self.open_context_book(&book);
                    return true;
                }
                return false;
            }
        }
        false
    }

    /// Open the series context menu (Download all / Delete series) for a
    /// stack card (C eh_context series branch).
    fn open_context_series(&mut self, scope: &str, label: &str, count: i64) {
        self.context_items = vec![ContextAction::DownloadAll, ContextAction::DeleteAll];
        self.context_series = 1;
        self.context_scope = scope.to_string();
        self.context_label = label.to_string();
        self.context_count = count;
        crate::logger::log("[bookshelf] context menu open series=1");
        self.set_overlay(Overlay::Context);
    }

    /// Open the book context menu (Open/Download/Delete).
    fn open_context_book(&mut self, book: &Book) {
        self.context_items = vec![ContextAction::Open, ContextAction::Download, ContextAction::Delete];
        self.context_series = 0;
        self.context_book = Some(book.clone());
        crate::logger::log("[bookshelf] context menu open series=0");
        self.set_overlay(Overlay::Context);
    }

    /// A context-menu row tap (C eh_context_item_handler).
    fn tap_context(&mut self, x: i32, y: i32) {
        for (i, r) in self.context_rects.iter().enumerate() {
            if r.contains(x, y) {
                if let Some(action) = self.context_items.get(i).copied() {
                    self.context_rects.clear();
                    self.context_items.clear();
                    let book = self.context_book.take();
                    self.set_overlay(Overlay::None);
                    match action {
                        ContextAction::Open => {
                            if let Some(b) = book {
                                self.press_book(&b);
                            }
                        }
                        ContextAction::Download => {
                            if let Some(b) = book {
                                let cur = book_local_path(&b, &self.downloads_dir());
                                self.dl_single = false;
                                self.enqueue_download(&b.id, &cur);
                            }
                        }
                        ContextAction::Delete => {
                            if let Some(b) = book {
                                self.delete_book(&b);
                            }
                        }
                        ContextAction::DownloadAll => {
                            let (scope, label) = (self.context_scope.clone(), self.context_label.clone());
                            let count = self.context_count;
                            self.context_scope.clear();
                            self.context_label.clear();
                            self.download_series(&scope, &label, count);
                        }
                        ContextAction::DeleteAll => {
                            let scope = self.context_scope.clone();
                            self.context_scope.clear();
                            self.delete_series(&scope);
                        }
                    }
                    self.refresh_shelf();
                }
                return;
            }
        }
        // Tap outside the sheet → dismiss.
        self.context_rects.clear();
        self.context_items.clear();
        self.context_book = None;
        self.set_overlay(Overlay::None);
        self.refresh_shelf();
    }

    /// Remove a downloaded book's local file (C eh_context Delete).
    fn delete_book(&mut self, book: &Book) {
        let dl = self.downloads_dir();
        let cur = book_local_path(book, &dl);
        let removed = std::fs::remove_file(&cur).is_ok()
            || (!book.local_path.is_empty() && std::fs::remove_file(&book.local_path).is_ok());
        if let Err(e) = self.store.set_downloaded(&book.id, false, "") {
            crate::log(&format!("[eh_app] set_downloaded: {e}"));
        }
        if removed {
            crate::logger::log(&format!("[bookshelf] delete_book_file removed path={}", cur.display()));
        } else {
            crate::log(&format!("[eh_app] delete_book_file missing path={}", cur.display()));
        }
    }

    /// Download every book of a series (C eh_context Download all): queue
    /// the scope's books on the worker + open the modal popup.
    fn download_series(&mut self, scope: &str, _label: &str, _count: i64) {
        let books = self
            .store
            .list_sorted(crate::store::SortMode::Recent, "", 1, scope)
            .unwrap_or_default();
        crate::logger::log(&format!("[bookshelf] download_series scope={scope} queued={}", books.len()));
        let dl = self.downloads_dir();
        for b in &books {
            let cur = book_local_path(b, &dl);
            // Via enqueue_download so an id already queued/in flight is a
            // dedup no-op (C eh_find_download guard), not a double fetch.
            self.enqueue_download(&b.id, &cur);
        }
        crate::logger::log("[bookshelf] draw_dl_popup");
        self.set_overlay(Overlay::Download);
        self.dl_single = false;
        self.dl_batch_all = false;
        self.dl_autopen = None;
    }

    /// Delete every downloaded file of a series (C eh_context Delete
    /// series).
    fn delete_series(&mut self, scope: &str) {
        let books = self
            .store
            .list_sorted(crate::store::SortMode::Recent, "", 1, scope)
            .unwrap_or_default();
        crate::logger::log(&format!("[bookshelf] delete_series scope={scope} books={}", books.len()));
        for b in &books {
            self.delete_book(b);
        }
    }

    /// Launch the reader (C eh_launch_reader → eh_plat_launch_reader).
    /// The standard reader — and the auto default — goes through
    /// OpenBook(), the firmware's canonical book-open path; only an
    /// explicitly selected third-party reader (KOReader) is exec'd via
    /// launch_app with the book path as argv[1] (argv[0] must be the
    /// program path: the task launcher passes args through as-is).
    pub(crate) fn open_reader(&mut self, path: &Path, title: &str) {
        let path_str = path.to_string_lossy().to_string();
        let readers = detect_readers();
        // C eh_launch_reader: the configured preference resolves against
        // the detected readers list before the firmware OpenBook fallback.
        let reader_path = if self.reader_pref > 0 && (self.reader_pref as usize) <= readers.len() {
            Some(readers[self.reader_pref as usize - 1])
        } else {
            None
        };
        let ok = match reader_path {
            Some(rp) if rp != STANDARD_READER => {
                crate::logger::log(&format!(
                    "[bookshelf] launching reader app={} path={}",
                    rp.rsplit('/').next().unwrap_or(rp),
                    path.display()
                ));
                crate::log(&format!("[eh_app] opening reader app={rp} path={}", path.display()));
                self.screen()
                    .framebuffer_mut()
                    .launch_app(rp, title, &[path_str.clone()])
            }
            _ => {
                crate::logger::log(&format!(
                    "[bookshelf] launching reader via OpenBook: {}",
                    path.display()
                ));
                crate::log(&format!("[eh_app] opening reader path={}", path.display()));
                self.screen().framebuffer_mut().open_book(&path_str, title)
            }
        };
        if !ok {
            /*
             * Same hourglass intent as the launcher (C eh_launch_reader):
             * the reader draws over it once it becomes the foreground
             * task, so a slow reader start reads as work-in-progress
             * instead of a dead tap.  On launch failure no reader will
             * ever draw over it — the C app hides the hourglass and
             * repaints the shelf; this port has no hourglass overlay to
             * drop, so we close the sheet WITHOUT a redraw and let the
             * next present bring the shelf back.
             */
            crate::log("[eh_app] reader launch failed");
            self.overlay = Overlay::None;
        }
    }

    // ── shelf state ───────────────────────────────────────────────────

    /// The offered group-chooser presets (C eh_view_dim_available), in the
    /// harness's row order: None, Author>Series, Series, Author, Year,
    /// Genre, minus dims the store has no values for.
    fn group_offer(&self) -> Vec<crate::store::GroupPreset> {
        let (a, s, y, g) = self.store.dim_availability().unwrap_or((true, false, true, true));
        use crate::store::GroupPreset;
        let mut out = vec![GroupPreset::None];
        if a && s {
            out.push(GroupPreset::AuthorSeries);
        }
        if a {
            out.push(GroupPreset::Author);
        }
        if s {
            out.push(GroupPreset::Series);
        }
        if y {
            out.push(GroupPreset::Year);
        }
        if g {
            out.push(GroupPreset::Genre);
        }
        out
    }

    /// Rebuild the materialised view for the active group/sort/drill and
    /// log the C `view_rebuild: view=… sort=… group=… drill=…` marker.
    pub(crate) fn rebuild_view(&mut self) {
        let (group, sort, drill, q) = (self.group, self.sort, self.drill, self.query.clone());
        let total = {
            let scopes = self.drill_scopes();
            self.store
                .view_rebuild(group as i64, sort as i64, drill as i64, &q, &scopes)
                .unwrap_or(0)
        };
        crate::logger::log(&format!(
            "[bookshelf] view_rebuild: view={} sort={} group={} drill={}",
            total, sort as i64, group as i64, drill
        ));
        self.dirty = true;
        self.refresh_shelf();
    }

    /// Open the Group by chooser sheet.
    fn open_group_chooser(&mut self) {
        self.chooser_rects.clear();
        self.set_overlay(Overlay::GroupChooser);
    }

    /// Open the Sort by chooser sheet.
    fn open_sort_chooser(&mut self) {
        self.chooser_rects.clear();
        self.set_overlay(Overlay::SortChooser);
    }

    /// A chooser-sheet row (or outside) tap: apply the choice, rebuild the
    /// view, close.  Outside the sheet dismisses (C sheet behaviour).
    fn tap_chooser(&mut self, x: i32, y: i32, kind: ChooserKind) {
        for (i, r) in self.chooser_rects.iter().enumerate() {
            if r.contains(x, y) {
                match kind {
                    ChooserKind::Group => {
                        let offer = self.group_offer();
                        if let Some(g) = offer.get(i) {
                            self.group = *g;
                            self.drill = 0;
                            self.drill_values = Default::default();
                            self.drill_names = Default::default();
                            self.rebuild_view();
                        }
                    }
                    ChooserKind::Sort => {
                        let mode = match i {
                            1 => crate::store::SortMode::Author,
                            2 => crate::store::SortMode::Series,
                            3 => crate::store::SortMode::Recent,
                            _ => crate::store::SortMode::Title,
                        };
                        self.sort = mode;
                        self.rebuild_view();
                    }
                }
                self.chooser_rects.clear();
                self.set_overlay(Overlay::None);
                return;
            }
        }
        // Tap outside the sheet → dismiss.
        self.chooser_rects.clear();
        self.set_overlay(Overlay::None);
    }

    /// The pinned drill scopes for the store, level 0..drill (C
    /// eh_g_drill_values[0..eh_g_drill_level]).
    fn drill_scopes(&self) -> Vec<&str> {
        self.drill_values[..self.drill as usize].iter().map(String::as_str).collect()
    }

    /// Drill into a tapped stack card (C eh_group_drill): record the
    /// group's value at the next drill level, so the shelf regroups within
    /// that group (or shows flat books at the preset's last level), and
    /// remember the page of the level we're leaving so drill-back lands
    /// back where they were.
    fn drill_into_card(&mut self, view_row: &crate::store::ViewRow) {
        const MAX_LEVELS: u32 = 2; // C EH_GROUP_MAX_LEVELS (Author -> Series)
        if self.drill >= MAX_LEVELS {
            return;
        }
        let lvl = self.drill as usize;
        self.drill_saved_pages[lvl] = self.page;
        self.drill_values[lvl] = view_row.series_id.clone();
        self.drill_names[lvl] = view_row.series_name.clone();
        self.drill += 1;
        self.page = 0;
        self.rebuild_view();
    }

    /// Back: pop the drill level (C eh_group_drill_back), restoring the
    /// saved page of the level we return into, so back from a deep drill
    /// continues where the user left off.
    fn drill_back(&mut self) {
        if self.drill > 0 {
            self.drill -= 1;
            let lvl = self.drill as usize;
            self.drill_values[lvl].clear();
            self.drill_names[lvl].clear();
            self.page = self.drill_saved_pages[lvl];
            self.rebuild_view();
        }
    }

    /// Resolve the reader preference from the config at boot (C
    /// eh_reader_pref_from_path) + log the C `reader_pref=N (cfg \`path\`)`
    /// marker the persist test greps for.
    fn resolve_reader(&mut self) {
        let readers = detect_readers();
        let cfg = self.config.reader.clone().unwrap_or_default();
        let pref = reader_pref_from_path(&cfg, &readers);
        self.reader_pref = pref;
        self.reader_path = if pref > 0 {
            readers[pref as usize - 1].to_string()
        } else {
            "auto".to_string()
        };
        crate::logger::log(&format!("[bookshelf] reader_pref={pref} (cfg `{cfg}`)"));
    }

    /// Cycle the reader preference (C eh_input.c reader row tap): Auto →
    /// each detected reader in offer order → Auto.
    pub fn cycle_reader(&mut self) {
        let readers = detect_readers();
        self.reader_pref = (self.reader_pref + 1) % (readers.len() as i32 + 1);
        self.apply_reader_pref(&readers);
        self.dirty = true;
        crate::logger::log(&format!("[bookshelf] reader_pref={}", self.reader_pref));
    }

    /// Persist + log the current preference against `readers` (the label
    /// shown in the settings row comes from [`App::reader_label`]).
    fn apply_reader_pref(&mut self, readers: &[&str]) {
        if self.reader_pref > 0 && (self.reader_pref as usize) <= readers.len() {
            let path = readers[self.reader_pref as usize - 1];
            self.config.reader = Some(path.to_string());
            self.reader_path = path.to_string();
        } else {
            // A stale index (the reader was uninstalled) falls back to Auto.
            self.reader_pref = 0;
            self.config.reader = None;
            self.reader_path = "auto".to_string();
        }
    }

    /// The settings row's value for the reader preference (C
    /// eh_settings_reader_label): the selected reader's label, or Auto.
    pub fn reader_label(&self) -> String {
        if self.reader_pref > 0 {
            let readers = detect_readers();
            if let Some(p) = readers.get(self.reader_pref as usize - 1) {
                return reader_label_of(p).to_string();
            }
        }
        crate::i18n::tr("settings.reader_auto").to_string()
    }

    /// Change the active overlay, marking the frame dirty (the present
    /// skip must repaint when the overlay changes).
    fn set_overlay(&mut self, o: Overlay) {
        if o != self.overlay {
            // Leaving the sync sheet retires its state machine (the C
            // popup flag lives in eh_g_state; ours rides the overlay).
            if self.overlay == Overlay::Sync {
                self.sync_popup.open = false;
            }
            self.dirty = true;
        }
        self.overlay = o;
    }

    /// The shelf page size for the current view mode + panel width.  Grid
    /// uses the C mode-aware grid dims (3×2 on the standard panel); list is
    /// always 1 column of fixed-height rows that fit above the pager.
    fn page_size(&self, width: u32) -> usize {
        match self.view_mode {
            ViewMode::List => {
                let band = (self.content_bottom as i32 - TOP_BAR_H as i32
                    - crate::appui::TOP_BAR_PAD as i32
                    - PAGER_H as i32 - 8)
                    .max(1) as u32;
                (band / shelf::LIST_ROW_H).max(1) as usize
            }
            ViewMode::Grid => {
                let g = shelf::grid_geom(width, self.content_bottom);
                (g.cols * g.rows) as usize
            }
        }
    }

    /// The centered top-bar title (C top_bar_title): the deepest drilled
    /// series/group name, the query on a filtered shelf, else nothing.
    fn top_title(&self) -> &str {
        for name in self.drill_names[..self.drill as usize].iter().rev() {
            if !name.is_empty() {
                return name;
            }
        }
        if self.query.is_empty() { "" } else { &self.query }
    }

    /// Re-read the reading-progress map from the firmware explorer db and
    /// repaint the shelf (the C eh_evt_show → eh_progress_reload flow).
    /// Public so lifecycle plumbing (EVT_SHOW/FOREGROUND delivery) can
    /// drive it too.
    pub fn reload_progress(&mut self) {
        self.progress = crate::progress::reload();
        self.refresh_shelf();
    }

    /// Rebuild the shelf at the current page (the caller presents).
    pub fn refresh_shelf(&mut self) {
        self.dirty = true;
        // Take the framebuffer out first: the new screen is built from the
        // same canvas (the C app's full-redraw navigation).
        let fb = self.screen.take().expect("screen present").into_framebuffer();
        let width = fb.screen().width;
        let mut screen = if self.tab == Tab::Search {
            self.build_search_page(fb, width)
        } else {
            self.build_library_page(fb, width)
        };
        screen.content_h = self.content_bottom;
        self.screen = Some(screen);
        // C draw_grid marker (the e2e harness's wait-for-grid token) with
        // the projected tile total — LIBRARY only: the C Search page logs
        // draw_search_tab instead, and the harness reads a draw_grid in a
        // search-invocation slice as "jumped to the library".
        if self.tab == Tab::Library {
            let sw = self.screen().framebuffer().screen().width;
            let view = self.view_total_books();
            crate::logger::log(&format!(
                "[bookshelf] draw_grid view={view} page={} cell={}x0 top=96 bot={}",
                self.page, sw, self.content_bottom
            ));
        }
        crate::log(&format!(
            "[eh_app] shelf page={}/{} entries={}",
            self.page + 1,
            self.pages,
            self.entries.len()
        ));
    }

    /// The library shelf (grid or list) at the current page.
    fn build_library_page(&mut self, fb: B, width: u32) -> Screen<B> {
        // Folder source: the directory browser IS the shelf body
        // (C BR_MODE_BROWSER); the top bar carries the current path.
        if self.source == Source::Folder && self.browser.open {
            self.pages = 1;
            self.entries.clear();
            let browser = std::mem::take(&mut self.browser);
            let screen = crate::local::build_browse_page(fb, &browser, self.content_bottom);
            self.browser = browser;
            return screen;
        }
        let per = self.page_size(width);
        let total = self.view_total_books();
        self.pages = if total == 0 { 1 } else { total.div_ceil(per) };
        if self.page >= self.pages {
            self.page = self.pages.saturating_sub(1);
        }
        self.entries = self.store_view_page(per, self.page * per);
        let page = self.page;
        let pages = self.pages;
        let content_bottom = self.content_bottom;
        let title = self.top_title().to_string();
        let (view_mode, source, syncing, drilled, sync_angle) =
            (self.view_mode, self.source, self.syncing, self.drill > 0, self.sync_angle);
        shelf::build_shelf(
            fb,
            &title,
            page,
            pages,
            &self.entries,
            content_bottom,
            view_mode,
            drilled, // back chevron when drilled into a group
            source,  // source
            false,   // not the search tab
            syncing,
            sync_angle,
        )
    }

    /// Tile count the shelf pages over: the materialised view when one is
    /// present, else the library count (the C eh_view_total).
    fn view_total_books(&self) -> usize {
        let vt = self.store.view_total();
        if vt > 0 {
            vt
        } else {
            self.store.count().unwrap_or(0) as usize
        }
    }

    /// One page of shelf entries from the materialised view.  A stack card
    /// (kind 1) is paired with its representative book so covers/drills
    /// keep working; flat tiles map to their book.
    fn store_view_page(&mut self, per: usize, offset: usize) -> Vec<ShelfEntry> {
        let rows = self.store.view_page(per, offset).unwrap_or_default();
        rows.into_iter()
            .map(|v| {
                let book = self.store.get_book(&v.book_id).ok().flatten().unwrap_or_default();
                let art = cover::load_cached(&self.covers_dir, &book.id)
                    .and_then(|bytes| cover::decode_rgb(&bytes).ok())
                    .map(|(w, h, rgb)| (rgb, w, h));
                if art.is_some() {
                    crate::logger::log(&format!("[bookshelf] cover_tick cache hit id={}", book.id));
                }
                let stack = v.kind == 1;
                let scope = if stack { v.series_id.clone() } else { String::new() };
                let progress = crate::progress::percent(&self.progress, &book.local_path);
                ShelfEntry { book, art, stack, stack_label: v.series_name, stack_count: v.series_count, stack_scope: scope, progress }
            })
            .collect()
    }

    /// The Search sub-page at the current page (input row + history).
    fn build_search_page(&mut self, fb: B, width: u32) -> Screen<B> {
        let _ = width;
        // History rows per page: the C eh_history_pagesize formula.
        let rows_per = ((self.content_bottom as i32 - PAGER_H as i32
            - TOP_BAR_H as i32
            - crate::appui::TOP_BAR_PAD as i32
            - 88)
            / 96)
            .max(1) as usize;
        let total = self.store.search_count().unwrap_or(0) as usize;
        self.pages = if total == 0 { 1 } else { total.div_ceil(rows_per) };
        if self.page >= self.pages {
            self.page = self.pages.saturating_sub(1);
        }
        let offset = self.page * rows_per;
        crate::logger::log("[bookshelf] draw_search_tab");
        let history = self.store.search_list(rows_per, offset).unwrap_or_default();
        // While the keyboard is open with hits, the suggestion band
        // replaces the history list (C suggest_debounce_tick →
        // eh_draw_suggestions); empty hits keep the history visible.
        let using_suggestions = self.search_kb && !self.suggestions.is_empty();
        let rows = if using_suggestions { &self.suggestions } else { &history };
        let (page, pages, query, content_bottom, syncing) =
            (self.page, self.pages, self.query.clone(), self.content_bottom, self.syncing);
        shelf::build_search(fb, &query, page, pages, rows, content_bottom, syncing, self.search_kb)
    }

    /// Flip to `page` (clamped): fetch the page's covers into the cache
    /// first (C cover-warm pass), then rebuild.
    pub fn goto_page(&mut self, page: usize) {
        if page >= self.pages || page == self.page {
            return;
        }
        self.page = page;
        let width = self.screen().framebuffer().screen().width;
        let per = self.page_size(width);
        let books = if self.query.is_empty() {
            self.store.list_books(per, page * per).unwrap_or_default()
        } else {
            self.store.search(&self.query, per, page * per).unwrap_or_default()
        };
        // C cover-warm pass — network-gated: an offline flip renders the
        // cached covers only (no remote fetches, C eh_plat_net_active).
        if self.screen().framebuffer().net_active() {
            for b in &books {
                let _ = cover::fetch(&self.client, &self.covers_dir, &b.id);
            }
        }
        self.refresh_shelf();
    }

    /// Save the settings screen's edits to the config file (C
    /// eh_save_config_file after the Save button / a keyboard commit).
    pub fn save_config(&mut self) {
        if let Some(p) = &self.cfg_path {
            if let Err(e) = self.config.save(p) {
                crate::log(&format!("[eh_app] config save failed: {e}"));
            } else {
                crate::log(&format!("[eh_app] settings: saved {}", p.display()));
                crate::logger::log(&format!(
                    "[bookshelf] settings: reader_pref={} (cfg `{}`)",
                    self.reader_pref,
                    self.reader_path,
                ));
            }
        }
    }

    /// The Save button's full side-effect chain (C eh_settings_apply):
    /// persist, rebuild the endpoint URLs from the (possibly edited)
    /// api_base/api_token, then re-sync so the shelf reflects the new
    /// server immediately.
    pub fn settings_apply(&mut self) {
        // C aborts any in-flight sync chain BEFORE the endpoints are
        // rebuilt (eh_sync_abort): the worker stops between rounds — and
        // drops a fetched-but-unapplied round — so it never fetches from
        // the new URL with the old cursor nor applies a stale response on
        // top of the new configuration.
        self.sync_abort();
        self.save_config();
        self.client = ApiClient::new(&self.config.api_url, &self.config.api_token);
        if self.source != Source::Folder {
            self.resync();
        }

        self.set_overlay(Overlay::None);
    }

    /// Re-derive the layout geometry from the framebuffer after a live
    /// resolution switch (C sdl_set_resolution's EVT_REPAINT: the app
    /// relayouts against the new ScreenWidth/Height), then rebuild the
    /// current page.
    pub fn relayout(&mut self) {
        let s = self.screen().framebuffer().screen();
        if self.screen().framebuffer().needs_self_panel() {
            self.content_bottom = s.height.saturating_sub(106);
            self.self_panel = 106;
        } else {
            self.content_bottom = s.content_height();
            self.self_panel = 0;
        }
        self.refresh_shelf();
        self.dirty = true;
    }
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

/// A bare `host[:port]` becomes `http://host[:port]` (C
/// eh_settings_keyboard_handler normalization).
pub fn normalize_host(v: &str) -> String {
    let v = v.trim();
    if v.starts_with("http://") || v.starts_with("https://") {
        v.to_string()
    } else {
        format!("http://{v}")
    }
}

/// "Weekday HH:MM" for the self-drawn status strip (real local time).
fn clock_label() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    // 1970-01-01 was a Thursday.
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(days.rem_euclid(7)) as usize];
    format!("{wd} {h:02}:{m:02}")
}

fn union_rects(dirty: &[Rect]) -> Option<Rect> {
    let mut u = *dirty.first()?;
    for d in &dirty[1..] {
        let x0 = u.x.min(d.x);
        let y0 = u.y.min(d.y);
        let x1 = (u.x + u.w).max(d.x + d.w);
        let y1 = (u.y + u.h).max(d.y + d.h);
        u = Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
    }
    Some(u)
}

// ── overlay tap routing ───────────────────────────────────────────────

impl<B: Framebuffer> App<B> {
    /// Overlay tap routing (each overlay rebuilds its rects at draw time,
    /// so taps share the paint geometry).
    pub fn tap_overlay(&mut self, x: i32, y: i32) {
        match self.overlay {
            Overlay::More => self.tap_more_menu(x, y),
            Overlay::Settings => crate::settings::tap_settings(x, y, self),
            Overlay::Launcher => crate::launcher::tap_launcher(x, y, self),
            Overlay::Source => crate::source::tap(self, x, y),
            Overlay::Context => self.tap_context(x, y),
            Overlay::GroupChooser => self.tap_chooser(x, y, ChooserKind::Group),
            Overlay::SortChooser => self.tap_chooser(x, y, ChooserKind::Sort),
            Overlay::LogViewer | Overlay::Licenses | Overlay::LicenseDetail => crate::viewer::tap(x, y, self),
            Overlay::Download => {
                // The X button aborts every open download (C eh_main's
                // eh_dl_cancel_rect hit → eh_cancel_downloads); any other
                // tap dismisses only a drained popup (modal in flight).
                let scr = self.screen().framebuffer().screen();
                let cx = dl_cancel_rect(scr.width, self.content_bottom);
                if cx.contains(x, y) {
                    self.cancel_downloads();
                } else if self.downloader.pending == 0 {
                    self.set_overlay(Overlay::None);
                }
            }
            Overlay::Sync => {
                // Modal while the sync runs (C pins the sheet); once the
                // chain finished or failed, any tap dismisses it.
                if !self.syncing {
                    self.set_overlay(Overlay::None);
                }
            }
            Overlay::None => {}
        }
    }

    /// The More drawer: an outside tap dismisses (C behaviour), a row tap
    /// acts.  GroupBy / SortBy / DownloadAll are logged no-ops in this
    /// slice; Settings + Applications navigate.
    fn tap_more_menu(&mut self, x: i32, y: i32) {
        let scr = self.screen().framebuffer().screen();
        let dw = (scr.width as i32) * 3 / 4;
        let card = Rect {
            x: (scr.width as i32 - dw) as u32,
            y: 0,
            w: dw as u32,
            h: self.content_bottom,
        };
        if !card.contains(x, y) {
            self.set_overlay(Overlay::None);
            self.menu_rows.clear();
            return;
        }
        for (r, row) in self.menu_rows.iter().cloned() {
            if r.contains(x, y) {
                match row {
                    MenuRow::Settings => self.set_overlay(Overlay::Settings),
                    MenuRow::Applications => {
                        if crate::launcher::build(self) {
                            self.set_overlay(Overlay::Launcher);
                            self.launcher_scroll = 0;
                        }
                    }
                    MenuRow::GroupBy => self.open_group_chooser(),
                    MenuRow::SortBy => self.open_sort_chooser(),
                    MenuRow::DownloadAll => self.download_all(),
                }
                self.menu_rows.clear();
                return;
            }
        }
    }
}

/// The clock's current minute (the self-panel strip's redraw cadence).
fn panel_minute() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 60)
        .unwrap_or(0)
}

/// Stamp the self-owned status strip (C `eh_plat_stamp_panel`): a white
/// band with the real clock + battery glyph, flushed as a band-only
/// partial update (the e-ink discipline — never a full refresh).
fn stamp_self_panel<B: Framebuffer>(fb: &mut B, y0: u32, panel: u32) {
    // Platform probes first (they read the device, not pixels — the
    // surface borrow below takes fb exclusively).
    let battery = fb.battery_level();
    let frontlight = fb.frontlight_on();
    let s = fb.screen();
    let h = panel as i32;
    let fmt = fb.format();
    let stride = fb.stride();
    let mut surf = eh_render::Surface::new(fb.surface_mut(), s.width, s.height, stride, fmt);
    let font = shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    use eh_shell::{GRAY_BLACK, GRAY_WHITE};
    surf.fill_gray(Rect { x: 0, y: y0, w: s.width, h: panel }, GRAY_WHITE);
    surf.hline(0, y0, s.width, 2, GRAY_BLACK);
    let top = y0 as i32 + h / 2;
    let clock = clock_label();
    eh_render::draw_text(&mut surf, font, 40.0, &clock, 24, top - 12, GRAY_BLACK, &mut glyph);
    // Frontlight bulb (C eh_draw_system_strip: circle with short rays),
    // drawn only when the light is actually on.
    if frontlight {
        let lx = s.width as i32 - 176;
        let ly = y0 as i32 + h / 2;
        surf.circle_outline(lx, ly, 12, 2, GRAY_BLACK);
        for a in 0..8u32 {
            let ang = a as f64 * core::f64::consts::PI / 4.0 + core::f64::consts::PI / 8.0;
            surf.line(
                lx + (16.0 * ang.cos()) as i32,
                ly + (16.0 * ang.sin()) as i32,
                lx + (22.0 * ang.cos()) as i32,
                ly + (22.0 * ang.sin()) as i32,
                2,
                GRAY_BLACK,
            );
        }
    }

    // Battery: outline + nub + fill proportional to charge (the C app's
    // shape; an unknown level draws empty, like the C lvl<0 clamp).
    let bw = 84u32;
    let bh = 40u32;
    let bx = s.width.saturating_sub(116);
    let by = y0 + (panel.saturating_sub(bh)) / 2;
    surf.rect_outline(Rect { x: bx, y: by, w: bw, h: bh }, 3, GRAY_BLACK);
    surf.fill_gray(Rect { x: bx + bw + 1, y: by + bh / 2 - 7, w: 6, h: 14 }, GRAY_BLACK);
    let lvl = battery.unwrap_or(0) as u32;
    let fw = (bw - 8) * lvl.min(100) / 100;
    if fw > 0 {
        surf.fill_gray(Rect { x: bx + 4, y: by + 4, w: fw, h: bh - 8 }, GRAY_BLACK);
    }
    fb.refresh(Rect { x: 0, y: y0, w: s.width, h: panel }, eh_hal::RefreshMode::Partial);
}
/// Size of the download-popup X button (C EH_DL_CANCEL_SIZE).
pub const DL_CANCEL_SIZE: u32 = 48;

/// The download-popup cancel-button rect (C eh_dl_cancel_rect mirrored
/// onto this popup's sheet geometry): right edge of the sheet, aligned
/// with the status line.  Draw + tap share this, so they never drift.
pub fn dl_cancel_rect(w: u32, h: u32) -> Rect {
    let pw = w * 3 / 4;
    let ph = 160u32;
    let px = (w - pw) / 2;
    let py = h.saturating_sub(ph) / 2;
    Rect {
        x: px + pw - DL_CANCEL_SIZE - 24,
        y: py + 96,
        w: DL_CANCEL_SIZE,
        h: DL_CANCEL_SIZE,
    }
}

/// The modal download-progress popup (C eh_draw_dl_popup): a dim + a
/// centered white sheet showing the remaining count (the count changes as
/// the queue drains, so the frame changes during a batch — the e2e
/// suite's event-loop-alive proof).  Modal while a batch is in flight.
fn draw_download_popup<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    use eh_shell::{GRAY_BLACK, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    // Dim starting BELOW the top bar (C eh_dim_content(EH_TOP_BAR_H)): the
    // icons — the spinning sync glyph among them — stay fully visible.
    eh_shell::dim_hatch(surf, crate::appui::TOP_BAR_H, h);
    let pw = w * 3 / 4;
    let ph = 160u32;
    let px = (w - pw) / 2;
    let py = h.saturating_sub(ph) / 2;
    surf.fill_gray(Rect { x: px, y: py, w: pw, h: ph }, GRAY_WHITE);
    surf.rect_outline(Rect { x: px, y: py, w: pw, h: ph }, 2, GRAY_BLACK);
    let font = crate::shelf::shelf_font();
    let mut g = eh_render::Glyph::new();
    eh_render::draw_text(
        surf,
        font,
        28.0,
        crate::i18n::tr("dl.in_progress"),
        (px + 32) as i32,
        (py + 72) as i32,
        GRAY_BLACK,
        &mut g,
    );
    let label = if app.dl_total > 0 && !app.dl_batch_all {
        format!(
            "{}, {}",
            crate::i18n::trn("dl.complete", &[app.dl_done as i64]),
            crate::i18n::trn("dl.failed_count", &[app.dl_failed as i64])
        )
    } else {
        crate::i18n::trn("dl.remaining", &[app.downloader.pending as i64])
    };
    eh_render::draw_text(surf, font, 24.0, &label, (px + 32) as i32, (py + 120) as i32, GRAY_BLACK, &mut g);
    // Cancel X button (C draw_dl_popup_sheet's boxed X).
    let cr = dl_cancel_rect(w, h);
    surf.fill_gray(cr, GRAY_WHITE);
    surf.rect_outline(cr, 2, GRAY_BLACK);
    surf.line(
        (cr.x + 12) as i32,
        (cr.y + 12) as i32,
        (cr.x + cr.w - 12) as i32,
        (cr.y + cr.h - 12) as i32,
        3,
        GRAY_BLACK,
    );
    surf.line(
        (cr.x + cr.w - 12) as i32,
        (cr.y + 12) as i32,
        (cr.x + 12) as i32,
        (cr.y + cr.h - 12) as i32,
        3,
        GRAY_BLACK,
    );
}

/// The modal sync-progress sheet (C eh_draw_sync_popup /
/// draw_sync_popup_sheet): a dim below the top bar + a centred 190px
/// sheet — title band, the phase line, the counter subline, and during
/// the covers stage a striped progress bar.
fn draw_sync_popup<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_LGRAY, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    // Dim starting BELOW the top bar (C eh_dim_content(EH_TOP_BAR_H)): the
    // icons — the spinning sync glyph among them — stay fully visible.
    eh_shell::dim_hatch(surf, crate::appui::TOP_BAR_H, h);
    let pw = w * 3 / 4;
    let ph = SYNC_SHEET_H;
    let px = (w - pw) / 2;
    let py = h.saturating_sub(ph) / 2;
    surf.fill_gray(Rect { x: px, y: py, w: pw, h: ph }, GRAY_WHITE);
    // C draws the border twice (outer + inset); an outline of 2 covers it.
    surf.rect_outline(Rect { x: px, y: py, w: pw, h: ph }, 2, GRAY_BLACK);
    const PAD: u32 = 24; // C EH_CTX_PAD
    const TITLE_H: u32 = 72; // C EH_CTX_TITLE_H
    let font = crate::shelf::shelf_font();
    let mut g = eh_render::Glyph::new();
    eh_render::draw_text(
        surf,
        font,
        30.0,
        crate::i18n::tr("action.sync"),
        (px + PAD) as i32,
        (py + 18) as i32,
        GRAY_BLACK,
        &mut g,
    );
    surf.hline(px + PAD, py + TITLE_H - 1, pw - 2 * PAD, 2, GRAY_LGRAY);

    // Whether the cover warm pass has drained — computed before the popup
    // borrow (the probe needs &mut self).
    let warm_drained = !app.cover_warm_active();
    let p = &app.sync_popup;
    let line;
    let subline;
    match p.stage {
        SyncStage::Meta => {
            line = crate::i18n::tr("sync.meta").to_string();
            subline = crate::i18n::trn("sync.batch", &[p.round as i64]);
        }
        SyncStage::Scan => {
            line = crate::i18n::tr("sync.scan").to_string();
            subline = crate::i18n::trn("sync.books", &[p.scanned as i64]);
        }
        SyncStage::Covers => {
            line = crate::i18n::tr("sync.covers").to_string();
            if p.covers_total > 0 {
                subline = crate::i18n::trn(
                    "sync.cover_count",
                    &[p.covers_done as i64, p.covers_total as i64],
                );
            } else {
                subline = crate::i18n::tr("sync.covers").to_string();
            }
        }
        SyncStage::Fail => {
            line = crate::i18n::tr("status.fail").to_string();
            subline = p.error.clone();
        }
        SyncStage::Done => {
            line = crate::i18n::tr("sync.done").to_string();
            subline = crate::i18n::trn("sync.books", &[app.store.count().unwrap_or(0)]);
        }
    }
    eh_render::draw_text(surf, font, 28.0, &line, (px + PAD) as i32, (py + TITLE_H + 24) as i32, GRAY_BLACK, &mut g);
    eh_render::draw_text(surf, font, 24.0, &subline, (px + PAD) as i32, (py + TITLE_H + 68) as i32, GRAY_DGRAY, &mut g);

    // Covers stage: progress bar under the counter (C draw_sync_popup_
    // sheet: bar top TITLE_H+96, h 12), filled by done/total with a
    // striped overlay over the unfilled part while covers still load.
    if p.stage == SyncStage::Covers && p.covers_total > 0 {
        let bar = Rect { x: px + PAD, y: py + TITLE_H + 96, w: pw - 48, h: 12 };
        surf.fill_gray(bar, GRAY_WHITE);
        surf.rect_outline(bar, 1, GRAY_BLACK);
        let fill = (p.covers_done * (bar.w - 2)) / p.covers_total;
        if fill > 0 {
            surf.fill_gray(
                Rect { x: bar.x + 1, y: bar.y + 1, w: fill.min(bar.w - 2), h: bar.h - 2 },
                GRAY_BLACK,
            );
        }
        let drained = warm_drained;
        let from = bar.x + 1 + fill;
        let mut sx = from;
        while sx + 3 < bar.x + bar.w - 1 && !drained {
            surf.line(sx as i32, (bar.y + 1) as i32, (sx + 2) as i32, (bar.y + bar.h - 2) as i32, 1, GRAY_DGRAY);
            sx += 6;
        }
    }
}

/// The long-press context menu (C eh_draw_context): a centered white sheet
/// with the action rows.  Geometry matches the harness's context_geom
/// (sheet centred on the FULL screen; title band 72 + n*96 + 24 rows).
fn draw_context_menu<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    use eh_shell::{GRAY_BLACK, GRAY_LGRAY, GRAY_WHITE};
    let w = surf.width();
    let h = surf.height();
    crate::logger::log(&format!("[eh_app] ctx draw w={w} h={h} content_bottom={}", app.content_bottom));
    dirty.push(Rect { x: 0, y: 0, w, h });
    eh_shell::dim_hatch(surf, 0, h); // LGRAY hatch (C eh_dim_content(0))
    let n = app.context_items.len().max(1);
    let pw = w * 3 / 4;
    let ph = (72 + n * 96 + 24) as u32;
    let px = (w - pw) / 2;
    let py = ((h as i32 - ph as i32) / 2).max(0) as u32;
    surf.fill_gray(Rect { x: px, y: py, w: pw, h: ph }, GRAY_WHITE);
    surf.rect_outline(Rect { x: px, y: py, w: pw, h: ph }, 2, GRAY_BLACK);
    let font = crate::shelf::shelf_font();
    let mut g = eh_render::Glyph::new();
    surf.hline(px + 24, py + 72, pw - 48, 2, GRAY_LGRAY);
    app.context_rects.clear();
    for (i, act) in app.context_items.iter().enumerate() {
        let iy = py + 72 + (i as u32) * 96;
        let label: &str = match act {
            ContextAction::Open => crate::i18n::tr("ctx.open"),
            ContextAction::Download => crate::i18n::tr("ctx.download"),
            ContextAction::Delete => crate::i18n::tr("ctx.delete"),
            ContextAction::DownloadAll => crate::i18n::tr("ctx.download_all"),
            ContextAction::DeleteAll => crate::i18n::tr("ctx.delete_series"),
        };
        surf.fill_gray(Rect { x: px + 12, y: iy, w: pw - 24, h: 84 }, GRAY_WHITE);
        surf.rect_outline(Rect { x: px + 12, y: iy, w: pw - 24, h: 84 }, 1, GRAY_BLACK);
        eh_render::draw_text(surf, font, 28.0, label, (px + 32) as i32, (iy + 30) as i32, GRAY_BLACK, &mut g);
        app.context_rects.push(Rect { x: px + 12, y: iy, w: pw - 24, h: 84 });
    }
}

/// The Group by / Sort by chooser sheet (C eh_draw_group / eh_draw_sort):
/// a dim + a centered sheet with a title band and N rows.  Row geometry
/// matches the harness's `_chooser_py` (centred on the CONTENT area).
fn draw_chooser_sheet<B: Framebuffer>(
    surf: &mut eh_render::Surface,
    app: &mut App<B>,
    dirty: &mut Vec<Rect>,
    kind: ChooserKind,
) {
    use eh_shell::{GRAY_BLACK, GRAY_LGRAY, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    eh_shell::dim_hatch(surf, 0, h); // LGRAY hatch (C eh_dim_content(0))
    let (n, labels, title): (usize, Vec<String>, &str) = match kind {
        ChooserKind::Group => {
            let offer = app.group_offer();
            (
                offer.len(),
                offer.iter().map(|g| crate::i18n::tr(GROUP_KEYS[*g as usize]).to_string()).collect(),
                crate::i18n::tr("action.group_by"),
            )
        }
        ChooserKind::Sort => {
            (4, SORT_KEYS.iter().map(|k| crate::i18n::tr(k).to_string()).collect(), crate::i18n::tr("action.sort_by"))
        }
    };
    let pw = w * 3 / 4;
    let ph = (72 + n as u32 * 96 + 24).max(1);
    let px = (w - pw) / 2;
    let py = ((h as i32 - ph as i32) / 2).max(0) as u32;
    surf.fill_gray(Rect { x: px, y: py, w: pw, h: ph }, GRAY_WHITE);
    surf.rect_outline(Rect { x: px, y: py, w: pw, h: ph }, 2, GRAY_BLACK);
    let font = crate::shelf::shelf_font();
    let mut g = eh_render::Glyph::new();
    eh_render::draw_text(surf, font, 28.0, title, (px + 24) as i32, (py + 20) as i32, GRAY_BLACK, &mut g);
    surf.hline(px + 24, py + 64, pw - 48, 2, GRAY_LGRAY);
    app.chooser_rects.clear();
    for (i, _) in labels.iter().enumerate() {
        let iy = py + 84 + (i as u32) * 96;
        surf.fill_gray(Rect { x: px + 12, y: iy, w: pw - 24, h: 84 }, GRAY_WHITE);
        surf.rect_outline(Rect { x: px + 12, y: iy, w: pw - 24, h: 84 }, 1, GRAY_BLACK);
        eh_render::draw_text(surf, font, 26.0, &labels[i], (px + 32) as i32, (iy + 30) as i32, GRAY_BLACK, &mut g);
        app.chooser_rects.push(Rect { x: px + 12, y: iy, w: pw - 24, h: 84 });
    }
}


/// i18n keys of the group-chooser rows, in the C order (None,
/// [Author>Series], Series, Author, Year, Genre) — the harness reads the
/// store to map a chosen dimension to its row index, so the order must
/// match; the drawn text comes from crate::i18n::tr at draw time.
/// Indexed by [`crate::store::GroupPreset`] value (None=0, AuthorSeries=1,
/// Author=2, Year=3, Genre=4, Series=5).
const GROUP_KEYS: [&str; 6] = [
    "group.all",
    "group.author_series",
    "group.author",
    "group.year",
    "group.genre",
    "group.series",
];
const SORT_KEYS: [&str; 4] = ["sort.title_az", "sort.author", "sort.series", "sort.recent"];

#[cfg(test)]
mod tests {
    use super::*;

    fn book(filename: &str, ext: &str) -> Book {
        Book {
            id: "1".into(),
            title: "T".into(),
            filename: filename.into(),
            ext: ext.into(),
            ..Default::default()
        }
    }

    #[test]
    fn local_path_prefers_sanitized_filename() {
        let p = book_local_path(&book("My Book (1).epub", "epub"), "/dl");
        assert_eq!(p, PathBuf::from("/dl/My Book (1).epub"));
    }

    #[test]
    fn local_path_sanitizes_slashes() {
        let p = book_local_path(&book("a/b.epub", "epub"), "/dl");
        assert_eq!(p, PathBuf::from("/dl/a_b.epub"));
    }

    #[test]
    fn local_path_falls_back_to_id_ext() {
        let p = book_local_path(&book("", "epub"), "/dl");
        assert_eq!(p, PathBuf::from("/dl/1.epub"));
        let q = book_local_path(&book("", ""), "/dl");
        assert_eq!(q, PathBuf::from("/dl/1"));
    }

    #[test]
    fn local_path_skips_dot_directories() {
        let p = book_local_path(&book("..", "epub"), "/dl");
        assert_eq!(p, PathBuf::from("/dl/1.epub"));
    }

    #[test]
    fn host_normalization_adds_scheme() {
        assert_eq!(normalize_host("192.168.1.5:8080"), "http://192.168.1.5:8080");
        assert_eq!(normalize_host("http://x/"), "http://x/");
        assert_eq!(normalize_host("https://x:1/"), "https://x:1/");
    }

    #[test]
    fn grid_dims_match_c_panel() {
        // The C app at 1072x1448 (SDL/emulator panel): 3×2 = 6 per page,
        // cells clamped to 352×600 (C eh_view_cols/rows + eh_grid_geom).
        let g = crate::shelf::grid_geom(1072, 1342);
        assert_eq!((g.cols, g.rows), (3, 2));
        assert_eq!((g.cell_w, g.cell_h), (352, 565));
        // Wide class: 4 columns once 4 minimum cells + 240 fit.
        assert_eq!(crate::shelf::grid_cols(1404 - 16), 4);
        // Very tall class: 3 rows (avail_h >= 3*280+560).
        assert_eq!(crate::shelf::grid_rows(1872 - 96 - 96 - 108 - 8), 3);
    }

    // ── live suggest flow (C suggest_debounce_tick + eh_pu_handle_search_kb)

    use std::cell::RefCell;

    /// Test framebuffer with a fake keyboard: `open_keyboard` arms the
    /// buffer, `live_keyboard_text` exposes it while open, and
    /// `cancel_keyboard` drops it WITHOUT firing the commit callback —
    /// the contract the inkview backend implements over the firmware.
    type KbDoneCell = RefCell<Option<fn(&[u8])>>;

    struct FakeKb {
        px: Vec<u8>,
        buf: RefCell<Vec<u8>>,
        open: RefCell<bool>,
        on_done: KbDoneCell,
        cancelled: RefCell<bool>,
    }

    impl FakeKb {
        fn new(w: u32, h: u32) -> Self {
            Self {
                px: vec![0xFF; (w * h) as usize],
                buf: RefCell::new(Vec::new()),
                open: RefCell::new(false),
                on_done: RefCell::new(None),
                cancelled: RefCell::new(false),
            }
        }
        fn type_text(&self, s: &str) {
            self.buf.borrow_mut().extend_from_slice(s.as_bytes());
        }
        /// Simulate RETURN: fire the commit callback with the buffer.
        fn commit(&self) {
            let f = self.on_done.borrow_mut().take();
            *self.open.borrow_mut() = false;
            if let Some(f) = f {
                let b = self.buf.borrow().clone();
                f(&b);
            }
        }
    }

    impl Framebuffer for FakeKb {
        fn screen(&self) -> eh_hal::Screen {
            eh_hal::Screen::full(1072, 1448)
        }
        fn format(&self) -> eh_hal::PixelFormat {
            eh_hal::PixelFormat::Grayscale8
        }
        fn surface_mut(&mut self) -> &mut [u8] {
            &mut self.px
        }
        fn stride(&self) -> usize {
            1072
        }
        fn refresh(&mut self, _r: Rect, _m: eh_hal::RefreshMode) {}
        fn mark_dirty(&mut self, _r: Rect) {}
        fn poll_event(&mut self) -> Option<InputEvent> {
            None
        }
        fn wait_for_event(&mut self, _ms: u32) {}
        fn present(&mut self, _m: eh_hal::RefreshMode) {}
        fn open_keyboard(&mut self, _title: &str, initial: &str, on_done: fn(&[u8])) {
            *self.buf.borrow_mut() = initial.as_bytes().to_vec();
            *self.on_done.borrow_mut() = Some(on_done);
            *self.open.borrow_mut() = true;
        }
        fn live_keyboard_text(&self) -> Option<String> {
            if *self.open.borrow() {
                Some(String::from_utf8_lossy(&self.buf.borrow()).into_owned())
            } else {
                None
            }
        }
        fn cancel_keyboard(&mut self) {
            *self.open.borrow_mut() = false;
            self.on_done.borrow_mut().take();
            *self.cancelled.borrow_mut() = true;
        }
    }

    /// An App over a FakeKb in a scratch dir, seeded with one suggestion
    /// term ("potter") so prefix queries have something to find.
    fn mk_app(tag: &str) -> App<FakeKb> {
        let dir = std::env::temp_dir().join(format!("eh_app_tick_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fb = FakeKb::new(1072, 1448);
        let app = App::new(fb, Config::default(), None, &dir);
        app.store.suggest_set("b1", &["potter".into(), "harry potter".into()]).unwrap();
        app
    }

    fn kb(app: &mut App<FakeKb>) -> &FakeKb {
        app.screen().framebuffer()
    }

    fn tap(app: &mut App<FakeKb>, x: i32, y: i32) {
        app.on_event(&InputEvent::PointerDown { x, y });
        app.on_event(&InputEvent::PointerUp { x, y });
    }

    #[test]
    fn tick_polls_buffer_and_debounces() {
        let mut app = mk_app("poll");
        app.enter_search();
        app.edit_search();
        assert!(app.search_kb);

        // No buffer movement yet: the first tick acts but finds nothing
        // for an empty prefix (store skips len < 2).
        assert!(!app.tick());
        assert!(app.suggestions.is_empty());

        kb(&mut app).type_text("pott");
        assert!(app.tick(), "first buffer move must query the store");
        assert_eq!(app.suggestions, vec!["potter"]);
        app.present(); // the facade repaints a due tick before more input

        // Same buffer again: debounced (C g_last_suggest_q compare).
        assert!(!app.tick());

        // Buffer moves ("pott" -> "potter"): re-query, but the hits are
        // identical so the band stays quiet (C `changed` check).
        kb(&mut app).type_text("er");
        assert!(!app.tick());
        assert_eq!(app.suggestions, vec!["potter"]);

        // Buffer moves to a prefix with no hits: the band empties
        // (C restores the history list).
        kb(&mut app).type_text("xyz");
        assert!(app.tick());
        assert!(app.suggestions.is_empty());

        // A second tick on the same buffer stays quiet.
        assert!(!app.tick());
    }

    #[test]
    fn tick_inactive_without_open_keyboard() {
        let mut app = mk_app("closed");
        app.enter_search();
        assert!(!app.search_kb);
        assert!(!app.tick(), "tick must be a no-op while no keyboard is open");
    }

    #[test]
    fn commit_via_keyboard_done_filters_grid() {
        let mut app = mk_app("done");
        app.enter_search();
        app.edit_search();
        kb(&mut app).type_text("potter");
        // The C kb_commit IPC: close + fire the handler with the buffer.
        kb(&mut app).commit();
        app.present(); // drains the pending commit
        assert!(!app.search_kb);
        assert_eq!(app.query, "potter");
        assert_eq!(app.tab, Tab::Library);
    }

    #[test]
    fn suggest_tap_cancels_keyboard_and_commits_term() {
        let mut app = mk_app("taptap");
        app.enter_search();
        app.edit_search();
        kb(&mut app).type_text("pott");
        assert!(app.tick());
        assert_eq!(app.suggestions, vec!["potter"]);
        app.present(); // compute the rebuilt page's layout before tapping

        // Tap the first row widget (index 3: [0] top bar, [1] input,
        // [2] body container).
        let r = app.screen().widget_rect(3);
        tap(&mut app, (r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);

        // C CloseKeyboard + app-side commit: keyboard cancelled (no commit
        // callback fired), the tapped term filters the shelf.
        assert!(*kb(&mut app).cancelled.borrow(), "keyboard must be cancelled, not committed");
        assert!(!app.search_kb);
        assert!(app.suggestions.is_empty());
        assert_eq!(app.query, "potter");
        assert_eq!(app.tab, Tab::Library);
    }

    #[test]
    fn outside_tap_below_history_rows_does_not_commit() {
        // Device regression: with ONE history row the body below it is
        // blank; a tap there must dismiss the keyboard, never re-run the
        // stored term (the row widget's rect must not swallow the body).
        let mut app = mk_app("outside500");
        app.store.search_add("alpha").unwrap();
        app.enter_search();
        app.edit_search();
        app.present(); // layout with the (empty-suggestion) history list

        let r = app.screen().widget_rect(3);
        assert_eq!(r.h, 96, "history row must keep its fixed 96px height");
        tap(&mut app, 536, 500);

        assert_eq!(app.query, "", "outside tap must not commit");
        assert_eq!(app.tab, Tab::Search);
        assert!(!app.search_kb);
    }

    #[test]
    fn outside_tap_dismisses_keyboard_staying_on_search() {
        let mut app = mk_app("outside");
        app.enter_search();
        app.edit_search();
        kb(&mut app).type_text("pott");
        assert!(app.tick());
        app.present(); // layout for the tap target

        // Tap the input row itself: with the keyboard open this is the
        // C outside-band branch — dismiss, stay on Search, keep query.
        let r = app.screen().widget_rect(1);
        tap(&mut app, (r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);

        assert!(*kb(&mut app).cancelled.borrow());
        assert!(!app.search_kb);
        assert_eq!(app.tab, Tab::Search);
        assert_eq!(app.query, "", "dismissal must not commit");
    }

    #[test]
    fn back_key_while_keyboard_open_returns_to_library() {
        let mut app = mk_app("backkey");
        app.enter_search();
        app.edit_search();
        kb(&mut app).type_text("pott");
        assert!(app.tick());
        app.present();

        app.on_event(&InputEvent::KeyDown { key: KeyCode::Back });

        assert!(*kb(&mut app).cancelled.borrow());
        assert!(!app.search_kb);
        assert_eq!(app.tab, Tab::Library);
        assert_eq!(app.query, "");
    }

    #[test]
    fn leave_search_with_open_history_rows_still_tappable() {
        // With the keyboard open but NO suggestions, the band shows the
        // history list and a tap there runs that search (C else-branch).
        let mut app = mk_app("hist");
        app.store.search_add("dune").unwrap();
        app.enter_search();
        app.edit_search();
        kb(&mut app).type_text("zz"); // no hits for "zz"
        assert!(!app.tick(), "empty hits == empty band: no repaint due");
        assert!(app.suggestions.is_empty());
        app.present();

        let r = app.screen().widget_rect(3); // first history row
        tap(&mut app, (r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);

        assert!(*kb(&mut app).cancelled.borrow());
        assert_eq!(app.query, "dune");
        assert_eq!(app.tab, Tab::Library);
    }

    // ── downloads: batch filter/top-up, cancel X, boot reconciliation ──

    /// A BookMeta with just enough shape for upsert_book + local paths.
    fn meta(id: &str) -> crate::client::BookMeta {
        crate::client::BookMeta {
            id: id.into(),
            title: format!("T{id}"),
            filename: Some(format!("{id}.epub")),
            format: Some("epub".into()),
            ..Default::default()
        }
    }

    /// An App in a scratch dir with a HERMETIC downloads dir and an inert
    /// downloader (no worker thread, no network).  Returns app + dl dir.
    fn mk_dl_app(tag: &str) -> (App<FakeKb>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("eh_app_dl_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dl = dir.join("dl");
        std::fs::create_dir_all(&dl).unwrap();
        let cfg = Config {
            downloads_dir: Some(dl.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let mut app = App::new(FakeKb::new(1072, 1448), cfg, None, &dir);
        app.downloader = crate::downloads::Downloader::inert();
        (app, dl)
    }

    #[test]
    fn download_all_excludes_downloaded_books() {
        let (mut app, _dl) = mk_dl_app("excl");
        app.store.upsert_book(&meta("a")).unwrap();
        app.store.upsert_book(&meta("b")).unwrap();
        app.store.set_downloaded("a", true, "").unwrap();
        app.download_all();
        assert_eq!(app.downloader.pending, 1, "only the undownloaded book joins");
        assert_eq!(app.downloader.live_ids(), vec!["b".to_string()]);
        assert_eq!(app.overlay, Overlay::Download);
    }

    #[test]
    fn download_all_noop_when_nothing_undownloaded() {
        let (mut app, _dl) = mk_dl_app("noop");
        app.store.upsert_book(&meta("a")).unwrap();
        app.store.set_downloaded("a", true, "").unwrap();
        app.download_all();
        assert_eq!(app.overlay, Overlay::None, "no popup without work (C eh_download_all_start)");
        assert_eq!(app.downloader.pending, 0);
    }

    #[test]
    fn download_all_bounds_queue_and_tops_up() {
        let (mut app, _dl) = mk_dl_app("topup");
        for i in 0..12 {
            app.store.upsert_book(&meta(&format!("b{i}"))).unwrap();
        }
        app.download_all();
        assert_eq!(app.downloader.pending, App::<FakeKb>::DL_BATCH_WINDOW, "queue stays bounded");
        assert_eq!(app.dl_batch_queue.len(), 12 - App::<FakeKb>::DL_BATCH_WINDOW);
        assert_eq!(app.dl_total, 12);
        // One job settles successfully: the window tops back up.
        app.downloader.pending -= 1;
        app.top_up_batch();
        assert_eq!(app.downloader.pending, App::<FakeKb>::DL_BATCH_WINDOW, "window refilled");
        assert_eq!(app.dl_batch_queue.len(), 3, "12 - window - 1 topped up");
    }

    #[test]
    fn failed_batch_ids_are_not_reenqueued() {
        let (mut app, _dl) = mk_dl_app("failed");
        app.store.upsert_book(&meta("x")).unwrap();
        app.download_all();
        assert_eq!(app.downloader.pending, 1);
        // Simulate the drain's failure settle for x.
        app.downloader.pending -= 1;
        app.dl_failed += 1;
        app.dl_batch_failed.insert("x".into());
        app.top_up_batch();
        assert_eq!(app.downloader.live_ids(), vec!["x".to_string()], "settled entry stays until drained");
        assert!(app.dl_batch_queue.is_empty());
    }

    #[test]
    fn download_popup_x_cancels_and_closes() {
        let (mut app, dl) = mk_dl_app("xtap");
        app.store.upsert_book(&meta("a")).unwrap();
        let b = app.store.get_book("a").unwrap().unwrap();
        let cur = book_local_path(&b, dl.to_str().unwrap());
        app.enqueue_download(&b.id, &cur);
        assert_eq!(app.overlay, Overlay::Download);
        assert_eq!(app.downloader.pending, 1);
        let r = dl_cancel_rect(1072, app.content_bottom);
        app.tap_overlay((r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);
        assert_eq!(app.overlay, Overlay::None, "X cancels AND closes (C eh_cancel_downloads)");
        assert_eq!(app.downloader.pending, 0);
        assert!(!app.dl_batch_all && app.dl_batch_queue.is_empty());
    }

    #[test]
    fn boot_reconciles_downloaded_flags() {
        // Pre-seed a store whose flags disagree with disk, then boot:
        // present files keep downloaded=1, missing files are cleared
        // (C eh_refresh_downloaded_flags).
        let dir = std::env::temp_dir().join(format!("eh_app_boot_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dl = dir.join("dl");
        std::fs::create_dir_all(&dl).unwrap();
        {
            let store =
                Store::open(&dir.join(Store::LIB_DB_FILENAME)).expect("seed store");
            store.upsert_book(&meta("keep")).unwrap();
            store.upsert_book(&meta("gone")).unwrap();
            store.set_downloaded("keep", true, "").unwrap();
            store.set_downloaded("gone", true, "").unwrap();
        }
        std::fs::write(dl.join("keep.epub"), b"x").unwrap();
        let cfg = Config {
            downloads_dir: Some(dl.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let mut app = App::new(FakeKb::new(1072, 1448), cfg, None, &dir);
        app.downloader = crate::downloads::Downloader::inert();
        assert!(app.store.get_book("keep").unwrap().unwrap().downloaded);
        assert!(
            !app.store.get_book("gone").unwrap().unwrap().downloaded,
            "stale flag must be cleared at boot"
        );
    }

    #[test]
    fn reader_pref_resolution_order() {
        let readers = [
            "/ebrmain/bin/eink-reader.app",
            "/mnt/ext1/applications/koreader.app",
        ];
        // auto / empty → 0 (server open-with)
        assert_eq!(reader_pref_from_path("auto", &readers), 0);
        assert_eq!(reader_pref_from_path("", &readers), 0);
        // A path matching a detected reader → its 1-based index.
        assert_eq!(reader_pref_from_path(readers[0], &readers), 1);
        assert_eq!(reader_pref_from_path(readers[1], &readers), 2);
        // Anything else (uninstalled reader) → 0.
        assert_eq!(
            reader_pref_from_path("/mnt/ext1/applications/gone.app", &readers),
            0
        );
    }

    #[test]
    fn drill_stack_push_pop_restores_pages_and_title() {
        let mut app = mk_app("drill");
        // Seed enough tiles that every drilled level has multiple pages
        // (grid = 6/page at the test panel): 40 lone authors + Ann with 20
        // two-book series.  Level 0 shows 41 tiles, level 1 (inside Ann)
        // 20 series cards, level 2 one series' 2 books.
        use crate::client::BookMeta;
        for i in 0..40 {
            app.store.upsert_book(&BookMeta {
                id: format!("l{i}"),
                title: format!("Lone {i}"),
                authors: vec![format!("A{i}")],
                ..Default::default()
            }).unwrap();
        }
        for i in 0..20 {
            for k in 0..2 {
                app.store.upsert_book(&BookMeta {
                    id: format!("s{i}-{k}"),
                    title: format!("Book {i}/{k}"),
                    authors: vec!["Ann".into()],
                    series: Some(format!("Series {i:02}")),
                    series_id: Some(format!("sid-{i:02}")),
                    ..Default::default()
                }).unwrap();
            }
        }
        app.group = crate::store::GroupPreset::AuthorSeries;
        app.rebuild_view();
        app.page = 3;
        let author = crate::store::ViewRow {
            kind: 1,
            book_id: "s00-0".into(),
            series_id: "Ann".into(),
            series_name: "Ann".into(),
            series_count: 40,
        };
        // Level 0 → 1: the level's page is saved, the view resets to 0.
        app.drill_into_card(&author);
        assert_eq!(app.drill, 1);
        assert_eq!(app.drill_saved_pages[0], 3);
        assert_eq!(app.drill_values[0], "Ann");
        assert_eq!(app.drill_names[0], "Ann");
        assert_eq!(app.page, 0);

        // Level 1 → 2 (series within the author): page inside level 1.
        app.goto_page(2);
        assert_eq!(app.page, 2, "level 1 must span >2 pages");
        let series = crate::store::ViewRow {
            kind: 1,
            book_id: "s50-0".into(),
            series_id: "sid-05".into(),
            series_name: "Series 05".into(),
            series_count: 2,
        };
        app.drill_into_card(&series);
        assert_eq!(app.drill, 2);
        // EH_GROUP_MAX_LEVELS: a deeper drill is refused.
        app.drill_into_card(&series);
        assert_eq!(app.drill, 2);
        // The title shows the deepest drilled name.
        assert_eq!(app.top_title(), "Series 05");

        // Back pops ONE level at a time and restores that level's page.
        app.drill_back();
        assert_eq!(app.drill, 1);
        assert!(app.drill_values[1].is_empty());
        assert_eq!(app.page, 2);
        assert_eq!(app.top_title(), "Ann");
        app.drill_back();
        assert_eq!(app.drill, 0);
        assert_eq!(app.page, 3);
        assert_eq!(app.top_title(), "");
    }

    #[test]
    fn reader_cycle_order_auto_then_detected() {
        let mut app = mk_app("cycle");
        // Host fallback: nothing on this filesystem → exactly one reader
        // (Standard), so the row cycles Auto -> Standard -> Auto.
        app.cycle_reader();
        assert_eq!(app.reader_pref, 1);
        assert_eq!(app.reader_label(), "Standard");
        assert_eq!(app.config.reader.as_deref(), Some(STANDARD_READER));
        app.cycle_reader();
        assert_eq!(app.reader_pref, 0);
        assert_eq!(app.reader_label(), crate::i18n::tr("settings.reader_auto"));
        assert_eq!(app.config.reader, None);
    }

    /// Drive the sync-popup state machine without a worker: the events
    /// are applied exactly as tick() would deliver them (the mocked
    /// multi-round event-sequence test lives in sync.rs).
    #[test]
    fn sync_popup_state_machine_transitions() {
        let mut app = mk_app("syncpopup");
        app.do_sync(); // opens the sheet over the (failing-fast) worker
        assert_eq!(app.overlay, Overlay::Sync);
        assert!(app.sync_popup.open);
        assert_eq!(app.sync_popup.stage, SyncStage::Meta);
        assert!(app.syncing);

        // Modal while the sync runs: a tap must not dismiss.
        tap(&mut app, 536, 700);
        assert_eq!(app.overlay, Overlay::Sync);

        app.apply_sync_event(crate::sync::SyncEvent::MetaBatch { done: 2, total: 0 });
        assert_eq!(app.sync_popup.stage, SyncStage::Meta);
        assert_eq!(app.sync_popup.round, 2);
        app.apply_sync_event(crate::sync::SyncEvent::ScanLocal);
        assert_eq!(app.sync_popup.stage, SyncStage::Scan);
        app.apply_sync_event(crate::sync::SyncEvent::Covers { done: 3, total: 10 });
        assert_eq!(app.sync_popup.stage, SyncStage::Covers);
        assert_eq!((app.sync_popup.covers_done, app.sync_popup.covers_total), (3, 10));

        // Complete: the sheet moves to COVERS (warm pass), then flashes
        // DONE and auto-closes (C eh_sync_popup_finish → close tick).
        app.apply_sync_event(crate::sync::SyncEvent::Complete { rounds: 0 });
        assert!(!app.syncing, "spinner stops at the terminal event");
        assert!(app.sync_rx.is_none(), "event stream detached at completion");
        assert_eq!(app.sync_popup.stage, SyncStage::Covers);
        // No warm pass queued → the next tick flashes Done, the next one
        // (after the 900 ms delay) closes.
        assert!(app.sync_popup_close_tick());
        assert_eq!(app.sync_popup.stage, SyncStage::Done);
        assert!(app.sync_popup.open);
        app.sync_popup.stage_at = Some(std::time::Instant::now()
            - std::time::Duration::from_millis(SYNC_DONE_CLOSE_MS + 1));
        assert!(app.sync_popup_close_tick());
        assert!(!app.sync_popup.open);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn sync_popup_tap_dismisses_only_after_finish() {
        let mut app = mk_app("syncdismiss");
        app.sync_popup_open();
        app.syncing = true; // simulate the live run
        tap(&mut app, 536, 700);
        assert_eq!(app.overlay, Overlay::Sync, "modal while the sync runs");
        app.apply_sync_event(crate::sync::SyncEvent::Failed("boom".into()));
        assert!(!app.syncing);
        assert_eq!(app.sync_popup.stage, SyncStage::Fail);
        assert_eq!(app.sync_popup.error, "boom");
        // Tap-to-dismiss after finish (C eh_popups tap path).
        tap(&mut app, 536, 700);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn sync_popup_fail_auto_closes_after_1500ms() {
        let mut app = mk_app("syncfailclose");
        app.sync_popup_open();
        app.apply_sync_event(crate::sync::SyncEvent::Failed("no server".into()));
        assert_eq!(app.sync_popup.stage, SyncStage::Fail);
        assert!(app.sync_popup.open);
        // Not yet expired: stays up.
        assert!(!app.sync_popup_close_tick());
        assert!(app.sync_popup.open);
        app.sync_popup.stage_at = Some(std::time::Instant::now()
            - std::time::Duration::from_millis(SYNC_FAIL_CLOSE_MS + 1));
        assert!(app.sync_popup_close_tick());
        assert!(!app.sync_popup.open);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn settings_apply_aborts_the_in_flight_chain_first() {
        let mut app = mk_app("syncabort");
        // Simulate a live chain: fresh flag + attached stream.
        app.sync_cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        app.syncing = true;
        let in_flight = std::sync::Arc::clone(&app.sync_cancel);
        app.settings_apply();
        assert!(in_flight.load(std::sync::atomic::Ordering::Relaxed),
            "cancel flag set before the endpoints are rebuilt (C eh_sync_abort)");
        assert!(app.syncing, "a fresh chain starts against the rebuilt endpoints");
        assert!(app.sync_rx.is_some(), "the fresh chain's stream is attached");
    }

}