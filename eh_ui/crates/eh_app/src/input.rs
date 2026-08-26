//! Intent routing: the Slint tree reports SEMANTIC targets (which button,
//! which tile) as [`crate::ui::Action`]s; every pointer tap lands here
//! second — the actions apply the same state changes the old coordinate
//! hit-tests drove.  Overlay taps still hit-test draw-time rect caches
//! until each overlay ports to Slint.
//!
//! C counterparts: eh_hit_top_bar / eh_hit_pager / eh_hit_thumbnail and
//! the per-mode overlay dispatchers in eh_main.c / eh_input.c.

use eh_hal::Framebuffer;

use crate::app::{App, KbField, Overlay, Tab, ViewMode};
use crate::store::Store;
use crate::ui::Action;

impl<B: Framebuffer> App<B> {
    /// Apply the intents the Slint tree queued during input dispatch.
    pub(crate) fn apply_actions(&mut self) {
        for a in crate::ui::drain_actions() {
            match a {
                Action::Home => {
                    // Left button: back chevron (search / drilled) or the
                    // house (no-op on the root library — the C app's house
                    // does nothing while foregrounded).
                    if self.tab == Tab::Search {
                        self.leave_search();
                    } else if self.drill > 0 {
                        // While drilled the house is replaced by the back
                        // chevron; tapping it pops one drill level (C
                        // eh_drill_back).
                        self.drill_back();
                    }
                }
                Action::Source => self.set_overlay(Overlay::Source),
                Action::Search => self.enter_search(),
                Action::Layout => self.toggle_layout(),
                Action::Sync => self.do_sync(),
                Action::Menu => self.set_overlay(Overlay::More),
                Action::Pager(k) => {
                    // "<" prev / "<<" first / ">>" last / ">" next (the C
                    // -1/-3/-4/-2 contract).
                    let target = match k {
                        0 => self.page.saturating_sub(1),
                        1 => 0,
                        2 => self.pages.saturating_sub(1),
                        _ => (self.page + 1).min(self.pages.saturating_sub(1)),
                    };
                    self.goto_page(target);
                }
                Action::TileRelease(idx) => {
                    if self.pending_long {
                        self.pending_long = false;
                        self.long_press_entry(idx);
                    } else {
                        self.tap_cover(idx);
                    }
                }
                Action::SystemBar => {
                    // Any tap in the status-strip band hands the tap to
                    // the firmware control panel (C eh_pu_handle_chrome_system).
                    crate::logger::log("[bookshelf] system bar tapped -> control panel");
                    self.fb().open_control_panel();
                }
                Action::SearchInput => {
                    if self.search_kb {
                        // With the keyboard already open a tap on the row
                        // dismisses it (C: outside-band branch).
                        self.dismiss_search_kb();
                    } else {
                        self.edit_search();
                    }
                }
                Action::SearchRow(idx) => self.tap_search_row(idx),
                Action::SearchOutside => {
                    if self.search_kb {
                        self.dismiss_search_kb();
                    }
                }
                Action::BrowseRow(idx) => {
                    if self.dl_picker.is_some() {
                        crate::local::tap_picker_row(self, idx);
                    } else {
                        crate::local::tap_browse_row(self, idx);
                    }
                }
                Action::MenuRow(i) => crate::menu::more_row(self, i),
                Action::MenuOutside => {
                    self.set_overlay(Overlay::None);
                }
                Action::SourceRow(i) => crate::source::apply_source(self, i),
                Action::SourceOutside => {
                    self.overlay = Overlay::None;
                    self.refresh_shelf();
                }
                Action::ChooserRow(i) => self.chooser_row(i),
                Action::ChooserOutside => {
                    self.set_overlay(Overlay::None);
                }
                Action::ContextRow(i) => self.context_row(i),
                Action::ContextOutside => {
                    self.context.dismiss();
                    self.set_overlay(Overlay::None);
                    self.refresh_shelf();
                }
                Action::DownloadCancel => self.cancel_downloads(),
                Action::DownloadDismiss => {
                    if self.downloader.pending == 0 {
                        self.set_overlay(Overlay::None);
                    }
                }
                Action::SyncDismiss => {
                    // Tap-outside dismisses at any stage: the sheet is only
                    // the progress view — the sync chain runs detached on
                    // its worker and keeps going behind the closed sheet.
                    self.set_overlay(Overlay::None);
                }
                Action::KbChar(c) => {
                    self.fb().kb_type_text(&c.to_string());
                    self.dirty = true;
                }
                Action::KbBackspace => {
                    self.fb().kb_backspace();
                    self.dirty = true;
                }
                Action::KbOk => {
                    // The static commit handler stashes the buffer; the
                    // drain applies it to the armed field immediately.
                    self.fb().kb_commit();
                    self.drain_keyboard();
                    self.dirty = true;
                }
                Action::KbCancel => {
                    if self.search_kb {
                        self.dismiss_search_kb();
                    }
                    self.kb_editing = None;
                    self.dirty = true;
                }
                Action::PickerSelect => crate::local::picker_commit_current(self),
                Action::SettingsBack => self.settings_back(),
                Action::SettingsRow(i) => self.settings_row(i),
                Action::ViewerBack => self.viewer_back(),
                Action::DetailBack => self.detail_back(),
                Action::ViewerScroll(d) => self.viewer_scroll(d),
                Action::LicenseRow(i) => self.license_row(i),
                Action::LauncherBack => self.launcher_back(),
                Action::LauncherScroll(d) => self.launcher_scroll_page(d),
                Action::LauncherCell(i) => self.launcher_cell(i),
            }
        }
    }

    /// Toggle grid / list view (C layout icon, which==7); resets to page 0.
    fn toggle_layout(&mut self) {
        self.view_mode = if self.view_mode == ViewMode::Grid {
            ViewMode::List
        } else {
            ViewMode::Grid
        };
        self.page = 0;
        self.refresh_shelf();
    }

    /// A cover tile tap (C eh_hit_thumbnail → eh_book_press_action).
    fn tap_cover(&mut self, idx: usize) {
        if idx >= self.entries.len() {
            return;
        }
        if self.entries[idx].stack {
            // Stack card: drill into the group (C eh_drill_card).
            let card = crate::store::ViewRow {
                kind: 1,
                book_id: self.entries[idx].book.id.clone(),
                series_id: self.entries[idx].stack_scope.clone(),
                series_name: self.entries[idx].stack_label.clone(),
                series_count: self.entries[idx].stack_count,
            };
            self.drill_into_card(&card);
            return;
        }
        let book = self.entries[idx].book.clone();
        self.press_book(&book);
    }
}

// ── full-screen page overlays (settings / viewers / launcher) ──────────

impl<B: Framebuffer> App<B> {
    /// The settings page's back chevron.
    pub(crate) fn settings_back(&mut self) {
        self.overlay = Overlay::None;
    }

    /// The Settings rows in display order: the System-app card only
    /// exists where a home-task override makes sense (PocketBook, or the
    /// e2e's EH_SYSAPP_DIR hook).
    fn settings_rows() -> Vec<crate::app::SettingsRow> {
        use crate::app::SettingsRow;
        let mut rows = vec![
            SettingsRow::ApiHost,
            SettingsRow::ApiKey,
            SettingsRow::ReaderApp,
            SettingsRow::DownloadFolder,
            SettingsRow::LocalFolder,
        ];
        if crate::sysapp::platform_supported() {
            rows.push(SettingsRow::SystemApp);
        }
        rows
    }

    /// A settings row/button tap (C eh_on_tap_settings's dispatch).  The
    /// Slint page numbers cards 0..n and the three buttons n..n+2, so
    /// the mapping follows the live row count.
    pub(crate) fn settings_row(&mut self, i: usize) {
        use crate::app::SettingsRow;
        let rows = Self::settings_rows();
        let row = if i < rows.len() {
            rows[i]
        } else {
            match i - rows.len() {
                0 => SettingsRow::Save,
                1 => SettingsRow::ShowLogs,
                2 => SettingsRow::Licenses,
                _ => SettingsRow::ResetDb,
            }
        };
        match row {
            SettingsRow::ApiHost => self.edit_field(KbField::ApiHost),
            SettingsRow::ApiKey => self.edit_field(KbField::ApiKey),
            SettingsRow::Save => self.settings_apply(),
            SettingsRow::ReaderApp => self.cycle_reader(),
            SettingsRow::ShowLogs => {
                self.overlay = Overlay::LogViewer;
                self.dirty = true;
            }
            SettingsRow::Licenses => {
                self.overlay = Overlay::Licenses;
                self.dirty = true;
            }
            SettingsRow::ResetDb => self.reset_database(),
            SettingsRow::DownloadFolder => {
                // Open the folder picker rooted at the storage root,
                // starting at the current downloads dir when it is under
                // the root (C eh_on_tap_settings_folder -> eh_folder_open).
                let root = crate::local::browse_root();
                let start = self
                    .config
                    .downloads_dir
                    .clone()
                    .filter(|d| d.starts_with(&root))
                    .unwrap_or_else(|| root.clone());
                let mut b = crate::local::browser::Browser {
                    picker: true,
                    root: root.clone(),
                    path: start,
                    ..Default::default()
                };
                b.load();
                self.dl_picker = Some(b);
                // The picker is NOT an overlay: it lives on the main page.
                self.overlay = Overlay::None;
                // The rows model syncs in refresh_shelf — without it the
                // picker opens empty until some other refresh happens.
                self.refresh_shelf();
            }
            SettingsRow::LocalFolder => {
                // Same picker, different commit target: the Local-source
                // base folder (starts at the current base when it is
                // under the browse root).
                let root = crate::local::browse_root();
                let start = self
                    .config
                    .local_dir
                    .clone()
                    .filter(|d| d.starts_with(&root))
                    .unwrap_or_else(|| root.clone());
                let mut b = crate::local::browser::Browser {
                    picker: true,
                    picker_local: true,
                    root: root.clone(),
                    path: start,
                    ..Default::default()
                };
                b.load();
                self.dl_picker = Some(b);
                self.overlay = Overlay::None;
                self.refresh_shelf();
            }
            SettingsRow::SystemApp => {
                if crate::sysapp::detect() {
                    crate::sysapp::unpromote();
                    crate::logger::log(
                        "[bookshelf] sysapp: removed from system — stock home returns after reboot",
                    );
                } else if crate::sysapp::promote(self) {
                    crate::logger::log(
                        "[bookshelf] sysapp: installed as system app — reboot to boot EinkHome as the home screen",
                    );
                }
                self.dirty = true;
            }
        }
    }

    /// Settings → Reset database: wipe the metadata store (every source's
    /// books, history, cursors, downloaded flags) and close the app — the
    /// next start re-syncs Kavita and re-imports Local from scratch.
    ///
    /// The sync worker and the local import scan hold their OWN store
    /// connections to the same file, so both are aborted before the wipe;
    /// a straggler writing into its unlinked handle is harmless, the
    /// fresh database this creates is the one that survives.  Downloads
    /// (the .epub files themselves) and the cover cache are NOT touched.
    fn reset_database(&mut self) {
        crate::logger::log("[bookshelf] settings: resetting the local database");
        self.sync_abort();
        crate::local::cancel_scan(self);
        for suffix in ["", "-wal", "-shm"] {
            let mut p = self.db_path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(std::path::PathBuf::from(p));
        }
        self.store = Store::open(&self.db_path).expect("reopen store after database reset");
        // A clean shelf: no drill scope, no pickers, no overlays.
        self.drill = 0;
        self.drill_values = Default::default();
        self.drill_names = Default::default();
        self.drill_saved_pages = [0; 2];
        self.context.dismiss();
        self.detail_book = None;
        self.dl_picker = None;
        self.set_overlay(Overlay::None);
        self.refresh_shelf();
        self.exit_requested = true;
    }

    /// The book-detail page's back chevron: close + drop the stashed book.
    pub(crate) fn detail_back(&mut self) {
        self.overlay = Overlay::None;
        self.detail_book = None;
    }

    /// The viewers' back chevron: detail -> list -> shelf (C
    /// eh_on_tap_licenses_view's back branch).
    pub(crate) fn viewer_back(&mut self) {
        match self.overlay {
            Overlay::LicenseDetail => {
                self.overlay = Overlay::Licenses;
                self.license_selected = None;
                self.lic_scroll = 0;
            }
            _ => {
                self.overlay = Overlay::None;
                self.lic_scroll = 0;
                self.log_scroll = -1; // re-pin on the next open
            }
        }
        self.dirty = true;
    }

    /// A corner scroll button in a viewer (C eh_on_tap_log_view's paging).
    pub(crate) fn viewer_scroll(&mut self, dir: i32) {
        let sw = self.screen_width() as i32;
        let sh = self.content_bottom as i32;
        match self.overlay {
            Overlay::LogViewer => {
                let btn_y = sh - crate::appui::SCROLL_BTN_H as i32 - 8;
                let page = (((btn_y - crate::viewer::LOG_BODY_TOP as i32).max(0) as u32)
                    / crate::viewer::LOG_ROW_H)
                    .max(1) as i32;
                let tf = crate::viewer::log_tail_first(sw as u32, self.content_bottom);
                self.log_scroll = crate::viewer::log_scroll_after(self.log_scroll, dir, page, tf);
            }
            Overlay::Licenses | Overlay::LicenseDetail => {
                let detail = self.overlay == Overlay::LicenseDetail;
                let (top, rh) = if detail {
                    (
                        crate::viewer::LOG_BODY_TOP as i32,
                        crate::viewer::LOG_ROW_H as i32,
                    )
                } else {
                    (
                        crate::viewer::LIC_LIST_TOP as i32,
                        crate::viewer::LIC_LIST_H as i32,
                    )
                };
                let btn_y = sh - crate::appui::SCROLL_BTN_H as i32 - 8;
                let page = ((btn_y - top - 8) / rh).max(1);
                self.lic_scroll = (self.lic_scroll + dir * page).max(0);
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// A licenses-list row tap: open that license's full text.
    pub(crate) fn license_row(&mut self, rel: usize) {
        if self.overlay != Overlay::Licenses {
            return;
        }
        let idx = self.lic_scroll as usize + rel;
        if idx < crate::viewer::LICENSES.len() {
            self.license_selected = Some(idx);
            self.overlay = Overlay::LicenseDetail;
            self.lic_scroll = 0;
            self.dirty = true;
        }
    }

    /// The launcher's back chevron.
    pub(crate) fn launcher_back(&mut self) {
        self.overlay = Overlay::None;
    }

    /// A launcher corner scroll button (C eh_on_tap_overlay_launcher's
    /// paging branch): page by the visible body height, clamped.
    pub(crate) fn launcher_scroll_page(&mut self, dir: i32) {
        let (scroll, max) = crate::launcher::scroll_of(self);
        if max > 0 {
            let (_, body_h) = crate::launcher::body_rects(self);
            self.launcher_scroll = (scroll + dir * body_h as i32).clamp(0, max);
            self.dirty = true;
        }
    }

    /// A launcher app cell tap: launch through the backend (NewTaskEx;
    /// the launched task draws over the shelf, so no redraw first).
    pub(crate) fn launcher_cell(&mut self, i: usize) {
        if self.pending_drag {
            self.pending_drag = false;
            return; // a drag-release is not a tap (C drag_scroll_move)
        }
        let Some(it) = self.launcher_items.get(i).cloned() else {
            return;
        };
        if it.group {
            return;
        }
        self.overlay = Overlay::None;
        crate::log(&format!(
            "[eh_app] launching app path={} params={}",
            it.path,
            it.params.len()
        ));
        if !self.fb().launch_app(&it.path, &it.text, &it.params) {
            crate::log("[eh_app] launch failed (no task system on this platform)");
        }
    }
}
