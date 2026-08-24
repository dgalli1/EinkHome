//! The Search sub-page's row elements (C eh_draw_search_tab): the bordered
//! input bar — magnifier + query or placeholder, inverted while the
//! on-screen keyboard is open ([`SearchInput`]) — and one history row, a
//! term over a light separator ([`HistoryRow`]).  Both are shell
//! [`Widget`]s laid out by `shelf::build_search`.

use eh_hal::Rect;
use eh_shell::{DrawCtx, Widget, GRAY_BLACK, GRAY_DGRAY, GRAY_LGRAY, GRAY_WHITE};

use crate::appui::circle_outline;

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
        Self {
            text: text.into(),
            rect: None,
            active: false,
        }
    }
    pub fn new_active(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            rect: None,
            active: true,
        }
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
        ctx.outline(
            Rect {
                x: bx,
                y: by,
                w: bw,
                h: bh,
            },
            2,
            GRAY_BLACK,
        );
        let fill_col = if self.active { GRAY_BLACK } else { GRAY_WHITE };
        let glyph_col = if self.active { GRAY_WHITE } else { GRAY_BLACK };
        ctx.fill(
            Rect {
                x: bx + 1,
                y: by + 1,
                w: bw.saturating_sub(2),
                h: bh.saturating_sub(2),
            },
            fill_col,
        );
        // Magnifier ring + handle.
        let gx = (bx + 30) as i32;
        let gy = (by + bh / 2) as i32;
        circle_outline(ctx, gx, gy, 13, glyph_col);
        ctx.line(gx + 9, gy + 10, gx + 22, gy + 23, 2, glyph_col);
        ctx.line(gx + 10, gy + 9, gx + 23, gy + 22, 2, glyph_col);
        // Query text (or placeholder), vertically centred in the box
        // (draw_text takes a baseline; C centres the hint in the field).
        let text = if self.text.is_empty() {
            crate::i18n::tr("search.ph")
        } else {
            self.text.as_str()
        };
        ctx.text(
            (bx + 68) as i32,
            (by + bh / 2) as i32,
            28.0,
            text,
            glyph_col,
        );
        // Edit cursor while the keyboard is open (C draw_search_input_text):
        // a white line right after the query.
        if self.active && !self.text.is_empty() {
            let cursor_x = (bx + 68) as i32 + ctx.font.width(text, 28.0) as i32 + 1;
            ctx.vline(cursor_x as u32, by + 6, bh - 12, 2, GRAY_WHITE);
        }
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
        Self {
            term: term.into(),
            hint: false,
            rect: None,
        }
    }
}

impl Widget for HistoryRow {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        if self.hint {
            // The empty-history hint is screen-centred in C
            // (eh_draw_search_tab's "no recent searches" line).
            ctx.text_center(
                rect.x as i32 + rect.w as i32 / 2,
                (rect.y + 34) as i32,
                28.0,
                &self.term,
                GRAY_DGRAY,
            );
        } else {
            ctx.text(
                (rect.x + 24) as i32,
                (rect.y + 34) as i32,
                28.0,
                &self.term,
                GRAY_BLACK,
            );
        }
        if rect.h > 0 && !self.hint {
            ctx.hline(
                rect.x + 20,
                rect.y + rect.h - 1,
                rect.w.saturating_sub(40),
                2,
                GRAY_LGRAY,
            );
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
