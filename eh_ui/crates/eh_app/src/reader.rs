//! Reader detection + preference plumbing (split from `app.rs`): probe
//! the firmware/KOReader binaries, map a stored `reader=` value to a
//! preference index, cycle the preference on the settings row, and label
//! it (C eh_detect_readers / eh_reader_pref_from_path /
//! eh_settings_reader_label).

use eh_hal::Framebuffer;

use std::path::{Path, PathBuf};

use crate::app::{App, Overlay};
use crate::downloads::book_local_path;
use crate::store::Book;

/// The standard firmware reader path (C eh_plat_standard_reader).
pub const STANDARD_READER: &str = "/ebrmain/bin/eink-reader.app";

/// The third-party reader path (C eh_plat_koreader_path): present only
/// when the user installed it under /mnt/ext1/applications.
pub const KOREADER_PATH: &str = "/mnt/ext1/applications/koreader.app";

/// Probe the known reader binaries (C eh_detect_readers): returns the
/// paths that are actually executable, in offer order.  The standard
/// reader is always in the firmware image; KOReader only when installed.
pub fn detect_readers() -> Vec<&'static str> {
    let executable = |p: &str| {
        std::fs::metadata(p)
            .map(|m| {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            })
            .unwrap_or(false)
    };
    let out: Vec<&'static str> = [(STANDARD_READER, "Standard"), (KOREADER_PATH, "KOReader")]
        .into_iter()
        .filter(|(p, label)| {
            let ok = executable(p);
            crate::logger::log(&format!(
                "[bookshelf] reader {}: {} ({p})",
                if ok { "detected" } else { "not found" },
                label
            ));
            ok
        })
        .map(|(p, _)| p)
        .collect();
    // Host/PC fallback (no firmware readers on the filesystem): offer the
    // standard reader so the row still cycles Auto → Standard.
    if out.is_empty() {
        vec![STANDARD_READER]
    } else {
        out
    }
}

/// Human label of a detected reader path (C eh_g_readers[].label).
pub fn reader_label_of(path: &str) -> &'static str {
    match path {
        KOREADER_PATH => "KOReader",
        _ => "Standard",
    }
}

/// Map a stored `reader=` value back to a preference index given the
/// detected readers (C eh_reader_pref_from_path): "auto"/""/NULL → 0
/// (server open-with); a path matching a detected reader → its 1-based
/// index; anything else (e.g. a reader that was uninstalled) → 0.
pub fn reader_pref_from_path(value: &str, readers: &[&str]) -> i32 {
    if value.is_empty() || value == "auto" {
        return 0;
    }
    readers
        .iter()
        .position(|p| *p == value)
        .map_or(0, |i| i as i32 + 1)
}

impl<B: Framebuffer> App<B> {
    /// Resolve the reader preference from the config at boot (C
    /// eh_reader_pref_from_path) + log the C `reader_pref=N (cfg \`path\`)`
    /// marker the persist test greps for.
    pub(crate) fn resolve_reader(&mut self) {
        let readers = detect_readers();
        let cfg = self.config.reader.clone().unwrap_or_default();
        let pref = reader_pref_from_path(&cfg, &readers);
        self.reader_pref = pref;
        self.reader_path = if pref > 0 {
            readers[pref as usize - 1].to_string()
        } else {
            "auto".to_string()
        };
        crate::logger::log(&format!("[bookshelf] reader_pref={pref} (cfg `{cfg}`)"));
    }

    /// Cycle the reader preference (C eh_input.c reader row tap): Auto →
    /// each detected reader in offer order → Auto.
    pub fn cycle_reader(&mut self) {
        let readers = detect_readers();
        self.reader_pref = (self.reader_pref + 1) % (readers.len() as i32 + 1);
        self.apply_reader_pref(&readers);
        self.dirty = true;
        crate::logger::log(&format!("[bookshelf] reader_pref={}", self.reader_pref));
    }

    /// Persist + log the current preference against `readers` (the label
    /// shown in the settings row comes from [`App::reader_label`]).
    pub(crate) fn apply_reader_pref(&mut self, readers: &[&str]) {
        if self.reader_pref > 0 && (self.reader_pref as usize) <= readers.len() {
            let path = readers[self.reader_pref as usize - 1];
            self.config.reader = Some(path.to_string());
            self.reader_path = path.to_string();
        } else {
            // A stale index (the reader was uninstalled) falls back to Auto.
            self.reader_pref = 0;
            self.config.reader = None;
            self.reader_path = "auto".to_string();
        }
    }

    /// The settings row's value for the reader preference (C
    /// eh_settings_reader_label): the selected reader's label, or Auto.
    pub fn reader_label(&self) -> String {
        if self.reader_pref > 0 {
            let readers = detect_readers();
            if let Some(p) = readers.get(self.reader_pref as usize - 1) {
                return crate::reader::reader_label_of(p).to_string();
            }
        }
        crate::i18n::tr("settings.reader_auto").to_string()
    }


    /// The C app's eh_book_press_action: probe the on-disk file (both the
    /// current downloads dir AND the stored path — the folder may have
    /// moved since the fetch), persist the state, then either
    /// download-then-open or open directly.
    pub(crate) fn press_book(&mut self, book: &Book) {
        let downloads_dir = self
            .config
            .downloads_dir
            .clone()
            .unwrap_or_else(crate::local::default_downloads_dir);
        let cur = book_local_path(book, &downloads_dir);
        let stored = PathBuf::from(&book.local_path);
        let exists = cur.is_file() || (!book.local_path.is_empty() && stored.is_file());
        if exists {
            let path = if cur.is_file() { cur } else { stored };
            if !book.downloaded || book.local_path != path.to_string_lossy() {
                if let Err(e) = self.store.set_downloaded(&book.id, true, &path.to_string_lossy()) {
                    crate::log(&format!("[eh_app] set_downloaded: {e}"));
                }
            }
            self.open_reader(&path, &book.title);
        } else {
            // Async: enqueue on the worker, show the modal popup, auto-open
            // the reader when the queue drains.
            self.dl_single = true;
            self.dl_autopen = Some((cur.to_string_lossy().into_owned(), book.title.clone()));
            self.enqueue_download(&book.id, &cur);
        }
    }

    /// Launch the reader (C eh_launch_reader → eh_plat_launch_reader).
    /// The standard reader — and the auto default — goes through
    /// OpenBook(), the firmware's canonical book-open path; only an
    /// explicitly selected third-party reader (KOReader) is exec'd via
    /// launch_app with the book path as argv[1] (argv[0] must be the
    /// program path: the task launcher passes args through as-is).
    pub(crate) fn open_reader(&mut self, path: &Path, title: &str) {
        let path_str = path.to_string_lossy().to_string();
        let readers = detect_readers();
        // C eh_launch_reader: the configured preference resolves against
        // the detected readers list before the firmware OpenBook fallback.
        let reader_path = if self.reader_pref > 0 && (self.reader_pref as usize) <= readers.len() {
            Some(readers[self.reader_pref as usize - 1])
        } else {
            None
        };
        let ok = match reader_path {
            Some(rp) if rp != STANDARD_READER => {
                crate::logger::log(&format!(
                    "[bookshelf] launching reader app={} path={}",
                    rp.rsplit('/').next().unwrap_or(rp),
                    path.display()
                ));
                crate::log(&format!("[eh_app] opening reader app={rp} path={}", path.display()));
                self.screen()
                    .framebuffer_mut()
                    .launch_app(rp, title, &[path_str.clone()])
            }
            _ => {
                crate::logger::log(&format!(
                    "[bookshelf] launching reader via OpenBook: {}",
                    path.display()
                ));
                crate::log(&format!("[eh_app] opening reader path={}", path.display()));
                self.screen().framebuffer_mut().open_book(&path_str, title)
            }
        };
        if !ok {
            /*
             * Same hourglass intent as the launcher (C eh_launch_reader):
             * the reader draws over it once it becomes the foreground
             * task, so a slow reader start reads as work-in-progress
             * instead of a dead tap.  On launch failure no reader will
             * ever draw over it — the C app hides the hourglass and
             * repaints the shelf; this port has no hourglass overlay to
             * drop, so we close the sheet WITHOUT a redraw and let the
             * next present bring the shelf back.
             */
            crate::log("[eh_app] reader launch failed");
            self.overlay = Overlay::None;
        }
    }
}
