//! The launcher overlay screen (C eh_launcher.c draw side): one continuous
//! column laid out into `launcher_rects` (parallel to `launcher_items`, so
//! draw and hit share one geometry), the scrolling 3-column grid paint
//! with group headers / icon cells, corner scroll buttons, drag scrolling,
//! and tap → NewTaskEx launch.

use eh_hal::{Framebuffer, Rect};
use eh_render::draw_text;
use eh_shell::{draw_scroll_buttons, hit_scroll_button_at, GRAY_BLACK, GRAY_WHITE, SCROLL_BTN_H};

use crate::app::{App, Overlay};

use super::{CELL_H, COLS, GROUP_H, ICON_SZ, MARGIN};

/// Lay every item out in one continuous column (C eh_launcher_layout):
/// headers span the width, app cells flow `COLS` per row.  `launcher_rects`
/// is parallel to `launcher_items` (C's BsLauncherItem carries its own
/// x/y/w/h), so draw and hit share one geometry.
pub(super) fn layout<B: Framebuffer>(app: &mut App<B>) {
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

fn body_rect<B: Framebuffer>(app: &App<B>) -> (u32, u32) {
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
fn scroll_state<B: Framebuffer>(app: &App<B>) -> (i32, i32) {
    let (_, body_h) = body_rect(app);
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
    let (scroll, max) = scroll_state(app);
    let new = (scroll + dy).clamp(0, max);
    if new != scroll {
        app.launcher_scroll = new;
        return true;
    }
    false
}

/// Draw the launcher overlay.  A row's screen y is `layout_y - scroll +
/// body_top` (C's draw loop).  Rows must fit the visible body outright
/// (C skips any row whose bottom would spill past the body — page scrolls
/// align rows to the body, so the gap is never visible), and the shared
/// header is repainted after the body so a row straddling the body top
/// can never bleed into it (C's SetClip(0, body_top, ...) discipline).
pub fn draw<B: Framebuffer>(
    surf: &mut eh_render::Surface,
    app: &mut App<B>,
    dirty: &mut Vec<Rect>,
) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    crate::widgets::header::draw_header(surf, crate::i18n::tr("launcher.title"), dirty);

    let (body_top, body_h) = body_rect(app);
    let (scroll, max_scroll) = scroll_state(app);
    let body_bottom = body_top as i32 + body_h as i32;
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    let fmt = surf.format();

    if app.launcher_items.is_empty() {
        let msg = crate::i18n::tr("launcher.empty");
        let tw = font.width(msg, 32.0) as i32;
        draw_text(
            surf,
            font,
            32.0,
            msg,
            (w as i32 - tw) / 2,
            (body_top + body_h / 2) as i32,
            GRAY_BLACK,
            &mut glyph,
        );
        return;
    }

    let items: Vec<(Rect, bool, Option<(Vec<u8>, u32, u32)>, String)> = app
        .launcher_items
        .iter()
        .zip(app.launcher_rects.iter())
        .map(|(it, r)| (*r, it.group, it.art.clone(), it.text.clone()))
        .collect();
    for (r, is_group, art, text) in items.iter() {
        let r = *r;
        let sy = r.y as i32 - scroll + body_top as i32;
        if sy + r.h as i32 <= body_top as i32 || sy + r.h as i32 > body_bottom {
            continue;
        }
        if *is_group {
            // Group heading row (C launcher_draw_heading): white band,
            // baseline rule, title at the left.
            surf.fill_gray(
                Rect::from_xy(r.x as i32, sy, r.w as i32, r.h as i32),
                GRAY_WHITE,
            );
            surf.hline(r.x, (sy + r.h as i32 - 2) as u32, r.w, 2, GRAY_BLACK);
            draw_text(
                surf,
                font,
                28.0,
                text,
                (r.x + 12) as i32,
                sy + (r.h as i32) / 2 + 10,
                GRAY_BLACK,
                &mut glyph,
            );
        } else {
            let cx = (r.x + r.w / 2) as i32;
            let icon_cy = sy + 12 + (ICON_SZ as i32) / 2;
            draw_icon(surf, art.as_ref(), cx, icon_cy, text, font, &mut glyph, fmt);
            let ly = sy + 12 + ICON_SZ as i32 + 8;
            let maxw = r.w as i32 - 8;
            draw_label(surf, font, &mut glyph, text, cx, ly, maxw);
        }
    }

    // The row loop has no Surface clip (the C SetClip is unreliable on
    // some SDK paths anyway): a row scrolled part-way past the body top
    // paints into the header band, so repaint the shared header over it
    // once the body is done (C resets the clip and draws the buttons
    // after; the header band and the button band never overlap).
    crate::widgets::header::draw_header(surf, crate::i18n::tr("launcher.title"), dirty);
    // Corner scroll buttons while the column overflows (C
    // eh_draw_scroll_buttons: bottom-left up / bottom-right down).  The
    // shell helper no-ops when neither direction can move.
    draw_scroll_buttons(surf, h, scroll > 0, scroll < max_scroll);
}

/// One launcher icon: the art resolved at build() time (see the
/// discovery half's resolve_icon_art) or the C app's single-letter
/// placeholder box.
fn draw_icon(
    surf: &mut eh_render::Surface,
    art: Option<&(Vec<u8>, u32, u32)>,
    cx: i32,
    cy: i32,
    title: &str,
    font: &eh_render::Font,
    glyph: &mut eh_render::Glyph,
    fmt: eh_hal::PixelFormat,
) {
    let x0 = cx - (ICON_SZ as i32) / 2;
    let y0 = cy - (ICON_SZ as i32) / 2;
    let mut ok = false;
    if let Some((rgb, iw, ih)) = art {
        // Scale down oversized icons aspect-preserving (C
        // launcher_draw_bitmap).
        let iw = *iw;
        let ih = *ih;
        let (bw, bh) = if iw > ICON_SZ || ih > ICON_SZ {
            if iw > ih {
                (ICON_SZ, ih * ICON_SZ / ih.max(1))
            } else {
                (iw * ICON_SZ / ih.max(1), ICON_SZ)
            }
        } else {
            (iw, ih)
        };
        surf.blit_image(
            rgb,
            iw,
            ih,
            fmt,
            Rect::from_xy(
                x0 + ((ICON_SZ as i32 - bw as i32) / 2),
                y0 + ((ICON_SZ as i32 - bh as i32) / 2),
                bw as i32,
                bh as i32,
            ),
        );
        ok = true;
    }
    if !ok {
        surf.fill_gray(
            Rect::from_xy(x0, y0, ICON_SZ as i32, ICON_SZ as i32),
            GRAY_WHITE,
        );
        surf.rect_outline(
            Rect::from_xy(x0, y0, ICON_SZ as i32, ICON_SZ as i32),
            2,
            GRAY_BLACK,
        );
        if let Some(ch) = title.chars().next() {
            let s = ch.to_string();
            let tw = font.width(&s, 56.0) as i32;
            draw_text(
                surf,
                font,
                56.0,
                &s,
                cx - tw / 2,
                cy + 20,
                GRAY_BLACK,
                glyph,
            );
        }
    }
}

/// Center the app label in the cell, wrapping to two lines at the last
/// space or ellipsizing (C launcher_draw_app_label's fallbacks).
fn draw_label(
    surf: &mut eh_render::Surface,
    font: &eh_render::Font,
    glyph: &mut eh_render::Glyph,
    text: &str,
    cx: i32,
    c_top: i32,
    maxw: i32,
) {
    // C launcher_draw_app_label passes the glyph TOP; draw_text takes a
    // baseline — convert once (24px font ascent ≈ 20px) so the label
    // sits below the icon box instead of overlapping it.
    let ly = c_top + 20;
    if font.width(text, 24.0) as i32 <= maxw {
        let tw = font.width(text, 24.0) as i32;
        draw_text(surf, font, 24.0, text, cx - tw / 2, ly, GRAY_BLACK, glyph);
        return;
    }
    if let Some(sp) = text.rfind(' ') {
        let l1 = &text[..sp];
        let l2 = &text[sp + 1..];
        let t1 = font.width(l1, 24.0) as i32;
        let t2 = font.width(l2, 24.0) as i32;
        draw_text(surf, font, 24.0, l1, cx - t1 / 2, ly, GRAY_BLACK, glyph);
        draw_text(
            surf,
            font,
            24.0,
            l2,
            cx - t2 / 2,
            ly + 26,
            GRAY_BLACK,
            glyph,
        );
        return;
    }
    let mut cut = text.chars().count();
    loop {
        let n: String = text.chars().take(cut).collect();
        let shown = format!("{n}…");
        if font.width(&shown, 24.0) as i32 <= maxw || cut == 0 {
            let tw = font.width(&shown, 24.0) as i32;
            draw_text(surf, font, 24.0, &shown, cx - tw / 2, ly, GRAY_BLACK, glyph);
            return;
        }
        cut -= 1;
    }
}

/// Launcher tap routing (C eh_on_tap_overlay_launcher): back chevron
/// closes; corner buttons page the column; a cell tap launches the app
/// (NewTaskEx via the backend — the launched task draws over the shelf, so
/// the shelf is NOT redrawn before the launch).
pub fn tap_launcher<B: Framebuffer>(x: i32, y: i32, app: &mut App<B>) {
    if crate::widgets::header::back_rect().contains(x, y) {
        app.overlay = Overlay::None;
        app.launcher_rects.clear();
        return;
    }
    let w = app.screen_width();
    // Corner scroll buttons (bottom band): a tap in the band never falls
    // through to the cells, even when it misses both buttons.
    let dir = hit_scroll_button_at(x, y, app.content_bottom.saturating_sub(SCROLL_BTN_H), w);
    if dir != 0 {
        let (scroll, max) = scroll_state(app);
        if max > 0 {
            let (_, body_h) = body_rect(app);
            app.launcher_scroll = (scroll + dir * body_h as i32).clamp(0, max);
        }
        return;
    }
    if y >= (app.content_bottom - SCROLL_BTN_H) as i32 {
        return;
    }
    let (body_top, _) = body_rect(app);
    if y < body_top as i32 || y >= app.content_bottom as i32 {
        return;
    }
    let scroll = scroll_state(app).0;
    // Tap in layout coordinates (C: by = y - body_top + scroll), matching
    // the layout rects.
    let by = y - body_top as i32 + scroll;
    for (i, it) in app.launcher_items.iter().enumerate() {
        if it.group {
            continue;
        }
        let r = app.launcher_rects[i];
        if x >= r.x as i32 && x < (r.x + r.w) as i32 && by >= r.y as i32 && by < (r.y + r.h) as i32
        {
            let it = it.clone();
            app.overlay = Overlay::None;
            app.launcher_rects.clear();
            // C eh_launch_app: argv[0] is the app path, then the item's
            // params (NewTaskEx passes the array through as-is).
            crate::log(&format!(
                "[eh_app] launching app path={} params={}",
                it.path,
                it.params.len()
            ));
            if !app
                .fb()
                .launch_app(&it.path, &it.text, &it.params)
            {
                crate::log("[eh_app] launch failed (no task system on this platform)");
            }
            return;
        }
    }
}
