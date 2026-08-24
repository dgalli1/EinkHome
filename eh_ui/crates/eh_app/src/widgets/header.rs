//! The shared overlay header (C eh_draw_overlay_header): a white bar with
//! a bottom rule, the back chevron in the shared touch box, and the
//! centred title.  Every full-screen overlay page (Settings, Applications,
//! the log/licence viewers) draws this band and routes its back affordance
//! through [`back_rect`] — draw and hit share the same numbers.

use eh_hal::Rect;
use eh_render::draw_text;
use eh_shell::{GRAY_BLACK, GRAY_WHITE};

/// Header + back-button rhythm (C EH_OVERLAY_*).
pub const HEADER_H: u32 = 96;
pub const BACK_X: u32 = 8;
pub const BACK_W: u32 = 96;
pub const BACK_H: u32 = 56;

/// Draw the shared overlay header (C eh_draw_overlay_header): white bar,
/// bottom rule, back chevron in the shared touch box, centred title.
pub fn draw_header(surf: &mut eh_render::Surface, title: &str, _dirty: &mut [Rect]) {
    let w = surf.width();
    surf.fill_gray(
        Rect {
            x: 0,
            y: 0,
            w,
            h: HEADER_H,
        },
        GRAY_WHITE,
    );
    surf.hline(0, HEADER_H - 1, w, 1, GRAY_BLACK);
    let bx = BACK_X as i32 + BACK_W as i32 / 2;
    let by = (HEADER_H as i32 - BACK_H as i32) / 2 + BACK_H as i32 / 2;
    draw_back_icon(surf, bx, by, GRAY_BLACK);
    let font = eh_shell::bold_font();
    let mut glyph = eh_render::Glyph::new();
    let tw = font.width(title, 36.0) as i32;
    // C DrawString tops the 36px bold title at (HEADER_H-36)/2; draw_text
    // takes the BASELINE — add the face's ascent.
    let asc = font.line_h(36.0).0 as i32;
    draw_text(
        surf,
        font,
        36.0,
        title,
        (w as i32 - tw) / 2,
        (HEADER_H as i32 - 36) / 2 + asc,
        GRAY_BLACK,
        &mut glyph,
    );
}

/// Left-pointing back chevron (C eh_draw_back_icon: two 2px strokes, 26px
/// arms) — every back affordance shares this glyph.
pub fn draw_back_icon(surf: &mut eh_render::Surface, cx: i32, cy: i32, col: u8) {
    let ax = cx - 8;
    let ay = cy;
    surf.line(ax, ay, ax + 26, ay - 26, 2, col);
    surf.line(ax, ay, ax + 26, ay + 26, 2, col);
    surf.line(ax + 4, ay, ax + 30, ay - 26, 2, col);
    surf.line(ax + 4, ay, ax + 30, ay + 26, 2, col);
}

/// The back-button touch box (C eh_overlay_back_rect).
pub fn back_rect() -> Rect {
    let y = (HEADER_H.saturating_sub(BACK_H)) / 2;
    Rect {
        x: BACK_X,
        y,
        w: BACK_W,
        h: BACK_H,
    }
}
