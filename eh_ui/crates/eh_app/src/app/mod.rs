//! The EinkHome application: owns the current screen + active overlay and
//! routes taps with one geometry source (the shell's taffy rects), exactly
//! the C app's eh_hit_top_bar / eh_hit_pager / eh_hit_thumbnail +
//! eh_book_press_action model.
//!
//! The app owns the framebuffer and the Slint bridge: navigation (page
//! flip, back) re-syncs the page model into the Slint tree, mirroring the
//! C app's full-redraw navigation. Overlays are Slint subtrees toggled by
//! the `overlay` property; present() renders the whole tree and flushes
//! only the renderer's dirty region (one partial update per input).

use std::path::{Path, PathBuf};

use eh_hal::{Framebuffer, InputEvent, KeyCode, Rect};

use crate::client::ApiClient;
use crate::config::{parse_kv_file, Config};
use crate::cover;
use crate::shelf::ShelfEntry;
use crate::store::{Book, Store};
use crate::widgets::sync_popup::SyncPopup;

mod data;
mod events;
mod frame;

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
    /// Icon art resolved at build() time — GetResource/LoadPNG are
    /// main-thread-only firmware calls and cannot resolve during the
    /// overlay draw (the screen is taken out of App there).
    pub art: Option<(Vec<u8>, u32, u32)>,
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
    /// One book's metadata page (long-press → Details).
    Detail,
    /// The full-screen log viewer.
    LogViewer,
    /// The licenses list viewer.
    Licenses,
    /// One license's full-text page.
    LicenseDetail,
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
    /// The i18n key of the source's display label (C source_short_label).
    pub fn ui_label_key(self) -> &'static str {
        match self {
            Source::Local => "source.local",
            Source::Folder => "source.folder",
            Source::Kavita => "source.kavita",
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
    KB_PENDING
        .with(|p| p.borrow_mut().take())
        .map(|t| (field, t))
}

/// The bookshelf app bound to one framebuffer backend.
pub struct App<B: Framebuffer> {
    /// The backend framebuffer (taken during present's overlay draws).
    pub(crate) fb: Option<B>,
    /// The Slint presentation bridge (window + component + intent queue).
    pub ui: crate::ui::Ui,
    /// Set when the release that just landed was classified as a long
    /// press — the tile-release action consumes it (opens the context
    /// menu instead of activating the tile).
    pub(crate) pending_long: bool,
    /// Set between a firmware long-press event and the trailing release:
    /// the gesture already opened the context menu, so the release must
    /// not re-classify (C handled EVT_POINTER_LONGPRESS directly).
    pub(crate) long_press_seen: bool,
    /// Sub-row drag travel for the log/detail viewers (px); a row step
    /// fires whenever it crosses one row pitch (see `viewer::drag_scroll`).
    pub(crate) log_drag_acc: i32,
    /// Set when the release that just landed ended a drag (>48px of
    /// travel) — the launcher's cell-release action consumes it (a drag
    /// must not launch).
    pub(crate) pending_drag: bool,
    /// Framebuffer facts cached so overlay draws (which run while
    /// `fb` is taken inside present) never need `fb()` — a re-entrant
    /// `fb()` there panics.  Refreshed whenever the fb is alive (see
    /// [`App::sync_fb_cache`]).
    fb_screen_w: u32,
    pub(crate) fb_net_active: bool,
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
    /// Settings → Download-folder picker (C BR_MODE_PICKER): Some while
    /// the directory chooser is open.
    pub dl_picker: Option<crate::local::Browser>,
    pub page: usize,
    pub pages: usize,
    /// The current page's entries; the grid widgets mirror these.
    pub entries: Vec<ShelfEntry>,
    pub overlay: Overlay,

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
    /// Full-library cover-warm pass: shared worker atomics + progress
    /// total (owner: [`crate::cover::WarmHandle`]).
    pub(crate) warm: crate::cover::WarmHandle,
    /// The settings row (or search input) currently owning the on-screen
    /// keyboard — the draw inverts the editing row (C eh_g_kb_field).
    pub kb_editing: Option<KbField>,
    /// Download queue + worker + completion channel.
    pub downloader: crate::downloads::Downloader,
    /// Active download batch (sheet labels + drain behavior; owner:
    /// [`crate::downloads::BatchUi`]).
    pub dl: crate::downloads::BatchUi,
    /// Long-press context menu (rows, geometry, target; owner:
    /// [`crate::context_menu::MenuState`]).
    pub context: crate::context_menu::MenuState,
    /// The license currently shown in the detail page (licenses viewer).
    pub license_selected: Option<usize>,
    /// First visible row of the log tail (<0 = pinned to the newest end,
    /// C eh_g_state.log_scroll) / of the licenses list or detail.
    pub log_scroll: i32,
    pub lic_scroll: i32,
    /// Decoded launcher icon art by path (decoded once; the emulator PNG
    /// decode is ~100ms each, so per-frame re-decoding froze the render).
    pub icon_cache: std::collections::HashMap<String, (u32, u32, Vec<u8>)>,
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
    /// In-flight Local import scan (generation guard + worker receiver;
    /// owner: [`crate::local::ScanJob`]).
    pub(crate) scan_job: crate::local::ScanJob,
    /// Path of the store DB — the async sync worker opens its own handle
    /// on the same file (Store::open's legacy import is once-guarded by
    /// the `.migrated` rename; the FTS backfill no-ops when populated).
    pub(crate) db_path: PathBuf,
    /// In-flight sync worker (event stream + cancel flag; owner:
    /// [`crate::sync::WorkerHandle`]).
    pub(crate) sync_worker: crate::sync::WorkerHandle,
    /// Sync-progress sheet state (visible while overlay == Overlay::Sync).
    pub sync_popup: SyncPopup,
    /// The book shown on the Detail page (long-press → Details).
    pub(crate) detail_book: Option<Book>,
}

/// True when `path` exists (or was just created) and accepts a write
/// probe — the C `access(wanted, W_OK)` + mkdir dance.
fn ensure_writable_dir(path: &str) -> bool {
    if let Err(e) = std::fs::create_dir_all(path) {
        crate::log(&format!("[eh_app] create {path} failed: {e}"));
        return false;
    }
    let probe = format!("{path}/.eh-probe");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
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
            .unwrap_or_else(crate::local::default_downloads_dir);
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
        let (sw, sh, content_bottom, self_panel) = {
            let s = fb.screen();
            // Live devices with no firmware panel painter draw their own
            // 106px status strip (C eh_plat_panel_height's *self_panel);
            // the SDL/PC build and firmware-panel platforms use the
            // content area as-is.  The BACKEND owns the decision.
            if fb.needs_self_panel() {
                (s.width, s.height, s.height.saturating_sub(106), 106)
            } else {
                (s.width, s.content_height(), s.content_height(), 0)
            }
        };
        // The Slint bridge: platform (once per thread), window, fonts,
        // baked icons, callback wiring.  The window is the CANVAS the
        // renderer paints into — on firmware-panel devices the canvas
        // excludes the strip the firmware draws itself, so the window
        // height is the content height there (sh == content_bottom).
        let ui = crate::ui::Ui::new(sw, sh);
        let source = Source::from_config(&config.source);
        // Persisted grouping preset (`group=` in bookshelf.cfg): restore
        // the shelf's grouping across restarts.
        let group = crate::menu::group_from_config(&config.group);
        let mut app = Self {
            fb: Some(fb),
            ui,
            pending_long: false,
            pending_drag: false,
            long_press_seen: false,
            log_drag_acc: 0,
            fb_screen_w: sw,
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
            group,
            sort: crate::store::SortMode::Title,
            dl: crate::downloads::BatchUi::default(),
            context: crate::context_menu::MenuState::default(),
            progress: crate::progress::reload(),
            dl_picker: None,
            page: 0,
            pages: 0,
            entries: Vec::new(),
            overlay: Overlay::None,
            launcher_items: Vec::new(),
            launcher_rects: Vec::new(),
            launcher_scroll: 0,
            launcher_body_h: 0,
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
            warm: crate::cover::WarmHandle::default(),
            downloader: crate::downloads::Downloader::new(),
            sync_angle: 0,
            log_scroll: -1,
            lic_scroll: 0,
            license_selected: None,
            icon_cache: std::collections::HashMap::new(),
            press_pos: None,
            press_start: None,
            drag_y: None,
            drag_total: 0,
            browser: Default::default(),
            scan_job: crate::local::ScanJob::default(),
            dirty: true,
            last_overlay: Overlay::None,
            db_path,
            sync_worker: crate::sync::WorkerHandle::default(),
            sync_popup: SyncPopup::default(),
            detail_book: None,
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
        self.fb().open_keyboard(title, init, crate::app::kb_commit);
    }

    /// Boot: sync the library delta (only when online or the source is
    /// local-only — C eh_evt_init), then build the first shelf page.
    fn boot(&mut self) {
        self.resolve_reader();
        let online = self.fb().net_active();
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
        let src = self.source.config_value();
        let total = {
            let scopes = self.drill_scopes();
            self.store
                .view_rebuild(g as i64, s as i64, d as i64, &q, &scopes, &src)
                .unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloads::book_local_path;
    use crate::reader::reader_pref_from_path;
    use crate::reader::STANDARD_READER;
    use crate::store::Book;
    use crate::widgets::sync_popup::{SyncStage, SYNC_DONE_CLOSE_MS, SYNC_FAIL_CLOSE_MS};
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
        assert_eq!(
            normalize_host("192.168.1.5:8080"),
            "http://192.168.1.5:8080"
        );
        assert_eq!(normalize_host("http://x/"), "http://x/");
        assert_eq!(normalize_host("https://x:1/"), "https://x:1/");
    }

    #[test]
    fn detail_opens_from_context_and_back_closes() {
        // Long-press → Details: the overlay swaps to Detail with the book
        // stashed; Back clears both (the shelf state stays untouched).
        let mut app = mk_app("detailopen");
        let book = Book {
            id: "d1".into(),
            title: "Detail Me".into(),
            author: "Author X".into(),
            series: "Saga".into(),
            series_idx: 3.0,
            ext: "epub".into(),
            size: 1_572_864,
            added_at: 1_700_000_000,
            genre: "Fantasy".into(),
            ..Default::default()
        };
        app.open_context_book(&book);
        assert_eq!(app.overlay, Overlay::Context);
        // The menu carries Details as the second row.
        assert_eq!(app.context.items[1].label_key(), "ctx.details");

        app.context_row(1); // Details
        assert_eq!(app.overlay, Overlay::Detail);
        let stashed = app.detail_book.as_ref().unwrap();
        assert_eq!(stashed.id, "d1");
        assert_eq!(app.top_title(), ""); // the shelf title is untouched

        app.detail_back();
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.detail_book.is_none());
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

    /// Test framebuffer with a fake keyboard: `open_keyboard` arms the
    /// buffer, `live_keyboard_text` exposes it while open, and
    /// `cancel_keyboard` drops it WITHOUT firing the commit callback —
    /// the contract the inkview backend implements over the firmware.
    pub(crate) struct FakeKb {
        px: Vec<u8>,
        /// Force the app to draw its own status strip (self-panel devices).
        self_panel: bool,
        buf: RefCell<Vec<u8>>,
        open: RefCell<bool>,
        on_done: KbDoneCell,
        cancelled: RefCell<bool>,
        /// Offline by default so App::new's boot auto-sync never runs in
        /// tests (no pending worker events racing tick assertions).
        offline: bool,
        /// Every panel update (region, mode) — the one-flush-per-frame
        /// contract the overlay path must hold.
        refreshes: RefCell<Vec<(Rect, eh_hal::RefreshMode)>>,
    }
    use std::cell::RefCell;

    type KbDoneCell = RefCell<Option<fn(&[u8])>>;

    impl FakeKb {
        fn new(w: u32, h: u32) -> Self {
            Self::with_panel(w, h, false)
        }

        pub(crate) fn with_panel(w: u32, h: u32, self_panel: bool) -> Self {
            Self {
                px: vec![0xFF; (w * h) as usize],
                self_panel,
                buf: RefCell::new(Vec::new()),
                open: RefCell::new(false),
                on_done: RefCell::new(None),
                cancelled: RefCell::new(false),
                offline: true,
                refreshes: RefCell::new(Vec::new()),
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
        fn net_active(&self) -> bool {
            !self.offline
        }

        fn needs_self_panel(&self) -> bool {
            self.self_panel
        }

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
        fn refresh(&mut self, r: Rect, m: eh_hal::RefreshMode) {
            self.refreshes.borrow_mut().push((r, m));
        }
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
        // A dead-but-configured host: explicit do_sync engages the
        // machinery while boot stays quiet (FakeKb is offline).
        let cfg = Config {
            api_url: "http://mock.invalid".into(),
            ..Default::default()
        };
        let app = App::new(fb, cfg, None, &dir);
        app.store
            .suggest_set("b1", &["potter".into(), "harry potter".into()])
            .unwrap();
        app
    }

    #[test]
    fn overlay_frame_flushes_once() {
        // Flicker regression: with an overlay up, one changed frame used
        // to issue TWO panel updates — the repainted base page first (a
        // full-waveform flush that showed the bare bookshelf) and the
        // overlay's partial second.  SDL presented both back-to-back and
        // e-ink blacked the panel, so launcher drags / settings taps
        // flashed the shelf through the page.  The coalesced path must
        // flush exactly once per frame.
        let mut app = mk_app("ovflush");
        app.present(); // initial shelf frame
        app.fb().refreshes.borrow_mut().clear();

        // Opening the launcher: one partial update carrying the merged
        // base + overlay regions.
        app.set_overlay(Overlay::Launcher);
        app.present();
        {
            let log = app.fb().refreshes.borrow();
            assert_eq!(log.len(), 1, "open produced {log:?} updates for one frame");
            assert_eq!(log[0].1, eh_hal::RefreshMode::Partial);
        }

        // A dirty frame while it stays open (a drag-scroll step): at most
        // one update — never a bare-base flash in front of the overlay
        // (the renderer may skip the flush entirely when nothing changed).
        app.fb().refreshes.borrow_mut().clear();
        app.dirty = true;
        app.present();
        let log = app.fb().refreshes.borrow();
        assert!(
            log.len() <= 1,
            "drag step produced {log:?} updates for one frame"
        );
    }

    fn kb(app: &mut App<FakeKb>) -> &FakeKb {
        app.fb()
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
        assert!(
            !app.tick(),
            "tick must be a no-op while no keyboard is open"
        );
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

        // Tap the first suggestion row (the input row is the 88px band
        // below the 96px bar; rows run 96px apart from there).
        tap(&mut app, 536, 96 + 88 + 48);

        // C CloseKeyboard + app-side commit: keyboard cancelled (no commit
        // callback fired), the tapped term filters the shelf.
        assert!(
            *kb(&mut app).cancelled.borrow(),
            "keyboard must be cancelled, not committed"
        );
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

        tap(&mut app, 536, 500); // below the single 96px history row

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
        tap(&mut app, 536, 96 + 44);

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

        tap(&mut app, 536, 96 + 88 + 48); // first history row

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
        assert_eq!(
            app.downloader.pending, 1,
            "only the undownloaded book joins"
        );
        assert_eq!(app.downloader.live_ids(), vec!["b".to_string()]);
        assert_eq!(app.overlay, Overlay::Download);
    }

    #[test]
    fn download_all_noop_when_nothing_undownloaded() {
        let (mut app, _dl) = mk_dl_app("noop");
        app.store.upsert_book(&meta("a")).unwrap();
        app.store.set_downloaded("a", true, "").unwrap();
        app.download_all();
        assert_eq!(
            app.overlay,
            Overlay::None,
            "no popup without work (C eh_download_all_start)"
        );
        assert_eq!(app.downloader.pending, 0);
    }

    #[test]
    fn download_all_skips_disk_native_sources() {
        // Regression (emulator e2e): a stale folder row — file deleted
        // under a later cleanup, flag reconciled to undownloaded at boot
        // — used to join the batch, 404 at the server, and be remembered
        let (mut app, _dl) = mk_dl_app("natv");
        app.store.upsert_book(&meta("srv")).unwrap();
        let folder = crate::store::Book {
            id: "fld_stale".into(),
            source: "folder".into(),
            downloaded: false, // the stale-reconciliation shape
            ..Default::default()
        };
        app.store.upsert_book_row(&folder).unwrap();
        app.download_all();
        assert_eq!(app.downloader.live_ids(), vec!["srv".to_string()]);
        assert_eq!(app.dl.total, 1, "disk-native rows never join the batch");
    }

    #[test]
    fn download_all_bounds_queue_and_tops_up() {
        let (mut app, _dl) = mk_dl_app("topup");
        for i in 0..12 {
            app.store.upsert_book(&meta(&format!("b{i}"))).unwrap();
        }
        app.download_all();
        assert_eq!(
            app.downloader.pending,
            App::<FakeKb>::DL_BATCH_WINDOW,
            "queue stays bounded"
        );
        assert_eq!(app.dl.queue.len(), 12 - App::<FakeKb>::DL_BATCH_WINDOW);
        assert_eq!(app.dl.total, 12);
        // One job settles successfully: the window tops back up.
        app.downloader.pending -= 1;
        app.top_up_batch();
        assert_eq!(
            app.downloader.pending,
            App::<FakeKb>::DL_BATCH_WINDOW,
            "window refilled"
        );
        assert_eq!(app.dl.queue.len(), 3, "12 - window - 1 topped up");
    }

    #[test]
    fn failed_batch_ids_are_not_reenqueued() {
        let (mut app, _dl) = mk_dl_app("failed");
        app.store.upsert_book(&meta("x")).unwrap();
        app.download_all();
        assert_eq!(app.downloader.pending, 1);
        // Simulate the drain's failure settle for x.
        app.downloader.pending -= 1;
        app.dl.failed += 1;
        app.dl.failed_ids.insert("x".into());
        app.top_up_batch();
        assert_eq!(
            app.downloader.live_ids(),
            vec!["x".to_string()],
            "settled entry stays until drained"
        );
        assert!(app.dl.queue.is_empty());
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
        app.present(); // sync the sheet into the Slint tree before tapping
        let r = crate::widgets::download::dl_cancel_rect(1072, app.content_bottom);
        tap(&mut app, (r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);
        assert_eq!(
            app.overlay,
            Overlay::None,
            "X cancels AND closes (C eh_cancel_downloads)"
        );
        assert_eq!(app.downloader.pending, 0);
        assert!(!app.dl.batch_all && app.dl.queue.is_empty());
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
            let store = Store::open(&dir.join(Store::LIB_DB_FILENAME)).expect("seed store");
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
            app.store
                .upsert_book(&BookMeta {
                    id: format!("l{i}"),
                    title: format!("Lone {i}"),
                    authors: vec![format!("A{i}")],
                    ..Default::default()
                })
                .unwrap();
        }
        for i in 0..20 {
            for k in 0..2 {
                app.store
                    .upsert_book(&BookMeta {
                        id: format!("s{i}-{k}"),
                        title: format!("Book {i}/{k}"),
                        authors: vec!["Ann".into()],
                        series: Some(format!("Series {i:02}")),
                        series_id: Some(format!("sid-{i:02}")),
                        ..Default::default()
                    })
                    .unwrap();
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
        assert_eq!(
            (app.sync_popup.covers_done, app.sync_popup.covers_total),
            (3, 10)
        );

        // Complete: the sheet moves to COVERS (warm pass), then flashes
        // DONE and auto-closes (C eh_sync_popup_finish → close tick).
        app.apply_sync_event(crate::sync::SyncEvent::Complete { rounds: 0 });
        assert!(!app.syncing, "spinner stops at the terminal event");
        assert!(
            app.sync_worker.rx.is_none(),
            "event stream detached at completion"
        );
        assert_eq!(app.sync_popup.stage, SyncStage::Covers);
        // No warm pass queued → the next tick flashes Done, the next one
        // (after the 900 ms delay) closes.
        assert!(app.sync_popup_close_tick());
        assert_eq!(app.sync_popup.stage, SyncStage::Done);
        assert!(app.sync_popup.open);
        app.sync_popup.stage_at = Some(
            std::time::Instant::now() - std::time::Duration::from_millis(SYNC_DONE_CLOSE_MS + 1),
        );
        assert!(app.sync_popup_close_tick());
        assert!(!app.sync_popup.open);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn sync_popup_tap_dismisses_only_after_finish() {
        let mut app = mk_app("syncdismiss");
        app.sync_popup_open();
        app.syncing = true; // simulate the live run
        app.present(); // sync the sheet into the Slint tree before tapping
        tap(&mut app, 536, 700);
        assert_eq!(app.overlay, Overlay::Sync, "modal while the sync runs");
        app.apply_sync_event(crate::sync::SyncEvent::Failed("boom".into()));
        app.present(); // the sheet must be current for the dismiss tap
        assert!(!app.syncing);
        assert_eq!(app.sync_popup.stage, SyncStage::Fail);
        assert_eq!(app.sync_popup.error, crate::i18n::tr("sync.failed"));
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
        app.sync_popup.stage_at = Some(
            std::time::Instant::now() - std::time::Duration::from_millis(SYNC_FAIL_CLOSE_MS + 1),
        );
        assert!(app.sync_popup_close_tick());
        assert!(!app.sync_popup.open);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn settings_apply_aborts_the_in_flight_chain_first() {
        let mut app = mk_app("syncabort");
        // Simulate a live chain: fresh flag + attached stream.
        app.sync_worker.cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        app.syncing = true;
        let in_flight = std::sync::Arc::clone(&app.sync_worker.cancel);
        app.settings_apply();
        assert!(
            in_flight.load(std::sync::atomic::Ordering::Relaxed),
            "cancel flag set before the endpoints are rebuilt (C eh_sync_abort)"
        );
        assert!(
            app.syncing,
            "a fresh chain starts against the rebuilt endpoints"
        );
        assert!(
            app.sync_worker.rx.is_some(),
            "the fresh chain's stream is attached"
        );
    }

    /// Draw the current overlay into the FakeKb buffer and return the
    /// grayscale pixels (1 byte per pixel, stride 1072).
    fn draw_overlay_pixels(app: &mut App<FakeKb>) -> Vec<u8> {
        app.present();
        kb(app).px.clone()
    }

    fn dark_in(px: &[u8], x0: u32, y0: u32, x1: u32, y1: u32, max_val: u8) -> usize {
        let w = 1072usize;
        let mut n = 0;
        for y in y0..y1 {
            for x in x0..x1 {
                if px[(y as usize) * w + (x as usize)] <= max_val {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn sync_popup_draws_sheet_title_and_lines() {
        let mut app = mk_app("syncdraw");
        app.overlay = Overlay::Sync;
        app.sync_popup = SyncPopup {
            open: true,
            stage: SyncStage::Fail,
            error: "boom".into(),
            ..Default::default()
        };
        let px = draw_overlay_pixels(&mut app);
        // Sheet geometry per widgets::sheet::open_sheet: centred on the
        // content area, w*3/4 wide, 190 high.
        let pw = 1072u32 * 3 / 4;
        let ph = 190i32; // the Slint sync sheet's fixed ph (C popup_geom 190)
        let sx = (1072 - pw) / 2;
        let sy = (((app.content_bottom as i32 - ph) / 2).max(0)) as u32;
        // Border row: the sheet outline spans nearly the full panel width.
        assert!(
            dark_in(&px, sx + 4, sy, sx + pw - 4, sy + 3, 100) > (pw - 8) as usize * 3 / 5,
            "top border row missing"
        );
        // Title + phase line + subline live in the upper half of the sheet.
        let text = dark_in(&px, sx + 20, sy + 10, sx + pw - 20, sy + ph as u32 / 2, 100);
        assert!(
            text > 200,
            "sync sheet title/phase text missing, dark={text}"
        );
        // Content OUTSIDE the sheet must be dimmed hatch, not blank white:
        // sample just above the sheet.
        let above = dark_in(&px, sx + 40, sy - 24, sx + pw - 40, sy - 8, 0xAA);
        assert!(above > 50, "dim band above the sheet missing, dark={above}");
    }

    /// Dark-pixel count in the More-menu row *i*'s right-hand value zone
    /// (panel px = w - w*3/4, rows start at menu::Y0).
    fn menu_row_value_dark(px: &[u8], row: u32) -> usize {
        let pw = 1072u32 * 3 / 4;
        let panel_x = 1072u32 - pw;
        let ry = crate::menu::Y0 + row * crate::menu::ITEM_H;
        dark_in(
            px,
            panel_x + pw - 260,
            ry + 8,
            panel_x + pw - 24,
            ry + 80,
            0xAA,
        )
    }

    #[test]
    fn more_menu_group_row_shows_active_selection() {
        // Regression: the Group-by row hid its value whenever a grouping
        // was active (Sort by always showed its mode). C vals[] always
        // carries group_summary + sort_label.
        let mut app = mk_app("menuval");
        app.group = crate::store::GroupPreset::AuthorSeries;
        app.overlay = Overlay::More;
        let px = draw_overlay_pixels(&mut app);
        let v0 = menu_row_value_dark(&px, 0);
        assert!(
            v0 > 40,
            "active grouping not shown on the Group-by row, dark={v0}"
        );
        let v1 = menu_row_value_dark(&px, 1);
        assert!(v1 > 40, "sort mode not shown on the Sort-by row, dark={v1}");
    }

    #[test]
    fn more_menu_group_row_shows_none_at_boot() {
        let mut app = mk_app("menuvalnone");
        app.overlay = Overlay::More;
        let px = draw_overlay_pixels(&mut app);
        let v0 = menu_row_value_dark(&px, 0);
        assert!(v0 > 40, "'None' value missing at boot, dark={v0}");
    }
}
