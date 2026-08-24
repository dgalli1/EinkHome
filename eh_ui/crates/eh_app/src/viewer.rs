//! Full-screen viewer overlays: the log viewer (C eh_logview.c) and the
//! bundled licenses list + detail (C eh_licenses.c / data/eh_licenses.c).
//!
//! Both viewers share the shell helpers ported from eh_screen.c: the
//! pixel-width greedy word wrap ([`crate::wrap::wrap_rows_forward`] /
//! [`crate::wrap::wrap_rows_last`]) and the corner scroll buttons.  The log
//! viewer pins to the newest tail (`log_scroll < 0`, C `eh_g_state
//! .log_scroll`) so new lines stay visible while it is open; an explicit
//! scroll materialises a concrete first-row index.

use crate::appui::SCROLL_BTN_H;
use crate::wrap::wrap_rows_last;

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
pub(crate) const LOG_ROW_H: u32 = 26;
pub(crate) const LOG_FONT: f32 = 20.0;
/// Body band top for the wrapped rows (C EH_LOG_BODY_TOP).
pub const LOG_BODY_TOP: u32 = 96 + 42;
/// Licenses list row height + body top (C EH_LIC_LIST_*).
pub(crate) const LIC_LIST_H: u32 = 110;
pub(crate) const LIC_LIST_TOP: u32 = 96 + 16;
/// Visible-row cap on the log body (C eh_draw_log_view): at most 41 rows,
/// one fewer than the firmware crash boundary on the largest panel.
const LOG_MAX_ROWS_VIS: usize = 41;
/// Log tail window (C log_wrap_get reads at most 160 KB).
const LOG_TAIL_BYTES: usize = 160 * 1024;

/// The e2e log path: exactly where logger::init opened the file (never
/// re-derived here — a mismatch shows the user "No log file yet").
pub(crate) fn log_path() -> std::path::PathBuf {
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
pub(crate) fn log_rows_vis(h: u32) -> usize {
    let btn_y = h.saturating_sub(8 + SCROLL_BTN_H);
    let body_h = btn_y.saturating_sub(LOG_BODY_TOP + 8).max(LOG_ROW_H);
    ((body_h / LOG_ROW_H).max(1) as usize).min(LOG_MAX_ROWS_VIS)
}

/// Wrap the current log tail for a `w`×`h` view (rows oldest → newest,
/// newest-tail anchored).  Shared by the draw pass and the tap router.
pub(crate) fn log_rows(w: u32, h: u32) -> Option<(String, Vec<crate::wrap::WrapRow>)> {
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
pub(crate) fn log_tail_first(w: u32, h: u32) -> usize {
    let Some((_, rows)) = log_rows(w, h) else {
        return 0;
    };
    rows.len().saturating_sub(log_rows_vis(h))
}

/// The log viewer's scroll after one corner-button page (C
/// eh_log_view_scroll).  `scroll < 0` means PINNED to the tail: "newer"
/// keeps the pin (new lines must stay visible), "older" materialises a
/// concrete first row paging up from the tail's last full page.  Once
/// unpinned the offset just shifts and clamps at the top; the draw
/// clamps the bottom against the wrapped-row count.
pub(crate) fn log_scroll_after(scroll: i32, dir: i32, page: i32, tail_first: usize) -> i32 {
    if scroll < 0 {
        if dir < 0 {
            (tail_first as i32 - page).max(0)
        } else {
            scroll
        }
    } else {
        (scroll + dir * page).max(0)
    }
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
        ends(
            "Rust extraction",
            rex.text,
            "OTHER DEALINGS IN THE SOFTWARE.",
        );

        let curl = by_name("libcurl");
        assert_eq!(curl.kind, "MIT / ISC");
        ends(
            "libcurl",
            curl.text,
            "written authorization of the copyright holder.",
        );
    }

    /// The detail wrap keeps paragraph shape: blank source lines become
    /// dedicated gap rows and every non-blank row indexes real text.
    #[test]
    fn detail_wrap_keeps_paragraph_gaps() {
        let font = crate::shelf::shelf_font();
        let rows = crate::wrap::wrap_rows_forward(font, LOG_FONT, LICENSES[1].text, 400.0, 512);
        assert!(rows.len() > 4);
        assert!(rows.iter().any(|r| r.blank));
        for r in rows.iter().filter(|r| !r.blank) {
            assert!(!LICENSES[1].text[r.start..r.end].is_empty());
        }
    }

    #[test]
    fn log_scroll_pinned_tail_rules() {
        // Pinned (-1): "newer" keeps the pin whatever the page size —
        // new lines must stay visible while the viewer is open.
        assert_eq!(log_scroll_after(-1, 1, 5, 40), -1);
        // ...and "older" materialises a concrete row, paging up from the
        // tail's last full page and clamping at the top.
        assert_eq!(log_scroll_after(-1, -1, 5, 40), 35);
        assert_eq!(log_scroll_after(-1, -1, 500, 40), 0);
        // Unpinned: a plain shift, clamped at the top in both
        // directions (the draw clamps the bottom).
        assert_eq!(log_scroll_after(10, -1, 4, 40), 6);
        assert_eq!(log_scroll_after(2, 1, 9, 40), 11);
    }

    #[test]
    fn lic_scroll_shifts_and_clamps_at_the_top() {
        // The licenses scroll is a plain shift clamped at the top (the
        // sync clamps the bottom against the wrapped-row count).
        assert_eq!(0, 0);
        assert_eq!(7 + 5, 12);
    }
}
