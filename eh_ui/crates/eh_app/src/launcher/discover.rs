//! Launcher item discovery (C eh_launcher.c data side): the firmware
//! desktop configs (apps_db.json applications + view.json groups / U_*
//! user apps), the /mnt/ext1/applications scan, the seven-dimension
//! conditional-resolution engine (C eh_lc_*), and [`build`] — the merge +
//! icon-resolution pass that runs while the framebuffer is attached.

use serde_json::Value;

use crate::app::{App, LauncherItem};

use super::LAUNCHER_MAX_PARAMS;

/// The user-apps scan dir (C EH_USER_APPS_DIR); EH_USER_APPS_DIR overrides
/// for host verification, else the platform's dir (PocketBook only).
fn user_apps_dir(paths: &eh_hal::PlatformPaths) -> String {
    std::env::var("EH_USER_APPS_DIR").unwrap_or_else(|_| paths.user_apps_dir.clone())
}

/// EH_DESKTOP_DIR overrides both for host verification.
fn desktop_paths() -> (Vec<String>, Vec<String>) {
    if let Ok(d) = std::env::var("EH_DESKTOP_DIR") {
        (
            vec![format!("{d}/apps_db.json")],
            vec![format!("{d}/view.json")],
        )
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

/// The device profile feeding the seven-dimension conditional resolution
/// (C BsLcProfile / eh_g_lcprof).  Built once per launcher build from the
/// hal's device profile + the configured language; `globalcfg` is a
/// resolution dimension only, not a stored value.
pub(crate) struct LcProfile {
    device: String,
    partner: String,
    has_audio: String,
    has_cloud: String,
    language: String,
    localization: String,
}

impl LcProfile {
    /// Neutral profile (C eh_g_lcprof's static init) with the device
    /// identity and language from the hal/config when available.
    pub(crate) fn from_env<B: eh_hal::Framebuffer>(app: &mut App<B>) -> Self {
        let prof = app.device_profile();
        Self {
            device: prof.device_number.to_string(),
            partner: "pocketbook".into(),
            has_audio: if prof.has_audio { "true" } else { "false" }.into(),
            has_cloud: "false".into(),
            language: app.config.language.clone().unwrap_or_else(|| "en".into()),
            localization: "WW".into(),
        }
    }

    /// The profile-mapped key for one dimension (C eh_lc_prof_val;
    /// `globalcfg` has no profile value).
    fn val(&self, dim: &str) -> Option<&str> {
        match dim {
            "device" => Some(&self.device),
            "partner" => Some(&self.partner),
            "has_audio" => Some(&self.has_audio),
            "has_cloud" => Some(&self.has_cloud),
            "language" => Some(&self.language),
            "localization" => Some(&self.localization),
            _ => None,
        }
    }
}

/// The seven dimensions tried, in order, when an object value is met (C
/// eh_lc_dims).
const LC_DIMS: [&str; 7] = [
    "device",
    "partner",
    "has_audio",
    "has_cloud",
    "language",
    "localization",
    "globalcfg",
];

/// Pick which key of a dimension object resolves (C eh_lc_pick_key): the
/// wanted key when present, else "all", else "default", else the first.
fn lc_pick_key<'a>(
    obj: &'a serde_json::Map<String, Value>,
    want: Option<&'a str>,
) -> Option<&'a str> {
    let mut first: Option<&str> = None;
    let mut all = false;
    let mut def = false;
    for k in obj.keys() {
        if first.is_none() {
            first = Some(k);
        }
        if want == Some(k.as_str()) {
            return want;
        }
        if k == "all" {
            all = true;
        }
        if k == "default" {
            def = true;
        }
    }
    if all {
        return Some("all");
    }
    if def {
        return Some("default");
    }
    first
}

/// The conditional-resolution engine (C eh_lc_resolve): a string copies
/// verbatim; an object resolves along the first present dimension key, or
/// — when no dimension matches — falls back per the current dimension
/// (`globalcfg` picks the first member carrying a "default"; others pick
/// the profile-mapped key).
pub(crate) fn lc_resolve(v: &Value, cur_dim: Option<&str>, prof: &LcProfile, out: &mut String) {
    out.clear();
    match v {
        Value::String(s) => out.push_str(s),
        Value::Object(obj) => {
            for d in LC_DIMS {
                if let Some(vp) = obj.get(d) {
                    lc_resolve(vp, Some(d), prof, out);
                    return;
                }
            }
            let Some(cur) = cur_dim else {
                // No current dimension: resolve the fallback key with a
                // NULL dimension (C lc_resolve_fallback).
                if let Some(k) = lc_pick_key(obj, None) {
                    lc_resolve(&obj[k], None, prof, out);
                }
                return;
            };
            if cur == "globalcfg" {
                // C lc_resolve_globalcfg: the first member whose value is
                // an object carrying a "default" wins — its "default"
                // child (any JSON type) is resolved with the same dim.
                for m in obj.values() {
                    if let Value::Object(mo) = m {
                        if let Some(defp) = mo.get("default") {
                            lc_resolve(defp, cur_dim, prof, out);
                            return;
                        }
                    }
                }
                return;
            }
            // Current dimension set: resolve the profile-mapped key (C
            // lc_resolve_dim).
            let want = prof.val(cur);
            if let Some(k) = lc_pick_key(obj, want) {
                lc_resolve(&obj[k], cur_dim, prof, out);
            }
        }
        _ => {}
    }
}

/// Conditional visibility (C eh_lc_resolve_bool): a JSON bool is taken
/// verbatim; a resolvable string is hidden only for the explicit falsey
/// spellings "0"/"false"/"no"/"off" (case-insensitive) — an empty value
/// (missing key) and anything else stays visible.
pub(crate) fn lc_visible(v: &Value, prof: &LcProfile) -> bool {
    if let Value::Bool(b) = v {
        return *b;
    }
    let mut buf = String::new();
    lc_resolve(v, None, prof, &mut buf);
    !matches!(
        buf.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// The @Token translation table (C eh_lc_token_en) — the firmware's
/// group/app name tokens with their English labels.
fn lc_token_en(tok: &str) -> Option<&'static str> {
    Some(match tok {
        "@Audio_books" => "Audio books",
        "@Browser" => "Browser",
        "@BookStoreShortName" => "Book Store",
        "@Legimi" => "Legimi",
        "@Calc" => "Calculator",
        "@Calendar" => "Calendar",
        "@Chess" => "Chess",
        "@coloring" => "Coloring",
        "@Sudoku" => "Sudoku",
        "@digital_frame" => "Digital Frame",
        "@Gallery" => "Gallery",
        "@Library" => "Library",
        "@Notes" => "Notes",
        "@Onleihe" => "Onleihe",
        "@Audio_player" => "Music",
        "@Pocketnews" => "RSS News",
        "@Settings" => "Settings",
        "@Snake" => "Snake",
        "@Scribble" => "Scribble",
        "@SendToPocketbook" => "Send to PB",
        "@Dictionary" => "Dictionary",
        "@Dropbox" => "Dropbox",
        "@Empik_store" => "Empik",
        "@Klondike" => "Solitaire",
        "@Kosynka" => "Solitaire",
        "@PBOnleiheLibrary" => "Onleihe",
        "@General" => "General",
        "@Games" => "Games",
        "@Users" => "Users",
        "@Empty" => "Empty",
        _ => return None,
    })
}

/// Raw resolved title → display text (C eh_lc_translate): an @Token maps
/// through the table (unknown tokens drop the @); otherwise `_` becomes a
/// space and the letter after each break is upper-cased.
pub(crate) fn lc_translate(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    let mut raw = raw;
    if let Some(stripped) = raw.strip_prefix('@') {
        if let Some(en) = lc_token_en(raw) {
            return en.to_string();
        }
        raw = stripped;
    }
    let mut out = String::with_capacity(raw.len());
    let mut cap_next = true;
    for c in raw.chars() {
        if c == '_' {
            out.push(' ');
            cap_next = true;
        } else if cap_next && c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
            cap_next = false;
        } else {
            out.push(c);
            cap_next = false;
        }
    }
    out
}

/// Resolve + translate a display title (C launcher_set_title's body).
fn lc_title(v: &Value, prof: &LcProfile) -> String {
    let mut raw = String::new();
    lc_resolve(v, None, prof, &mut raw);
    lc_translate(&raw)
}

/// Copy the optional "params"/"param" argument list into the item (C
/// launcher_set_params): an array of strings capped at
/// [`LAUNCHER_MAX_PARAMS`], or a single string becoming the one argument.
fn lc_params(def: &Value) -> Vec<String> {
    let mut par = def.get("params");
    if !par.is_some_and(|p| p.is_array()) {
        par = def.get("param");
    }
    match par {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|q| q.as_str())
            .take(LAUNCHER_MAX_PARAMS)
            .map(|s| s.to_string())
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

/// Resolve a path/icon value to a plain string (no @Token/_ translation —
/// C resolves those fields with eh_lc_resolve only).
fn lc_resolve_str(v: &Value, prof: &LcProfile) -> String {
    let mut s = String::new();
    lc_resolve(v, None, prof, &mut s);
    s
}

fn add_header(items: &mut Vec<LauncherItem>, text: &str, already: &mut bool) {
    if *already {
        return;
    }
    *already = true;
    items.push(LauncherItem {
        group: true,
        text: text.to_string(),
        ..Default::default()
    });
}

fn has_path(items: &[LauncherItem], path: &str) -> bool {
    items.iter().any(|it| !it.group && it.path == path)
}

/// 1 when a "User"/"Users" group header is already present (C
/// launcher_has_user_header) — the ext1 scan reuses an existing one.
fn has_user_header(items: &[LauncherItem]) -> bool {
    items
        .iter()
        .any(|it| it.group && (it.text == "User" || it.text == "Users"))
}

/// Build the item list (C eh_plat_launcher_build → pb_launcher_build):
/// view.json groups → apps_db definitions, then view.json U_* user apps,
/// then the /mnt/ext1/applications scan.  Returns false when the list is
/// empty (nothing to launch anywhere).
pub fn build<B: eh_hal::Framebuffer>(app: &mut App<B>) -> bool {
    let prof = LcProfile::from_env(app);

    let (db_paths, vw_paths) = desktop_paths();
    let db_paths: Vec<&str> = db_paths.iter().map(|s| s.as_str()).collect();
    let vw_paths: Vec<&str> = vw_paths.iter().map(|s| s.as_str()).collect();
    let db = load_json(&db_paths);
    let vw = load_json(&vw_paths);
    let db_apps = db
        .as_ref()
        .and_then(|v| v.get("applications"))
        .and_then(|a| a.as_object());

    // Merge the firmware desktop configs + the platform user-apps scan
    // (pure; see the assemble contract tests).
    let ext1_files = scan_ext1_app_files(&app.paths);
    app.launcher_items = assemble(db_apps, vw.as_ref(), &prof, &ext1_files);

    // Host fallback (the C SDL build's freedesktop discovery): when the
    // firmware desktop configs and the ext1 scan yield nothing, list the
    // standard .desktop application dirs so the launcher still opens.
    if app.launcher_items.is_empty() {
        scan_desktop_apps(app);
    }

    // Resolve every icon NOW, while the framebuffer is attached — the
    // firmware theme store cannot be consulted from the overlay draw
    // (present() has taken the screen out of App by then).
    let icons: Vec<String> = app
        .launcher_items
        .iter()
        .map(|it| {
            if it.group {
                String::new()
            } else {
                it.icon.clone()
            }
        })
        .collect();
    for (i, icon) in icons.iter().enumerate() {
        if !icon.is_empty() && app.launcher_items[i].art.is_none() {
            app.launcher_items[i].art = resolve_icon_art(app, icon);
        }
    }
    super::ui::layout(app);
    crate::log(&format!(
        "[eh_app] launcher built: {} items, body_h={}",
        app.launcher_items.len(),
        app.launcher_body_h
    ));
    !app.launcher_items.is_empty()
}

/// The user-apps dir's `*.app` files as full paths, SORTED by file name —
/// readdir order is arbitrary, so without the sort the launcher's grid
/// would reshuffle across boots.  An empty platform dir (hosts, Android)
/// scans nothing.
fn scan_ext1_app_files(paths: &eh_hal::PlatformPaths) -> Vec<String> {
    let dir = user_apps_dir(paths);
    if dir.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".app"))
        .map(|n| format!("{dir}/{n}"))
        .collect();
    out.sort();
    out
}

/// Assemble the launcher item list (C eh_plat_launcher_build →
/// pb_launcher_build's merge, pure — no I/O, no App):
///
/// 1. view.json `view.groups`: a header per titled group + each app id
///    resolved against apps_db, deduped by launch path;
/// 2. view.json `applications` U_* user apps under ONE reused "Users"
///    header;
/// 3. the scanned ext1 `*.app` files, joining that same header and never
///    duplicating a known path.
///
/// Empty everywhere → an empty list (build() then tries the host
/// freedesktop fallback).
fn assemble(
    db_apps: Option<&serde_json::Map<String, Value>>,
    vw: Option<&Value>,
    prof: &LcProfile,
    ext1_files: &[String],
) -> Vec<LauncherItem> {
    let mut items: Vec<LauncherItem> = Vec::new();
    if let (Some(db_apps), Some(vw)) = (db_apps, vw) {
        // 1. view.json "view.groups": a header + each app id.
        if let Some(groups) = vw.pointer("/view/groups").and_then(|g| g.as_array()) {
            for g in groups {
                let Some(apps_arr) = g.get("apps").and_then(|a| a.as_array()) else {
                    continue;
                };
                let title = g
                    .get("title")
                    .map(|t| lc_title(t, prof))
                    .filter(|t| !t.is_empty());
                if let Some(t) = title {
                    // C pb_build_groups: a header row per titled group,
                    // unconditional (no cross-group dedup).
                    items.push(LauncherItem {
                        group: true,
                        text: t,
                        ..Default::default()
                    });
                }
                for a in apps_arr {
                    let Some(id) = a.as_str() else {
                        continue;
                    };
                    let Some(def) = db_apps.get(id) else {
                        continue;
                    };
                    if let Some(vis) = def.get("visible") {
                        if !lc_visible(vis, prof) {
                            continue;
                        }
                    }
                    let text = def
                        .get("title")
                        .map(|t| lc_title(t, prof))
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| id.to_string());
                    let it = LauncherItem {
                        text,
                        path: def
                            .get("path")
                            .map(|v| lc_resolve_str(v, prof))
                            .unwrap_or_default(),
                        icon: def
                            .get("icon")
                            .map(|v| lc_resolve_str(v, prof))
                            .unwrap_or_default(),
                        params: lc_params(def),
                        ..Default::default()
                    };
                    if !it.path.is_empty() && !has_path(&items, &it.path) {
                        items.push(it);
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
                    if !lc_visible(vis, prof) {
                        continue;
                    }
                }
                add_header(&mut items, "Users", &mut hdr);
                // C pb_build_user_app resolves the U_* title verbatim
                // (no @Token/_→space translation) and takes no params.
                let text = val
                    .get("title")
                    .map(|t| {
                        let mut s = String::new();
                        lc_resolve(t, None, prof, &mut s);
                        s
                    })
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| key.clone());
                let it = LauncherItem {
                    text,
                    path: val
                        .get("path")
                        .map(|v| lc_resolve_str(v, prof))
                        .unwrap_or_default(),
                    icon: val
                        .get("icon")
                        .map(|v| lc_resolve_str(v, prof))
                        .unwrap_or_default(),
                    ..Default::default()
                };
                if !it.path.is_empty() && !has_path(&items, &it.path) {
                    items.push(it);
                }
            }
        }
    }

    // 3. The scanned /mnt/ext1/applications *.app files the firmware
    //    hasn't recorded (C eh_launcher_scan_ext1_apps), under a "Users"
    //    header — reusing the U_* loop's header instead of adding a
    //    second one.
    for path in ext1_files {
        // Belt-and-braces: the scanner already filtered, but this fn's
        // contract is "ext1 *.app files" (C checked the suffix in-scan).
        if !path.ends_with(".app") || has_path(&items, path) {
            continue;
        }
        if !has_user_header(&items) {
            items.push(LauncherItem {
                group: true,
                text: "Users".into(),
                ..Default::default()
            });
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        items.push(LauncherItem {
            group: false,
            text: name.trim_end_matches(".app").to_string(),
            path: path.clone(),
            ..Default::default()
        });
    }
    items
}

/// Scan the freedesktop application dirs (C eh_plat_launcher_build on the
/// SDL backend): /usr/share/applications then $HOME/.local/share/
/// applications, mapping Name=/Exec=/Icon= onto launcher items and
/// skipping Hidden/NoDisplay/non-application entries.
fn scan_desktop_apps<B: eh_hal::Framebuffer>(app: &mut App<B>) {
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
                params: Vec::new(),
                art: None,
            });
        }
    }
}

/// Resolve one launcher icon to decoded RGB while the framebuffer is
/// still attached (C eh_launcher.c launcher_icon_get): GetResource first,
/// then LoadPNG (which on this firmware also resolves bare theme names),
/// then a direct file read + decode for absolute paths.  MUST run from
/// build()/tap context — during the overlay draw the screen is taken out
/// of App and the firmware theme store cannot be consulted.
fn resolve_icon_art<B: eh_hal::Framebuffer>(
    app: &mut App<B>,
    icon: &str,
) -> Option<(Vec<u8>, u32, u32)> {
    if icon.is_empty() {
        return None;
    }
    if !icon.starts_with('/') {
        if let Some(tb) = app.theme_resource(icon).or_else(|| app.load_png(icon)) {
            let (w, h) = (tb.width as u32, tb.height as u32);
            if let Some(rgb) = tb.to_rgb() {
                return Some((rgb, w, h));
            }
        }
    }
    std::fs::read(icon)
        .ok()
        .and_then(|bytes| crate::cover::decode_rgb(&bytes).ok())
        .map(|(w, h, rgb)| (rgb, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prof() -> LcProfile {
        LcProfile {
            device: "741".into(),
            partner: "pocketbook".into(),
            has_audio: "true".into(),
            has_cloud: "false".into(),
            language: "en".into(),
            localization: "WW".into(),
        }
    }

    fn resolve(v: &Value) -> String {
        let mut s = String::new();
        lc_resolve(v, None, &prof(), &mut s);
        s
    }

    #[test]
    fn visible_truth_table() {
        let p = prof();
        // Explicit falsey spellings hide; JSON false hides.
        for v in [
            json!("0"),
            json!("false"),
            json!("no"),
            json!("off"),
            json!(false),
        ] {
            assert!(!lc_visible(&v, &p), "must be hidden: {v}");
        }
        // An empty value (missing key) and anything else stays visible —
        // including truthy spellings and unknown dimension objects.
        for v in [
            json!(""),
            json!("1"),
            json!("true"),
            json!("yes"),
            json!({"device": {"741": "on"}}),
        ] {
            assert!(lc_visible(&v, &p), "must be visible: {v}");
        }
    }

    #[test]
    fn resolves_profile_dimensions() {
        assert_eq!(
            resolve(&json!({"has_audio": {"true": "/audio.app", "false": "/plain.app"}})),
            "/audio.app"
        );
        let mut p = prof();
        p.has_audio = "false".into();
        let mut s = String::new();
        lc_resolve(
            &json!({"has_audio": {"true": "/audio.app", "false": "/plain.app"}}),
            None,
            &p,
            &mut s,
        );
        assert_eq!(s, "/plain.app");
        // Language + localization dims.
        assert_eq!(
            resolve(&json!({"language": {"de": "Bücher", "en": "Books", "all": "Books"}})),
            "Books"
        );
        let mut de = prof();
        de.language = "de".into();
        let mut s = String::new();
        lc_resolve(
            &json!({"language": {"de": "Bücher", "en": "Books"}}),
            None,
            &de,
            &mut s,
        );
        assert_eq!(s, "Bücher");
    }

    #[test]
    fn falls_back_to_all_then_default_then_first() {
        // Wanted key absent → "all" wins over "default" over first.
        assert_eq!(
            resolve(&json!({"partner": {"all": "A", "default": "D", "zzz": "Z"}})),
            "A"
        );
        assert_eq!(
            resolve(&json!({"partner": {"default": "D", "zzz": "Z"}})),
            "D"
        );
        assert_eq!(resolve(&json!({"partner": {"zzz": "Z"}})), "Z");
        // A profile-matched key beats the fallbacks.
        assert_eq!(
            resolve(&json!({"device": {"741": "mine", "all": "any", "default": "def"}})),
            "mine"
        );
    }

    #[test]
    fn globalcfg_takes_first_member_with_default() {
        let v = json!({
            "reader_a": {"enabled": "no"},
            "reader_b": {"default": {"font": "droid"}}
        });
        let mut s = String::new();
        lc_resolve(&v, Some("globalcfg"), &prof(), &mut s);
        // Resolves INTO the chosen member's value with the same dim.
        assert_eq!(s, "");
        let v2 = json!({"x": {"default": "picked"}});
        let mut s2 = String::new();
        lc_resolve(&v2, Some("globalcfg"), &prof(), &mut s2);
        assert_eq!(s2, "picked");
    }

    #[test]
    fn token_table_and_title_casing() {
        assert_eq!(lc_translate("@Gallery"), "Gallery");
        assert_eq!(lc_translate("@Audio_player"), "Music");
        assert_eq!(lc_translate("@Klondike"), "Solitaire");
        // Unknown token: drop the @, then _→space title-casing.
        assert_eq!(lc_translate("@chess_master"), "Chess Master");
        assert_eq!(lc_translate("digital_frame"), "Digital Frame");
        assert_eq!(lc_translate(""), "");
    }

    #[test]
    fn params_parse_and_cap() {
        // Array capped at EH_LAUNCHER_MAX_PARAMS.
        let def = json!({"params": ["a", "b", "c", "d", "e", "f"]});
        let got = lc_params(&def);
        assert_eq!(got.len(), LAUNCHER_MAX_PARAMS);
        assert_eq!(got, vec!["a", "b", "c", "d"]);
        // C only honours a bare-string form via "param" (a string
        // "params" is overwritten by the "param" lookup and lost when
        // "param" is absent — launcher_set_params verbatim).
        assert_eq!(lc_params(&json!({"param": "solo"})), vec!["solo"]);
        assert_eq!(lc_params(&json!({"params": "lost"})), Vec::<String>::new());
        assert_eq!(lc_params(&json!({"param": ["x"]})), vec!["x"]);
        assert_eq!(lc_params(&json!({})), Vec::<String>::new());
        // Non-string members are skipped (C only snprintf's strings).
        assert_eq!(
            lc_params(&json!({"params": [1, "keep", null]})),
            vec!["keep"]
        );
    }

    // ── assemble() contracts: the home-screen merge rules ───────────

    fn db_apps() -> Value {
        json!({
            "reader": {"title": {"all": "Reader"}, "path": "/ebrmain/reader.app", "icon": "READER"},
            "gallery": {"title": "@Gallery", "path": "/ebrmain/gallery.app"},
            "hidden": {"visible": "no", "path": "/x/hidden.app"}
        })
    }

    #[test]
    fn groups_merge_dedupe_and_respect_visibility() {
        let vw = json!({"view": {"groups": [
            {"title": "Main", "apps": ["reader", "gallery", "missing", "hidden"]},
            // All of this group's apps dedupe away, but C pb_build_groups
            // still emits the titled header unconditionally.
            {"title": "Again", "apps": ["reader"]}
        ]}});
        let items = assemble(
            Some(db_apps().as_object().unwrap()),
            Some(&vw),
            &prof(),
            &[],
        );
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(texts, ["Main", "Reader", "Gallery", "Again"]);
        // reader listed in both groups appears exactly once...
        assert_eq!(
            items
                .iter()
                .filter(|i| i.path == "/ebrmain/reader.app")
                .count(),
            1
        );
        // ...invisible apps and ids missing from apps_db never land.
        assert!(!texts.contains(&"hidden"));
        assert!(items.iter().all(|i| i.group || !i.path.is_empty()));
    }

    #[test]
    fn user_apps_share_one_users_header_and_fall_back_to_the_key() {
        let vw = json!({"applications": {
            "U_ko": {"title": "KOReader", "path": "/mnt/ext1/koreader.app"},
            "U_pl": {"path": "/mnt/ext1/plumber.app"}
        }});
        // U_* assembly runs under build()'s both-configs guard (the C
        // shape), so the fixture carries an EMPTY apps_db, not none.
        let empty = json!({});
        let items = assemble(Some(empty.as_object().unwrap()), Some(&vw), &prof(), &[]);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        // ONE header for the whole U_* run (add_header's `already` flag);
        // an untitled user app falls back to its view.json key.
        assert_eq!(texts, ["Users", "KOReader", "U_pl"]);
    }

    #[test]
    fn ext1_files_join_the_same_header_without_duplicates() {
        let vw = json!({"applications": {"U_ko": {"path": "/mnt/ext1/applications/koreader.app"}}});
        let ext1 = vec![
            "/mnt/ext1/applications/koreader.app".into(), // known → skipped
            "/mnt/ext1/applications/mytool.app".into(),   // fresh → joins
            "/mnt/ext1/applications/notes.txt".into(),    // not scanned here
        ];
        // U_* + ext1 phases sit behind build()'s both-configs guard
        // (C shape): an EMPTY apps_db rather than none.
        let empty = json!({});
        let items = assemble(Some(empty.as_object().unwrap()), Some(&vw), &prof(), &ext1);
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        // Same single "Users" header; no duplicate koreader row; the
        // ".app" suffix is stripped from the label.
        assert_eq!(texts, ["Users", "U_ko", "mytool"]);
        assert_eq!(items.iter().filter(|i| i.group).count(), 1);

        // Nothing anywhere: an empty list (build() then tries the host
        // freedesktop fallback).
        assert!(assemble(None, None, &prof(), &[]).is_empty());
    }
}
