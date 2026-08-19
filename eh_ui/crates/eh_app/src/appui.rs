//! App UI chrome: the top bar and pager as shell widgets.
//!
//! Ports the C app's eh_topbar.c / eh_draw_pager structure onto the Rust
//! shell: a white top bar (house icon, centered title, right hamburger,
//! vertical separators) and a pager band (top border, "<" "<<" "N / M"
//! ">>" ">", the four C pager buttons with the disabled state greyed).
//! These are app-level widgets built on the shell's [`Widget`] trait.

use eh_hal::Rect;
use eh_shell::{DrawCtx, GRAY_BLACK, GRAY_LGRAY, GRAY_WHITE, Widget};

/// Layout constants (mirror eh_core.h).
pub const TOP_BAR_H: u32 = 96;
pub const PAGER_H: u32 = 96;
pub const BTN_SIZE: u32 = 96;
pub const BTN_PAD: u32 = 8;

/// The white top bar: house icon (left), centered title, hamburger (right)
/// and vertical separators.  The C app's full right icon stack (search /
/// layout / sync) arrives with those features; this slice keeps the house,
/// title and menu, which is what the shelf needs.
pub struct TopBar {
    pub title: String,
    pub back: bool, // draw back-chevron (drilled/overlay) instead of house
    rect: Option<Rect>,
}

impl TopBar {
    pub fn new(title: impl Into<String>) -> Self {
        Self { title: title.into(), back: false, rect: None }
    }
}

impl Widget for TopBar {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        // White bar + bottom separator.
        ctx.fill(rect, GRAY_WHITE);
        ctx.hline(0, rect.y + rect.h - 2, rect.w, 2, GRAY_BLACK);

        let cx = (rect.x + BTN_PAD + BTN_SIZE / 2) as i32;
        let cy = (rect.y + rect.h / 2) as i32;
        if self.back {
            draw_back_chevron(ctx, cx, cy, GRAY_BLACK);
        } else {
            draw_house(ctx, cx, cy, GRAY_BLACK);
        }

        // Centered title, fitted to the space between the flanking buttons.
        let half_w = (rect.w - (BTN_PAD * 2 + BTN_SIZE) * 2) as i32 / 2;
        ctx.text_center_fit(
            (rect.x as i32 + rect.w as i32) / 2,
            cy + 16,
            44.0,
            &self.title,
            half_w.max(10),
            GRAY_BLACK,
        );

        // Right hamburger (3 lines) in the corner button box.
        let menu_cx = (rect.x + rect.w - BTN_PAD - BTN_SIZE / 2) as i32;
        let menu_cy = cy;
        ctx.fill(Rect::from_xy(menu_cx - 24, menu_cy - 21, 48, 6), GRAY_BLACK);
        ctx.fill(Rect::from_xy(menu_cx - 24, menu_cy - 3, 48, 6), GRAY_BLACK);
        ctx.fill(Rect::from_xy(menu_cx - 24, menu_cy + 15, 48, 6), GRAY_BLACK);

        // Vertical separators (C: drawn last so no fill covers them): one
        // after the left button, one before the right button.
        ctx.vline((BTN_PAD + BTN_SIZE + 4) as u32, rect.y as u32, rect.h as u32, 2, GRAY_BLACK);
        ctx.vline(
            rect.x as u32 + rect.w - BTN_PAD - BTN_SIZE - 4,
            rect.y as u32,
            rect.h as u32,
            2,
            GRAY_BLACK,
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