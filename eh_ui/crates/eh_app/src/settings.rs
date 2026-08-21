//! The Settings screen (C eh_draw_overlay_settings): full-screen white, a
//! shared overlay header (back chevron + centred title), four editable
//! rows (API host / API key / Reader app / Download folder) + a System app
//! row, then the Save / Show logs / Licenses buttons.  The API host + key
//! rows edit through the firmware's on-screen keyboard (async commit,
//! drained by the app on its next event).

use eh_hal::{Framebuffer, Rect};
use eh_render::draw_text;
use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_WHITE};

use crate::app::{App, KbField, SettingsRow};

/// Header + row rhythm (C EH_OVERLAY_* / EH_SETTINGS_*).
pub const HEADER_H: u32 = 96;
pub const BACK_X: u32 = 8;
pub const BACK_W: u32 = 96;
pub const BACK_H: u32 = 56;
pub const MARGIN: u32 = 32;
pub const ROW_H: u32 = 120;
pub const BTN_H: u32 = 96;
pub const ROWS_Y0: u32 = 112;

/// Draw the shared overlay header (C eh_draw_overlay_header): white bar,
/// bottom rule, back chevron in the shared touch box, centred title.
pub fn draw_header(surf: &mut eh_render::Surface, title: &str, _dirty: &mut [Rect]) {
    let w = surf.width();
    surf.fill_gray(Rect { x: 0, y: 0, w, h: HEADER_H }, GRAY_WHITE);
    surf.hline(0, HEADER_H - 2, w, 2, GRAY_BLACK);
    let bx = BACK_X as i32 + BACK_W as i32 / 2;
    let by = (HEADER_H as i32 - BACK_H as i32) / 2 + BACK_H as i32 / 2;
    draw_back_icon(surf, bx, by, GRAY_BLACK);
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    let tw = font.width(title, 36.0) as i32;
    draw_text(surf, font, 36.0, title, (w as i32 - tw) / 2, (HEADER_H as i32) / 2 + 12, GRAY_BLACK, &mut glyph);
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
    Rect { x: BACK_X, y, w: BACK_W, h: BACK_H }
}

/// Draw the settings page; records row rects into `app.settings_rows`.
pub fn draw<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    draw_header(surf, "Settings", dirty);

    app.settings_rows.clear();
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();

    let dl = app.config.downloads_dir.clone().unwrap_or_default();
    let reader_val = if app.reader_pref == 1 { "Standard".to_string() } else { "Auto".to_string() };
    // Live install state (C eh_sysapp_detect): toggling flips the row.
    let sysapp_val = if crate::sysapp::detect() { "On" } else { "Off" };
    let rows: [(SettingsRow, &str, &str); 5] = [
        (SettingsRow::ApiHost, "API host", &app.config.api_url),
        (SettingsRow::ApiKey, "API key", &app.config.api_token),
        (SettingsRow::ReaderApp, "Reader app", &reader_val),
        (SettingsRow::DownloadFolder, "Download folder", &dl),
        (SettingsRow::SystemApp, "System app", sysapp_val),
    ];
    let mut y = ROWS_Y0 as i32;
    for (row, label, value) in rows.iter() {
        // Row card (C eh_settings_draw_row: 32px margins, 120-12 tall,
        // label on top, value below).  The row owning the keyboard draws
        // inverted (BLACK card, WHITE text — the C `editing` flag).
        let editing = matches!(
            (app.kb_editing, row),
            (Some(KbField::ApiHost), SettingsRow::ApiHost)
                | (Some(KbField::ApiKey), SettingsRow::ApiKey)
        );
        let (card_col, text_col, value_col) =
            if editing { (GRAY_BLACK, GRAY_WHITE, GRAY_WHITE) } else { (GRAY_WHITE, GRAY_BLACK, GRAY_DGRAY) };
        let ry = y as u32;
        surf.fill_gray(Rect { x: MARGIN, y: ry, w: w - 2 * MARGIN, h: ROW_H - 12 }, card_col);
        surf.rect_outline(Rect { x: MARGIN, y: ry, w: w - 2 * MARGIN, h: ROW_H - 12 }, 2, GRAY_BLACK);
        draw_text(surf, font, 26.0, label, (MARGIN + 16) as i32, ry as i32 + 40, text_col, &mut glyph);
        draw_text(surf, font, 30.0, value, (MARGIN + 16) as i32, ry as i32 + 82, value_col, &mut glyph);
        app.settings_rows.push((Rect { x: MARGIN, y: ry, w: w - 2 * MARGIN, h: ROW_H - 12 }, *row));
        y += ROW_H as i32;
    }
    y += 24;
    // Buttons (C eh_settings_draw_button): filled Save, outlined
    // Show logs + Licenses.
    for (row, label, filled) in [
        (SettingsRow::Save, "Save", true),
        (SettingsRow::ShowLogs, "Show logs", false),
        (SettingsRow::Licenses, "Licenses", false),
    ] {
        let ry = y as u32;
        surf.fill_gray(
            Rect { x: MARGIN, y: ry, w: w - 2 * MARGIN, h: BTN_H - 12 },
            if filled { GRAY_BLACK } else { GRAY_WHITE },
        );
        surf.rect_outline(Rect { x: MARGIN, y: ry, w: w - 2 * MARGIN, h: BTN_H - 12 }, 2, GRAY_BLACK);
        let col = if filled { GRAY_WHITE } else { GRAY_BLACK };
        let tw = font.width(label, 32.0) as i32;
        draw_text(surf, font, 32.0, label, (w as i32 - tw) / 2, ry as i32 + (BTN_H - 12) as i32 / 2 + 11, col, &mut glyph);
        app.settings_rows.push((Rect { x: MARGIN, y: ry, w: w - 2 * MARGIN, h: BTN_H - 12 }, row));
        y += BTN_H as i32;
    }
}

/// Settings tap routing: back chevron closes; the API host / key rows open
/// the on-screen keyboard (the C app's only real editing path); Save
/// persists; the rest are logged no-ops in this slice.
pub fn tap_settings<B: Framebuffer>(x: i32, y: i32, app: &mut App<B>) {
    if back_rect().contains(x, y) {
        app.overlay = crate::app::Overlay::None;
        app.settings_rows.clear();
        return;
    }
    for (r, row) in app.settings_rows.iter().cloned() {
        if !r.contains(x, y) {
            continue;
        }
        match row {
            SettingsRow::ApiHost => app.edit_field(KbField::ApiHost),
            SettingsRow::ApiKey => app.edit_field(KbField::ApiKey),
            SettingsRow::Save => {
                app.save_config();
            }
            SettingsRow::ReaderApp => {
                app.cycle_reader();
            }
            SettingsRow::ShowLogs => {
                app.overlay = crate::app::Overlay::LogViewer;
                app.dirty = true;
            }
            SettingsRow::Licenses => {
                app.overlay = crate::app::Overlay::Licenses;
                app.dirty = true;
            }
            SettingsRow::DownloadFolder => {
                crate::log("[eh_app] settings: DownloadFolder not ported yet");
            }
            SettingsRow::SystemApp => {
                // Toggle: promote the running binary, or unpromote when
                // already installed (C eh_settings_toggle_sysapp).
                if crate::sysapp::detect() {
                    crate::sysapp::unpromote();
                    crate::logger::log(
                        "[bookshelf] sysapp: removed from system — stock home returns after reboot",
                    );
                } else if crate::sysapp::promote(app) {
                    crate::logger::log(
                        "[bookshelf] sysapp: installed as system app — reboot to boot EinkHome as the home screen",
                    );
                }
                app.dirty = true;
            }
        }
        return;
    }
}