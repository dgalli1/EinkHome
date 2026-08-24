//! The source chooser (C eh_draw_overlay_source / eh_on_tap_source): a
//! LGRAY hatch dim over the content area and a vertically-centred
//! 3/4-width white sheet (widgets::sheet::open_sheet, double border)
//! with an inline title, a divider, and three text-only rows (Kavita /
//! Local / Folder).  The selected row is filled black with white text.
//!
//! Geometry is the sheet's own (the C sheet does NOT use the shared
//! overlay header — no back arrow): pw=w*3/4, ph=72+3*96+24, vertically
//! centred in the content area; rows at py+80, 96px each.  [`sheet`](fn@sheet)
//! and [`crate::widgets::sheet::open_sheet`] derive the same rect by formula,
//! so tap routing and paint cannot drift.

use eh_hal::Rect;

use crate::app::{App, Source};

/// The sheet rect (C eh_source_geom).
pub fn sheet(w: u32, h: u32) -> (u32, u32, u32, u32) {
    let pw = (w as i32 * 3) / 4;
    let ph = 72 + 3 * 96 + 24;
    let px = (w as i32 - pw) as u32 / 2;
    let py = (h as i32 - ph) as u32 / 2;
    (px, py, pw as u32, ph as u32)
}

fn source_label(s: Source) -> &'static str {
    match s {
        Source::Kavita => crate::i18n::tr("source.kavita"),
        Source::Local => crate::i18n::tr("source.local"),
        Source::Folder => crate::i18n::tr("source.folder"),
    }
}
/// Draw the chooser; records the three row rects for tap routing.
pub fn draw<B: eh_hal::Framebuffer>(
    surf: &mut eh_render::Surface,
    app: &mut App<B>,
    dirty: &mut Vec<Rect>,
) {
    use eh_shell::{GRAY_BLACK, GRAY_LGRAY, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom;
    // Hatch dim over the content area + the double-bordered white sheet
    // (C eh_dim_content(0), eh_source_geom) — same scaffold as every
    // other popup, not the old solid-black full-screen fill.
    let sh = crate::widgets::sheet::open_sheet(surf, dirty, h, 0, h, h, sheet(w, h).3, true);

    // Inline title + divider.
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    // C DrawString is TOP-anchored (title top at py+18); draw_text takes
    // a BASELINE — add the face's ascent or the 32pt title rides up into
    // the sheet border.
    let title_asc = font.line_h(32.0).0 as i32;
    eh_render::draw_text(
        surf,
        font,
        32.0,
        crate::i18n::tr("source.title"),
        (sh.px + 24) as i32,
        (sh.py + 18) as i32 + title_asc,
        GRAY_BLACK,
        &mut glyph,
    );
    surf.hline(sh.px + 24, sh.py + 64, sh.pw - 48, 2, GRAY_LGRAY);

    // Three text-only rows.
    app.source_rows.clear();
    let row_asc = font.line_h(28.0).0 as i32;
    for i in 0..3 {
        let ry = sh.py + 80 + i as u32 * 96;
        let selected = source_index(app.source) == i;
        let bg = if selected { GRAY_BLACK } else { GRAY_WHITE };
        surf.fill_gray(
            Rect {
                x: sh.px + 12,
                y: ry,
                w: sh.pw - 24,
                h: 84,
            },
            bg,
        );
        surf.rect_outline(
            Rect {
                x: sh.px + 12,
                y: ry,
                w: sh.pw - 24,
                h: 84,
            },
            1,
            GRAY_BLACK,
        );
        let col = if selected { GRAY_WHITE } else { GRAY_BLACK };
        eh_render::draw_text(
            surf,
            font,
            28.0,
            source_label(match i {
                0 => Source::Kavita,
                1 => Source::Local,
                _ => Source::Folder,
            }),
            (sh.px + 32) as i32,
            (ry + 32) as i32 + row_asc,
            col,
            &mut glyph,
        );
        app.source_rows.push(Rect {
            x: sh.px + 12,
            y: ry,
            w: sh.pw - 24,
            h: 84,
        });
    }
}

/// 0-based row index of a source (Kavita=0, Local=1, Folder=2).
fn source_index(s: Source) -> usize {
    match s {
        Source::Kavita => 0,
        Source::Local => 1,
        Source::Folder => 2,
    }
}

/// Tap dispatch (C eh_on_tap_source): a tap outside the sheet closes the
/// chooser; a row tap switches source, persists it, and runs the source's
/// data path — Kavita syncs, Local kicks the storage-root import, Folder
/// opens the directory browser as the shelf body.
pub fn tap<B: eh_hal::Framebuffer>(app: &mut App<B>, x: i32, y: i32) {
    let w = app.screen_width();
    let h = app.content_bottom;
    let (px, py, pw, ph) = sheet(w, h);

    // Outside the sheet → just close (C closes + redraws).
    if x < px as i32 || x >= (px + pw) as i32 || y < py as i32 || y >= (py + ph) as i32 {
        app.overlay = crate::app::Overlay::None;
        app.refresh_shelf();
        return;
    }
    // Rows are painted at py+80, 96px apart (C eh_on_tap_source).  Check
    // the offset BEFORE dividing: C's plain `(y-(py+80))/96` truncates
    // toward zero, so a tap on the title band (negative quotient) became
    // row 0 and switched to Kavita — kept the sheet-open no-op instead.
    let rel = y - (py as i32 + 80);
    if rel < 0 {
        return; // title band: handled, chooser stays up (C sheet behaviour)
    }
    let row = rel / 96;
    if row > 2 {
        return; // bottom padding strip below the last row
    }
    app.overlay = crate::app::Overlay::None;
    // Source switch: abort any in-flight sync chain BEFORE the source
    // changes / config saves (C eh_sync_abort), and drop a still-running
    // local import scan — its result would otherwise land under the new
    // source (or hold `syncing` so resync() silently no-ops).
    app.sync_abort();
    crate::local::cancel_scan(app);
    let new = match row {
        0 => Source::Kavita,
        1 => Source::Local,
        _ => Source::Folder,
    };
    app.source = new;
    app.config.source = Some(new.config_value());
    app.save_config();
    crate::log(&format!(
        "[eh_app] source switched to {}",
        source_label(new)
    ));
    app.tab = crate::app::Tab::Library;
    app.page = 0;
    match new {
        Source::Kavita => {
            app.browser.open = false;
            // Re-project the view under Kavita NOW (C rebuilds when
            // g_view_source != source even if the delta applies nothing):
            // the shelf must drop local rows immediately, not whenever
            // the async sync happens to land.
            app.rebuild_view();
            app.resync();
        }
        Source::Local => {
            // The import applies on a later tick and rebuilds the view
            // (C eh_local_import_scanner → async apply chain).
            app.browser.open = false;
            crate::local::kick_import(app);
            app.refresh_shelf();
        }
        Source::Folder => {
            crate::local::start_browse(app);
        }
    }
}
