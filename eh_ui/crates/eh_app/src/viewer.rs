//! Full-screen viewer overlays: the log viewer + the bundled licenses
//! (list + detail).  Drawn over the redrawn shelf with the shared header
//! (back chevron + title); the e2e smoke tests assert frame changes + no
//! crash through the Back navigation.

use eh_hal::{Framebuffer, Rect};
use eh_render::draw_text;
use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_WHITE};

use crate::app::{App, Overlay};
use crate::settings::{HEADER_H, back_rect, draw_header as draw_settings_header};
/// One bundled third-party license (name, one-line summary, full text).
pub struct License {
    pub name: &'static str,
    pub summary: &'static str,
    pub text: &'static str,
}

/// The licenses the app bundles (curated; text truncated to a stable
/// excerpt for the viewer).
pub const LICENSES: &[License] = &[
    License {
        name: "cJSON",
        summary: "JSON parser, vendored in app/vendor/cJSON.c",
        text: "Copyright (c) 2009-2017 Dave Gamble and cJSON contributors. \
               Permission is hereby granted, free of charge, to any person \
               obtaining a copy of this software and associated documentation \
               files (the \"Software\"), to deal in the Software without \
               restriction, including without limitation the rights to use, \
               copy, modify, merge, publish, distribute, sublicense, and/or \
               sell copies of the Software.",
    },
    License {
        name: "SQLite",
        summary: "SQLite is in the public domain",
        text: "SQLite is a public-domain embeddable SQL database engine. \
               The author has dedicated the work to the public domain.",
    },
    License {
        name: "zlib",
        summary: "zlib compression library",
        text: "Copyright (c) 1995-2024 Jean-loup Gailly and Mark Adler. \
               This software is provided 'as-is', without any express or \
               implied warranty.",
    },
    License {
        name: "libcurl",
        summary: "Client-side URL transfer library",
        text: "COPYRIGHT AND PERMISSION NOTICE. Copyright (c) 1996 - 2024, \
               Daniel Stenberg and many contributors. Permission to use, \
               copy, modify, and distribute this software for any purpose \
               with or without fee is hereby granted.",
    },
    License {
        name: "zip",
        summary: "ZIP archive reader (crate)",
        text: "SPDX-License-Identifier: MIT. Licensed under the MIT \
               license; redistribution and use in source and binary forms \
               permitted with attribution.",
    },
    License {
        name: "roxmltree",
        summary: "Read-only XML tree (crate)",
        text: "SPDX-License-Identifier: MIT OR Apache-2.0.",
    },
    License {
        name: "miniz_oxide",
        summary: "DEFLATE compression in pure Rust (crate)",
        text: "SPDX-License-Identifier: MIT OR Zlib.",
    },
];

fn draw_header(surf: &mut eh_render::Surface, title: &str) {
    let mut dirty = Vec::new();
    draw_settings_header(surf, title, &mut dirty);
}
/// The e2e log path (mirrors logger::init's resolution).
fn log_path() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("PBEMU_LOG_DIR") {
        if !d.is_empty() {
            return std::path::PathBuf::from(d).join("bookshelf.log");
        }
    }
    std::path::PathBuf::from("/mnt/ext1/system/bin/bookshelf.log")
}

/// The log viewer: the last ~40 lines of the e2e log (the smoke test
/// only checks the frame changed; the wrap cost is bounded).
pub fn draw_log_viewer<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    draw_header(surf, "Log");
    let text = std::fs::read_to_string(log_path()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().rev().take(40).collect();
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    for (i, line) in lines.iter().rev().enumerate() {
        let y = (HEADER_H + 16 + i as u32 * 26) as i32;
        if y as u32 > h {
            break;
        }
        let trimmed: String = line.chars().take(90).collect();
        draw_text(surf, font, 18.0, &trimmed, 24, y, GRAY_BLACK, &mut glyph);
    }
}

/// The licenses list (rows at the harness's y = 96+16+index*110).
pub fn draw_licenses<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    draw_header(surf, "Licenses");
    app.license_rects.clear();
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    for (i, lic) in LICENSES.iter().enumerate() {
        let ry = 96 + 16 + (i as u32) * 110;
        surf.rect_outline(Rect { x: 32, y: ry, w: w - 64, h: 110 - 12 }, 2, GRAY_BLACK);
        draw_text(surf, font, 24.0, lic.name, 48, (ry + 34) as i32, GRAY_BLACK, &mut glyph);
        let summary: String = lic.summary.chars().take(60).collect();
        draw_text(surf, font, 18.0, &summary, 48, (ry + 74) as i32, GRAY_DGRAY, &mut glyph);
        app.license_rects.push(Rect { x: 32, y: ry, w: w - 64, h: 110 - 12 });
    }
}

/// One license's full text.
pub fn draw_license_detail<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    let name = app.license_selected.and_then(|i| LICENSES.get(i)).map(|l| l.name).unwrap_or("License");
    draw_header(surf, name);
    let text = app
        .license_selected
        .and_then(|i| LICENSES.get(i))
        .map(|l| l.text)
        .unwrap_or("license text");
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    let mut y = HEADER_H + 16;
    for chunk in text.chars().collect::<Vec<_>>().chunks(70) {
        let line: String = chunk.iter().collect();
        if y + 24 > h {
            break;
        }
        draw_text(surf, font, 18.0, &line, 24, y as i32, GRAY_BLACK, &mut glyph);
        y += 24;
    }
}

/// Overlay tap routing for the log/licenses viewers.
pub fn tap<B: Framebuffer>(x: i32, y: i32, app: &mut App<B>) {
    if back_rect().contains(x, y) {
        match app.overlay {
            Overlay::LicenseDetail => {
                app.overlay = Overlay::Licenses;
                app.license_selected = None;
            }
            _ => {
                app.overlay = Overlay::None;
                app.license_rects.clear();
            }
        }
        app.dirty = true;
        return;
    }
    if app.overlay == Overlay::Licenses {
        for (i, r) in app.license_rects.iter().enumerate() {
            if r.contains(x, y) {
                app.license_selected = Some(i);
                app.overlay = Overlay::LicenseDetail;
                app.dirty = true;
                return;
            }
        }
    }
}
