//! The Folder source's live directory browser (C eh_browser.c
//! BR_MODE_BROWSER): the [`Browser`] cursor over one directory tree, the
//! full-width [`BrowseRow`] shelf body, page/ascend taps, and the Settings
//! download-folder picker variant (BR_MODE_PICKER — directories only, a
//! tap commits the directory).

use std::path::Path;

use eh_hal::Framebuffer;

use crate::app::App;
use crate::store::Book;

use super::{
    browse_root, ext_of, hash_hex, is_book_ext, stem_title, BROWSE_MAX_ENTRIES, FOLDER_ROW_H,
};

// ── folder browser (C eh_browser.c BR_MODE_BROWSER) ─────────────────────

/// One listed entry: `..` (when below the root), then subdirectories,
/// then book files — each group alphabetical (C browser_load's sort).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseEntry {
    pub name: String,
    pub is_dir: bool,
}

/// The browser state (C eh_g_browse_*).  `root` pins the storage root so
/// ascent stops there and display paths stay relative to it.
#[derive(Debug, Default)]
pub struct Browser {
    pub root: String,
    pub path: String,
    pub scroll: usize,
    pub entries: Vec<BrowseEntry>,
    pub open: bool,
    /// BR_MODE_PICKER: the Settings download-folder chooser — only
    /// directories are listed and a tap commits that directory.
    pub picker: bool,
}

impl Browser {
    /// Start browsing `dir` (C eh_browse_start).
    pub fn start(&mut self, dir: &str) {
        self.root = dir.to_string();
        self.path = dir.to_string();
        self.scroll = 0;
        self.load();
        self.open = true;
    }

    /// No ascent above the storage root (C browser_can_go_up).
    pub fn can_go_up(&self) -> bool {
        self.path != self.root
    }

    /// Refill `entries` from the current directory.
    pub fn load(&mut self) {
        self.entries.clear();
        if self.can_go_up() {
            self.entries.push(BrowseEntry {
                name: "..".into(),
                is_dir: true,
            });
        }
        let Ok(rd) = std::fs::read_dir(&self.path) else {
            crate::log(&format!("[eh_app] browser: opendir {} failed", self.path));
            return;
        };
        let mut dirs: Vec<String> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        for e in rd.flatten() {
            if dirs.len() + files.len() >= BROWSE_MAX_ENTRIES {
                break;
            }
            let name = e.file_name();
            let name = name.to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if self.picker && !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue; // the folder picker lists directories only
            }
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            let (is_dir, is_reg) = if ft.is_symlink() {
                match std::fs::metadata(&path) {
                    Ok(m) => (m.is_dir(), m.is_file()),
                    Err(_) => (false, false),
                }
            } else {
                (ft.is_dir(), ft.is_file())
            };
            if is_dir {
                dirs.push(name);
            } else if is_reg && ext_of(&name).is_some_and(|x| is_book_ext(&x)) {
                files.push(name);
            }
        }
        dirs.sort();
        files.sort();
        self.entries.extend(
            dirs.into_iter()
                .map(|name| BrowseEntry { name, is_dir: true }),
        );
        self.entries
            .extend(files.into_iter().map(|name| BrowseEntry {
                name,
                is_dir: false,
            }));
        crate::logger::log(&format!(
            "[bookshelf] browser: {} -> {} entries",
            self.path,
            self.entries.len()
        ));
    }

    /// Visible row count: the body runs from below the top bar to the
    /// content bottom (C browser_rows_visible, browser branch).
    pub fn rows_visible(content_bottom: u32) -> usize {
        let avail = (content_bottom as i32
            - (crate::appui::TOP_BAR_H + crate::appui::TOP_BAR_PAD) as i32
            - 8)
        .max(1);
        (avail as u32 / FOLDER_ROW_H).max(1) as usize
    }

    /// Descend into a listed subdirectory (or ascend via `..`)
    /// (C browser_navigate).
    pub fn navigate(&mut self, name: &str) {
        if name == ".." {
            self.path = match std::path::Path::new(&self.path).parent() {
                Some(p) if self.path != self.root => p.to_string_lossy().into_owned(),
                _ => self.root.clone(),
            };
        } else {
            let next = format!("{}/{}", self.path, name);
            self.path = next;
        }
        self.scroll = 0;
        self.load();
    }

    /// Ascend one level; false when already at the browser root — the
    /// caller then decides what Back means (C eh_browse_up).
    pub fn up(&mut self) -> bool {
        if !self.can_go_up() {
            return false;
        }
        if let Some(p) = std::path::Path::new(&self.path).parent() {
            self.path = p.to_string_lossy().into_owned();
        }
        self.scroll = 0;
        self.load();
        true
    }

    /// Page the list one screen (C eh_browse_page); dir > 0 = forward.
    /// The draw path clamps, so the raw arithmetic mirrors the C app.
    pub fn page(&mut self, dir: i32, content_bottom: u32) {
        let rows = Self::rows_visible(content_bottom) as i32;
        let max = self.entries.len().saturating_sub(rows as usize) as i32;
        self.scroll = (self.scroll as i32 + dir * rows).clamp(0, max) as usize;
    }

    /// Display form of an absolute path: everything under the storage root
    /// shows relative to it; the root itself shows as "/" (C
    /// eh_user_path_display).
    pub fn user_display(path: &str, root: &str) -> String {
        if let Some(rest) = path.strip_prefix(root) {
            if rest.is_empty() {
                return "/".into();
            }
            if let Some(stripped) = rest.strip_prefix('/') {
                return stripped.to_string();
            }
        }
        path.to_string()
    }
}

/// The Book a folder-browser tap opens (C browser_open_book): the file IS
/// the book — filename-derived title, `fld_` id from the same hash as the
/// Local import, downloaded=1, source `folder`.
pub fn folder_book(path: &str, name: &str) -> Book {
    Book {
        id: format!("fld_{}", hash_hex(path)),
        title: stem_title(name),
        ext: ext_of(name).unwrap_or_default(),
        downloaded: true,
        local_path: path.to_string(),
        filename: name.to_string(),
        source: "folder".into(),
        ..Default::default()
    }
}

/// Open the folder browser at the storage root (C source-tap →
/// eh_browse_start): the browser becomes the shelf body.
pub fn start_browse<B: Framebuffer>(app: &mut App<B>) {
    let root = browse_root();
    app.browser.start(&root);
    app.refresh_shelf();
}

/// Body tap in browser mode (C eh_on_tap_browse): a directory row
/// navigates, a book file opens through the reader flow.  `idx` is the
/// ABSOLUTE entry index (the Slint callback adds the scroll offset).
pub fn tap_browse_row<B: Framebuffer>(app: &mut App<B>, idx: usize) {
    let Some(entry) = app.browser.entries.get(idx).cloned() else {
        return;
    };
    if entry.is_dir {
        app.browser.navigate(&entry.name);
        app.refresh_shelf();
    } else {
        let path = format!("{}/{}", app.browser.path, entry.name);
        let book = folder_book(&path, &entry.name);
        crate::logger::log(&format!("[bookshelf] browse: opening {path}"));
        app.open_reader(Path::new(&path), &book.title);
    }
}

/// Page key in browser mode: scroll the listing one screen and rebuild.
pub fn browse_page<B: Framebuffer>(app: &mut App<B>, dir: i32) {
    app.browser.page(dir, app.content_bottom);
    app.refresh_shelf();
}

/// Back key in browser mode: ascend one level; false at the root (the
/// caller falls through) (C eh_browse_up).
pub fn browse_up<B: Framebuffer>(app: &mut App<B>) -> bool {
    if !app.browser.up() {
        return false;
    }
    app.refresh_shelf();
    true
}

/// Picker-mode row tap: ".." ascends, a directory tap DESCENDS into it
/// (normal navigation — the C app committed on first tap, which made
/// subfolders unreachable).  Committing is the explicit "use this
/// folder" button ([`picker_commit_current`]).  `idx` is the ABSOLUTE
/// entry index.
pub fn tap_picker_row<B: Framebuffer>(app: &mut App<B>, idx: usize) {
    let Some(entry) = app.dl_picker.as_ref().unwrap().entries.get(idx).cloned() else {
        return;
    };
    if entry.name == ".." {
        app.dl_picker.as_mut().unwrap().up();
    } else if entry.is_dir {
        app.dl_picker.as_mut().unwrap().navigate(&entry.name);
    } else {
        return; // the picker lists directories only
    }
    app.dirty = true;
    app.refresh_shelf();
}

/// The picker's "use this folder" button: the CURRENT browse path
/// becomes the downloads dir (the app saves the config and re-resolves,
/// C eh_settings_apply).
pub fn picker_commit_current<B: Framebuffer>(app: &mut App<B>) {
    let Some(path) = app.dl_picker.as_ref().map(|b| b.path.clone()) else {
        return;
    };
    app.commit_downloads_dir(&path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
    #[test]
    fn browser_navigation_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("beta")).unwrap();
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        touch(root, "zbook.epub");
        touch(root, "abook.epub");
        std::fs::create_dir_all(root.join("beta").join("inner")).unwrap();

        let root_str = root.to_string_lossy().into_owned();
        let mut b = Browser::default();
        b.start(&root_str);
        assert!(b.open);
        // At the root there is no ".."; dirs first (alpha, beta), then files.
        assert!(!b.can_go_up());
        let names: Vec<&str> = b.entries.iter().map(|e| e.name.as_str()).collect();
        // Directories first (alpha, beta), then files (abook, zbook).
        assert_eq!(names, ["alpha", "beta", "abook.epub", "zbook.epub"]);

        // Descend: ".." leads the list.
        b.navigate("beta");
        assert_eq!(b.path, format!("{root_str}/beta"));
        assert_eq!(b.entries[0].name, "..");
        assert!(b.entries[0].is_dir);
        assert_eq!(b.entries[1].name, "inner");

        // Ascend back to the root; one more up stays at the root.
        assert!(b.up());
        assert_eq!(b.path, root_str);
        assert!(!b.up());

        // Paging clamps at both ends.
        b.page(5, 480); // forward past the end
        let maxed = b.scroll;
        assert!(maxed <= b.entries.len().saturating_sub(Browser::rows_visible(480)));
        b.page(-50, 480); // backward before the start
        assert_eq!(b.scroll, 0);

        // Display paths strip the root; the root itself shows as "/".
        assert_eq!(Browser::user_display(&root_str, &root_str), "/");
        assert_eq!(
            Browser::user_display(&format!("{root_str}/beta/inner"), &root_str),
            "beta/inner"
        );
        assert_eq!(Browser::user_display("/elsewhere", &root_str), "/elsewhere");
    }

    #[test]
    fn folder_book_derives_fld_id_like_local_scan() {
        let path = "/mnt/ext1/Books/x.epub";
        let b = folder_book(path, "x.epub");
        assert_eq!(b.id, format!("fld_{}", hash_hex(path)));
        assert_eq!(b.source, "folder");
        assert!(b.downloaded);
        assert_eq!(b.local_path, path);
        // Same id derivation as the Local import for identical paths.
        let f = crate::local::LocalFile {
            id: format!("fld_{}", hash_hex(path)),
            title: String::new(),
            filename: "x.epub".into(),
            local_path: path.into(),
            ext: "epub".into(),
            size: 0,
        };
        assert_eq!(f.to_book().id, b.id);
        assert!(f.to_book().downloaded);
        assert_eq!(f.to_book().source, "local");
    }
}
