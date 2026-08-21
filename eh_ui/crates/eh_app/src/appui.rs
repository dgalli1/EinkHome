//! App UI chrome: the top bar and pager as shell widgets.
//!
//! Ports the C app's eh_topbar.c / eh_draw_pager structure onto the Rust
//! shell: a white top bar (house or back chevron, source button, centered
//! title, and the right icon stack — search / layout / sync / menu — with
//! vertical separators) and a pager band (top border, "<" "<<" "N / M"
//! ">>" ">", the four C pager buttons with the disabled state greyed).
//!
//! Geometry mirrors eh_topbar.c / eh_core.h verbatim (the C app is the
//! reference model): fixed 96px buttons, 8px pad, source button at x=112
//! w=176 (the ≤758px growth is skipped — the device/emulator panels are
//! ≥1072px), right icon stack packed from the right edge.  The hit-testing
//! in app.rs `tap_top_bar` shares these same numbers so taps land on the
//! icons exactly as drawn.

use eh_hal::Rect;
use eh_shell::{DrawCtx, GRAY_BLACK, GRAY_DGRAY, GRAY_LGRAY, GRAY_WHITE, Widget};

use crate::app::{Source, ViewMode};

/// Layout constants (mirror eh_core.h).
pub const TOP_BAR_H: u32 = 96;
pub const PAGER_H: u32 = 96;
pub const BTN_SIZE: u32 = 96;
pub const BTN_PAD: u32 = 8;
pub const TOP_BAR_PAD: u32 = 12;
/// Source button geometry (fixed width on standard panels; the 6-inch
/// ≤758px growth is skipped as panels here are ≥1072px).
pub const SOURCE_BTN_X: i32 = 112;
pub const SOURCE_BTN_W: i32 = 176;
const TOP_ICON_HALF: i32 = 26; // EH_TOP_ICON_SIZE/2

/// Everything the top bar needs to draw one frame, driven by app state at
/// build time (the C app reads eh_g_state in eh_draw_top_bar).
pub struct TopBarState {
    /// Draw a back chevron (search tab / drilled) instead of the house.
    pub back: bool,
    pub source: Source,
    pub view_mode: ViewMode,
    /// On the search tab: only the back affordance + centered "Search"
    /// title; the source button and right icon stack are hidden.
    pub search: bool,
    pub syncing: bool,
    pub title: String,
}

/// The white top bar.  See `TopBarState` for what it draws per frame.
pub struct TopBar {
    pub state: TopBarState,
    rect: Option<Rect>,
}

impl TopBar {
    pub fn new(state: TopBarState) -> Self {
        Self { state, rect: None }
    }
}

impl Widget for TopBar {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        let col = GRAY_BLACK;
        // White bar + bottom separator.
        ctx.fill(rect, GRAY_WHITE);
        ctx.hline(0, rect.y + rect.h - 2, rect.w, 2, col);

        let cy = (rect.y + rect.h / 2) as i32;
        let y0 = rect.y as i32;
        let w = rect.w as i32;

        // Left button: back chevron (search / drilled) or house.
        let hcx = (BTN_PAD + BTN_SIZE / 2) as i32;
        if self.state.back {
            draw_back_chevron(ctx, hcx, cy, col);
        } else {
            draw_house(ctx, hcx, cy, col);
        }

        // Source button (+ label), hidden on the search tab.
        if !self.state.search {
            draw_source_button(ctx, w, y0, cy, self.state.source, col);
        }

        // Centered title: "Search" centered on the whole width on the search
        // tab; else centered in the free band between the flanking icon
        // stacks (left = pad+btn+pad+source; right = pad+4*btn).
        let (center, budget) = if self.state.search {
            let guard = (BTN_PAD + BTN_SIZE + BTN_PAD) as i32;
            (w / 2, w - 2 * guard)
        } else {
            let left_w = (BTN_PAD + BTN_SIZE + BTN_PAD) as i32 + SOURCE_BTN_W;
            let right_w = (BTN_PAD + 4 * BTN_SIZE) as i32;
            let band_w = (w - left_w - right_w).max(64);
            (left_w + band_w / 2, band_w)
        };
        if !self.state.title.is_empty() {
            // C draws the 40px title with its TOP at y0+(TOP_BAR_H-40)/2
            // (DrawString is top-anchored); our draw_text takes the
            // BASELINE, so add the ascent (~40px at 44px).
            ctx.text_center_fit(center, y0 + 28 + 40, 44.0, &self.state.title, budget, col);
        }

        // Right icon stack, hidden on the search tab.
        if !self.state.search {
            draw_search_icon(ctx, w - 344, cy, col);
            draw_layout_icon(ctx, w - 248, cy, self.state.view_mode, col);
            draw_sync_icon(ctx, w - 152, cy, self.state.syncing, col);
            // Menu hamburger in the corner button.
            let menu_cx = (rect.x + rect.w - BTN_PAD - BTN_SIZE / 2) as i32;
            ctx.fill(Rect::from_xy(menu_cx - 24, cy - 21, 48, 6), col);
            ctx.fill(Rect::from_xy(menu_cx - 24, cy - 3, 48, 6), col);
            ctx.fill(Rect::from_xy(menu_cx - 24, cy + 15, 48, 6), col);
        }

        // Vertical separators (drawn last so no button's white fill covers
        // them): after the left button, after the source button, and the
        // left edges of the four right buttons.
        if !self.state.search {
            ctx.vline((BTN_PAD + BTN_SIZE + 4) as u32, rect.y, rect.h, 2, col);
            ctx.vline((SOURCE_BTN_X + SOURCE_BTN_W) as u32, rect.y, rect.h, 2, col);
            for k in 1..=4 {
                let x = w - (BTN_PAD + k * BTN_SIZE) as i32;
                ctx.vline(x as u32, rect.y, rect.h, 2, col);
            }
        }
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let Some(r) = self.rect {
            out.push(r);
        }
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        let w = self.rect.map_or(0, |r| r.w as i32);
        matches!(self.rect, Some(r) if (x as u32) >= r.x && (x as u32) < r.x + r.w && (y as u32) >= r.y && (y as u32) < r.y + r.h)
            && w > 0
    }
}

/// The source button (C draw_source_button): a white fill over the button
/// band, the source's line-art icon in the common 52px box, + its label.
fn draw_source_button(ctx: &mut DrawCtx, _w: i32, _y0: i32, cy: i32, source: Source, col: u8) {
    let x0 = SOURCE_BTN_X;
    let btn_w = SOURCE_BTN_W;
    ctx.fill(Rect { x: x0 as u32, y: 0, w: btn_w as u32, h: TOP_BAR_H }, GRAY_WHITE);
    let ic_x = x0 + 8;
    let ic_y = cy - TOP_ICON_HALF;
    match source {
        Source::Kavita => draw_globe_icon(ctx, ic_x, ic_y, col),
        Source::Local => draw_book_icon(ctx, ic_x, ic_y, col),
        Source::Folder => draw_folder_icon(ctx, ic_x, ic_y, col),
    }
    ctx.text(ic_x + 60, cy - 15, 30.0, source_label(source), col);
}

/// Short label of the active source (C source_short_label).
fn source_label(source: Source) -> &'static str {
    match source {
        Source::Local => "Local",
        Source::Folder => "Folder",
        Source::Kavita => "Kavita",
    }
}

/// Line-art globe (Kavita): circle + equator + meridian.
fn draw_globe_icon(ctx: &mut DrawCtx, x: i32, y: i32, col: u8) {
    let cx = x + TOP_ICON_HALF;
    let cy = y + TOP_ICON_HALF;
    let r = 24;
    circle_outline(ctx, cx, cy, r, col);
    // equator + meridian (ellipses through the centre).
    ellipse_piece(ctx, cx, cy, r, r * 42 / 100, true, col);
    ellipse_piece(ctx, cx, cy, r * 42 / 100, r, false, col);
}

/// A horizontal (eq=true) or vertical (eq=false) ellipse segment set.
fn ellipse_piece(ctx: &mut DrawCtx, cx: i32, cy: i32, rx: i32, ry: i32, _eq: bool, col: u8) {
    let n = 32;
    let mut px = 0i32;
    let mut py = 0i32;
    let mut first = true;
    for s in 0..=n {
        let a = (s as f64) * std::f64::consts::TAU / (n as f64);
        let xx = cx + (rx as f64 * a.cos()).round() as i32;
        let yy = cy + (ry as f64 * a.sin()).round() as i32;
        if !first {
            ctx.line(px, py, xx, yy, 2, col);
        }
        px = xx;
        py = yy;
        first = false;
    }
}

/// Line-art open book (Local): two pages over a spine.
fn draw_book_icon(ctx: &mut DrawCtx, x: i32, y: i32, col: u8) {
    let cx = x + TOP_ICON_HALF;
    let cy = y + TOP_ICON_HALF;
    ctx.line(cx - 24, cy + 20, cx - 24, cy - 16, 2, col);
    ctx.line(cx - 24, cy - 16, cx, cy - 6, 2, col);
    ctx.line(cx + 24, cy + 20, cx + 24, cy - 16, 2, col);
    ctx.line(cx + 24, cy - 16, cx, cy - 6, 2, col);
    ctx.line(cx - 24, cy + 20, cx, cy + 24, 2, col);
    ctx.line(cx + 24, cy + 20, cx, cy + 24, 2, col);
}

/// Line-art folder (Folder source): tab + body.
fn draw_folder_icon(ctx: &mut DrawCtx, x: i32, y: i32, col: u8) {
    ctx.line(x + 3, y + 10, x + 3, y + 50, 2, col);
    ctx.line(x + 3, y + 50, x + 49, y + 50, 2, col);
    ctx.line(x + 49, y + 50, x + 49, y + 10, 2, col);
    ctx.line(x + 49, y + 10, x + 21, y + 10, 2, col);
    ctx.line(x + 21, y + 10, x + 21, y + 4, 2, col);
    ctx.line(x + 21, y + 4, x + 3, y + 4, 2, col);
    ctx.line(x + 3, y + 4, x + 3, y + 10, 2, col);
}

/// Magnifying-glass icon (opens the Search sub-page): ring + handle.
fn draw_search_icon(ctx: &mut DrawCtx, cx0: i32, cy: i32, col: u8) {
    let cx = cx0 - 5;
    let cyy = cy - 5;
    let r = 20;
    circle_outline(ctx, cx, cyy, r, col);
    ctx.line(cx + r - 4, cyy + r - 4, cx + r + 10, cyy + r + 10, 2, col);
    ctx.line(cx + r - 3, cyy + r - 5, cx + r + 11, cyy + r + 9, 2, col);
}

/// Layout-switch icon: a 2×2 grid in grid mode, three rows with leading
/// squares in list mode (the glyph reflects the CURRENT layout).
fn draw_layout_icon(ctx: &mut DrawCtx, cx0: i32, cy: i32, view_mode: ViewMode, col: u8) {
    let cx = cx0;
    if view_mode == ViewMode::List {
        for i in 0..3 {
            let ry = cy - 16 + i * 16;
            ctx.outline(Rect { x: (cx - 18) as u32, y: ry as u32, w: 14, h: 13 }, 2, col);
            ctx.line(cx - 1, ry, cx + 22, ry, 2, col);
        }
    } else {
        for r in 0..2 {
            for c in 0..2 {
                ctx.outline(
                    Rect {
                        x: (cx - 23 + c * 26) as u32,
                        y: (cy - 23 + r * 26) as u32,
                        w: 20,
                        h: 20,
                    },
                    2,
                    col,
                );
            }
        }
    }
}

/// Sync (refresh) button left of the menu: two arc arrows.  A stable glyph
/// when idle; the C app rotates while a sync/download is in flight.
fn draw_sync_icon(ctx: &mut DrawCtx, cx0: i32, cy: i32, active: bool, col: u8) {
    let r = 22;
    // A continuous double-arrow arc: two opposing 120° arcs (C: half*180°),
    // each with an arrowhead at its end.
    let _ = active;
    for half in 0..2 {
        let a0 = (half * 180) as f64; // degrees
        let mut px = 0i32;
        let mut py = 0i32;
        let mut ex = 0i32;
        let mut ey = 0i32;
        for s in 0..=8 {
            let a = (a0 + (s as f64) * 15.0).to_radians();
            let x = cx0 + (r as f64 * a.cos()).round() as i32;
            let y = cy + (r as f64 * a.sin()).round() as i32;
            if s > 0 {
                ctx.line(px, py, x, y, 2, col);
            }
            px = x;
            py = y;
            if s == 8 {
                ex = x;
                ey = y;
            }
        }
        // Arrowhead: two ticks trailing the tangent at the arc end.
        let ta = (a0 + 120.0).to_radians() + std::f64::consts::FRAC_PI_2;
        for t in 0..2 {
            let ha = ta + std::f64::consts::PI + if t == 0 { 0.6 } else { -0.6 };
            ctx.line(ex, ey, ex + (11.0 * ha.cos()).round() as i32, ey + (11.0 * ha.sin()).round() as i32, 2, col);
        }
    }
}

/// Approximate circle outline (polyline), used by the search + globe icons.
fn circle_outline(ctx: &mut DrawCtx, cx: i32, cy: i32, r: i32, col: u8) {
    let n = 32;
    let mut px = 0i32;
    let mut py = 0i32;
    let mut first = true;
    for s in 0..=n {
        let a = (s as f64) * std::f64::consts::TAU / (n as f64);
        let x = cx + (r as f64 * a.cos()).round() as i32;
        let y = cy + (r as f64 * a.sin()).round() as i32;
        if !first {
            ctx.line(px, py, x, y, 2, col);
        }
        px = x;
        py = y;
        first = false;
    }
}

/// Pager band: top border + centered "N / M" + four 96×64 buttons
/// ("<" prev, "<<" first, ">>" last, ">" next), disabled ones greyed — the
/// C app's eh_draw_pager verbatim.
pub struct Pager {
    pub page: usize,
    pub pages: usize,
    rect: Option<Rect>,
}

impl Pager {
    pub fn new(page: usize, pages: usize) -> Self {
        Self { page, pages, rect: None }
    }
}

impl Widget for Pager {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        ctx.hline(0, rect.y, rect.w, 2, GRAY_BLACK);

        let cy = (rect.y + rect.h / 2) as i32;
        let mid = (rect.x + rect.w / 2) as i32;
        let label = format!("{} / {}", self.page + 1, self.pages.max(1));
        ctx.text_center_fit(mid, cy + 12, 30.0, &label, (rect.w / 2) as i32, GRAY_BLACK);

        // Four 96×64 buttons, the C geometry (x offsets from the band edges).
        let by = (rect.y + (rect.h - 64) / 2) as i32;
        let bh = 64i32;
        let bw = 96i32;
        let gray = GRAY_LGRAY;
        let can_prev = self.page > 0;
        let can_next = self.page + 1 < self.pages;
        draw_pager_button(ctx, (rect.x + 12) as i32, by, bw, bh, "<", can_prev, gray);
        draw_pager_button(ctx, (rect.x + 116) as i32, by, bw, bh, "<<", can_prev, gray);
        draw_pager_button(
            ctx,
            (rect.x + rect.w - 212) as i32,
            by,
            bw,
            bh,
            ">>",
            can_next,
            gray,
        );
        draw_pager_button(
            ctx,
            (rect.x + rect.w - 108) as i32,
            by,
            bw,
            bh,
            ">",
            can_next,
            gray,
        );
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

/// One C pager button: a bordered box with a centered label; disabled state
/// forces the grey label (the C app skips the selected fill and greys the
/// text instead).
fn draw_pager_button(ctx: &mut DrawCtx, x: i32, y: i32, w: i32, h: i32, label: &str, enabled: bool, gray: u8) {
    ctx.outline(Rect { x: x as u32, y: y as u32, w: w as u32, h: h as u32 }, 2, GRAY_BLACK);
    let col = if enabled { GRAY_BLACK } else { gray };
    ctx.text_center_fit(x + w / 2, y + h / 2 + 10, 28.0, label, w - 12, col);
}

/// House outline (the C app's pentagon + door) as Bresenham segments, scaled
/// to the 96px button box.
fn draw_house(ctx: &mut DrawCtx, cx: i32, cy: i32, col: u8) {
    ctx.line(cx - 24, cy + 8, cx - 24, cy + 26, 2, col); // left wall
    ctx.line(cx - 24, cy + 8, cx, cy - 24, 2, col); // roof left
    ctx.line(cx, cy - 24, cx + 24, cy + 8, 2, col); // roof right
    ctx.line(cx + 24, cy + 8, cx + 24, cy + 26, 2, col); // right wall
    // floor with a break for the door
    ctx.line(cx - 24, cy + 26, cx - 8, cy + 26, 2, col);
    ctx.line(cx + 8, cy + 26, cx + 24, cy + 26, 2, col);
    // door
    ctx.line(cx - 8, cy + 26, cx - 8, cy + 12, 2, col);
    ctx.line(cx - 8, cy + 12, cx + 8, cy + 12, 2, col);
    ctx.line(cx + 8, cy + 12, cx + 8, cy + 26, 2, col);
}

/// Left-pointing back chevron (used on drilled/overlay pages).
fn draw_back_chevron(ctx: &mut DrawCtx, cx: i32, cy: i32, col: u8) {
    ctx.line(cx + 12, cy - 18, cx - 12, cy, 3, col);
    ctx.line(cx - 12, cy, cx + 12, cy + 18, 3, col);
}

/// The Search page's input row (C eh_draw_search_tab input row): a bordered
/// bar with a magnifier and the current query (or a placeholder).
/// When `active`, the bar inverts (C search_kb state) — black bg, white glyphs.
pub struct SearchInput {
    pub text: String,
    rect: Option<Rect>,
    active: bool,
}

impl SearchInput {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), rect: None, active: false }
    }
    pub fn new_active(text: impl Into<String>) -> Self {
        Self { text: text.into(), rect: None, active: true }
    }
}

impl Widget for SearchInput {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        let bx = rect.x + 16;
        let by = rect.y + 10;
        let bh = rect.h.saturating_sub(20);
        let bw = rect.w.saturating_sub(32);
        ctx.outline(Rect { x: bx, y: by, w: bw, h: bh }, 2, GRAY_BLACK);
        let fill_col = if self.active { GRAY_BLACK } else { GRAY_WHITE };
        let glyph_col = if self.active { GRAY_WHITE } else { GRAY_BLACK };
        ctx.fill(Rect { x: bx + 1, y: by + 1, w: bw.saturating_sub(2), h: bh.saturating_sub(2) }, fill_col);
        // Magnifier ring + handle.
        let gx = (bx + 30) as i32;
        let gy = (by + bh / 2) as i32;
        circle_outline(ctx, gx, gy, 13, glyph_col);
        ctx.line(gx + 9, gy + 10, gx + 22, gy + 23, 2, glyph_col);
        ctx.line(gx + 10, gy + 9, gx + 23, gy + 22, 2, glyph_col);
        // Query text (or placeholder).
        let text = if self.text.is_empty() { "search…" } else { self.text.as_str() };
        ctx.text((bx + 68) as i32, (by + 18) as i32, 28.0, text, glyph_col);
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let Some(r) = self.rect {
            out.push(r);
        }
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        matches!(self.rect, Some(r) if (x as u32) >= r.x + 16 && (x as u32) < r.x + r.w - 16 && (y as u32) >= r.y + 10 && (y as u32) < r.y + r.h - 10)
    }
}

/// One search-history row (C eh_draw_search_tab history list): a term +
/// light separator.
pub struct HistoryRow {
    pub term: String,
    /// Render as a grey hint (empty-history placeholder) instead of a
    /// tappable history term.
    pub hint: bool,
    pub rect: Option<Rect>,
}

impl HistoryRow {
    pub fn new(term: impl Into<String>) -> Self {
        Self { term: term.into(), hint: false, rect: None }
    }
}

impl Widget for HistoryRow {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        ctx.text(
            (rect.x + 24) as i32,
            (rect.y + 34) as i32,
            28.0,
            &self.term,
            if self.hint { GRAY_DGRAY } else { GRAY_BLACK },
        );
        if rect.h > 0 && !self.hint {
            ctx.hline(rect.x + 20, rect.y + rect.h - 1, rect.w.saturating_sub(40), 2, GRAY_LGRAY);
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