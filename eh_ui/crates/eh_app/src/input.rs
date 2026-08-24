//! Intent routing: the Slint tree reports SEMANTIC targets (which button,
//! which tile) as [`crate::ui::Action`]s; every pointer tap lands here
//! second — the actions apply the same state changes the old coordinate
//! hit-tests drove.  Overlay taps still hit-test draw-time rect caches
//! until each overlay ports to Slint.
//!
//! C counterparts: eh_hit_top_bar / eh_hit_pager / eh_hit_thumbnail and
//! the per-mode overlay dispatchers in eh_main.c / eh_input.c.

use eh_hal::{Framebuffer, Rect};

use crate::ui::Action;
use crate::widgets::chooser::ChooserKind;
use crate::app::{App, MenuRow, Overlay, Tab, ViewMode};

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
                Action::BrowseRow(idx) => {
                    if self.dl_picker.is_some() {
                        crate::local::tap_picker_row(self, idx);
                    } else {
                        crate::local::tap_browse_row(self, idx);
                    }
                }
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

// ── overlay tap routing (interim: rect caches until each overlay ports) ──

impl<B: Framebuffer> App<B> {
    /// Overlay tap routing (each overlay rebuilds its rects at draw time,
    /// so taps share the paint geometry).
    pub fn tap_overlay(&mut self, x: i32, y: i32) {
        match self.overlay {
            Overlay::More => self.tap_more_menu(x, y),
            Overlay::Settings => crate::settings::tap_settings(x, y, self),
            Overlay::Launcher => crate::launcher::tap_launcher(x, y, self),
            Overlay::Source => crate::source::tap(self, x, y),
            Overlay::Context => self.tap_context(x, y),
            Overlay::GroupChooser => self.tap_chooser(x, y, ChooserKind::Group),
            Overlay::SortChooser => self.tap_chooser(x, y, ChooserKind::Sort),
            Overlay::LogViewer | Overlay::Licenses | Overlay::LicenseDetail => {
                crate::viewer::tap(x, y, self)
            }
            Overlay::Download => {
                // The X button aborts every open download (C eh_main's
                // eh_dl_cancel_rect hit → eh_cancel_downloads); any other
                // tap dismisses only a drained popup (modal in flight).
                let scr = self.fb().screen();
                let cx = crate::widgets::download::dl_cancel_rect(scr.width, self.content_bottom);
                if cx.contains(x, y) {
                    self.cancel_downloads();
                } else if self.downloader.pending == 0 {
                    self.set_overlay(Overlay::None);
                }
            }
            Overlay::Sync => {
                // Modal while the sync runs (C pins the sheet); once the
                // chain finished or failed, any tap dismisses it.
                if !self.syncing {
                    self.set_overlay(Overlay::None);
                }
            }
            Overlay::None => {}
        }
    }

    /// The More drawer: an outside tap dismisses (C behaviour); a row tap
    /// opens Settings or the launcher, opens the group/sort choosers, or
    /// starts a download-all batch.
    fn tap_more_menu(&mut self, x: i32, y: i32) {
        let scr = self.fb().screen();
        let dw = (scr.width as i32) * 3 / 4;
        let card = Rect {
            x: (scr.width as i32 - dw) as u32,
            y: 0,
            w: dw as u32,
            h: self.content_bottom,
        };
        if !card.contains(x, y) {
            self.set_overlay(Overlay::None);
            self.menu_rows.clear();
            return;
        }
        for (r, row) in self.menu_rows.iter().cloned() {
            if r.contains(x, y) {
                match row {
                    MenuRow::Settings => self.set_overlay(Overlay::Settings),
                    MenuRow::Applications => {
                        if crate::launcher::build(self) {
                            self.set_overlay(Overlay::Launcher);
                            self.launcher_scroll = 0;
                        }
                    }
                    MenuRow::GroupBy => self.open_group_chooser(),
                    MenuRow::SortBy => self.open_sort_chooser(),
                    MenuRow::DownloadAll => self.download_all(),
                }
                self.menu_rows.clear();
                return;
            }
        }
    }
}
