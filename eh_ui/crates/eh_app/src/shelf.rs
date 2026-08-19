//! Shelf screen — the real library grid, bound to the store + cover layers.
//!
//! Builds the shell's [`Cover`] widgets from the persisted `books` rows and
//! the on-disk/cover cache, so the grid shows real titles/authors/art.  This
//! is the vertical slice that replaces the demo's placeholder tiles.
//!
//! Cover art is fetched BEFORE building the widgets (the caller passes books
//! + their decoded covers), so the shell stays free of image-lifetime
//! concerns — [`apply_cover_art`] mutates a `Cover` through a downcast the
//! caller drives once.

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
}

/// One stack-card tile (C eh_draw_thumbnail stack branch): a bordered card
/// centred in the tile showing the group label + member count.
pub struct StackCard {
    pub label: String,
    pub count: i64,
    rect: Option<Rect>,
}

impl StackCard {
    pub fn new(label: impl Into<String>, count: i64) -> Self {
        Self { label: label.into(), count, rect: None }
    }
}

impl Widget for StackCard {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        let border = 2u32;
        let pad = 8u32;
        if rect.w > pad * 2 + 4 && rect.h > pad * 2 + 4 {
            ctx.outline(
                Rect { x: rect.x + pad, y: rect.y + pad, w: rect.w - pad * 2, h: rect.h - pad * 2 },
                border,
                GRAY_BLACK,
            );
        }
        let cx = rect.x as i32 + rect.w as i32 / 2;
        let cy = rect.y as i32 + rect.h as i32 / 2;
        let max_w = rect.w.saturating_sub(12) as i32;
        ctx.text_center_fit(cx, cy - 8, 20.0, &self.label, max_w, GRAY_BLACK);
        let sub = format!("{} books", self.count);
        ctx.text_center_fit(cx, cy + 26, 16.0, &sub, max_w, GRAY_DGRAY);
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
    rect: Option<Rect>,
}

impl ListCell {
    pub fn new(title: impl Into<String>) -> Self {
        Self { img: None, img_w: 0, img_h: 0, title: title.into(), author: String::new(), rect: None }
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
    let grid = screen.add_container(Style {
        flex_grow: 1.0,
        flex_shrink: 1.0,
        flex_wrap: taffy::style::FlexWrap::Wrap,
        ..Style::default()
    });

    // Rows sized to the band left between the chrome bands.  Grid mode uses
    // the breakpoint column count and sizes rows to fill the body; list mode
    // is a single full-width column of fixed-height (LIST_ROW_H) rows.
    let grid_h = (content_h as i32 - TOP_BAR_H as i32 - PAGER_H as i32).max(1) as u32;
    match view_mode {
        crate::app::ViewMode::Grid => {
            let cols = columns_for(screen.breakpoint);
            let rows = if entries.is_empty() { 1 } else { (entries.len() as u32 + cols - 1) / cols };
            let row_h = grid_h / rows;
            for e in entries {
                if e.stack {
                    let c = StackCard::new(e.stack_label.clone(), e.stack_count);
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
                screen.add_to(grid, Box::new(c), style);
            }
        }
        crate::app::ViewMode::List => {
            for e in entries {
                let mut c = ListCell::new(e.book.title.clone());
                c.author = e.book.author.clone();
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
            ShelfEntry { book, art, stack: false, stack_label: String::new(), stack_count: 0, stack_scope: String::new() }
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
        title: "Search".to_string(),
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
        ..Style::default()
    });
    if history.is_empty() {
        let mut w = HistoryRow::new("No recent searches");
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
    ctx.text(x, baseline, size, &format!("{}…", shown), gray);
}
