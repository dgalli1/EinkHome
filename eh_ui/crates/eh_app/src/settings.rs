//! The Settings screen (C eh_draw_overlay_settings): full-screen white, a
//! shared overlay header (back chevron + centred title), four editable
//! rows (API host / API key / Reader app / Download folder) + a System app
//! row, then the Save / Show logs / Licenses buttons.  The API host + key
//! rows edit through the firmware's on-screen keyboard (async commit,
//! drained by the app on its next event).

use eh_hal::{Framebuffer, Rect};
use eh_render::draw_text;
use eh_shell::{GRAY_BLACK, GRAY_WHITE};

use crate::app::{App, KbField, SettingsRow};
use crate::widgets::header::{back_rect, draw_header};

/// Row rhythm (C EH_SETTINGS_*).
pub const MARGIN: u32 = 32;
pub const ROW_H: u32 = 120;
pub const BTN_H: u32 = 96;
pub const ROWS_Y0: u32 = 112;

/// Draw the settings page; records row rects into `app.settings_rows`.
pub fn draw<B: Framebuffer>(
    surf: &mut eh_render::Surface,
    app: &mut App<B>,
    dirty: &mut Vec<Rect>,
) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    draw_header(surf, crate::i18n::tr("settings.title"), dirty);

    app.settings_rows.clear();
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();

    let dl = app.config.downloads_dir.clone().unwrap_or_default();
    let reader_val = app.reader_label(); // Auto + every detected reader (C eh_settings_reader_label)
                                         // Live install state (C eh_sysapp_detect): toggling flips the row.
    let sysapp_val = if crate::sysapp::detect() {
        crate::i18n::tr("settings.sysapp_on")
    } else {
        crate::i18n::tr("settings.sysapp_off")
    };
    let rows: [(SettingsRow, &str, &str); 5] = [
        (
            SettingsRow::ApiHost,
            crate::i18n::tr("settings.api_host"),
            &app.config.api_url,
        ),
        (
            SettingsRow::ApiKey,
            crate::i18n::tr("settings.api_key"),
            &app.config.api_token,
        ),
        (
            SettingsRow::ReaderApp,
            crate::i18n::tr("settings.reader"),
            &reader_val,
        ),
        (
            SettingsRow::DownloadFolder,
            crate::i18n::tr("settings.dl_dir"),
            &dl,
        ),
        (
            SettingsRow::SystemApp,
            crate::i18n::tr("settings.system_app"),
            sysapp_val,
        ),
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
        let (card_col, text_col, value_col) = if editing {
            (GRAY_BLACK, GRAY_WHITE, GRAY_WHITE)
        } else {
            (GRAY_WHITE, GRAY_BLACK, GRAY_BLACK)
        };
        let ry = y as u32;
        surf.fill_gray(
            Rect {
                x: MARGIN,
                y: ry,
                w: w - 2 * MARGIN,
                h: ROW_H - 12,
            },
            card_col,
        );
        surf.rect_outline(
            Rect {
                x: MARGIN,
                y: ry,
                w: w - 2 * MARGIN,
                h: ROW_H - 12,
            },
            2,
            GRAY_BLACK,
        );
        draw_text(
            surf,
            eh_shell::bold_font(),
            26.0,
            label,
            (MARGIN + 16) as i32,
            ry as i32 + 40,
            text_col,
            &mut glyph,
        );
        draw_text(
            surf,
            font,
            30.0,
            value,
            (MARGIN + 16) as i32,
            ry as i32 + 82,
            value_col,
            &mut glyph,
        );
        app.settings_rows.push((
            Rect {
                x: MARGIN,
                y: ry,
                w: w - 2 * MARGIN,
                h: ROW_H - 12,
            },
            *row,
        ));
        y += ROW_H as i32;
    }
    y += 24;
    // Buttons (C eh_settings_draw_button): filled Save, outlined
    // Show logs + Licenses.
    for (row, label, filled) in [
        (SettingsRow::Save, crate::i18n::tr("settings.save"), true),
        (
            SettingsRow::ShowLogs,
            crate::i18n::tr("settings.logs"),
            false,
        ),
        (
            SettingsRow::Licenses,
            crate::i18n::tr("settings.licenses"),
            false,
        ),
    ] {
        let ry = y as u32;
        surf.fill_gray(
            Rect {
                x: MARGIN,
                y: ry,
                w: w - 2 * MARGIN,
                h: BTN_H - 12,
            },
            if filled { GRAY_BLACK } else { GRAY_WHITE },
        );
        surf.rect_outline(
            Rect {
                x: MARGIN,
                y: ry,
                w: w - 2 * MARGIN,
                h: BTN_H - 12,
            },
            2,
            GRAY_BLACK,
        );
        let col = if filled { GRAY_WHITE } else { GRAY_BLACK };
        let tw = font.width(label, 32.0) as i32;
        draw_text(
            surf,
            eh_shell::bold_font(),
            32.0,
            label,
            (w as i32 - tw) / 2,
            ry as i32 + (BTN_H - 12) as i32 / 2 + 11,
            col,
            &mut glyph,
        );
        app.settings_rows.push((
            Rect {
                x: MARGIN,
                y: ry,
                w: w - 2 * MARGIN,
                h: BTN_H - 12,
            },
            row,
        ));
        y += BTN_H as i32;
    }
}

/// Settings tap routing.  The back chevron closes; a tap on no row is
/// ignored.  API host / key rows open the on-screen keyboard (the C
/// app's editing path), Save applies + persists, ReaderApp cycles the
/// reader preference, DownloadFolder opens the directory picker,
/// SystemApp toggles the home-task override, and ShowLogs / Licenses
/// swap to their viewers.
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
                app.settings_apply(); // CoreAgent hook: C eh_settings_apply side effects
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
                // Open the folder picker rooted at the storage root,
                // starting at the current downloads dir when it is under
                // the root (C eh_on_tap_settings_folder -> eh_folder_open).
                let root = crate::local::browse_root();
                let start = app
                    .config
                    .downloads_dir
                    .clone()
                    .filter(|d| d.starts_with(&root))
                    .unwrap_or_else(|| root.clone());
                // Root at the STORAGE ROOT but start in the current
                // downloads dir (C eh_folder_open): starting rooted AT
                // the downloads dir leaves no ".." to ascend from.
                let mut b = crate::local::Browser {
                    picker: true,
                    root: root.clone(),
                    path: start,
                    ..Default::default()
                };
                b.load();
                app.dl_picker = Some(b);
                // The picker is NOT an overlay: it lives on the main page
                // (app.dl_picker) and tap_screen routes its taps. Leaving
                // Overlay::Settings up would keep routing body taps to
                // tap_settings, so drop the overlay before opening.
                app.overlay = crate::app::Overlay::None;
                app.settings_rows.clear();
                app.dirty = true;
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
