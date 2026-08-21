//! The source chooser (C eh_draw_overlay_source / eh_on_tap_source): a
//! black dim over the shelf and a vertically-centred 3/4-width white sheet
//! with an inline title, a divider, and three text-only rows (Kavita /
//! Local / Folder).  The selected row is filled black with white text.
//!
//! Geometry is the shield's own (the C sheet does NOT use the shared
//! overlay header — no back arrow): pw=w*3/4, ph=72+3*96+24, vertically
//! centred in the content area; rows at py+80, 96px each.

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
        Source::Kavita => "Kavita",
        Source::Local => "Local",
        Source::Folder => "Folder",
    }
}

/// Draw the chooser; records the three row rects for tap routing.
pub fn draw<B: eh_hal::Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_LGRAY, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });

    let (px, py, pw, ph) = sheet(w, h);

    // Dim the shelf + draw the sheet.
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_BLACK);
    surf.fill_gray(Rect { x: px, y: py, w: pw, h: ph }, GRAY_WHITE);
    // Double border (outer + inner frame at +1).
    surf.rect_outline(Rect { x: px, y: py, w: pw, h: ph }, 1, GRAY_BLACK);
    surf.rect_outline(Rect { x: px + 1, y: py + 1, w: pw - 2, h: ph - 2 }, 1, GRAY_BLACK);

    // Inline title + divider.
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    eh_render::draw_text(surf, font, 32.0, "Source", (px + 24) as i32, (py + 18) as i32, GRAY_BLACK, &mut glyph);
    surf.hline(px + 24, py + 64, pw - 48, 2, GRAY_LGRAY);

    // Three text-only rows.
    app.source_rows.clear();
    for i in 0..3 {
        let ry = py + 80 + i as u32 * 96;
        let selected = source_index(app.source) == i;
        let bg = if selected { GRAY_BLACK } else { GRAY_WHITE };
        surf.fill_gray(Rect { x: px + 12, y: ry, w: pw - 24, h: 84 }, bg);
        surf.rect_outline(Rect { x: px + 12, y: ry, w: pw - 24, h: 84 }, 1, GRAY_BLACK);
        let col = if selected { GRAY_WHITE } else { GRAY_BLACK };
        eh_render::draw_text(surf, font, 28.0, source_label(match i {
            0 => Source::Kavita,
            1 => Source::Local,
            _ => Source::Folder,
        }), (px + 32) as i32, (ry + 32) as i32, col, &mut glyph);
        app.source_rows.push(Rect { x: px + 12, y: ry, w: pw - 24, h: 84 });
    }
    let _ = GRAY_DGRAY;
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
/// chooser; a row tap switches source, persists it, runs the sync (Kavita
/// only) and redraws.  Local/Folder data paths aren't ported yet — they
/// persist the selection and log (house rule: never fake state).
pub fn tap<B: eh_hal::Framebuffer>(app: &mut App<B>, x: i32, y: i32) {
    let w = app.screen().framebuffer().screen().width;
    let h = app.content_bottom;
    let (px, py, pw, ph) = sheet(w, h);
    app.overlay = crate::app::Overlay::None;

    // Outside the sheet → just close (C closes + redraws).
    if x < px as i32 || x >= (px + pw) as i32 || y < py as i32 || y >= (py + ph) as i32 {
        app.refresh_shelf();
        return;
    }
    let row = (y - py as i32) / 96;
    let new = match row {
        0 => Source::Kavita,
        1 => Source::Local,
        _ => Source::Folder,
    };
    app.source = new;
    app.config.source = Some(new.config_value());
    app.save_config();
    crate::log(&format!("[eh_app] source switched to {}", source_label(new)));
    match new {
        Source::Kavita => {
            app.tab = crate::app::Tab::Library;
            app.page = 0;
            app.do_sync();
        }
        _ => {
            crate::log(&format!(
                "[eh_app] source={}: local/folder data path not yet ported (still showing the Kavita library)",
                source_label(new)
            ));
            app.refresh_shelf();
        }
    }
}