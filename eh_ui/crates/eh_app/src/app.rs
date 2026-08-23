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

use crate::client::ApiClient;
use crate::config::{Config, parse_kv_file};
use crate::cover;
use crate::shelf::{self, ShelfEntry};
use crate::widgets::chooser::ChooserKind;
use crate::widgets::sync_popup::SyncPopup;
use crate::store::Store;

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
    pub(crate) screen: Option<Screen<B>>,
    /// Framebuffer facts cached so overlay draws (which run while
    /// `screen` is take()n inside present) never need `screen()` — a
    /// re-entrant `screen()` there panics.  Refreshed whenever the screen
    /// is alive (see [`App::sync_fb_cache`]).
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
    /// Full-library cover-warm pass: shared worker atomics + progress
    /// total (owner: [`crate::cover::WarmHandle`]).
    pub(crate) warm: crate::cover::WarmHandle,
    /// The settings row (or search input) currently owning the on-screen
    /// keyboard — the draw inverts the editing row (C eh_g_kb_field).
    pub kb_editing: Option<KbField>,
    /// Group/sort chooser row rects (drawn in the chooser sheet overlays).
    pub chooser_rects: Vec<Rect>,
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
        // Persisted grouping preset (`group=` in bookshelf.cfg): restore
        // the shelf's grouping across restarts.
        let group = crate::menu::group_from_config(&config.group);
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
            menu_rows: Vec::new(),
            settings_rows: Vec::new(),
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
            chooser_rects: Vec::new(),
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
        let src = self.source.config_value();
        let total = {
            let scopes = self.drill_scopes();
            self.store.view_rebuild(g as i64, s as i64, d as i64, &q, &scopes, &src).unwrap_or(0)
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
    pub fn theme_resource(&mut self, name: &str) -> Option<eh_hal::ThemeBitmap> {
        if self.screen.is_some() {
            self.sync_fb_cache();
            let t = self.screen.as_mut().unwrap().framebuffer().theme_resource(name);
            self.theme_cache.insert(name.to_string(), t.clone());
            t
        } else {
            self.theme_cache.get(name).cloned().flatten()
        }
    }
    /// Firmware-loader lookup (C LoadPNG fallback).  Deliberately does NOT
    /// consult theme_cache: a failed theme_resource() call caches None for
    /// the same name, which would shadow this lookup.
    pub(crate) fn load_png(&mut self, name: &str) -> Option<eh_hal::ThemeBitmap> {
        if self.screen.is_some() {
            let t = self.screen.as_mut().unwrap().framebuffer().load_png(name);
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
        if ov == Overlay::None {
            // Plain page frame: one full-waveform flush (page flips /
            // big changes deep-clean the panel).
            s.redraw_full();
        } else {
            // Overlay frame: exactly ONE panel update per input.  The old
            // flow flushed the repainted base page first and the overlay
            // second, so every input (launcher drag-scroll, settings taps)
            // flashed the bare bookshelf for a frame — SDL presented both
            // updates back-to-back and an e-ink FullUpdate blacks the
            // panel before settling.  Paint the base into the canvas
            // silently, draw the overlay over it, then flush their merged
            // dirty union once.
            s.paint();
            let scr = s.framebuffer().screen();
            let fmt = s.framebuffer().format();
            let stride = s.framebuffer().stride();
            let mut dirty: Vec<Rect> = s.drain_dirty();
            {
                let fb = s.framebuffer_mut();
                let mut surf = eh_render::Surface::new(fb.surface_mut(), scr.width, scr.height, stride, fmt);
                match ov {
                    Overlay::More => crate::menu::draw(&mut surf, self, &mut dirty),
                    Overlay::Settings => crate::settings::draw(&mut surf, self, &mut dirty),
                    Overlay::Launcher => crate::launcher::draw(&mut surf, self, &mut dirty),
                    Overlay::Source => crate::source::draw(&mut surf, self, &mut dirty),
                    Overlay::Download => crate::widgets::download::draw_download_popup(&mut surf, self, &mut dirty),
                    Overlay::Sync => crate::widgets::sync_popup::draw_sync_popup(&mut surf, self, &mut dirty),
                    Overlay::Context => crate::widgets::context::draw_context_menu(&mut surf, self, &mut dirty),
                    Overlay::GroupChooser => crate::widgets::chooser::draw_chooser_sheet(&mut surf, self, &mut dirty, ChooserKind::Group),
                    Overlay::SortChooser => crate::widgets::chooser::draw_chooser_sheet(&mut surf, self, &mut dirty, ChooserKind::Sort),
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
                // Launcher vertical drag (C eh_main.c drag_scroll_move):
                // travel below DRAG_SLOP leaves the list alone (a
                // stationary hold must not jitter it), and once dragging,
                // launcher::drag_move clamps the offset against the same
                // geometry the painter uses and reports a change only when
                // the visible scroll moved — so a held pointer produces at
                // most one dirty transition per real scroll step, never a
                // repaint loop.
                if self.overlay == Overlay::Launcher {
                    if let (Some(prev), Some(_)) = (self.drag_y, self.press_start) {
                        let dy = prev - *y;
                        self.drag_total += dy;
                        if self.drag_total.abs() >= crate::launcher::DRAG_SLOP
                            && crate::launcher::drag_move(self, dy)
                        {
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
            self.context.rects.clear();
            self.context.items.clear();
            self.context.book = None;
            return;
        }
        // Drilled into a group: pop the drill level first.
        if self.drill > 0 {
            self.drill_back();
            return;
        }
        // The download-folder picker closes on Back and returns to the
        // Settings page it was opened from (C eh_folder_close).
        if self.dl_picker.take().is_some() {
            self.set_overlay(Overlay::Settings);
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


    /// The active downloads dir (C eh_resolve_downloads_dir default).
    pub(crate) fn downloads_dir(&self) -> String {
        self.config
            .downloads_dir
            .clone()
            .unwrap_or_else(crate::local::default_downloads_dir)
    }

    // ── shelf state ───────────────────────────────────────────────────

    /// The offered group-chooser presets (C eh_view_dim_available), in the
    /// harness's row order: None, Author>Series, Series, Author, Year,
    /// Genre, minus dims the store has no values for.
    pub(crate) fn group_offer(&self) -> Vec<crate::store::GroupPreset> {
        let (a, s, y, g) = self.store.dim_availability().unwrap_or((true, false, true, true));
        use crate::store::GroupPreset;
        let mut out = vec![GroupPreset::None];
        if a && s {
            out.push(GroupPreset::AuthorSeries);
        }
        if s {
            out.push(GroupPreset::Series);
        }
        if a {
            out.push(GroupPreset::Author);
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
            let src = self.source.config_value();
            self.store
                .view_rebuild(group as i64, sort as i64, drill as i64, &q, &scopes, &src)
                .unwrap_or(0)
        };
        crate::logger::log(&format!(
            "[bookshelf] view_rebuild: view={} sort={} group={} drill={}",
            total, sort as i64, group as i64, drill
        ));
        self.dirty = true;
        self.refresh_shelf();
    }

    /// Change the active overlay, marking the frame dirty (the present
    /// skip must repaint when the overlay changes).
    pub(crate) fn set_overlay(&mut self, o: Overlay) {
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

    /// The centered top-bar title (C top_bar_title): the deepest drilled
    /// series/group name, the query on a filtered shelf, else nothing.
    pub(crate) fn top_title(&self) -> &str {
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
        if let Some(b) = self.dl_picker.as_mut() {
            // The download-folder picker owns the whole page (C
            // BR_MODE_PICKER draws over the settings screen).
            let mut screen = crate::local::build_browse_page(fb, b, self.content_bottom);
            screen.content_h = self.content_bottom;
            self.screen = Some(screen);
            return;
        }
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
            self.store
                .search(&self.query, per, page * per, &self.source.config_value())
                .unwrap_or_default()
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

    /// Picker commit (C folder_commit + eh_settings_apply's dir
    /// re-resolve): store the chosen downloads dir, persist it, log the
    /// saved marker and repaint.  Back returns to Settings.
    pub(crate) fn commit_downloads_dir(&mut self, path: &str) {
        ensure_writable_dir(path);
        self.config.downloads_dir = Some(path.to_string());
        self.save_config();
        crate::logger::log("[bookshelf] settings: saved");
        self.dl_picker = None;
        self.set_overlay(Overlay::Settings);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::reader_pref_from_path;
    use crate::widgets::sync_popup::{SyncStage, SYNC_DONE_CLOSE_MS, SYNC_FAIL_CLOSE_MS};
    use crate::store::Book;
    use crate::downloads::book_local_path;
    use crate::reader::STANDARD_READER;
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

    /// Test framebuffer with a fake keyboard: `open_keyboard` arms the
    /// buffer, `live_keyboard_text` exposes it while open, and
    /// `cancel_keyboard` drops it WITHOUT firing the commit callback —
    /// the contract the inkview backend implements over the firmware.
    struct FakeKb {
        px: Vec<u8>,
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
            Self {
                px: vec![0xFF; (w * h) as usize],
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
        fn net_active(&self) -> bool { !self.offline }

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
        app.store.suggest_set("b1", &["potter".into(), "harry potter".into()]).unwrap();
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
        app.screen().framebuffer().refreshes.borrow_mut().clear();

        // Opening the launcher: one partial update carrying the merged
        // base + overlay regions.
        app.set_overlay(Overlay::Launcher);
        app.present();
        {
            let log = app.screen().framebuffer().refreshes.borrow();
            assert_eq!(log.len(), 1, "open produced {log:?} updates for one frame");
            assert_eq!(log[0].1, eh_hal::RefreshMode::Partial);
        }

        // A dirty frame while it stays open (a drag-scroll step): still
        // exactly one update, never a bare-base flash in front of it.
        app.screen().framebuffer().refreshes.borrow_mut().clear();
        app.dirty = true;
        app.present();
        let log = app.screen().framebuffer().refreshes.borrow();
        assert_eq!(log.len(), 1, "drag step produced {log:?} updates for one frame");
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
        assert_eq!(app.dl.queue.len(), 12 - App::<FakeKb>::DL_BATCH_WINDOW);
        assert_eq!(app.dl.total, 12);
        // One job settles successfully: the window tops back up.
        app.downloader.pending -= 1;
        app.top_up_batch();
        assert_eq!(app.downloader.pending, App::<FakeKb>::DL_BATCH_WINDOW, "window refilled");
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
        assert_eq!(app.downloader.live_ids(), vec!["x".to_string()], "settled entry stays until drained");
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
        let r = crate::widgets::download::dl_cancel_rect(1072, app.content_bottom);
        app.tap_overlay((r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);
        assert_eq!(app.overlay, Overlay::None, "X cancels AND closes (C eh_cancel_downloads)");
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
        assert!(app.sync_worker.rx.is_none(), "event stream detached at completion");
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
        app.sync_worker.cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        app.syncing = true;
        let in_flight = std::sync::Arc::clone(&app.sync_worker.cancel);
        app.settings_apply();
        assert!(in_flight.load(std::sync::atomic::Ordering::Relaxed),
            "cancel flag set before the endpoints are rebuilt (C eh_sync_abort)");
        assert!(app.syncing, "a fresh chain starts against the rebuilt endpoints");
        assert!(app.sync_worker.rx.is_some(), "the fresh chain's stream is attached");
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
        let ph = crate::widgets::sync_popup::SYNC_SHEET_H as i32;
        let sx = (1072 - pw) / 2;
        let sy = (((app.content_bottom as i32 - ph) / 2).max(0)) as u32;
        // Border row: the sheet outline spans nearly the full panel width.
        assert!(dark_in(&px, sx + 4, sy, sx + pw - 4, sy + 3, 100) > (pw - 8) as usize * 3 / 5,
            "top border row missing");
        // Title + phase line + subline live in the upper half of the sheet.
        let text = dark_in(&px, sx + 20, sy + 10, sx + pw - 20, sy + ph as u32 / 2, 100);
        assert!(text > 200, "sync sheet title/phase text missing, dark={text}");
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
        dark_in(px, panel_x + pw - 260, ry + 8, panel_x + pw - 24, ry + 80, 0xAA)
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
        assert!(v0 > 40, "active grouping not shown on the Group-by row, dark={v0}");
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