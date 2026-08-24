//! Shelf data: the [`ShelfEntry`] page model, cover loading, and the C
//! grid geometry (eh_grid.c) the Slint layout and the e2e tap targets
//! share.  (The tiles themselves are Slint markup — see `ui/shelf.slint`.)

use std::path::Path;

use eh_render::Font;

use crate::client::ApiClient;
use crate::cover;
use crate::store::{Book, Store};

/// List-mode row height (C EH_LIST_ROW_H).
pub const LIST_ROW_H: u32 = 150;

/// A book paired with its decoded cover raw-RGB (if available).
pub struct ShelfEntry {
    pub book: Book,
    /// (rgb, width, height) cover art, or None for a placeholder tile.
    pub art: Option<(Vec<u8>, u32, u32)>,
    /// Stack-card tile (kind 1 in the view): shows the group label +
    /// member count instead of one book's title/author.
    pub stack: bool,
    pub stack_label: String,
    pub stack_count: i64,
    /// The raw group value (author / series_id / genre / year) this card
    /// drills into.
    pub stack_scope: String,
    /// Percent read (0..=100) from the firmware explorer db; 0 hides the
    /// cover progress bar entirely (C eh_progress_percent semantics).
    pub progress: u8,
}

/// Load one page of books + their covers from the store/cache/API.
/// `count` = max books on the page; `offset` = page start.
/// Cover fetch is best-effort: an undecodable/missing cover → placeholder.
pub fn load_page(
    client: &ApiClient,
    store: &Store,
    covers_dir: &Path,
    count: usize,
    offset: usize,
) -> Vec<ShelfEntry> {
    let books = store.list_books(count, offset).unwrap_or_default();
    books
        .into_iter()
        .map(|book| {
            let art = fetch_cover(client, covers_dir, &book.id).ok().flatten();
            ShelfEntry {
                book,
                art,
                stack: false,
                stack_label: String::new(),
                stack_count: 0,
                stack_scope: String::new(),
                progress: 0,
            }
        })
        .collect()
}

/// Fetch + decode a single book's cover; Ok(None) when none/undecodable.
fn fetch_cover(
    client: &ApiClient,
    covers_dir: &Path,
    id: &str,
) -> Result<Option<(Vec<u8>, u32, u32)>, String> {
    let bytes = cover::fetch(client, covers_dir, id)?;
    let (w, h, rgb) = cover::decode_rgb(&bytes)?;
    Ok(Some((rgb, w, h)))
}

// ── C grid geometry (eh_grid.c) ─────────────────────────────────────
// The tile layout the e2e tap targets are written against: mode-aware
// column/row counts, min/max-clamped cells, and the 8px side inset.

pub const CELL_MIN_W: u32 = 280;
pub const CELL_MIN_H: u32 = 280;
/// C EH_CELL_MAX_W / EH_CELL_MAX_H (the wide-panel clamps).
pub const CELL_MAX_W: u32 = 420;
pub const CELL_MAX_H: u32 = 600;

/// C eh_view_cols (grid mode): 4 on the 1404px class, 3 on standard
/// panels, 2 when three minimum-width covers cannot fit.
pub fn grid_cols(avail_w: u32) -> u32 {
    if avail_w >= 4 * CELL_MIN_W + 240 {
        4
    } else if avail_w >= 3 * CELL_MIN_W {
        3
    } else {
        2
    }
}

/// C eh_view_rows (grid mode): three rows only on the very tall class.
pub fn grid_rows(avail_h: u32) -> u32 {
    if avail_h >= 3 * CELL_MIN_H + 560 {
        3
    } else {
        2
    }
}

/// The active grid's column/row counts and clamped cell size (C
/// eh_view_cols × eh_view_rows × eh_grid_geom, grid mode).
pub fn grid_geom(screen_w: u32, content_bottom: u32) -> GridGeom {
    let avail_w = screen_w.saturating_sub(16);
    let bot = content_bottom.saturating_sub(crate::appui::PAGER_H);
    let avail_h = bot.saturating_sub(crate::appui::TOP_BAR_H + crate::appui::TOP_BAR_PAD + 8);
    let cols = grid_cols(avail_w);
    let rows = grid_rows(avail_h);
    let cell_w = (avail_w / cols).clamp(CELL_MIN_W, CELL_MAX_W);
    let cell_h = (avail_h / rows).clamp(CELL_MIN_H, CELL_MAX_H);
    GridGeom {
        cols,
        rows,
        cell_w,
        cell_h,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GridGeom {
    pub cols: u32,
    pub rows: u32,
    pub cell_w: u32,
    pub cell_h: u32,
}

/// The regular UI face (DejaVu Sans), shared by the icon baker and the
/// TXT-cover typesetter.  A SINGLETON: fontdue's `Font::from_bytes`
/// pre-parses every glyph (~30MB per instance) — re-loading per rebuild
/// leaked ~15MB each.
pub(crate) fn load_font() -> &'static Font {
    static FONT: std::sync::LazyLock<Font> = std::sync::LazyLock::new(|| {
        Font::from_bytes(include_bytes!("../../../fonts/DejaVuSans.ttf")).expect("embed font")
    });
    &FONT
}

/// The font the shelf uses (exposed for the status strip / top bar).
pub fn shelf_font() -> &'static Font {
    load_font()
}
