//! Shelf screen — the real library grid, bound to the store + cover layers.
//!
//! Builds the shell's [`Cover`] widgets from the persisted `books` rows and
//! the on-disk/cover cache, so the grid shows real titles/authors/art.  This
//! is the vertical slice that replaces the demo's placeholder tiles.
//!
//! Cover art is fetched BEFORE building the widgets (the caller passes
//!   books + their decoded covers), so the shell stays free of
//!   image-lifetime concerns — [`apply_cover_art`] mutates a `Cover`
//!   through a downcast the caller drives once.

use std::path::Path;

use eh_hal::{Framebuffer, PixelFormat, Rect};
use eh_layout::taffy;
use eh_layout::taffy::{Dimension, Style};
use eh_render::Font;
use eh_shell::{DrawCtx, Cover, GRAY_BLACK, GRAY_DGRAY, GRAY_LGRAY, GRAY_WHITE, Screen, Widget};

use crate::appui::{HistoryRow, Pager, SearchInput, TopBar, PAGER_H, TOP_BAR_H};
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

/// One stack-card tile (C eh_draw_thumbnail stack branch): the two offset
/// page sheets behind (C eh_draw_series_stack_back), the representative
/// book's cover art on top, and the count badge over the art (C
/// eh_draw_series_stack_badge) with the group label beneath.
pub struct StackCard {
    pub label: String,
    pub count: i64,
    /// The representative book's decoded cover (rgb, w, h), if cached.
    pub img: Option<Vec<u8>>,
    pub img_w: u32,
    pub img_h: u32,
    rect: Option<Rect>,
}

impl StackCard {
    pub fn new(label: impl Into<String>, count: i64) -> Self {
        Self { label: label.into(), count, img: None, img_w: 0, img_h: 0, rect: None }
    }
    pub fn set_image(&mut self, data: Vec<u8>, w: u32, h: u32) {
        self.img = Some(data);
        self.img_w = w;
        self.img_h = h;
    }
}

impl Widget for StackCard {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        let pad = 8u32;
        let step = 5i32; // C eh_draw_series_stack_back's offset step
        // Cover area inside the tile (C eh_cover_rect's inset).
        let cx = rect.x as i32 + pad as i32;
        let cy = rect.y as i32 + pad as i32;
        let cw = rect.w.saturating_sub(pad * 2) as i32;
        let ch = rect.h.saturating_sub(pad * 2) as i32;
        if cw > step * 3 && ch > step * 3 {
            // Back page sheet (furthest up-left), then the front sheet.
            for off in [2 * step, step] {
                ctx.outline(
                    Rect { x: (cx - off) as u32, y: (cy - off) as u32, w: cw as u32, h: ch as u32 },
                    2,
                    GRAY_BLACK,
                );
            }
            let art = Rect { x: cx as u32, y: cy as u32, w: cw as u32, h: ch as u32 };
            if let (Some(img), true) = (&self.img, cw > 4 && ch > 4) {
                ctx.blit(img, self.img_w, self.img_h, PixelFormat::Rgb24, art);
            } else {
                ctx.fill(art, GRAY_WHITE);
            }
            // Outline the cover rect so it reads as the top book.
            ctx.outline(art, 2, GRAY_BLACK);
            // Count badge over the art, top-right (white digits on black).
            let badge = format!("{}", self.count);
            let bw = ctx.font.width(&badge, 20.0) as u32 + 12;
            let bh = 26u32;
            let bx = cx + cw - bw as i32 - 2;
            let by = cy + 2;
            ctx.fill(Rect { x: bx as u32, y: by as u32, w: bw, h: bh }, GRAY_BLACK);
            ctx.text(bx + 6, by + 20, 20.0, &badge, GRAY_WHITE);
        }
        // Group label beneath the stack.
        let ccx = rect.x as i32 + rect.w as i32 / 2;
        let max_w = rect.w.saturating_sub(12) as i32;
        ctx.text_center_fit(ccx, (rect.y + rect.h - 10) as i32, 20.0, &self.label, max_w, GRAY_BLACK);
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let Some(r) = self.rect {
            out.push(r);
        }
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        matches!(self.rect, Some(r) if (x as u32) >= r.x && (x as u32) < r.x + r.w && (y as u32) >= r.y && (y as u32) < r.y + r.h)
    }
}

/// One list-mode shelf row (C eh_draw_thumbnail_fonts list branch): a fixed
/// 150px band with an 85×128 cover thumb flush-left and the title (30) +
/// author (24) lines beside it.
pub struct ListCell {
    pub img: Option<Vec<u8>>,
    pub img_w: u32,
    pub img_h: u32,
    pub title: String,
    pub author: String,
    /// Percent read (0..=100); >0 draws the bar over the thumb bottom.
    pub progress: u8,
    rect: Option<Rect>,
}

impl ListCell {
    pub fn new(title: impl Into<String>) -> Self {
        Self { img: None, img_w: 0, img_h: 0, title: title.into(), author: String::new(), progress: 0, rect: None }
    }
    pub fn set_image(&mut self, data: Vec<u8>, w: u32, h: u32) {
        self.img = Some(data);
        self.img_w = w;
        self.img_h = h;
    }
}

impl Widget for ListCell {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        let col = GRAY_BLACK;
        ctx.fill(rect, GRAY_WHITE);
        let pad = 8i32;
        let tx = rect.x as i32 + pad;
        let ty = rect.y as i32 + pad;
        let cww = 85i32;
        let chh = 128i32;
        let thumb = Rect { x: tx as u32, y: ty as u32, w: cww as u32, h: chh as u32 };
        ctx.fill(thumb, GRAY_WHITE);
        if let Some(img) = &self.img {
            ctx.blit(img, self.img_w, self.img_h, PixelFormat::Rgb24, thumb);
        } else {
            ctx.outline(thumb, 2, GRAY_LGRAY);
            ctx.line(tx, ty, tx + cww, ty + chh, 2, GRAY_LGRAY); // diagonal placeholder
            ctx.line(tx + cww, ty, tx, ty + chh, 2, GRAY_LGRAY);
        }
        // Reading progress: black bar at the thumb's bottom edge (the C
        // list branch draws draw_progress_bar over the small cover).
        if self.progress > 0 {
            draw_progress_bar(ctx, tx, ty, cww, chh, self.progress as i32);
        }
        // Title / author beside the thumb (C: DEFAULTFONTB 30 / DEFAULTFONT 24,
        // left-aligned at the C text origin).
        let text_x = tx + cww + 16;
        let max_w = ((rect.x + rect.w) as i32 - pad - text_x).max(64);
        text_left_fit(ctx, text_x, rect.y as i32 + pad + 8, 30.0, &self.title, max_w, col);
        if !self.author.is_empty() {
            text_left_fit(ctx, text_x, rect.y as i32 + pad + 48, 24.0, &self.author, max_w, GRAY_DGRAY);
        }
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let Some(r) = self.rect {
            out.push(r);
        }
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        matches!(self.rect, Some(r) if (x as u32) >= r.x && (x as u32) < r.x + r.w && (y as u32) >= r.y && (y as u32) < r.y + r.h)
    }
}

/// A grid-mode cover tile plus its reading-progress bar (the C
/// draw_thumbnail_fonts grid branch paints draw_progress_bar over the
/// cover's bottom edge).
struct CoverTile {
    cover: Cover,
    progress: u8,
}

impl Widget for CoverTile {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.cover.draw(ctx, rect);
        if self.progress > 0 {
            // The shell's Cover paints its art across the top ~78% of the
            // tile (its letterboxed cover area) — anchor the bar there,
            // mirroring the C eh_cover_rect cover-card bottom edge.
            let img_h = (rect.h as f32 * 0.78) as i32;
            draw_progress_bar(ctx, rect.x as i32, rect.y as i32, rect.w as i32, img_h, self.progress as i32);
        }
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        self.cover.dirty(out);
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        self.cover.hit(x, y)
    }
}

/// Bar height by cover width (C draw_progress_bar): 10px on covers ≥150px
/// wide, 6px on small thumbs.
pub fn progress_bar_h(width: i32) -> i32 {
    if width >= 150 { 10 } else { 6 }
}

/// Inner fill width for `pct` (C: fill = cw*pct/100, drawn only once it
/// leaves a ≥1px white margin on each side).
pub fn progress_fill_w(width: i32, pct: i32) -> i32 {
    width * pct.clamp(0, 100) / 100
}

/// Reading-progress bar inside the bottom edge of a cover (port of C
/// eh_grid.c draw_progress_bar): a thin white track with black outline and
/// a black fill proportional to the percent read (0..100).
fn draw_progress_bar(ctx: &mut DrawCtx, x: i32, y: i32, w: i32, h: i32, pct: i32) {
    if w <= 0 || h <= 0 || x < 0 || y < 0 {
        return;
    }
    let bar_h = progress_bar_h(w).min(h);
    let by = y + h - bar_h;
    let track = Rect { x: x as u32, y: by as u32, w: w as u32, h: bar_h as u32 };
    ctx.fill(track, GRAY_WHITE);
    ctx.outline(track, 1, GRAY_BLACK);
    let fill = progress_fill_w(w, pct);
    if fill >= 2 && bar_h >= 3 {
        ctx.fill(
            Rect { x: x as u32 + 1, y: by as u32 + 1, w: (fill - 2) as u32, h: (bar_h - 2) as u32 },
            GRAY_BLACK,
        );
    }
}

/// Build the full shelf screen: top bar + cover grid + pager, laid out as a
/// vertical column that reserves the chrome bands at top/bottom.  `content_h`
/// is the band the column must fit inside (the screen height minus the
/// self-drawn status strip) — the rows are sized against it so the pager
/// always keeps its band (a too-tall grid would overflow and collapse it).
pub fn build_shelf<B: Framebuffer>(
    fb: B,
    title: &str,
    page: usize,
    pages: usize,
    entries: &[ShelfEntry],
    content_h: u32,
    view_mode: crate::app::ViewMode,
    back: bool,
    source: crate::app::Source,
    search_tab: bool,
    syncing: bool,
    // Current sync-glyph rotation (deg) — 0 idle, advancing while a
    // sync/download is in flight (C sync_spin_tick).
    sync_angle: i32,
) -> Screen<B> {
    let font = load_font();
    let mut screen = Screen::new(fb, font);
    // Root is a vertical column: [topbar, grid, pager].
    screen.layout_mut().root_flex_column();

    // --- top bar band ---
    let tb_state = crate::appui::TopBarState {
        back,
        source,
        view_mode,
        search: search_tab,
        syncing,
        sync_angle,
        title: title.to_string(),
    };
    screen.add_styled(
        Box::new(TopBar::new(tb_state)),
        Style {
            flex_shrink: 0.0,
            size: taffy::geometry::Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(TOP_BAR_H as f32),
            },
            ..Style::default()
        },
    );

    // --- grid container (fills remaining vertical space, wraps covers) ---
    let screen_w = screen.framebuffer().screen().width;
    // --- grid container (fills remaining vertical space, wraps covers).
    // The 8px side margin is the C grid's left/right inset: cells sized
    // 1/cols of (w-16) then land exactly on the C tile rects.
    let grid = screen.add_container(Style {
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_wrap: taffy::style::FlexWrap::Wrap,
        align_items: Some(taffy::style::AlignItems::FLEX_START),
        padding: taffy::geometry::Rect {
            left: taffy::style::LengthPercentage::length(8.0),
            right: taffy::style::LengthPercentage::length(8.0),
            top: taffy::style::LengthPercentage::length(0.0),
            bottom: taffy::style::LengthPercentage::length(0.0),
        },
        ..Style::default()
    });

    // Grid mode mirrors the C eh_grid_geom exactly: the mode-aware
    // column/row counts (3×2 on the standard panel), clamped cell sizes
    // and an 8px side margin — the tile rects the e2e taps assume.  List
    // mode is a single full-width column of fixed-height (LIST_ROW_H) rows.
    let grid_h = (content_h as i32 - TOP_BAR_H as i32 - PAGER_H as i32).max(1) as u32;
    let g = grid_geom(screen_w, content_h);
    match view_mode {
        crate::app::ViewMode::Grid => {
            let cols = g.cols;
            let row_h = if entries.is_empty() { grid_h } else { g.cell_h };
            for e in entries {
                if e.stack {
                    let mut c = StackCard::new(e.stack_label.clone(), e.stack_count);
                    if let Some((rgb, w, h)) = &e.art {
                        c.set_image(rgb.clone(), *w, *h);
                    }
                    let style = Style {
                        size: taffy::geometry::Size {
                            width: Dimension::percent(1.0 / cols as f32),
                            height: Dimension::length(row_h as f32),
                        },
                        ..Style::default()
                    };
                    screen.add_to(grid, Box::new(c), style);
                    continue;
                }
                let mut c = Cover::new(e.book.title.clone());
                c.author = e.book.author.clone();
                c.title_size = 18.0;
                c.author_size = 15.0;
                if let Some((rgb, w, h)) = &e.art {
                    c.set_image(rgb.clone(), *w, *h);
                }
                let style = Style {
                    size: taffy::geometry::Size {
                        width: Dimension::percent(1.0 / cols as f32),
                        height: Dimension::length(row_h as f32),
                    },
                    ..Style::default()
                };
                let tile = CoverTile { cover: c, progress: e.progress };
                screen.add_to(grid, Box::new(tile), style);
            }
        }
        crate::app::ViewMode::List => {
            for e in entries {
                let mut c = ListCell::new(e.book.title.clone());
                c.author = e.book.author.clone();
                c.progress = e.progress;
                if let Some((rgb, w, h)) = &e.art {
                    c.set_image(rgb.clone(), *w, *h);
                }
                let style = Style {
                    size: taffy::geometry::Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(LIST_ROW_H as f32),
                    },
                    ..Style::default()
                };
                screen.add_to(grid, Box::new(c), style);
            }
        }
    }

    // --- pager band ---
    screen.add_styled(
        Box::new(Pager::new(page, pages)),
        Style {
            flex_shrink: 0.0,
            size: taffy::geometry::Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(PAGER_H as f32),
            },
            ..Style::default()
        },
    );
    screen
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
            ShelfEntry { book, art, stack: false, stack_label: String::new(), stack_count: 0, stack_scope: String::new(), progress: 0 }
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

/// Column count for the active breakpoint (same table as the demo).
pub fn columns_for(bp: eh_layout::Breakpoint) -> u32 {
    match bp {
        eh_layout::Breakpoint::Narrow => 2,
        eh_layout::Breakpoint::Std => 3,
        eh_layout::Breakpoint::Wide => 4,
    }
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
    if avail_h >= 3 * CELL_MIN_H + 560 { 3 } else { 2 }
}

/// The active grid's column/row counts and clamped cell size (C
/// eh_view_cols × eh_view_rows × eh_grid_geom, grid mode).
pub fn grid_geom(screen_w: u32, content_bottom: u32) -> GridGeom {
    let avail_w = screen_w.saturating_sub(16);
    let bot = content_bottom.saturating_sub(PAGER_H);
    let avail_h = bot.saturating_sub(TOP_BAR_H + crate::appui::TOP_BAR_PAD + 8);
    let cols = grid_cols(avail_w);
    let rows = grid_rows(avail_h);
    let cell_w = (avail_w / cols).clamp(CELL_MIN_W, CELL_MAX_W);
    let cell_h = (avail_h / rows).clamp(CELL_MIN_H, CELL_MAX_H);
    GridGeom { cols, rows, cell_w, cell_h }
}

#[derive(Clone, Copy, Debug)]
pub struct GridGeom {
    pub cols: u32,
    pub rows: u32,
    pub cell_w: u32,
    pub cell_h: u32,
}

pub(crate) fn load_font() -> &'static Font {
    let f = Font::from_bytes(include_bytes!("../../../fonts/DejaVuSans.ttf"))
        .expect("embed font");
    Box::leak(Box::new(f))
}

/// The font the shelf uses (exposed for the status strip / top bar).
pub fn shelf_font() -> &'static Font {
    load_font()
}

/// The Search sub-page: top bar (back + "Search"), the input row, a column
/// of `history` rows, and the pager.  `content_h` is the band height; rows
/// are the fixed C history-row height (96px).  Built exactly like the shelf
/// so the pager keeps its band.
pub fn build_search<B: Framebuffer>(
    fb: B,
    query: &str,
    page: usize,
    pages: usize,
    history: &[String],
    _content_h: u32,
    syncing: bool,
    search_kb: bool,
) -> Screen<B> {
    let font = load_font();
    let mut screen = Screen::new(fb, font);
    screen.layout_mut().root_flex_column();

    // Top bar: back chevron + centered "Search" title (no source/rights).
    let tb_state = crate::appui::TopBarState {
        back: true,
        source: crate::app::Source::Kavita,
        view_mode: crate::app::ViewMode::Grid,
        search: true,
        syncing,
        sync_angle: 0,
        title: crate::i18n::tr("tab.search").to_string(),
    };
    screen.add_styled(
        Box::new(TopBar::new(tb_state)),
        Style {
            flex_shrink: 0.0,
            size: taffy::geometry::Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(TOP_BAR_H as f32),
            },
            ..Style::default()
        },
    );

    // Input row with keyboard state.
    let si = if search_kb { SearchInput::new_active(query) } else { SearchInput::new(query) };
    screen.add_styled(
        Box::new(si),
        Style {
            flex_shrink: 0.0,
            size: taffy::geometry::Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(88.0),
            },
            ..Style::default()
        },
    );

    let body = screen.add_container(Style {
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_wrap: taffy::style::FlexWrap::Wrap,
        // Rows keep their fixed 96px height: without this the wrap
        // container's default cross-axis stretch blows a short history
        // list up over the whole body (a tap anywhere then commits).
        align_items: Some(taffy::style::AlignItems::FLEX_START),
        ..Style::default()
    });
    if history.is_empty() {
        let mut w = HistoryRow::new(crate::i18n::tr("search.empty"));
        w.hint = true;
        screen.add_to(
            body,
            Box::new(w),
            Style {
                size: taffy::geometry::Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(96.0),
                },
                ..Style::default()
            },
        );
    } else {
        for term in history {
            screen.add_to(
                body,
                Box::new(HistoryRow::new(term.clone())),
                Style {
                    size: taffy::geometry::Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(96.0),
                    },
                    ..Style::default()
                },
            );
        }
    }

    // Pager band.
    screen.add_styled(
        Box::new(Pager::new(page, pages)),
        Style {
            flex_shrink: 0.0,
            size: taffy::geometry::Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(PAGER_H as f32),
            },
            ..Style::default()
        },
    );
    screen
}
/// Left-aligned text truncated (with "…") to fit `max_w` px (the C app's
/// eh_utf8_fit_width for list rows, which are left-anchored).
fn text_left_fit(ctx: &mut DrawCtx, x: i32, baseline: i32, size: f32, s: &str, max_w: i32, gray: u8) {
    if ctx.font.width(s, size) as i32 <= max_w {
        ctx.text(x, baseline, size, s, gray);
        return;
    }
    let ell = ctx.font.width("…", size);
    let budget = max_w as f32 - ell;
    let mut shown = s.to_string();
    while ctx.font.width(&shown, size) > budget && !shown.is_empty() {
        shown.pop();
    }
    ctx.text(x, baseline, size, &format!("{shown}…"), gray);
}

#[cfg(test)]
mod progress_bar_tests {
    use super::*;

    #[test]
    fn bar_height_switches_at_150px() {
        // C draw_progress_bar: 10px on wide covers, 6px on small thumbs.
        assert_eq!(progress_bar_h(150), 10);
        assert_eq!(progress_bar_h(400), 10);
        assert_eq!(progress_bar_h(149), 6);
        assert_eq!(progress_bar_h(85), 6);
    }

    #[test]
    fn fill_width_is_proportional_and_clamped() {
        // C: fill = cw * pct / 100 (integer division floors).
        assert_eq!(progress_fill_w(300, 50), 150);
        assert_eq!(progress_fill_w(300, 33), 99);
        assert_eq!(progress_fill_w(85, 100), 85);
        assert_eq!(progress_fill_w(85, 0), 0);
        // Out-of-range percents clamp before scaling (C clamps first).
        assert_eq!(progress_fill_w(300, -20), 0);
        assert_eq!(progress_fill_w(300, 140), 300);
    }

    #[test]
    fn fill_never_leaves_the_track() {
        for w in [1i32, 6, 85, 149, 150, 280, 420] {
            let f = progress_fill_w(w, 100);
            assert!(f <= w);
            // The drawn inner fill keeps a ≥1px white margin per side.
            if f >= 2 {
                assert!(f - 2 <= w - 2);
            }
        }
    }
}
