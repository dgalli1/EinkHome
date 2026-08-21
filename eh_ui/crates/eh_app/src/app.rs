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
/// app cell with its firmware icon path + launch path.
#[derive(Clone, Default)]
pub struct LauncherItem {
    pub group: bool,
    pub text: String,
    pub path: String,
    pub icon: String,
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
    /// Source-chooser row rects (parallel to the three rows).
    pub source_rows: Vec<Rect>,
    /// Active grouping preset + drill level (C eh_g_group / drill).
    pub group: crate::store::GroupPreset,
    pub sort: crate::store::SortMode,
    pub drill: u32,
    /// The drilled group's raw scope value (author / series_id / genre).
    pub group_scope: String,
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
    pub context_items: Vec<ContextAction>,
    pub context_rects: Vec<Rect>,
    /// Series set by long-press (for the `context menu open series=N` log).
    pub context_series: u32,
    /// The license currently shown in the detail page (licenses viewer).
    pub license_selected: Option<usize>,
    pub license_rects: Vec<Rect>,
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
}

/// Books per shelf page by breakpoint (the C app's per-breakpoint grid).
pub fn per_page(bp: eh_layout::Breakpoint) -> usize {
    match bp {
        eh_layout::Breakpoint::Narrow => 6,
        eh_layout::Breakpoint::Std => 15,
        eh_layout::Breakpoint::Wide => 24,
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
        let downloads_dir = config
            .downloads_dir
            .clone()
            .unwrap_or_else(|| "/mnt/ext1/Downloads".to_string());
        if let Err(e) = std::fs::create_dir_all(&downloads_dir) {
            crate::log(&format!("[eh_app] create downloads dir failed: {e}"));
        }
        let config = Self::ensure_config(&config, cfg_path.as_deref(), &downloads_dir);
        let screen = Screen::new(fb, shelf::shelf_font());
        let (content_bottom, self_panel) = {
            let s = screen.framebuffer().screen();
            let panel = s.height.saturating_sub(s.content_height());
            // Live devices with no firmware panel reserve the 106px
            // self-drawn status strip (C: EH_SELF_PANEL_H); devices where
            // the firmware owns the panel (or emulators) use the content
            // area as-is.
            if panel == 0 {
                (s.height.saturating_sub(106), 106)
            } else {
                (s.content_height(), 0)
            }
        };
        let source = Source::from_config(&config.source);
        let mut app = Self {
            screen: Some(screen),
            content_bottom,
            self_panel,
            last_panel_min: -1,
            client,
            store,
            config,
            cfg_path,
            covers_dir,
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
            source_rows: Vec::new(),
            group: crate::store::GroupPreset::None,
            sort: crate::store::SortMode::Recent,
            drill: 0,
            group_scope: String::new(),
            reader_pref: 0,
            reader_path: "auto".to_string(),
            search_kb: false,
            suggestions: Vec::new(),
            suggest_q: String::new(),
            chooser_rects: Vec::new(),
            downloader: crate::downloads::Downloader::new(),
            dl_single: false,
            dl_batch_all: false,
            dl_done: 0,
            dl_failed: 0,
            dl_total: 0,
            dl_autopen: None,
            context_items: Vec::new(),
            context_rects: Vec::new(),
            context_series: 0,
            license_selected: None,
            license_rects: Vec::new(),
            icon_cache: std::collections::HashMap::new(),
            context_book: None,
            context_scope: String::new(),
            context_label: String::new(),
            context_count: 0,
            press_pos: None,
            press_start: None,
            drag_y: None,
            drag_total: 0,
            dirty: true,
            last_overlay: Overlay::None,
        };
        app.boot();
        app
    }

    /// Boot: sync the library delta, then build the first shelf page.
    fn boot(&mut self) {
        crate::logger::log("[bookshelf] do_sync ENTER");
        self.resolve_reader();
        if let Err(e) = crate::sync::sync(&self.client, &self.store, 50) {
            crate::logger::log(&format!("[bookshelf] do_sync FAILED: {e}"));
            crate::log(&format!("[eh_app] sync failed: {e} (showing cached library)"));
        }
        // Materialise the default view (flat, recent order) — the shelf
        // reads from `view`, and the group/sort choosers rebuild it.
        let (g, s, d, q, sc) = (self.group, self.sort, self.drill, self.query.clone(), self.group_scope.clone());
        let total = self.store.view_rebuild(self.group as i64, self.sort as i64, self.drill as i64, &q, &sc).unwrap_or(0);
        crate::logger::log(&format!(
            "[bookshelf] view_rebuild: view={} sort={} group={} drill={}",
            total, s as i64, g as i64, d
        ));
        self.refresh_shelf();
        // C cover-warm pass: fetch the visible page's covers into the cache
        // so the next launch renders from disk (the offline suite waits for
        // cached covers after an online boot).  Idempotent (fetch skips
        // cache hits); best-effort on a dead API.
        let ids: Vec<String> = self.entries.iter().map(|e| e.book.id.clone()).collect();
        for id in ids {
            if cover::load_cached(&self.covers_dir, &id).is_none() {
                let _ = cover::fetch(&self.client, &self.covers_dir, &id);
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
                self.save_config();
                true
            }
            Some((KbField::ApiKey, text)) => {
                self.config.api_token = text;
                self.client = ApiClient::new(&self.config.api_url, &self.config.api_token);
                self.save_config();
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
        if !self.search_kb || self.tab != Tab::Search {
            return false;
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
        if self.tab == Tab::Search {
            self.leave_search();
        }
    }

    /// Shelf tap routing (C eh_hit_top_bar / eh_hit_pager /
    /// eh_hit_thumbnail), sharing the shell's taffy geometry: widget 0 is
    /// the top bar, the last widget the pager, the rest are the covers
    /// (or, on the search tab, the input row + history rows).
    fn tap_screen(&mut self, x: i32, y: i32) {
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
        // Left button.
        if x >= BTN_PAD as i32 && x < (BTN_PAD + BTN_SIZE) as i32 {
            if self.tab == Tab::Search {
                self.leave_search();
            }
            return;
        }
        if self.tab == Tab::Search {
            return; // search bar has no other zones
        }
        // Source button.
        if x >= SOURCE_BTN_X && x < SOURCE_BTN_X + SOURCE_BTN_W {
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

    /// Manual library sync (C top-bar sync icon, which==2).
    pub(crate) fn do_sync(&mut self) {
        crate::logger::log("[bookshelf] do_sync ENTER");
        self.syncing = true;
        let res = crate::sync::sync(&self.client, &self.store, 50);
        self.syncing = false;
        match res {
            Ok(n) => {
                let cursor = self.store.cursor().unwrap_or(0);
                crate::logger::log(&format!("[bookshelf] do_sync: rounds=1 cursor={cursor} (books={n})"));
                crate::log(&format!("[eh_app] manual sync: {n} books in store"));
                self.rebuild_view();
            }
            Err(e) => crate::log(&format!("[eh_app] sync failed: {e}")),
        }
        self.refresh_shelf();
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
        self.page = 0;
        // Re-project the materialised view under the new query filter
        // BEFORE redrawing (the C commit path's eh_view_rebuild).
        self.rebuild_view();
        self.refresh_shelf();
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
        // Rows are widget indices 2..last.  With the keyboard open and
        // suggestions showing, the rows parallel self.suggestions (the
        // band replaced the history list); otherwise the store's
        // newest-first history list (row i maps to term i-2).
        let mut hit: Option<usize> = None;
        let mut rects: Vec<Rect> = Vec::new();
        for i in 2..last {
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
            }
        }
        if self.downloader.pending == 0 && self.overlay == Overlay::Download {
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

    /// Download every book in the library (C More → Download all), show the
    /// modal popup, drain one-per-tick until empty.
    fn download_all(&mut self) {
        let n = self.store.count().unwrap_or(0) as usize;
        let books = self.store.list_books(n, 0).unwrap_or_default();
        let dl = self.downloads_dir();
        for b in &books {
            let cur = book_local_path(b, &dl);
            self.downloader
                .enqueue(&self.config.api_url, &self.config.api_token, &b.id, &cur.to_string_lossy());
        }
        crate::logger::log(&format!("[bookshelf] download-all queued={}", books.len()));
        crate::logger::log("[bookshelf] draw_dl_popup");
        self.set_overlay(Overlay::Download);
        self.dl_single = false;
        self.dl_batch_all = true;
        self.dl_done = 0;
        self.dl_failed = 0;
        self.dl_total = books.len();
        self.dl_autopen = None;
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
            self.downloader
                .enqueue(&self.config.api_url, &self.config.api_token, &b.id, &cur.to_string_lossy());
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
            self.delete_book(&b);
        }
    }

    /// Launch the reader (C eh_launch_reader → eh_plat_launch_reader: the
    /// default reader is the firmware's OpenBook path; a configured
    /// third-party reader would go through launch_app).
    fn open_reader(&mut self, path: &Path, title: &str) {
        crate::logger::log(&format!("[bookshelf] launching reader via OpenBook: {}", path.display()));
        crate::log(&format!("[eh_app] opening reader path={}", path.display()));
        if !self.screen().framebuffer_mut().open_book(&path.to_string_lossy(), title) {
            crate::log("[eh_app] reader launch failed (no reader on this platform)");
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
    fn rebuild_view(&mut self) {
        let (group, sort, drill, q, scope) = (self.group, self.sort, self.drill, self.query.clone(), self.group_scope.clone());
        let total = self
            .store
            .view_rebuild(group as i64, sort as i64, drill as i64, &q, &scope)
            .unwrap_or(0);
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
                            self.group_scope.clear();
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

    /// Drill into a tapped stack card (or a flat row's group scope).
    fn drill_into_card(&mut self, view_row: &crate::store::ViewRow) {
        self.drill = 1;
        self.group_scope = view_row.series_id.clone();
        self.rebuild_view();
    }

    /// Back: pop the drill level (C eh_group_drill_back).
    fn drill_back(&mut self) {
        if self.drill > 0 {
            self.drill = 0;
            self.group_scope.clear();
            self.rebuild_view();
        }
    }

    /// Resolve the reader preference from the config at boot (C
    /// eh_reader_pref_from_path) + log the C `reader_pref=N (cfg \`path\`)`
    /// marker the persist test greps for.
    fn resolve_reader(&mut self) {
        let cfg = self.config.reader.clone().unwrap_or_default();
        let pref: i32 = if cfg.contains("eink-reader") { 1 } else { 0 };
        self.reader_pref = pref;
        self.reader_path = if pref == 1 { STANDARD_READER.to_string() } else { cfg.clone() };
        crate::logger::log(&format!(
            "[bookshelf] reader_pref={pref} (cfg `{}`)",
            if pref == 1 { STANDARD_READER.to_string() } else { cfg }
        ));
    }

    /// Cycle the reader preference (C eh_settings reader row tap): Auto
    /// -> Standard -> Auto with the single detected reader.
    pub fn cycle_reader(&mut self) {
        self.reader_pref = if self.reader_pref == 0 { 1 } else { 0 };
        if self.reader_pref == 1 {
            self.config.reader = Some(STANDARD_READER.to_string());
            self.reader_path = STANDARD_READER.to_string();
        } else {
            self.config.reader = None;
            self.reader_path = "auto".to_string();
        }
        self.dirty = true;
        crate::logger::log(&format!("[bookshelf] reader_pref={}", self.reader_pref));
    }

    /// Change the active overlay, marking the frame dirty (the present
    /// skip must repaint when the overlay changes).
    fn set_overlay(&mut self, o: Overlay) {
        if o != self.overlay {
            self.dirty = true;
        }
        self.overlay = o;
    }

    /// The shelf page size for the current view mode + panel width.  Grid
    /// uses the breakpoint table; list is always 1 column of fixed-height
    /// rows that fit the band below the top bar / above the pager.
    fn page_size(&self, width: u32) -> usize {
        match self.view_mode {
            ViewMode::List => {
                let band = (self.content_bottom as i32 - TOP_BAR_H as i32 - crate::appui::TOP_BAR_PAD as i32
                    - PAGER_H as i32 - 8)
                    .max(1) as u32;
                (band / shelf::LIST_ROW_H).max(1) as usize
            }
            ViewMode::Grid => per_page(eh_layout::Breakpoint::from_width(width)),
        }
    }

    /// The centered top-bar title (C top_bar_title): the query on a
    /// filtered shelf, "Search" on the search page, else nothing.
    fn top_title(&self) -> &str {
        if self.query.is_empty() { "" } else { &self.query }
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
        // C draw_grid marker (the e2e harness's wait-for-grid token)
        // with the projected tile total.
        let sw = self.screen().framebuffer().screen().width;
        let view = self.view_total_books();
        crate::logger::log(&format!(
            "[bookshelf] draw_grid view={view} page={} cell={}x0 top=96 bot={}",
            self.page, sw, self.content_bottom
        ));
        crate::log(&format!(
            "[eh_app] shelf page={}/{} entries={}",
            self.page + 1,
            self.pages,
            self.entries.len()
        ));
    }

    /// The library shelf (grid or list) at the current page.
    fn build_library_page(&mut self, fb: B, width: u32) -> Screen<B> {
        let per = self.page_size(width);
        let total = self.view_total_books();
        self.pages = if total == 0 { 1 } else { (total + per - 1) / per };
        if self.page >= self.pages {
            self.page = self.pages.saturating_sub(1);
        }
        self.entries = self.store_view_page(per, self.page * per);
        let page = self.page;
        let pages = self.pages;
        let content_bottom = self.content_bottom;
        let title = self.top_title().to_string();
        let (view_mode, source, syncing, drilled) = (self.view_mode, self.source, self.syncing, self.drill > 0);
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
        )
    }

    /// Tile count the shelf pages over: the materialised view when one is
    /// present, else the library count (the C eh_view_total).
    fn view_total_books(&self) -> usize {
        let vt = self.store.view_total();
        if vt >= 0 {
            vt as usize
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
                ShelfEntry { book, art, stack, stack_label: v.series_name, stack_count: v.series_count, stack_scope: scope }
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
        self.pages = if total == 0 { 1 } else { (total + rows_per - 1) / rows_per };
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
        for b in &books {
            let _ = cover::fetch(&self.client, &self.covers_dir, &b.id);
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
                    if self.reader_pref == 1 { STANDARD_READER } else { "auto" }
                ));
            }
        }
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
            // The download popup is modal while a batch is in flight; once
            // the queue drains, any tap dismisses it (C behavior).
            Overlay::Download => {
                if self.downloader.pending == 0 {
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
    // Battery glyph at the right edge (the C app's shape).
    let bw = 84u32;
    let bh = 40u32;
    let bx = s.width.saturating_sub(116);
    let by = y0 + (panel.saturating_sub(bh)) / 2;
    surf.rect_outline(Rect { x: bx, y: by, w: bw, h: bh }, 3, GRAY_BLACK);
    surf.fill_gray(Rect { x: bx + 4, y: by + 4, w: (bw - 8) / 2, h: bh - 8 }, GRAY_BLACK);
    fb.refresh(Rect { x: 0, y: y0, w: s.width, h: panel }, eh_hal::RefreshMode::Partial);
}

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
    fn pages_per_breakpoint() {
        assert_eq!(per_page(eh_layout::Breakpoint::Narrow), 6);
        assert_eq!(per_page(eh_layout::Breakpoint::Std), 15);
        assert_eq!(per_page(eh_layout::Breakpoint::Wide), 24);
    }

    // ── live suggest flow (C suggest_debounce_tick + eh_pu_handle_search_kb)

    use std::cell::RefCell;

    /// Test framebuffer with a fake keyboard: `open_keyboard` arms the
    /// buffer, `live_keyboard_text` exposes it while open, and
    /// `cancel_keyboard` drops it WITHOUT firing the commit callback —
    /// the contract the inkview backend implements over the firmware.
    struct FakeKb {
        px: Vec<u8>,
        buf: RefCell<Vec<u8>>,
        open: RefCell<bool>,
        on_done: RefCell<Option<fn(&[u8])>>,
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

        // Tap the first row widget (index 2: [0] top bar, [1] input).
        let r = app.screen().widget_rect(2);
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

        let r = app.screen().widget_rect(2); // first history row
        tap(&mut app, (r.x + r.w / 2) as i32, (r.y + r.h / 2) as i32);

        assert!(*kb(&mut app).cancelled.borrow());
        assert_eq!(app.query, "dune");
        assert_eq!(app.tab, Tab::Library);
    }
}
/// The modal download-progress popup (C eh_draw_dl_popup): a dim + a
/// centered white sheet showing the remaining count (the count changes as
/// the queue drains, so the frame changes during a batch — the e2e
/// suite's event-loop-alive proof).  Modal while a batch is in flight.
fn draw_download_popup<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    use eh_shell::{GRAY_BLACK, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom as u32;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_BLACK);
    let pw = w * 3 / 4;
    let ph = 160u32;
    let px = (w - pw) / 2;
    let py = h.saturating_sub(ph) / 2;
    surf.fill_gray(Rect { x: px, y: py, w: pw, h: ph }, GRAY_WHITE);
    surf.rect_outline(Rect { x: px, y: py, w: pw, h: ph }, 2, GRAY_BLACK);
    let font = crate::shelf::shelf_font();
    let mut g = eh_render::Glyph::new();
    eh_render::draw_text(surf, font, 28.0, "Downloading\u{2026}", (px + 32) as i32, (py + 72) as i32, GRAY_BLACK, &mut g);
    let label = if app.dl_total > 0 && !app.dl_batch_all {
        format!("{} downloaded, {} failed", app.dl_done, app.dl_failed)
    } else {
        format!("{} remaining", app.downloader.pending)
    };
    eh_render::draw_text(surf, font, 24.0, &label, (px + 32) as i32, (py + 120) as i32, GRAY_BLACK, &mut g);
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
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_BLACK);
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
            ContextAction::Open => "Open",
            ContextAction::Download => "Download",
            ContextAction::Delete => "Delete",
            ContextAction::DownloadAll => "Download all",
            ContextAction::DeleteAll => "Delete series",
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
    let h = app.content_bottom as u32;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_BLACK);
    let (n, labels, title): (usize, Vec<String>, &str) = match kind {
        ChooserKind::Group => {
            let offer = app.group_offer();
            (
                offer.len(),
                offer.iter().map(|g| GROUP_LABELS[*g as usize].to_string()).collect(),
                "Group by",
            )
        }
        ChooserKind::Sort => (4, SORT_LABELS.iter().map(|s| s.to_string()).collect(), "Sort by"),
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
    for i in 0..n {
        let iy = py + 84 + (i as u32) * 96;
        surf.fill_gray(Rect { x: px + 12, y: iy, w: pw - 24, h: 84 }, GRAY_WHITE);
        surf.rect_outline(Rect { x: px + 12, y: iy, w: pw - 24, h: 84 }, 1, GRAY_BLACK);
        eh_render::draw_text(surf, font, 26.0, &labels[i], (px + 32) as i32, (iy + 30) as i32, GRAY_BLACK, &mut g);
        app.chooser_rects.push(Rect { x: px + 12, y: iy, w: pw - 24, h: 84 });
    }
}


/// Labels of the group-chooser rows, in the C order (None, [Author>Series],
/// Series, Author, Year, Genre) — the harness reads the store to map a
/// chosen dimension to its row index, so the order must match.
const GROUP_LABELS: [&str; 6] = ["All books", "Author > Series", "Series", "Author", "Year", "Genre"];
const SORT_LABELS: [&str; 4] = ["Title A-Z", "By author", "By series", "Recent"];
