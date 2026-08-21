//! The Applications launcher (C eh_launcher.c + eh_plat_pb_launcher.c): a
//! full-screen overlay with the shared header, a scrolling 3-column grid
//! (group headers span the width, app cells flow three per row), corner
//! scroll buttons when the column overflows, and a tap → NewTaskEx launch.
//!
//! The item list comes from the firmware desktop configs (apps_db.json
//! application definitions + view.json groups / U_* user apps) plus a scan
//! of /mnt/ext1/applications — the stock bookshelf's exact sources.  Icons
//! are the firmware's (theme names or image paths); undecodable ones get
//! the C app's single-letter placeholder box.

use serde_json::Value;
use eh_hal::{Framebuffer, Rect};
use eh_render::draw_text;
use eh_shell::{GRAY_BLACK, GRAY_LGRAY, GRAY_WHITE};

use crate::app::{App, LauncherItem, Overlay};

/// Grid rhythm (C EH_LAUNCHER_*).
pub const COLS: u32 = 3;
pub const CELL_H: u32 = 232;
pub const ICON_SZ: u32 = 120;
pub const GROUP_H: u32 = 64;
pub const MARGIN: u32 = 16;
/// Corner scroll-button band (C EH_SCROLL_BTN_W/H).
pub const SCROLL_W: u32 = 150;
pub const SCROLL_H: u32 = 96;

fn env_or(key: &str, dflt: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| dflt.to_string())
}

/// The user-apps scan dir (C EH_USER_APPS_DIR); EH_USER_APPS_DIR overrides
/// for host verification.
fn user_apps_dir() -> String {
    env_or("EH_USER_APPS_DIR", "/mnt/ext1/applications")
}

/// EH_DESKTOP_DIR overrides both for host verification.
fn desktop_paths() -> (Vec<String>, Vec<String>) {
    if let Ok(d) = std::env::var("EH_DESKTOP_DIR") {
        (vec![format!("{d}/apps_db.json")], vec![format!("{d}/view.json")])
    } else {
        (
            vec![
                "/mnt/ext1/system/config/desktop/apps_db.json".into(),
                "/ebrmain/config/desktop/apps_db.json".into(),
            ],
            vec![
                "/mnt/ext1/system/config/desktop/view.json".into(),
                "/ebrmain/config/desktop/view.json".into(),
            ],
        )
    }
}

/// Resolve one desktop-config path (PB tries /mnt/ext1 then /ebrmain).
fn load_json(paths: &[&str]) -> Option<Value> {
    for p in paths {
        let pb = std::path::Path::new(p);
        if let Ok(text) = std::fs::read_to_string(pb) {
            if let Ok(v) = serde_json::from_str(&text) {
                return Some(v);
            }
        }
    }
    None
}

/// A desktop-config value: a plain string, an `{"path": ...}` object, or a
/// device/partner/localization object (the C app's eh_lc_resolve,
/// simplified to the first string it finds).
fn lc_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => {
            for key in ["path", "value", "all"] {
                if let Some(Value::String(s)) = o.get(key) {
                    return s.clone();
                }
            }
            o.values()
                .find_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default()
        }
        _ => String::new(),
    }
}

fn lc_visible(v: &Value) -> bool {
    match v {
        Value::String(s) => s != "0",
        _ => true,
    }
}

fn add_header(items: &mut Vec<LauncherItem>, text: &str, already: &mut bool) {
    if *already {
        return;
    }
    *already = true;
    items.push(LauncherItem { group: true, text: text.to_string(), ..Default::default() });
}

fn has_path(items: &[LauncherItem], path: &str) -> bool {
    items.iter().any(|it| !it.group && it.path == path)
}

/// 1 when a "User"/"Users" group header is already present (C
/// launcher_has_user_header) — the ext1 scan reuses an existing one.
fn has_user_header(items: &[LauncherItem]) -> bool {
    items.iter().any(|it| it.group && (it.text == "User" || it.text == "Users"))
}

/// Build the item list (C eh_plat_launcher_build → pb_launcher_build):
/// view.json groups → apps_db definitions, then view.json U_* user apps,
/// then the /mnt/ext1/applications scan.  Returns false when the list is
/// empty (nothing to launch anywhere).
pub fn build<B: Framebuffer>(app: &mut App<B>) -> bool {
    app.launcher_items.clear();

    let (db_paths, vw_paths) = desktop_paths();
    let db_paths: Vec<&str> = db_paths.iter().map(|s| s.as_str()).collect();
    let vw_paths: Vec<&str> = vw_paths.iter().map(|s| s.as_str()).collect();
    let db = load_json(&db_paths);
    let vw = load_json(&vw_paths);
    let db_apps = db.as_ref().and_then(|v| v.get("applications")).cloned();
    if let (Some(db_apps), Some(vw)) = (db_apps.as_ref(), vw.as_ref()) {
        // 1. view.json "view.groups": a header + each app id.
        if let Some(groups) = vw.pointer("/view/groups").and_then(|g| g.as_array()) {
            for g in groups {
                let Some(apps_arr) = g.get("apps").and_then(|a| a.as_array()) else {
                    continue;
                };
                let title = g.get("title").map(lc_str).filter(|t| !t.is_empty());
                if let Some(t) = title {
                    // C pb_build_groups: a header row per titled group,
                    // unconditional (no cross-group dedup).
                    app.launcher_items.push(LauncherItem { group: true, text: t, ..Default::default() });
                }
                for a in apps_arr {
                    let Some(id) = a.as_str() else {
                        continue;
                    };
                    let Some(def) = db_apps.get(id) else {
                        continue;
                    };
                    if let Some(vis) = def.get("visible") {
                        if !lc_visible(vis) {
                            continue;
                        }
                    }
                    let text = def
                        .get("title")
                        .map(lc_str)
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| id.to_string());
                    let it = LauncherItem {
                        text,
                        path: def.get("path").map(lc_str).unwrap_or_default(),
                        icon: def.get("icon").map(lc_str).unwrap_or_default(),
                        ..Default::default()
                    };
                    if !it.path.is_empty() && !has_path(&app.launcher_items, &it.path) {
                        app.launcher_items.push(it);
                    }
                }
            }
        }
        // 2. view.json "applications" U_* user apps not in a group.
        if let Some(vw_apps) = vw.get("applications").and_then(|a| a.as_object()) {
            let mut hdr = false;
            for (key, val) in vw_apps {
                if !key.starts_with("U_") {
                    continue;
                }
                if let Some(vis) = val.get("visible") {
                    if !lc_visible(vis) {
                        continue;
                    }
                }
                add_header(&mut app.launcher_items, "Users", &mut hdr);
                let text = val
                    .get("title")
                    .map(lc_str)
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| key.clone());
                let it = LauncherItem {
                    text,
                    path: val.get("path").map(lc_str).unwrap_or_default(),
                    icon: val.get("icon").map(lc_str).unwrap_or_default(),
                    ..Default::default()
                };
                if !it.path.is_empty() && !has_path(&app.launcher_items, &it.path) {
                    app.launcher_items.push(it);
                }
            }
        }
    }

    // 3. Scan /mnt/ext1/applications for *.app the firmware hasn't
    //    recorded (C eh_launcher_scan_ext1_apps), under a "Users" header.
    let scan_dir = user_apps_dir();
    if let Ok(rd) = std::fs::read_dir(&scan_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".app") {
                continue;
            }
            let path = format!("{scan_dir}/{name}");
            if has_path(&app.launcher_items, &path) {
                continue;
            }
            // C eh_launcher_scan_ext1_apps: reuse an existing "Users"
            // header (from the U_* loop) instead of adding a second one.
            if !has_user_header(&app.launcher_items) {
                app.launcher_items.push(LauncherItem { group: true, text: "Users".into(), ..Default::default() });
            }
            app.launcher_items.push(LauncherItem {
                group: false,
                text: name.trim_end_matches(".app").to_string(),
                path,
                icon: String::new(),
            });
        }
    }


    // Host fallback (the C SDL build's freedesktop discovery): when the
    // firmware desktop configs and the ext1 scan yield nothing, list the
    // standard .desktop application dirs so the launcher still opens.
    if app.launcher_items.is_empty() {
        scan_desktop_apps(app);
    }

    layout(app);
    crate::log(&format!(
        "[eh_app] launcher built: {} items, body_h={}",
        app.launcher_items.len(),
        app.launcher_body_h
    ));
    !app.launcher_items.is_empty()
}

/// Scan the freedesktop application dirs (C eh_plat_launcher_build on the
/// SDL backend): /usr/share/applications then $HOME/.local/share/
/// applications, mapping Name=/Exec=/Icon= onto launcher items and
/// skipping Hidden/NoDisplay/non-application entries.
fn scan_desktop_apps<B: Framebuffer>(app: &mut App<B>) {
    let mut dirs = vec!["/usr/share/applications".to_string()];
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(format!("{home}/.local/share/applications"));
    }
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".desktop"))
            .collect();
        files.sort();
        for name in files {
            let Ok(text) = std::fs::read_to_string(format!("{dir}/{name}")) else {
                continue;
            };
            let mut entry_type = String::new();
            let mut hidden = false;
            let mut no_display = false;
            let mut title = String::new();
            let mut exec = String::new();
            let mut icon = String::new();
            for line in text.lines() {
                let Some((key, val)) = line.split_once('=') else {
                    continue;
                };
                match key.trim() {
                    "Type" => entry_type = val.trim().to_string(),
                    "Hidden" => hidden = val.trim() == "true",
                    "NoDisplay" => no_display = val.trim() == "true",
                    "Name" => title = val.trim().to_string(),
                    "Exec" => exec = val.trim().to_string(),
                    "Icon" => icon = val.trim().to_string(),
                    _ => {}
                }
            }
            if entry_type != "Application" || hidden || no_display {
                continue;
            }
            // Exec's first argv token is the launch path (C
            // parse_desktop_exec); strip quotes.
            let cmd = exec
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches('"')
                .to_string();
            if cmd.is_empty() {
                continue;
            }
            app.launcher_items.push(LauncherItem {
                group: false,
                text: if title.is_empty() {
                    name.trim_end_matches(".desktop").to_string()
                } else {
                    title
                },
                path: cmd,
                icon,
            });
        }
    }
}

/// Lay every item out in one continuous column (C eh_launcher_layout):
/// headers span the width, app cells flow `COLS` per row.  `launcher_rects`
/// is parallel to `launcher_items` (C's BsLauncherItem carries its own
/// x/y/w/h), so draw and hit share one geometry.
pub fn layout<B: Framebuffer>(app: &mut App<B>) {
    let w = app.screen().framebuffer().screen().width;
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
            app.launcher_rects.push(Rect { x: MARGIN, y: y as u32, w: w - 2 * MARGIN, h: GROUP_H });
            y += GROUP_H as i32;
        } else {
            if col >= COLS {
                col = 0;
                y += CELL_H as i32;
            }
            app.launcher_rects.push(Rect { x: MARGIN + col * cell_w, y: y as u32, w: cell_w, h: CELL_H });
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
        h = h.saturating_sub(SCROLL_H);
    }
    (top, h)
}

/// The clamped scroll offset + max (C's max_scroll clamp).
fn scroll_state<B: Framebuffer>(app: &App<B>) -> (i32, i32) {
    let (_, body_h) = body_rect(app);
    let max = (app.launcher_body_h - body_h as i32).max(0);
    (app.launcher_scroll.clamp(0, max), max)
}

/// Draw the launcher overlay.  A row's screen y is `layout_y - scroll +
/// body_top` (C's draw loop); rows fully outside the body are skipped.
pub fn draw<B: Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_WHITE);
    crate::settings::draw_header(surf, "Applications", dirty);

    let (body_top, body_h) = body_rect(app);
    let (scroll, max_scroll) = scroll_state(app);
    let body_bottom = body_top as i32 + body_h as i32;
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    let fmt = surf.format();

    if app.launcher_items.is_empty() {
        let msg = "No applications";
        let tw = font.width(msg, 32.0) as i32;
        draw_text(surf, font, 32.0, msg, (w as i32 - tw) / 2, (body_top + body_h / 2) as i32, GRAY_BLACK, &mut glyph);
        return;
    }

    let items: Vec<(Rect, bool, String, String)> = app
        .launcher_items
        .iter()
        .zip(app.launcher_rects.iter())
        .map(|(it, r)| (*r, it.group, it.icon.clone(), it.text.clone()))
        .collect();
    for (r, is_group, icon, text) in items.iter() {
        let r = *r;
        let sy = r.y as i32 - scroll + body_top as i32;
        if sy + r.h as i32 <= body_top as i32 || sy >= body_bottom {
            continue;
        }
        if *is_group {
            // Group heading row (C launcher_draw_heading): white band,
            // baseline rule, title at the left.
            surf.fill_gray(Rect::from_xy(r.x as i32, sy, r.w as i32, r.h as i32), GRAY_WHITE);
            surf.hline(r.x, (sy + r.h as i32 - 2) as u32, r.w, 2, GRAY_BLACK);
            draw_text(surf, font, 28.0, text, (r.x + 12) as i32, sy + (r.h as i32) / 2 + 10, GRAY_BLACK, &mut glyph);
        } else {
            let cx = (r.x + r.w / 2) as i32;
            let icon_cy = sy + 12 + (ICON_SZ as i32) / 2;
            draw_icon(surf, icon, cx, icon_cy, text, app, font, &mut glyph, fmt);
            let ly = sy + 12 + ICON_SZ as i32 + 8;
            let maxw = r.w as i32 - 8;
            draw_label(surf, font, &mut glyph, text, cx, ly, maxw);
        }
    }

    // Corner scroll buttons while the column overflows (C
    // eh_draw_scroll_buttons: bottom-left up / bottom-right down).
    if max_scroll > 0 {
        let y0 = h.saturating_sub(SCROLL_H);
        let (up_ok, down_ok) = (scroll > 0, scroll < max_scroll);
        for (bx, ok, up) in [(0u32, up_ok, true), (w - SCROLL_W, down_ok, false)] {
            surf.fill_gray(Rect { x: bx, y: y0, w: SCROLL_W, h: SCROLL_H }, GRAY_WHITE);
            let col = if ok { GRAY_BLACK } else { GRAY_LGRAY };
            surf.rect_outline(Rect { x: bx, y: y0, w: SCROLL_W, h: SCROLL_H }, 2, col);
            let cx = bx as i32 + SCROLL_W as i32 / 2;
            let cy = y0 as i32 + SCROLL_H as i32 / 2;
            if up {
                surf.line(cx - 24, cy + 14, cx, cy - 14, 2, col);
                surf.line(cx + 24, cy + 14, cx, cy - 14, 2, col);
            } else {
                surf.line(cx - 24, cy - 14, cx, cy + 14, 2, col);
                surf.line(cx + 24, cy - 14, cx, cy + 14, 2, col);
            }
        }
    }
}

/// One launcher icon: decoded firmware art (image paths only — theme names
/// need GetResource, not available here) or the C app's single-letter
/// placeholder box.
fn draw_icon<B: eh_hal::Framebuffer>(
    surf: &mut eh_render::Surface,
    icon: &str,
    cx: i32,
    cy: i32,
    title: &str,
    app: &mut crate::app::App<B>,
    font: &eh_render::Font,
    glyph: &mut eh_render::Glyph,
    fmt: eh_hal::PixelFormat,
) {
    let _t0 = std::time::Instant::now();
    let x0 = cx - (ICON_SZ as i32) / 2;
    let y0 = cy - (ICON_SZ as i32) / 2;
    let mut ok = false;
    let art = app.icon_cache.get(icon).cloned().or_else(|| {
        if icon.is_empty() || !icon.starts_with('/') {
            return None;
        }
        let decoded = std::fs::read(icon)
            .ok()
            .and_then(|bytes| crate::cover::decode_rgb(&bytes).ok());
        if let Some(a) = &decoded {
            app.icon_cache.insert(icon.to_string(), a.clone());
        }
        decoded
    });
    if let Some((iw, ih, rgb)) = art {
        // Scale down oversized icons aspect-preserving (C
        // launcher_draw_bitmap).
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
            &rgb,
            iw,
            ih,
            fmt,
            Rect::from_xy(x0 + ((ICON_SZ as i32 - bw as i32) / 2), y0 + ((ICON_SZ as i32 - bh as i32) / 2), bw as i32, bh as i32),
        );
        ok = true;
    }
    if !ok {
        surf.fill_gray(Rect::from_xy(x0, y0, ICON_SZ as i32, ICON_SZ as i32), GRAY_WHITE);
        surf.rect_outline(Rect::from_xy(x0, y0, ICON_SZ as i32, ICON_SZ as i32), 2, GRAY_BLACK);
        if let Some(ch) = title.chars().next() {
            let s = ch.to_string();
            let tw = font.width(&s, 56.0) as i32;
            draw_text(surf, font, 56.0, &s, cx - tw / 2, cy + 20, GRAY_BLACK, glyph);
        }
    }
}

/// Center the app label in the cell, wrapping to two lines at the last
/// space or ellipsizing (C launcher_draw_app_label's fallbacks).
fn draw_label(surf: &mut eh_render::Surface, font: &eh_render::Font, glyph: &mut eh_render::Glyph, text: &str, cx: i32, ly: i32, maxw: i32) {
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
        draw_text(surf, font, 24.0, l2, cx - t2 / 2, ly + 26, GRAY_BLACK, glyph);
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
    if crate::settings::back_rect().contains(x, y) {
        app.overlay = Overlay::None;
        app.launcher_rects.clear();
        return;
    }
    let w = app.screen().framebuffer().screen().width;
    // Corner scroll buttons (bottom band).
    if y >= (app.content_bottom - SCROLL_H) as i32 {
        let (scroll, max) = scroll_state(app);
        if max > 0 {
            let (_, body_h) = body_rect(app);
            let dir = if x < SCROLL_W as i32 {
                -1
            } else if x >= (w - SCROLL_W) as i32 {
                1
            } else {
                0
            };
            if dir != 0 {
                app.launcher_scroll = (scroll + dir * body_h as i32).clamp(0, max);
            }
        }
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
        if x >= r.x as i32 && x < (r.x + r.w) as i32 && by >= r.y as i32 && by < (r.y + r.h) as i32 {
            let it = it.clone();
            app.overlay = Overlay::None;
            app.launcher_rects.clear();
            crate::log(&format!("[eh_app] launching app path={}", it.path));
            if !app.screen().framebuffer_mut().launch_app(&it.path, &it.text) {
                crate::log("[eh_app] launch failed (no task system on this platform)");
            }
            return;
        }
    }
}