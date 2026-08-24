//! The launcher overlay screen (C eh_launcher.c draw side): one continuous
//! column laid out into `launcher_rects` (parallel to `launcher_items`, so
//! draw and hit share one geometry), the scrolling 3-column grid paint
//! with group headers / icon cells, corner scroll buttons, drag scrolling,
//! and tap → NewTaskEx launch.

use crate::appui::SCROLL_BTN_H;
use eh_hal::{Framebuffer, Rect};

use crate::app::App;

use super::{CELL_H, COLS, GROUP_H, MARGIN};

/// Lay every item out in one continuous column (C eh_launcher_layout):
/// headers span the width, app cells flow `COLS` per row.  `launcher_rects`
/// is parallel to `launcher_items` (C's BsLauncherItem carries its own
/// x/y/w/h), so draw and hit share one geometry.
pub(crate) fn layout<B: Framebuffer>(app: &mut App<B>) {
    let w = app.screen_width();
    let cell_w = (w - 2 * MARGIN) / COLS;
    let mut col = 0u32;
    let mut y = 0i32;
    app.launcher_rects.clear();
    for it in &app.launcher_items {
        if it.group {
            if col > 0 {
                y += CELL_H as i32;
                col = 0;
            }
            app.launcher_rects.push(Rect {
                x: MARGIN,
                y: y as u32,
                w: w - 2 * MARGIN,
                h: GROUP_H,
            });
            y += GROUP_H as i32;
        } else {
            if col >= COLS {
                col = 0;
                y += CELL_H as i32;
            }
            app.launcher_rects.push(Rect {
                x: MARGIN + col * cell_w,
                y: y as u32,
                w: cell_w,
                h: CELL_H,
            });
            col += 1;
        }
    }
    if col > 0 {
        y += CELL_H as i32;
    }
    app.launcher_body_h = y;
}

pub(crate) fn body_rects<B: Framebuffer>(app: &App<B>) -> (u32, u32) {
    // (body_top, body_h): the header band is reserved; a column that
    // overflows reserves the corner scroll-button band too (C
    // launcher_body_h).
    let top = 96u32;
    let mut h = app.content_bottom.saturating_sub(top);
    if (app.launcher_body_h as u32) > h {
        h = h.saturating_sub(SCROLL_BTN_H);
    }
    (top, h)
}

/// The clamped scroll offset + max (C's max_scroll clamp).
pub(crate) fn scroll_of<B: Framebuffer>(app: &App<B>) -> (i32, i32) {
    let (_, body_h) = body_rects(app);
    let max = (app.launcher_body_h - body_h as i32).max(0);
    (app.launcher_scroll.clamp(0, max), max)
}

/// Pointer travel before a drag starts scrolling (C
/// EH_LAUNCHER_DRAG_SLOP): keeps a stationary press's tremor from
/// jittering the list.
pub const DRAG_SLOP: i32 = 24;

/// Feed one pointer-move delta into the scroll offset while a drag is in
/// flight (C eh_main.c drag_scroll_move's scroll update).  The offset is
/// clamped against the SAME geometry the painter clamps with
/// (`scroll_state` → `body_rect`) — never a separate view height — so
/// a held pointer can only change state when the visible scroll actually
/// moves.  Returns true when it did (the caller marks the frame dirty).
pub fn drag_move<B: Framebuffer>(app: &mut App<B>, dy: i32) -> bool {
    let (scroll, max) = scroll_of(app);
    let new = (scroll + dy).clamp(0, max);
    if new != scroll {
        app.launcher_scroll = new;
        return true;
    }
    false
}

/// Split a cell label into up to two centred lines (C
/// launcher_draw_app_label): fits `maxw` -> whole; else split at the
/// last space; else ellipsize.  Returns (line1, line2-or-empty).
pub(crate) fn split_label(text: &str, maxw: i32) -> (String, String) {
    let font = crate::shelf::shelf_font();
    if font.width(text, 24.0) as i32 <= maxw {
        return (text.to_string(), String::new());
    }
    if let Some(sp) = text.rfind(' ') {
        return (text[..sp].to_string(), text[sp + 1..].to_string());
    }
    let mut cut = text.chars().count();
    loop {
        let n: String = text.chars().take(cut).collect();
        let shown = format!("{n}\u{2026}");
        if font.width(&shown, 24.0) as i32 <= maxw || cut == 0 {
            return (shown, String::new());
        }
        cut -= 1;
    }
}
