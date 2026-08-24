//! App chrome constants + line-art icon drawing (C eh_core.h / eh_topbar.c
//! geometry, verbatim).  The top bar and pager are Slint markup now
//! (`ui/topbar.slint`, `ui/pager.slint`); these fns survive because the
//! icon baker (`ui/icons.rs`) rasterises their glyphs ONCE at boot into
//! the images the Slint tree displays — pixel-identical to the old
//! per-frame renderer.

use eh_hal::Rect;

use eh_shell::{DrawCtx, GRAY_BLACK};

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
pub(crate) const TOP_ICON_HALF: i32 = 26; // EH_TOP_ICON_SIZE/2

/// Line-art globe (Kavita): circle + equator + meridian.
pub(crate) fn draw_globe_icon(ctx: &mut DrawCtx, x: i32, y: i32, col: u8) {
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
pub(crate) fn draw_book_icon(ctx: &mut DrawCtx, x: i32, y: i32, col: u8) {
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
pub(crate) fn draw_folder_icon(ctx: &mut DrawCtx, x: i32, y: i32, col: u8) {
    ctx.line(x + 3, y + 10, x + 3, y + 50, 2, col);
    ctx.line(x + 3, y + 50, x + 49, y + 50, 2, col);
    ctx.line(x + 49, y + 50, x + 49, y + 10, 2, col);
    ctx.line(x + 49, y + 10, x + 21, y + 10, 2, col);
    ctx.line(x + 21, y + 10, x + 21, y + 4, 2, col);
    ctx.line(x + 21, y + 4, x + 3, y + 4, 2, col);
    ctx.line(x + 3, y + 4, x + 3, y + 10, 2, col);
}

/// Magnifying-glass icon (opens the Search sub-page): ring + handle.
pub(crate) fn draw_search_icon(ctx: &mut DrawCtx, cx0: i32, cy: i32, col: u8) {
    let cx = cx0 - 5;
    let cyy = cy - 5;
    let r = 20;
    circle_outline(ctx, cx, cyy, r, col);
    ctx.line(cx + r - 4, cyy + r - 4, cx + r + 10, cyy + r + 10, 2, col);
    ctx.line(cx + r - 3, cyy + r - 5, cx + r + 11, cyy + r + 9, 2, col);
}

/// Layout-switch icon: a 2×2 grid in grid mode, three rows with leading
/// squares in list mode (the glyph reflects the CURRENT layout).
pub(crate) fn draw_layout_icon(ctx: &mut DrawCtx, cx0: i32, cy: i32, view_mode: ViewMode, col: u8) {
    let cx = cx0;
    if view_mode == ViewMode::List {
        for i in 0..3 {
            let ry = cy - 16 + i * 16;
            ctx.outline(
                Rect {
                    x: (cx - 18) as u32,
                    y: ry as u32,
                    w: 14,
                    h: 13,
                },
                2,
                col,
            );
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

/// Sync (refresh) button left of the menu: two arc arrows.  `angle` is
/// the current rotation in degrees — idle the app passes 0 (a stable
/// glyph); while a sync/download is in flight the tick advances it 15°/s
/// and the arcs spin (C eh_draw_sync_icon / sync_spin_tick).
pub(crate) fn draw_sync_icon(ctx: &mut DrawCtx, cx0: i32, cy: i32, angle: i32, col: u8) {
    let r = 22;
    // A continuous double-arrow arc: two opposing 120° arcs (C: half*180°),
    // each with an arrowhead at its end.
    for half in 0..2 {
        let a0 = ((angle % 360) + half * 180) as f64; // degrees
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
            ctx.line(
                ex,
                ey,
                ex + (11.0 * ha.cos()).round() as i32,
                ey + (11.0 * ha.sin()).round() as i32,
                2,
                col,
            );
        }
    }
}

/// Approximate circle outline (polyline), used by the search + globe icons
/// and the SearchInput magnifier.
pub(crate) fn circle_outline(ctx: &mut DrawCtx, cx: i32, cy: i32, r: i32, col: u8) {
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

/// House outline (the C app's pentagon + door) as Bresenham segments, scaled
/// to the 96px button box.
pub(crate) fn draw_house(ctx: &mut DrawCtx, cx: i32, cy: i32, col: u8) {
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
pub(crate) fn draw_back_chevron(ctx: &mut DrawCtx, cx: i32, cy: i32, col: u8) {
    ctx.line(cx + 12, cy - 18, cx - 12, cy, 3, col);
    ctx.line(cx - 12, cy, cx + 12, cy + 18, 3, col);
}

// The Source import survives for the icon dispatch sites (ui/icons.rs
// matches on the active source to pick the baked glyph).
#[allow(unused)]
fn _source_type_assert(_: Source) {}
