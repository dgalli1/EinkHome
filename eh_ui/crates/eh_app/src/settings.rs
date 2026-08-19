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
pub fn draw_header(surf: &mut eh_render::Surface, title: &str, _dirty: &mut Vec<Rect>) {
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
    let h = app.content_bottom as u32;
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    draw_header(surf, "Settings", dirty);

    app.settings_rows.clear();
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();

    let dl = app.config.downloads_dir.clone().unwrap_or_default();
    let rows: [(SettingsRow, &str, &str); 5] = [
        (SettingsRow::ApiHost, "API host", &app.config.api_url),
        (SettingsRow::ApiKey, "API key", &app.config.api_token),
        (
            SettingsRow::ReaderApp,
            "Reader app",
            app.config.reader.as_deref().unwrap_or("auto"),
        ),
        (SettingsRow::DownloadFolder, "Download folder", &dl),
        (SettingsRow::SystemApp, "System app", "Off"),
    ];
    let mut y = ROWS_Y0 as i32;
    for (row, label, value) in rows.iter() {
        // Row card (C eh_settings_draw_row: 32px margins, 120-12 tall,
        // label on top, value below).
        let ry = y as u32;
        surf.fill_gray(Rect { x: MARGIN, y: ry, w: w - 2 * MARGIN, h: ROW_H - 12 }, GRAY_WHITE);
        surf.rect_outline(Rect { x: MARGIN, y: ry, w: w - 2 * MARGIN, h: ROW_H - 12 }, 2, GRAY_BLACK);
        draw_text(surf, font, 26.0, label, (MARGIN + 16) as i32, ry as i32 + 40, GRAY_BLACK, &mut glyph);
        draw_text(surf, font, 30.0, value, (MARGIN + 16) as i32, ry as i32 + 82, GRAY_DGRAY, &mut glyph);
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
                // Cycling reader detection lands with the reader-preference
                // slice; the row is shown for parity.
                crate::log("[eh_app] settings: reader list not ported yet");
            }
            SettingsRow::DownloadFolder | SettingsRow::SystemApp | SettingsRow::ShowLogs | SettingsRow::Licenses => {
                crate::log(&format!("[eh_app] settings: {row:?} not ported yet"));
            }
        }
        return;
    }
}

impl<B: Framebuffer> App<B> {
    /// Open the firmware keyboard on a settings field (C
    /// eh_input.c:435/453): the commit is async — the handler stashes the
    /// text and [`App::on_event`] drains it.
    pub fn edit_field(&mut self, field: KbField) {
        use crate::app::{kb_arm, kb_take_pending};
        let initial = match field {
            KbField::ApiHost => self.config.api_url.clone(),
            KbField::ApiKey => self.config.api_token.clone(),
            KbField::Search => self.query.clone(),
        };
        // Any stale pending commit is discarded (a new edit supersedes it).
        let _ = kb_take_pending();
        kb_arm(field);
        let (title, init) = match field {
            KbField::ApiHost => ("API host", initial.as_str()),
            KbField::ApiKey => ("API key", initial.as_str()),
            KbField::Search => ("Search", initial.as_str()),
        };
        // The commit handler lives in eh_backend_inkview (static fn
        // pointer); it pushes into app's thread_local and we drain on the
        // next event.
        self.screen().framebuffer_mut().open_keyboard(
            title,
            init,
            crate::app::kb_commit,
        );
    }
}