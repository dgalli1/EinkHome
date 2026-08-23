//! Full-screen viewer overlays: the log viewer (C eh_logview.c) and the
//! bundled licenses list + detail (C eh_licenses.c / data/eh_licenses.c).
//!
//! Both viewers share the shell helpers ported from eh_screen.c: the
//! pixel-width greedy word wrap ([`eh_shell::wrap_rows_forward`] /
//! [`eh_shell::wrap_rows_last`]) and the corner scroll buttons.  The log
//! viewer pins to the newest tail (`log_scroll < 0`, C `eh_g_state
//! .log_scroll`) so new lines stay visible while it is open; an explicit
//! scroll materialises a concrete first-row index.

use eh_hal::{Framebuffer, Rect};
use eh_render::draw_text;
use eh_shell::{
    draw_scroll_buttons, hit_scroll_button_at, wrap_rows_forward, wrap_rows_last, GRAY_BLACK,
    GRAY_DGRAY, GRAY_WHITE, SCROLL_BTN_H,
};

use crate::app::{App, Overlay};
use crate::settings::{HEADER_H, back_rect, draw_header as draw_settings_header};

/// One bundled third-party license (name, type, where-used note, and the
/// FULL text shipped as a string — C BsLicense).
pub struct License {
    pub name: &'static str,
    /// License type ("MIT", "zlib", …) — shown in the list rows and the
    /// detail attribution band.
    pub kind: &'static str,
    /// Where the component is used.
    pub note: &'static str,
    pub text: &'static str,
}

/// The licenses the app bundles, verbatim from C data/eh_licenses.c (the
/// texts are the actual licenses of the bundled components; they live here
/// as strings so they ship inside the binary and are viewable on every
/// platform with no filesystem dependency).
pub const LICENSES: &[License] = &[
    License {
        name: "cJSON",
        kind: "MIT",
        note: "JSON parser, bundled in app/vendor/cJSON.c",
        text: "Copyright (c) 2009-2017 Dave Gamble and cJSON contributors\n\
               \n\
               Permission is hereby granted, free of charge, to any person \
               obtaining a copy of this software and associated documentation \
               files (the \"Software\"), to deal in the Software without \
               restriction, including without limitation the rights to use, \
               copy, modify, merge, publish, distribute, sublicense, and/or \
               sell copies of the Software, and to permit persons to whom the \
               Software is furnished to do so, subject to the following \
               conditions:\n\
               \n\
               The above copyright notice and this permission notice shall be \
               included in all copies or substantial portions of the \
               Software.\n\
               \n\
               THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY \
               KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE \
               WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE \
               AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT \
               HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, \
               WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING \
               FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR \
               OTHER DEALINGS IN THE SOFTWARE.",
    },
    License {
        name: "SQLite",
        kind: "Public Domain",
        note: "Library store and reading-progress database (firmware/system SQLite)",
        text: "The author disclaims copyright to this source code.  In place \
               of a legal notice, here is a blessing:\n\
               \n\
                   May you do good and not evil.\n\
                   May you find forgiveness for yourself and forgive others.\n\
                   May you share freely, never taking more than you give.\n\
               \n\
               SQLite is in the public domain and imposes no licence or \
               attribution requirement.",
    },
    License {
        name: "zlib",
        kind: "zlib",
        note: "firmware image/network stack (EPUB inflate is now the bundled Rust lib)",
        text: "Copyright notice:\n\
               \n\
                (C) 1995-2026 Jean-loup Gailly and Mark Adler\n\
               \n\
                 This software is provided 'as-is', without any express or \
               implied warranty.  In no event will the authors be held liable \
               for any damages arising from the use of this software.\n\
               \n\
                 Permission is granted to anyone to use this software for any \
               purpose, including commercial applications, and to alter it \
               and redistribute it freely, subject to the following \
               restrictions:\n\
               \n\
                 1. The origin of this software must not be misrepresented; \
               you must not claim that you wrote the original software.  If \
               you use this software in a product, an acknowledgment in the \
               product documentation would be appreciated but is not \
               required.\n\
                 2. Altered source versions must be plainly marked as such, \
               and must not be misrepresented as being the original \
               software.\n\
                 3. This notice may not be removed or altered from any source \
               distribution.\n\
               \n\
                 Jean-loup Gailly        Mark Adler\n\
                 jloup@gzip.org          madler@alumni.caltech.edu",
    },
    License {
        name: "Rust extraction (zip / roxmltree / miniz_oxide)",
        kind: "MIT / Zlib",
        note: "EPUB/PDF/FB2 title, author and cover extraction (rust_extract staticlib)",
        text: "Handles: zip archive reading (zip crate, MIT), XML parsing\
               \u{a0}(roxmltree, MIT or Apache-2.0), and raw-deflate inflation\
               \u{a0}(miniz_oxide, MIT or Apache-2.0 or Zlib).\n\
               \n\
               MIT License: permission is hereby granted, free of charge, to\
               any person obtaining a copy of this software and associated\
               documentation files (the \u{201c}Software\u{201d}), to deal in the\
               Software without restriction, including without limitation the\
               rights to use, copy, modify, merge, publish, distribute,\
               sublicense, and/or sell copies of the Software, and to permit\
               persons to whom the Software is furnished to do so, subject to\
               the following conditions:\n\
               \n\
               The above copyright notice and this permission notice shall be\
               included in all copies or substantial portions of the\
               Software.\n\
               \n\
               THE SOFTWARE IS PROVIDED \u{201c}AS IS\u{201d}, WITHOUT WARRANTY OF\
               ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE\
               WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE\
               AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT\
               HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,\
               WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING\
               FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR\
               OTHER DEALINGS IN THE SOFTWARE.",
    },
    License {
        name: "libcurl",
        kind: "MIT / ISC",
        note: "HTTP backend of the PC/SDL build (app/platform/eh_plat_sdl.c)",
        text: "COPYRIGHT AND PERMISSION NOTICE\n\
               \n\
               Copyright (c) 1996 - 2026, Daniel Stenberg, <daniel@haxx.se>, \
               and many contributors, see the THANKS file.\n\
               \n\
               All rights reserved.\n\
               \n\
               Permission to use, copy, modify, and distribute this software \
               for any purpose with or without fee is hereby granted, provided \
               that the above copyright notice and this permission notice \
               appear in all copies.\n\
               \n\
               THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY \
               KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE \
               WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE \
               AND NONINFRINGEMENT OF THIRD PARTY RIGHTS.  IN NO EVENT SHALL \
               THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, \
               DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, \
               TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH \
               THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.\n\
               \n\
               Except as contained in this notice, the name of a copyright \
               holder shall not be used in advertising or otherwise to promote \
               the sale, use or other dealings in this Software without prior \
               written authorization of the copyright holder.",
    },
];

// ── geometry (C eh_core.h) ──────────────────────────────────────────────

/// Log/detail text row height (C EH_LOG_ROW_H) and font px (EH_LOG_FONT_PX).
const LOG_ROW_H: u32 = 26;
const LOG_FONT: f32 = 20.0;
/// Body band top for the wrapped rows (C EH_LOG_BODY_TOP).
pub const LOG_BODY_TOP: u32 = HEADER_H + 42;
/// Licenses list row height + body top (C EH_LIC_LIST_*).
const LIC_LIST_H: u32 = 110;
const LIC_LIST_TOP: u32 = HEADER_H + 16;
/// Wrapped detail rows cap (C EH_LIC_MAX_ROWS — any licence fits).
const LIC_MAX_ROWS: usize = 512;
/// Visible-row cap on the log body (C eh_draw_log_view): at most 41 rows,
/// one fewer than the firmware crash boundary on the largest panel.
const LOG_MAX_ROWS_VIS: usize = 41;
/// Log tail window (C log_wrap_get reads at most 160 KB).
const LOG_TAIL_BYTES: usize = 160 * 1024;

fn draw_header(surf: &mut eh_render::Surface, title: &str) {
    let mut dirty = Vec::new();
    draw_settings_header(surf, title, &mut dirty);
}

/// The e2e log path: exactly where logger::init opened the file (never
/// re-derived here — a mismatch shows the user "No log file yet").
fn log_path() -> std::path::PathBuf {
    crate::logger::path().cloned().unwrap_or_else(|| {
        // Logger not initialised (cannot happen in the running app):
        // keep the device default for the draw label.
        std::path::PathBuf::from("/mnt/ext1/system/bin/bookshelf.log")
    })
}

/// Read the log tail: at most `cap` bytes, aligned to a line boundary
/// (C log_tail_read).  `None` when the log does not exist yet.
fn log_tail(cap: usize) -> Option<String> {
    let raw = std::fs::read(log_path()).ok()?;
    if raw.len() <= cap {
        return Some(String::from_utf8_lossy(&raw).into_owned());
    }
    // Skip forward past one LF so the window starts at a line boundary
    // (no newline inside the window → the tail is that partial line's end:
    // nothing readable, as in C).
    let mut start = raw.len() - cap;
    while start < raw.len() && raw[start] != b'\n' {
        start += 1;
    }
    start = (start + 1).min(raw.len());
    Some(String::from_utf8_lossy(&raw[start..]).into_owned())
}

/// Visible log rows for the current view geometry (shared by drawing and
/// tap routing): the body between the header band and the scroll buttons,
/// capped at the firmware-crash boundary.
fn log_rows_vis(h: u32) -> usize {
    let btn_y = h.saturating_sub(8 + SCROLL_BTN_H);
    let body_h = btn_y.saturating_sub(LOG_BODY_TOP + 8).max(LOG_ROW_H);
    ((body_h / LOG_ROW_H).max(1) as usize).min(LOG_MAX_ROWS_VIS)
}

/// Wrap the current log tail for a `w`×`h` view (rows oldest → newest,
/// newest-tail anchored).  Shared by the draw pass and the tap router.
fn log_rows(w: u32, h: u32) -> Option<(String, Vec<eh_shell::WrapRow>)> {
    let text = log_tail(LOG_TAIL_BYTES)?;
    let rows = wrap_rows_last(
        crate::shelf::shelf_font(),
        LOG_FONT,
        &text,
        (w.saturating_sub(48)) as f32,
        log_rows_vis(h) * 8, // ~8 wrapped rows per source line, as in C
    );
    Some((text, rows))
}

/// Resolve the first visible row of the log tail's last full page (C
/// eh_log_view_tail_first) — the position the pinned viewer shows, and
/// the anchor for paging up from the tail.  0 when the log is absent or
/// fits entirely on one page.
fn log_tail_first(w: u32, h: u32) -> usize {
    let Some((_, rows)) = log_rows(w, h) else { return 0 };
    rows.len().saturating_sub(log_rows_vis(h))
}

/// The full-screen log viewer (C eh_draw_log_view): the app log tail,
/// pixel-width word-wrapped with newest-tail pinning, page-scrolled with
/// the corner buttons; the log file path rides in its own band just below
/// the header border (inside the header it would collide with the centred
/// title).
pub fn draw_log_viewer<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    draw_header(surf, crate::i18n::tr("log.title"));
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();

    let shown = log_path().display().to_string();
    let mut fitted = String::new();
    eh_render::fit_width(font, 20.0, &shown, (w.saturating_sub(64)) as f32, &mut fitted);
    let tw = font.width(&fitted, 20.0) as i32;
    // C DrawString takes the glyph TOP; draw_text a baseline — +20 keeps
    // the 20px path fully below the header rule instead of straddling it.
    draw_text(surf, font, 20.0, &fitted, ((w as i32 - tw) / 2).max(0), (HEADER_H + 30) as i32, GRAY_DGRAY, &mut glyph);

    let body_top = LOG_BODY_TOP;
    let rows_vis = log_rows_vis(h);
    let Some((text, rows)) = log_rows(w, h) else {
        draw_text(surf, font, 26.0, crate::i18n::tr("log.empty"), 32, (body_top + 40) as i32, GRAY_DGRAY, &mut glyph);
        return;
    };

    let nrows = rows.len();
    let max_first = nrows.saturating_sub(rows_vis);
    // log_scroll < 0 means pinned to the tail: keep re-pinning on every
    // redraw so new lines stay visible while the viewer is open.  Only an
    // explicit scroll materialises a concrete first-row index.
    let first = if app.log_scroll < 0 {
        max_first
    } else {
        let f = (app.log_scroll as usize).min(max_first);
        app.log_scroll = f as i32;
        f
    };
    for i in 0..rows_vis.min(rows.len().saturating_sub(first)) {
        let r = &rows[first + i];
        draw_text(surf, font, LOG_FONT, &text[r.start..r.end], 24, (body_top + i as u32 * LOG_ROW_H + 20) as i32, GRAY_BLACK, &mut glyph);
    }
    // Corner scroll buttons: older = up, newer = down.
    draw_scroll_buttons(surf, h, first > 0, first < max_first);
}

/// The licenses LIST view (C lic_draw_list): one bordered row per license
/// (name over type), scrollable, with the corner scroll buttons.
pub fn draw_licenses<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    draw_header(surf, crate::i18n::tr("licenses.title"));
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();

    let body_top = LIC_LIST_TOP;
    let btn_y = h.saturating_sub(8 + SCROLL_BTN_H);
    let body_h = btn_y.saturating_sub(body_top + 8);
    let rows_vis = ((body_h / LIC_LIST_H).max(1)) as usize;
    let max_first = LICENSES.len().saturating_sub(rows_vis);
    let first = {
        let f = (app.lic_scroll.max(0) as usize).min(max_first);
        app.lic_scroll = f as i32;
        f
    };
    for i in 0..rows_vis {
        let idx = first + i;
        if idx >= LICENSES.len() {
            break;
        }
        let lic = &LICENSES[idx];
        let ry = body_top + i as u32 * LIC_LIST_H;
        surf.fill_gray(Rect { x: 16, y: ry, w: w - 32, h: LIC_LIST_H - 12 }, GRAY_WHITE);
        surf.rect_outline(Rect { x: 16, y: ry, w: w - 32, h: LIC_LIST_H - 12 }, 2, GRAY_BLACK);
        draw_text(surf, font, 30.0, lic.name, 32, (ry + 40) as i32, GRAY_BLACK, &mut glyph);
        draw_text(surf, font, 24.0, lic.kind, 32, (ry + 76) as i32, GRAY_DGRAY, &mut glyph);
    }
    draw_scroll_buttons(surf, h, first > 0, first < max_first);
}

/// One license's FULL text (C lic_draw_detail): a one-line attribution
/// band under the header ("type · where used"), then the word-wrapped
/// text with blank-line paragraph gaps, page-scrolled via the buttons.
pub fn draw_license_detail<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    let sel = app.license_selected.map(|i| i.min(LICENSES.len() - 1)).unwrap_or(0);
    let lic = &LICENSES[sel];
    draw_header(surf, lic.name);
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();

    // Attribution band: type · where it is used.
    let band = format!("{}  \u{b7}  {}", lic.kind, lic.note);
    let mut fitted = String::new();
    eh_render::fit_width(font, 20.0, &band, (w.saturating_sub(64)) as f32, &mut fitted);
    let tw = font.width(&fitted, 20.0) as i32;
    draw_text(surf, font, 20.0, &fitted, ((w as i32 - tw) / 2).max(0), (HEADER_H + 30) as i32, GRAY_DGRAY, &mut glyph);

    let btn_y = h.saturating_sub(8 + SCROLL_BTN_H);
    let body_h = btn_y.saturating_sub(LOG_BODY_TOP + 8).max(LOG_ROW_H);
    let rows_vis = (body_h / LOG_ROW_H).max(1) as usize;
    let rows = wrap_rows_forward(font, LOG_FONT, lic.text, (w.saturating_sub(48)) as f32, LIC_MAX_ROWS);
    let max_first = rows.len().saturating_sub(rows_vis);
    let first = {
        let f = (app.lic_scroll.max(0) as usize).min(max_first);
        app.lic_scroll = f as i32;
        f
    };
    for i in 0..rows_vis.min(rows.len().saturating_sub(first)) {
        let r = &rows[first + i];
        if r.blank {
            continue; // paragraph gap row
        }
        draw_text(surf, font, LOG_FONT, &lic.text[r.start..r.end], 24, (LOG_BODY_TOP + i as u32 * LOG_ROW_H) as i32, GRAY_BLACK, &mut glyph);
    }
    draw_scroll_buttons(surf, h, first > 0, first < max_first);
}

/// Overlay tap routing for the log/licenses viewers (C eh_on_tap_log_view
/// / eh_on_tap_licenses_view): Back steps detail → list → shelf, the
/// corner buttons page-scroll the current view, and a list-row tap opens
/// that license's full text.
pub fn tap<B: Framebuffer>(x: i32, y: i32, app: &mut App<B>) {
    let sw = app.screen_width() as i32;
    let sh = app.content_bottom as i32;
    if back_rect().contains(x, y) {
        match app.overlay {
            Overlay::LicenseDetail => {
                app.overlay = Overlay::Licenses;
                app.license_selected = None;
                app.lic_scroll = 0;
            }
            _ => {
                app.overlay = Overlay::None;
                app.lic_scroll = 0;
                app.log_scroll = -1; // re-pin on the next open
            }
        }
        app.dirty = true;
        return;
    }

    // Corner scroll buttons page the current view (up = older, down =
    // newer); only the body moves, the header stays.
    let dir = hit_scroll_button_at(x, y, (sh as u32).saturating_sub(SCROLL_BTN_H), sw as u32);
    if dir != 0 {
        let btn_y = sh - SCROLL_BTN_H as i32 - 8;
        match app.overlay {
            Overlay::LogViewer => {
                let page = (((btn_y - LOG_BODY_TOP as i32).max(0) as u32) / LOG_ROW_H).max(1) as i32;
                if app.log_scroll < 0 {
                    // Pinned to the tail.  "Newer" is already at the
                    // newest lines, so it stays pinned; "older" pages up
                    // from the tail's last full page.
                    if dir < 0 {
                        let tf = log_tail_first(sw as u32, app.content_bottom) as i32;
                        app.log_scroll = (tf - page).max(0);
                    }
                } else {
                    // Rows are ordered oldest → newest; up goes older.
                    app.log_scroll = (app.log_scroll + dir * page).max(0);
                }
            }
            Overlay::Licenses | Overlay::LicenseDetail => {
                let detail = app.overlay == Overlay::LicenseDetail;
                let (top, rh) = if detail {
                    (LOG_BODY_TOP as i32, LOG_ROW_H as i32)
                } else {
                    (LIC_LIST_TOP as i32, LIC_LIST_H as i32)
                };
                let page = (((btn_y - top - 8) / rh).max(1)).max(1);
                app.lic_scroll = (app.lic_scroll + dir * page).max(0);
            }
            _ => {}
        }
        app.dirty = true;
        return;
    }

    if app.overlay == Overlay::Licenses {
        // A tap on a visible row opens that license's full text.
        let body_top = LIC_LIST_TOP as i32;
        if y >= body_top {
            let rows_vis = ((((sh - 8 - SCROLL_BTN_H as i32) - body_top - 8).max(0) as u32) / LIC_LIST_H).max(1) as i32;
            let rel = (y - body_top) / LIC_LIST_H as i32;
            if rel >= 0 && rel < rows_vis {
                let idx = app.lic_scroll as usize + rel as usize;
                if idx < LICENSES.len() {
                    app.license_selected = Some(idx);
                    app.overlay = Overlay::LicenseDetail;
                    app.lic_scroll = 0;
                    app.dirty = true;
                }
            }
        }
    }
    // Detail: taps in the body are ignored (only Back / scroll).
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled table mirrors C data/eh_licenses.c: five licenses,
    /// each carrying its COMPLETE text — spot-check the distinctive
    /// closing lines so a "curated excerpt" truncation can't slip back.
    #[test]
    fn license_table_matches_c_source() {
        assert_eq!(LICENSES.len(), 5);
        let by_name = |n: &str| LICENSES.iter().find(|l| l.name == n).unwrap();
        let ends = |name: &str, t: &str, s: &str| {
            assert!(t.trim_end().ends_with(s), "license `{name}` text truncated");
        };

        let cjson = by_name("cJSON");
        assert_eq!(cjson.kind, "MIT");
        ends("cJSON", cjson.text, "OTHER DEALINGS IN THE SOFTWARE.");

        let sqlite = by_name("SQLite");
        ends("SQLite", sqlite.text, "attribution requirement.");
        assert!(sqlite.text.contains("May you share freely"));

        let zlib = by_name("zlib");
        assert_eq!(zlib.kind, "zlib");
        ends("zlib", zlib.text, "madler@alumni.caltech.edu");
        assert!(zlib.text.contains("1. The origin of this software"));

        let rex = by_name("Rust extraction (zip / roxmltree / miniz_oxide)");
        assert!(rex.text.starts_with("Handles: zip archive reading"));
        ends("Rust extraction", rex.text, "OTHER DEALINGS IN THE SOFTWARE.");

        let curl = by_name("libcurl");
        assert_eq!(curl.kind, "MIT / ISC");
        ends("libcurl", curl.text, "written authorization of the copyright holder.");
    }

    /// The detail wrap keeps paragraph shape: blank source lines become
    /// dedicated gap rows and every non-blank row indexes real text.
    #[test]
    fn detail_wrap_keeps_paragraph_gaps() {
        let font = crate::shelf::shelf_font();
        let rows = wrap_rows_forward(font, LOG_FONT, LICENSES[1].text, 400.0, LIC_MAX_ROWS);
        assert!(rows.len() > 4);
        assert!(rows.iter().any(|r| r.blank));
        for r in rows.iter().filter(|r| !r.blank) {
            assert!(!LICENSES[1].text[r.start..r.end].is_empty());
        }
    }
}
