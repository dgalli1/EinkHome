//! Local + Folder book sources (C eh_local.c / eh_browser.c BR_MODE_BROWSER).
//!
//! Two filesystem-backed sources sit next to the remote Kavita library:
//!
//! * **Local** — a background walk of the storage root collects every book
//!   file into the store (`source='local'`, `downloaded=1` — the files ARE
//!   the books).  The walk runs on a plain [`std::thread`] and hands its
//!   result back over a channel; the apply (SQLite) happens on the UI
//!   thread, mirroring C's worker-walk → main-thread apply chain.
//! * **Folder** — the shelf body becomes a live directory browser
//!   (C `BR_MODE_BROWSER`): directory rows, `..` ascent, page keys, and a
//!   tap on a book file opens it through the same reader flow the Kavita
//!   library uses.
//!
//! Book-file metadata (title/author) is extracted in pure Rust from epub
//! (zip container + OPF), fb2 and PDF files, cached in the store's
//! `local_meta` table keyed by the stable `fld_<djb2>` id so a re-import
//! never re-parses a known book.

use std::path::Path;

use eh_hal::{Framebuffer, Rect};
use eh_layout::taffy::{self, Dimension, Style};
use eh_shell::{DrawCtx, Screen, Widget, GRAY_LGRAY, GRAY_WHITE};

use crate::app::{App, Source, ViewMode};
use crate::appui::{TopBar, TopBarState, TOP_BAR_H, TOP_BAR_PAD};
use crate::extract::{extract_book_meta, ExtractedMeta, MAX_TITLE_LEN};
use crate::store::Book;

// ── shared facts ─────────────────────────────────────────────────────────

/// The on-device storage root (C eh_plat_browse_root).  Host/SDL tests
/// override it with EH_BROWSE_ROOT.
pub const DEVICE_BROWSE_ROOT: &str = "/mnt/ext1";

/// The directory walk's caps (C EH_LOCAL_SCAN_DEPTH / EH_LOCAL_SCAN_CAP):
/// recursion depth and total books per import.
pub const SCAN_DEPTH: u32 = 8;
pub const SCAN_CAP: usize = 20_000;

/// Browser list cap + row height (C EH_BROWSE_MAX_ENTRIES /
/// EH_FOLDER_ROW_H).
pub const BROWSE_MAX_ENTRIES: usize = 512;
pub const FOLDER_ROW_H: u32 = 96;

/// Extensions the shelf treats as book files (C BOOK_EXTS in
/// eh_browser.c; shared by the Local import and the Folder browser).
pub const BOOK_EXTS: [&str; 10] = [
    "epub", "pdf", "mobi", "azw", "azw3", "fb2", "djvu", "txt", "cbr", "cbz",
];

/// True when `ext` (already lowercase) is a book extension (C eh_is_book_ext).
pub fn is_book_ext(ext: &str) -> bool {
    BOOK_EXTS.contains(&ext)
}

/// The lowercase extension of `name`, or None when there is none
/// (C local_scan_is_book's ext normalization).
pub fn ext_of(name: &str) -> Option<String> {
    let dot = name.rfind('.')?;
    if dot + 1 >= name.len() {
        return None;
    }
    Some(name[dot + 1..].to_ascii_lowercase())
}

/// djb2 hash → 8 hex chars: the stable opaque `fld_` ids both the Local
/// import and the Folder browser derive from file paths (C eh_hash_hex).
pub fn hash_hex(s: &str) -> String {
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u64);
    }
    format!("{h:08x}")
}

/// Filename without its extension, capped like the C title field.
fn stem_title(name: &str) -> String {
    let stem = match name.rfind('.') {
        Some(d) if d > 0 => &name[..d],
        _ => name,
    };
    stem.chars().take(MAX_TITLE_LEN - 1).collect()
}

/// True when running on PocketBook hardware (the ext1 mount exists).
/// Platform seam for the path defaults: device builds keep the firmware
/// layout, PC hosts (SDL / linuxfb desktop) get useful $HOME-based ones.
fn on_device() -> bool {
    Path::new(DEVICE_BROWSE_ROOT).is_dir()
}

/// Fallback storage root on PC hosts.
fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
}

/// The storage root for this run (env override first — the SDL/host test
/// path), then the device mount on hardware, else the PC home directory
/// (browsing /mnt/ext1 on a desktop can never list anything).
pub fn browse_root() -> String {
    match std::env::var("EH_BROWSE_ROOT") {
        Ok(d) if !d.is_empty() => d,
        _ if on_device() => DEVICE_BROWSE_ROOT.to_string(),
        _ => home_dir(),
    }
}

/// The default downloads directory per platform (C eh_plat_downloads_dir):
/// the device's ext1 Downloads mount on hardware, $HOME/Downloads on PC
/// hosts.  App::new resolves + creates it and falls back to /tmp when
/// unwritable, so this stays a pure default.
pub fn default_downloads_dir() -> String {
    if on_device() {
        format!("{DEVICE_BROWSE_ROOT}/Downloads")
    } else {
        format!("{}/Downloads", home_dir())
    }
}

// ── scanner (C eh_local.c) ───────────────────────────────────────────────

/// One collected file record — the lean subset of Book the walk fills
/// without the metadata cache (author/title come from extraction during
/// the apply).
#[derive(Debug, Clone)]
pub struct LocalFile {
    pub id: String,
    pub title: String,
    pub filename: String,
    pub local_path: String,
    pub ext: String,
    pub size: i64,
}

impl LocalFile {
    /// The full Book record the apply writes (C local_file_to_book's leaf
    /// fields: downloaded=1 — the files ARE the books; added_at stays 0
    /// like the C memset).
    pub fn to_book(&self) -> Book {
        Book {
            id: self.id.clone(),
            title: self.title.clone(),
            ext: self.ext.clone(),
            size: self.size,
            downloaded: true,
            local_path: self.local_path.clone(),
            filename: self.filename.clone(),
            source: "local".into(),
            ..Default::default()
        }
    }
}

/// Walk `root` collecting every book file under the scan caps.  Hidden
/// entries (leading '.') are skipped at every level; symlinks resolve to
/// their real type so FAT/FUSE filesystems behave like the C stat fallback.
pub fn scan(root: &str) -> Vec<LocalFile> {
    scan_limited(root, SCAN_CAP)
}

/// `scan` with an explicit cap (the unit tests shrink it).
fn scan_limited(root: &str, cap: usize) -> Vec<LocalFile> {
    let mut out = Vec::new();
    let mut truncated = false;
    collect(Path::new(root), 0, cap, &mut out, &mut truncated);
    if truncated {
        crate::log(&format!(
            "[eh_app] local: scan cap {cap} reached, import truncated"
        ));
    }
    out
}

fn collect(dir: &Path, depth: u32, cap: usize, out: &mut Vec<LocalFile>, truncated: &mut bool) {
    if depth > SCAN_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if out.len() >= cap {
            *truncated = true;
            break;
        }
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        // d_type is only a hint: resolve symlinks (and DT_UNKNOWN-style
        // filesystems) through metadata like C local_scan_classify.
        let (is_dir, is_reg) = if ft.is_symlink() || (!ft.is_dir() && !ft.is_file()) {
            match std::fs::metadata(&path) {
                Ok(m) => (m.is_dir(), m.is_file()),
                Err(_) => (false, false),
            }
        } else {
            (ft.is_dir(), ft.is_file())
        };
        if is_dir {
            collect(&path, depth + 1, cap, out, truncated);
            continue;
        }
        if !is_reg {
            continue;
        }
        let Some(ext) = ext_of(&name) else { continue };
        if !is_book_ext(&ext) {
            continue;
        }
        let size = e.metadata().map(|m| m.len() as i64).unwrap_or(0);
        let path_str = path.to_string_lossy().into_owned();
        out.push(LocalFile {
            id: format!("fld_{}", hash_hex(&path_str)),
            title: stem_title(&name),
            filename: name.into_owned(),
            local_path: path_str,
            ext,
            size,
        });
    }
}

// ── async import chain (C worker walk → main-thread apply) ──────────────

/// One scanned book plus its freshly extracted metadata; the apply step
/// prefers the store's local_meta cache over re-extraction.
#[derive(Debug, Clone)]
pub struct LocalBook {
    pub book: Book,
    pub meta: ExtractedMeta,
}

/// The in-flight Local import scan job (the C g_local_scan* globals):
/// the worker → main-thread receiver plus the chain generation — a new
/// kick or a source switch bumps the generation, and a landed result
/// whose generation no longer matches is discarded as stale.
#[derive(Default)]
pub(crate) struct ScanJob {
    /// Scan results arrive here once the worker finishes.
    pub rx: Option<std::sync::mpsc::Receiver<(u32, Vec<LocalBook>)>>,
    /// Bumped on every kick/cancel; pollers compare before applying.
    pub gen: u32,
}

/// Kick the Local-source import (C eh_local_import_scanner): bump the
/// generation, spawn the scan thread, remember its receiver.  Safe to call
/// from the boot path and on every Local selection — a new kick invalidates
/// any in-flight result.
pub fn kick_import<B: Framebuffer>(app: &mut App<B>) {
    app.scan_job.gen += 1;
    let gen = app.scan_job.gen;
    let root = browse_root();
    crate::logger::log("[bookshelf] local: import scan started");
    let (tx, rx) = std::sync::mpsc::channel();
    app.scan_job.rx = Some(rx);
    app.syncing = true;
    let _ = std::thread::Builder::new()
        .name("local-scan".into())
        .spawn(move || {
            let files = scan(&root);
            let books: Vec<LocalBook> = files
                .iter()
                .map(|f| LocalBook {
                    book: f.to_book(),
                    meta: extract_book_meta(Path::new(&f.local_path), &f.ext),
                })
                .collect();
            crate::log(&format!(
                "[eh_app] local: scanned {} books under {root}",
                books.len()
            ));
            let _ = tx.send((gen, books));
        });
}

/// Drop an in-flight local import scan: bump the generation so a landed
/// result is discarded as stale by [`poll_import`] (the C scanner's gen
/// guard; a source switch must not apply a scan under the new source).
pub fn cancel_scan<B: Framebuffer>(app: &mut App<B>) {
    app.scan_job.gen += 1;
    app.scan_job.rx = None;
}

/// Drain a finished local scan into the store (C local_apply_slice's tail):
/// replace the whole 'local' source with the fresh results, cache unknown
/// metadata, then rebuild the view.  Stale generations drop their result.
pub fn poll_import<B: Framebuffer>(app: &mut App<B>) {
    let Some(rx) = &app.scan_job.rx else { return };
    let Ok((gen, books)) = rx.try_recv() else {
        return;
    };
    app.scan_job.rx = None;
    if gen != app.scan_job.gen {
        return; // stale chain (source switch / settings change): drop
    }
    app.syncing = false;
    let applied = (|| -> rusqlite::Result<()> {
        app.store.begin()?;
        app.store.delete_source("local")?;
        for lb in &books {
            let mut b = lb.book.clone();
            match app.store.local_meta_get(&b.id) {
                Some((t, a)) => {
                    if !t.is_empty() {
                        b.title = t;
                    }
                    if !a.is_empty() {
                        b.author = a;
                    }
                }
                None => {
                    if !lb.meta.title.is_empty() {
                        b.title = lb.meta.title.clone();
                    }
                    if !lb.meta.author.is_empty() {
                        b.author = lb.meta.author.clone();
                    }
                    app.store
                        .local_meta_put(&b.id, &lb.meta.title, &lb.meta.author)?;
                }
            }
            app.store.upsert_book_row(&b)?;
        }
        app.store.commit()
    })();
    match applied {
        Ok(()) => {
            crate::logger::log(&format!(
                "[bookshelf] local: imported {} books (local) from {}",
                books.len(),
                browse_root()
            ));
            app.rebuild_view();
            app.refresh_shelf();
        }
        Err(e) => {
            let _ = app.store.rollback();
            crate::log(&format!("[eh_app] local: import aborted: {e}"));
            app.syncing = false;
        }
    }
}

// ── folder browser (C eh_browser.c BR_MODE_BROWSER) ─────────────────────

/// One listed entry: `..` (when below the root), then subdirectories,
/// then book files — each group alphabetical (C browser_load's sort).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseEntry {
    pub name: String,
    pub is_dir: bool,
}

/// The browser state (C eh_g_browse_*).  `root` pins the storage root so
/// ascent stops there and display paths stay relative to it.
#[derive(Debug, Default)]
pub struct Browser {
    pub root: String,
    pub path: String,
    pub scroll: usize,
    pub entries: Vec<BrowseEntry>,
    pub open: bool,
    /// BR_MODE_PICKER: the Settings download-folder chooser — only
    /// directories are listed and a tap commits that directory.
    pub picker: bool,
}

impl Browser {
    /// Start browsing `dir` (C eh_browse_start).
    pub fn start(&mut self, dir: &str) {
        self.root = dir.to_string();
        self.path = dir.to_string();
        self.scroll = 0;
        self.load();
        self.open = true;
    }

    /// No ascent above the storage root (C browser_can_go_up).
    pub fn can_go_up(&self) -> bool {
        self.path != self.root
    }

    /// Refill `entries` from the current directory.
    pub fn load(&mut self) {
        self.entries.clear();
        if self.can_go_up() {
            self.entries.push(BrowseEntry {
                name: "..".into(),
                is_dir: true,
            });
        }
        let Ok(rd) = std::fs::read_dir(&self.path) else {
            crate::log(&format!("[eh_app] browser: opendir {} failed", self.path));
            return;
        };
        let mut dirs: Vec<String> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        for e in rd.flatten() {
            if dirs.len() + files.len() >= BROWSE_MAX_ENTRIES {
                break;
            }
            let name = e.file_name();
            let name = name.to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if self.picker && !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue; // the folder picker lists directories only
            }
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            let (is_dir, is_reg) = if ft.is_symlink() {
                match std::fs::metadata(&path) {
                    Ok(m) => (m.is_dir(), m.is_file()),
                    Err(_) => (false, false),
                }
            } else {
                (ft.is_dir(), ft.is_file())
            };
            if is_dir {
                dirs.push(name);
            } else if is_reg && ext_of(&name).is_some_and(|x| is_book_ext(&x)) {
                files.push(name);
            }
        }
        dirs.sort();
        files.sort();
        self.entries.extend(
            dirs.into_iter()
                .map(|name| BrowseEntry { name, is_dir: true }),
        );
        self.entries
            .extend(files.into_iter().map(|name| BrowseEntry {
                name,
                is_dir: false,
            }));
        crate::logger::log(&format!(
            "[bookshelf] browser: {} -> {} entries",
            self.path,
            self.entries.len()
        ));
    }

    /// Visible row count: the body runs from below the top bar to the
    /// content bottom (C browser_rows_visible, browser branch).
    pub fn rows_visible(content_bottom: u32) -> usize {
        let avail = (content_bottom as i32 - (TOP_BAR_H + TOP_BAR_PAD) as i32 - 8).max(1);
        (avail as u32 / FOLDER_ROW_H).max(1) as usize
    }

    /// Descend into a listed subdirectory (or ascend via `..`)
    /// (C browser_navigate).
    pub fn navigate(&mut self, name: &str) {
        if name == ".." {
            self.path = match std::path::Path::new(&self.path).parent() {
                Some(p) if self.path != self.root => p.to_string_lossy().into_owned(),
                _ => self.root.clone(),
            };
        } else {
            let next = format!("{}/{}", self.path, name);
            self.path = next;
        }
        self.scroll = 0;
        self.load();
    }

    /// Ascend one level; false when already at the browser root — the
    /// caller then decides what Back means (C eh_browse_up).
    pub fn up(&mut self) -> bool {
        if !self.can_go_up() {
            return false;
        }
        if let Some(p) = std::path::Path::new(&self.path).parent() {
            self.path = p.to_string_lossy().into_owned();
        }
        self.scroll = 0;
        self.load();
        true
    }

    /// Page the list one screen (C eh_browse_page); dir > 0 = forward.
    /// The draw path clamps, so the raw arithmetic mirrors the C app.
    pub fn page(&mut self, dir: i32, content_bottom: u32) {
        let rows = Self::rows_visible(content_bottom) as i32;
        let max = self.entries.len().saturating_sub(rows as usize) as i32;
        self.scroll = (self.scroll as i32 + dir * rows).clamp(0, max) as usize;
    }

    /// Display form of an absolute path: everything under the storage root
    /// shows relative to it; the root itself shows as "/" (C
    /// eh_user_path_display).
    pub fn user_display(path: &str, root: &str) -> String {
        if let Some(rest) = path.strip_prefix(root) {
            if rest.is_empty() {
                return "/".into();
            }
            if let Some(stripped) = rest.strip_prefix('/') {
                return stripped.to_string();
            }
        }
        path.to_string()
    }
}

/// The Book a folder-browser tap opens (C browser_open_book): the file IS
/// the book — filename-derived title, `fld_` id from the same hash as the
/// Local import, downloaded=1, source `folder`.
pub fn folder_book(path: &str, name: &str) -> Book {
    Book {
        id: format!("fld_{}", hash_hex(path)),
        title: stem_title(name),
        ext: ext_of(name).unwrap_or_default(),
        downloaded: true,
        local_path: path.to_string(),
        filename: name.to_string(),
        source: "folder".into(),
        ..Default::default()
    }
}

// ── browser page (the Folder source's shelf body) ───────────────────────

/// One full-width directory row (C browser_draw_row): white fill, a
/// separator line, the name in the row font with a trailing "/" for dirs.
struct BrowseRow {
    name: Option<String>,
    is_dir: bool,
    rect: Option<Rect>,
}

impl BrowseRow {
    fn blank() -> Self {
        Self {
            name: None,
            is_dir: false,
            rect: None,
        }
    }
}

impl Widget for BrowseRow {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        let w = ctx.surf.width();
        ctx.hline(0, rect.y + rect.h - 1, w, 1, GRAY_LGRAY);
        let Some(name) = &self.name else { return };
        let label = if self.is_dir {
            format!("{name}/")
        } else {
            name.clone()
        };
        // Pixel-fit truncation to w - 64 (C eh_utf8_fit_width).
        let mut label = label;
        while label.len() > 1 && ctx.font.width(&label, 28.0) as i32 > w as i32 - 64 {
            label.pop();
        }
        let baseline = rect.y as i32 + (FOLDER_ROW_H as i32 - 28) / 2 + 20;
        ctx.text(32, baseline, 28.0, &label, eh_shell::GRAY_BLACK);
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let Some(r) = self.rect {
            out.push(r);
        }
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        matches!(self.rect, Some(r) if r.contains(x, y))
    }
}

/// Build the browser screen: the top bar carries the current directory as
/// its title; the body lists the visible rows (C eh_draw_browse — body
/// only, the caller owns the chrome).
pub fn build_browse_page<B: Framebuffer>(
    fb: B,
    browser: &Browser,
    content_bottom: u32,
) -> Screen<B> {
    let font = crate::shelf::shelf_font();
    let mut screen = Screen::new(fb, font);
    screen.bg_fill = true; // browser rows may not cover the band
    screen.layout_mut().root_flex_column();
    let tb = TopBar::new(TopBarState {
        back: false,
        source: Source::Folder,
        view_mode: ViewMode::Grid,
        search: false,
        syncing: false,
        sync_angle: 0,
        title: Browser::user_display(&browser.path, &browser.root),
    });
    screen.add_styled(
        Box::new(tb),
        Style {
            flex_shrink: 0.0,
            size: taffy::geometry::Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(TOP_BAR_H as f32),
            },
            ..Style::default()
        },
    );
    let body = screen.add_container(Style {
        flex_grow: 1.0,
        flex_shrink: 1.0,
        // Full-width rows must STACK: without flex_wrap the container's
        // default ROW direction lays them out side-by-side and only the
        // first row is ever on-screen (the rest sit at x ≥ screen width).
        flex_wrap: taffy::style::FlexWrap::Wrap,
        align_items: Some(taffy::style::AlignItems::FLEX_START),
        // C eh_draw_browse: rows start 8px below the TOP_BAR_H+TOP_BAR_PAD
        // band, and eh_on_tap_browse shares that origin.
        padding: taffy::geometry::Rect {
            top: taffy::style::LengthPercentage::length((TOP_BAR_PAD + 8) as f32),
            left: taffy::style::LengthPercentage::length(0.0),
            right: taffy::style::LengthPercentage::length(0.0),
            bottom: taffy::style::LengthPercentage::length(0.0),
        },
        ..Style::default()
    });
    let rows = Browser::rows_visible(content_bottom);
    for i in 0..rows {
        let idx = browser.scroll + i;
        let row = match browser.entries.get(idx) {
            Some(e) => BrowseRow {
                name: Some(e.name.clone()),
                is_dir: e.is_dir,
                rect: None,
            },
            None => BrowseRow::blank(),
        };
        screen.add_to(
            body,
            Box::new(row),
            Style {
                flex_shrink: 0.0,
                size: taffy::geometry::Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(FOLDER_ROW_H as f32),
                },
                ..Style::default()
            },
        );
    }
    screen
}

/// Open the folder browser at the storage root (C source-tap →
/// eh_browse_start): the browser becomes the shelf body.
pub fn start_browse<B: Framebuffer>(app: &mut App<B>) {
    let root = browse_root();
    app.browser.start(&root);
    app.refresh_shelf();
}

/// Body tap in browser mode (C eh_on_tap_browse, below the top bar): a
/// directory row navigates, a book file opens through the reader flow.
pub fn tap_browse<B: Framebuffer>(app: &mut App<B>, x: i32, y: i32) {
    let _ = x; // rows span the full width; only y matters
               // C eh_on_tap_browse origin: rows start at TOP_BAR_H+TOP_BAR_PAD+8 —
               // the same offset build_browse_page's body padding gives the paint.
    let top = TOP_BAR_H + TOP_BAR_PAD + 8;
    if (y as u32) < top {
        return;
    }
    let idx = ((y as u32 - top) / FOLDER_ROW_H) as usize + app.browser.scroll;
    let Some(entry) = app.browser.entries.get(idx).cloned() else {
        return;
    };
    if entry.is_dir {
        app.browser.navigate(&entry.name);
        app.refresh_shelf();
    } else {
        let path = format!("{}/{}", app.browser.path, entry.name);
        let book = folder_book(&path, &entry.name);
        crate::logger::log(&format!("[bookshelf] browse: opening {path}"));
        app.open_reader(Path::new(&path), &book.title);
    }
}

/// Page key in browser mode: scroll the listing one screen and rebuild.
pub fn browse_page<B: Framebuffer>(app: &mut App<B>, dir: i32) {
    app.browser.page(dir, app.content_bottom);
    app.refresh_shelf();
}

/// Back key in browser mode: ascend one level; false at the root (the
/// caller falls through) (C eh_browse_up).
pub fn browse_up<B: Framebuffer>(app: &mut App<B>) -> bool {
    if !app.browser.up() {
        return false;
    }
    app.refresh_shelf();
    true
}

/// Picker-mode row tap (C eh_on_tap_browse in BR_MODE_PICKER): ".."
/// ascends, any directory tap COMMITS it as the downloads dir (the app
/// saves the config and re-resolves, C eh_settings_apply).
pub fn tap_picker<B: Framebuffer>(app: &mut App<B>, _x: i32, y: i32) {
    // C eh_on_tap_browse (picker mode) shares the browser row origin:
    // rows start at TOP_BAR_H+TOP_BAR_PAD+8, matching the paint padding.
    let top = TOP_BAR_H + TOP_BAR_PAD + 8;
    if (y as u32) < top {
        return;
    }
    let (scroll, path) = {
        let b = match app.dl_picker.as_ref() {
            Some(b) => b,
            None => return,
        };
        (b.scroll, b.path.clone())
    };
    let idx = ((y as u32 - top) / FOLDER_ROW_H) as usize + scroll;
    let Some(entry) = app.dl_picker.as_ref().unwrap().entries.get(idx).cloned() else {
        return;
    };
    if entry.name == ".." {
        app.dl_picker.as_mut().unwrap().up();
        app.dirty = true;
        app.refresh_shelf();
        return;
    }
    if entry.is_dir {
        // C folder_commit: the tapped directory becomes the downloads dir.
        let path = format!("{}/{}", path.trim_end_matches('/'), entry.name);
        app.commit_downloads_dir(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::extract_book_cover;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn scan_filters_extensions_and_skips_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "a.epub");
        touch(root, "b.EPUB"); // case-insensitive extension
        touch(root, "c.xyz"); // not a book extension
        touch(root, "d"); // no extension
        touch(root, ".hidden.epub"); // hidden file
        std::fs::create_dir_all(root.join(".h")).unwrap(); // hidden dir
        touch(&root.join(".h"), "nested.epub");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        touch(&root.join("sub"), "s.fb2");

        let mut found = scan_limited(root.to_str().unwrap(), 100);
        found.sort_by(|a, b| a.filename.cmp(&b.filename));
        let names: Vec<&str> = found.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, ["a.epub", "b.EPUB", "s.fb2"]);
        let a = found.iter().find(|f| f.filename == "a.epub").unwrap();
        assert_eq!(a.title, "a");
        assert_eq!(a.ext, "epub");
        assert!(a.id.starts_with("fld_"));
    }

    #[test]
    fn scan_stops_at_count_cap() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            touch(dir.path(), &format!("book{i}.epub"));
        }
        let found = scan_limited(dir.path().to_str().unwrap(), 5);
        assert_eq!(found.len(), 5);
    }

    /// A minimal but structurally valid epub: zip container, container.xml
    /// pointing at an OPF with dc:title/dc:creator and a cover manifest.
    fn write_epub(path: &Path) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.start_file("mimetype", SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"application/epub+zip").unwrap();
        z.start_file("META-INF/container.xml", SimpleFileOptions::default())
            .unwrap();
        z.write_all(
            br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles>
</container>"#,
        )
        .unwrap();
        z.start_file("OEBPS/content.opf", SimpleFileOptions::default())
            .unwrap();
        z.write_all(
            br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata>
    <dc:title>The Rust Book</dc:title>
    <dc:creator>A. Coder</dc:creator>
    <meta name="cover" content="cover-img"/>
  </metadata>
  <manifest><item id="cover-img" href="cover.png" media-type="image/png"/></manifest>
</package>"#,
        )
        .unwrap();
        z.finish().unwrap();
    }

    /// A 4x4 white Grayscale8 PNG (built with the same encoder the txt
    /// cover uses, so no hand-rolled bytes to rot).
    fn tiny_png() -> Vec<u8> {
        let px = vec![0xFFu8; 16];
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut enc = png::Encoder::new(&mut out, 4, 4);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&px).unwrap();
        }
        out.into_inner()
    }

    #[test]
    fn epub_cover_extraction_reads_the_named_member() {
        // The OPF names OEBPS/cover.png via meta[name=cover]; the href is
        // OPF-dir relative, so resolution must find it inside the zip.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.epub");
        {
            use std::io::Write as _;
            use zip::write::SimpleFileOptions;
            let f = std::fs::File::create(&path).unwrap();
            let mut z = zip::ZipWriter::new(f);
            z.start_file("META-INF/container.xml", SimpleFileOptions::default())
                .unwrap();
            z.write_all(br#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#).unwrap();
            z.start_file("OEBPS/content.opf", SimpleFileOptions::default())
                .unwrap();
            z.write_all(br#"<package xmlns="http://www.idpf.org/2007/opf"><metadata><meta name="cover" content="cover-img"/></metadata><manifest><item id="cover-img" href="cover.png"/></manifest></package>"#).unwrap();
            z.start_file("OEBPS/cover.png", SimpleFileOptions::default())
                .unwrap();
            z.write_all(&tiny_png()).unwrap();
            z.finish().unwrap();
        }
        let bytes = extract_book_cover(&path, "epub").expect("epub cover extracted");
        assert!(bytes.starts_with(b"\x89PNG"));
        assert!(
            crate::cover::decode_rgb(&bytes).is_ok(),
            "extracted cover must decode"
        );
    }

    #[test]
    fn broken_epub_yields_no_meta_and_filename_fallback_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("My Great Novel.epub");
        std::fs::write(&path, b"this is not a zip file").unwrap();
        // Metadata extraction fails cleanly -> poll_import keeps the
        // to_book() title, i.e. the filename WITHOUT the extension.
        assert!(extract_book_meta(&path, "epub").is_empty());
        assert_eq!(
            stem_title(path.file_name().unwrap().to_str().unwrap()),
            "My Great Novel"
        );
    }

    #[test]
    fn txt_cover_typesets_the_opening_words() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(
            &path,
            "The Hobbit\n\nIn a hole in the ground there lived a hobbit...\n",
        )
        .unwrap();
        let bytes = extract_book_cover(&path, "txt").expect("txt cover generated");
        assert!(bytes.starts_with(b"\x89PNG"));
        let decoded = crate::cover::decode_rgb(&bytes).unwrap();
        // Mostly white sheet with SOME dark text pixels.
        let dark = decoded.2.iter().filter(|&&v| v < 100).count();
        assert!(dark > 20, "typeset words missing, dark={dark}");
        assert!(dark < decoded.2.len() / 2, "sheet should stay mostly white");
        // A blank text file has nothing to catch: placeholder instead.
        let empty = dir.path().join("empty.txt");
        std::fs::write(&empty, b"   \n\t\n").unwrap();
        assert!(extract_book_cover(&empty, "txt").is_none());
    }

    #[test]
    fn pdf_first_page_renders_via_bundled_mupdf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.pdf");
        // Minimal single-page PDF (no metadata at all — the point of the
        // first-page fallback).
        std::fs::write(
            &path,
            concat!(
                "%PDF-1.4\n",
                "1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n",
                "2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n",
                "3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 280] >> endobj\n",
                "trailer << /Root 1 0 R /Size 4 >>\n",
                "%%EOF\n"
            ),
        )
        .unwrap();
        // Metadata: none -> title falls back to the filename stem.
        assert!(extract_book_meta(&path, "pdf").is_empty());
        let bytes = extract_book_cover(&path, "pdf").expect("mupdf must render page 1");
        assert!(bytes.starts_with(b"\x89PNG"));
        let (w, h, rgb) = crate::cover::decode_rgb(&bytes).unwrap();
        assert!((w, h) == (300, 420), "fit-to-card render, got {w}x{h}");
        // A blank white page: samples stay bright.
        let dark = rgb.iter().filter(|&&v| v < 100).count();
        assert_eq!(dark, 0, "blank page should have no dark pixels");
    }

    #[test]
    fn epub_title_author_cover_hint() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("book.epub");
        write_epub(&p);
        let m = extract_book_meta(&p, "epub");
        assert_eq!(m.title, "The Rust Book");
        assert_eq!(m.author, "A. Coder");
        assert_eq!(m.cover_hint.as_deref(), Some("cover.png"));
    }

    #[test]
    fn fb2_title_author() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("book.fb2");
        std::fs::write(
            &p,
            br#"<FictionBook>
  <description>
    <title-info>
      <author><first-name>Ivan</first-name><middle-name>P.</middle-name><last-name>Petrov</last-name></author>
      <book-title>War and Peace</book-title>
    </title-info>
  </description>
  <body/>
</FictionBook>"#,
        )
        .unwrap();
        let m = extract_book_meta(&p, "fb2");
        assert_eq!(m.title, "War and Peace");
        assert_eq!(m.author, "Ivan P. Petrov");
    }

    #[test]
    fn pdf_info_dict_strings() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.pdf");
        // Literal string title + UTF-16BE hex author (FEFF BOM + "деж").
        let mut raw = b"%PDF-1.4\n1 0 obj\n<< /Title (My \\(Great\\) Doc) /Author <FEFF043404350436>>\nendobj\n".to_vec();
        raw.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
        std::fs::write(&p, &raw).unwrap();
        let m = extract_book_meta(&p, "pdf");
        assert_eq!(m.title, "My (Great) Doc");
        assert_eq!(m.author, "деж");
    }

    #[test]
    fn unknown_ext_yields_empty_meta() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.txt");
        std::fs::write(&p, b"hello").unwrap();
        let m = extract_book_meta(&p, "txt");
        assert!(m.title.is_empty());
        assert!(m.author.is_empty());
    }
    #[test]
    fn hash_is_djb2_8hex() {
        // djb2("a") = 5381*33 + 97 = 177670 → 0002b606
        assert_eq!(hash_hex("a"), "0002b606");
        assert_eq!(hash_hex("").len(), 8);
    }

    #[test]
    fn browser_navigation_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("beta")).unwrap();
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        touch(root, "zbook.epub");
        touch(root, "abook.epub");
        std::fs::create_dir_all(root.join("beta").join("inner")).unwrap();

        let root_str = root.to_string_lossy().into_owned();
        let mut b = Browser::default();
        b.start(&root_str);
        assert!(b.open);
        // At the root there is no ".."; dirs first (alpha, beta), then files.
        assert!(!b.can_go_up());
        let names: Vec<&str> = b.entries.iter().map(|e| e.name.as_str()).collect();
        // Directories first (alpha, beta), then files (abook, zbook).
        assert_eq!(names, ["alpha", "beta", "abook.epub", "zbook.epub"]);

        // Descend: ".." leads the list.
        b.navigate("beta");
        assert_eq!(b.path, format!("{root_str}/beta"));
        assert_eq!(b.entries[0].name, "..");
        assert!(b.entries[0].is_dir);
        assert_eq!(b.entries[1].name, "inner");

        // Ascend back to the root; one more up stays at the root.
        assert!(b.up());
        assert_eq!(b.path, root_str);
        assert!(!b.up());

        // Paging clamps at both ends.
        b.page(5, 480); // forward past the end
        let maxed = b.scroll;
        assert!(maxed <= b.entries.len().saturating_sub(Browser::rows_visible(480)));
        b.page(-50, 480); // backward before the start
        assert_eq!(b.scroll, 0);

        // Display paths strip the root; the root itself shows as "/".
        assert_eq!(Browser::user_display(&root_str, &root_str), "/");
        assert_eq!(
            Browser::user_display(&format!("{root_str}/beta/inner"), &root_str),
            "beta/inner"
        );
        assert_eq!(Browser::user_display("/elsewhere", &root_str), "/elsewhere");
    }

    #[test]
    fn folder_book_derives_fld_id_like_local_scan() {
        let path = "/mnt/ext1/Books/x.epub";
        let b = folder_book(path, "x.epub");
        assert_eq!(b.id, format!("fld_{}", hash_hex(path)));
        assert_eq!(b.source, "folder");
        assert!(b.downloaded);
        assert_eq!(b.local_path, path);
        // Same id derivation as the Local import for identical paths.
        let f = LocalFile {
            id: format!("fld_{}", hash_hex(path)),
            title: String::new(),
            filename: "x.epub".into(),
            local_path: path.into(),
            ext: "epub".into(),
            size: 0,
        };
        assert_eq!(f.to_book().id, b.id);
        assert!(f.to_book().downloaded);
        assert_eq!(f.to_book().source, "local");
    }
}
