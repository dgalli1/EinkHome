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
use crate::config::Config;
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
#[derive(Clone, Copy, PartialEq)]
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
        };
        app.boot();
        app
    }

    /// Boot: sync the library delta, then build the first shelf page.
    fn boot(&mut self) {
        if let Err(e) = crate::sync::sync(&self.client, &self.store, 50) {
            crate::log(&format!("[eh_app] sync failed: {e} (showing cached library)"));
        }
        self.refresh_shelf();
    }

    /// Persist the resolved config (C: eh_save_config_file at boot, so the
    /// defaults become visible + editable in Settings).
    fn ensure_config(config: &Config, cfg_path: Option<&Path>, downloads_dir: &str) -> Config {
        let mut config = config.clone();
        if config.downloads_dir.as_deref().unwrap_or("") != downloads_dir {
            config.downloads_dir = Some(downloads_dir.to_string());
        }
        if let Some(p) = cfg_path {
            if let Err(e) = config.save(p) {
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
        self.drain_keyboard();
        let ov = self.overlay;
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
            InputEvent::PointerUp { x, y } => {
                let (x, y) = (*x, *y);
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
                self.commit_search(&text);
                true
            }
        }
    }

    /// Back (hardware key): close the topmost overlay; on the search tab
    /// leave search keeping the active query filter (C: 'the grid stays
    /// filtered').
    fn back(&mut self) {
        if self.overlay != Overlay::None {
            self.overlay = Overlay::None;
            self.menu_rows.clear();
            self.settings_rows.clear();
            self.launcher_rects.clear();
            self.source_rows.clear();
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
        for (i, w) in self.screen().widgets.iter().rev().enumerate() {
            if i == 0 || i == last {
                continue;
            }
            if w.hit(x, y) {
                self.tap_cover(i);
                return;
            }
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
            self.overlay = Overlay::Source;
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
            self.overlay = Overlay::More;
        }
    }

    /// Open the Search sub-page (C top-bar search-icon tap, which==5).
    fn enter_search(&mut self) {
        self.tab = Tab::Search;
        self.page = 0;
        self.refresh_shelf();
    }

    /// Leave Search back to the library shelf, keeping the query filter.
    fn leave_search(&mut self) {
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
        self.syncing = true;
        let res = crate::sync::sync(&self.client, &self.store, 50);
        self.syncing = false;
        match res {
            Ok(n) => crate::log(&format!("[eh_app] manual sync: {n} books in store")),
            Err(e) => crate::log(&format!("[eh_app] sync failed: {e}")),
        }
        self.refresh_shelf();
    }

    /// Apply a committed search query (C eh_keyboard_handler non-empty
    /// branch): record it, filter the shelf, return to the library tab.
    fn commit_search(&mut self, term: &str) {
        let term = term.trim().to_string();
        if term.is_empty() {
            // Dismissed unedited: leave search, don't teleport home.
            self.tab = Tab::Library;
            self.refresh_shelf();
            return;
        }
        self.query = term.clone();
        if let Err(e) = self.store.search_add(&term) {
            crate::log(&format!("[eh_app] search_add: {e}"));
        }
        self.tab = Tab::Library;
        self.page = 0;
        self.refresh_shelf();
    }

    /// Search-tab body taps: the input row opens the keyboard; a history
    /// row re-runs that stored query (C eh_hit_search_input / history tap).
    fn tap_search_body(&mut self, x: i32, y: i32) {
        let n = self.screen().widgets.len();
        let last = n.saturating_sub(1);
        // Input row is widget index 1 (bordered box inset like its draw).
        if n > 1 {
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
        // History rows are widget indices 2..last (parallel to the store's
        // newest-first list, so row i maps to term i-2).
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
            let terms = self.store.search_list(1000, 0).unwrap_or_default();
            if let Some(t) = terms.get(idx) {
                let t = t.clone();
                self.commit_search(&t);
            }
        }
    }

    /// Open the search keyboard with the current query as initial text.
    fn edit_search(&mut self) {
        use crate::app::{kb_arm, kb_commit, kb_take_pending};
        let initial = self.query.clone();
        let _ = kb_take_pending();
        kb_arm(KbField::Search);
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
            crate::log(&format!("[eh_app] downloading book id={} size={}", book.id, book.size));
            match self.client.file(&book.id) {
                Ok(bytes) => {
                    let tmp = cur.with_extension("part");
                    if let Err(e) = std::fs::write(&tmp, &bytes) {
                        crate::log(&format!("[eh_app] download write failed: {e}"));
                        return;
                    }
                    if let Err(e) = std::fs::rename(&tmp, &cur) {
                        crate::log(&format!("[eh_app] download rename failed: {e}"));
                        let _ = std::fs::remove_file(&tmp);
                        return;
                    }
                    if let Err(e) = self.store.set_downloaded(&book.id, true, &cur.to_string_lossy()) {
                        crate::log(&format!("[eh_app] set_downloaded: {e}"));
                    }
                    crate::log(&format!(
                        "[eh_app] download OK id={} bytes={} path={}",
                        book.id,
                        bytes.len(),
                        cur.display()
                    ));
                    self.open_reader(&cur, &book.title);
                }
                Err(e) => crate::log(&format!("[eh_app] download FAILED id={}: {e}", book.id)),
            }
        }
    }

    /// Launch the reader (C eh_launch_reader → eh_plat_launch_reader: the
    /// default reader is the firmware's OpenBook path; a configured
    /// third-party reader would go through launch_app).
    fn open_reader(&mut self, path: &Path, title: &str) {
        crate::log(&format!("[eh_app] opening reader path={}", path.display()));
        if !self.screen().framebuffer_mut().open_book(&path.to_string_lossy(), title) {
            crate::log("[eh_app] reader launch failed (no reader on this platform)");
        }
    }

    // ── shelf state ───────────────────────────────────────────────────

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
        let total = self.store.count().unwrap_or(0) as usize;
        self.pages = if total == 0 { 1 } else { (total + per - 1) / per };
        if self.page >= self.pages {
            self.page = self.pages.saturating_sub(1);
        }
        self.entries = self.store_list_page(per, self.page * per);
        let page = self.page;
        let pages = self.pages;
        let content_bottom = self.content_bottom;
        let title = self.top_title().to_string();
        let (view_mode, source, syncing) = (self.view_mode, self.source, self.syncing);
        shelf::build_shelf(
            fb,
            &title,
            page,
            pages,
            &self.entries,
            content_bottom,
            view_mode,
            false,   // back
            source,  // source
            false,   // not the search tab
            syncing,
        )
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
        let history = self.store.search_list(rows_per, offset).unwrap_or_default();
        let (page, pages, query, content_bottom, syncing) =
            (self.page, self.pages, self.query.clone(), self.content_bottom, self.syncing);
        shelf::build_search(fb, &query, page, pages, &history, content_bottom, syncing)
    }

    fn store_list_page(&self, per: usize, offset: usize) -> Vec<ShelfEntry> {
        let books = if self.query.is_empty() {
            self.store.list_books(per, offset).unwrap_or_default()
        } else {
            self.store.search(&self.query, per, offset).unwrap_or_default()
        };
        books
            .into_iter()
            .map(|book| {
                // The cover is the cached art if present (page flips fetch
                // it first; a missing cache is a placeholder tile).
                let art = cover::load_cached(&self.covers_dir, &book.id)
                    .and_then(|bytes| cover::decode_rgb(&bytes).ok())
                    .map(|(w, h, rgb)| (rgb, w, h));
                ShelfEntry { book, art }
            })
            .collect()
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
            self.overlay = Overlay::None;
            self.menu_rows.clear();
            return;
        }
        for (r, row) in self.menu_rows.iter().cloned() {
            if r.contains(x, y) {
                match row {
                    MenuRow::Settings => self.overlay = Overlay::Settings,
                    MenuRow::Applications => {
                        if crate::launcher::build(self) {
                            self.overlay = Overlay::Launcher;
                            self.launcher_scroll = 0;
                        }
                    }
                    MenuRow::GroupBy | MenuRow::SortBy | MenuRow::DownloadAll => {
                        crate::log("[eh_app] menu: feature not ported yet");
                    }
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
}