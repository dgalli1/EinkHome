//! Tap routing (split from `app.rs`): every pointer tap lands here
//! first — the shelf body (top bar / pager / cover tiles) and each
//! overlay's card.  Hit geometry always mirrors what the painters drew:
//! the shelf page hit-tests the shell's taffy widget rects (widget 0 is
//! the top bar, the last the pager, 2..last the covers), and overlays
//! walk the per-overlay rect caches rebuilt at draw time (`menu_rows`,
//! `settings_rows`, …), so a tap can never land on a row that is not on
//! screen.
//!
//! C counterparts: eh_hit_top_bar / eh_hit_pager / eh_hit_thumbnail and
//! the per-mode overlay dispatchers in eh_main.c / eh_input.c.

use eh_hal::{Framebuffer, Rect};

use crate::app::{App, MenuRow, Overlay, Source, Tab, ViewMode};
use crate::widgets::chooser::ChooserKind;

/// The pager's four page actions (the C contract: -1/-3/-4/-2 →
/// prev/first/last/next).
#[derive(Clone, Copy, PartialEq)]
pub enum PageAction {
    Prev,
    First,
    Last,
    Next,
}

impl<B: Framebuffer> App<B> {
    /// Shelf tap routing (C eh_hit_top_bar / eh_hit_pager /
    /// eh_hit_thumbnail), sharing the shell's taffy geometry: widget 0 is
    /// the top bar, the last widget the pager, the rest are the covers
    /// (or, on the search tab, the input row + history rows).
    pub(crate) fn tap_screen(&mut self, x: i32, y: i32) {
        // System-bar tap (C eh_pu_handle_chrome_system): any tap in the
        // status-strip band below the content area hands the tap to the
        // firmware control panel.
        if y >= self.content_bottom as i32 {
            crate::logger::log("[bookshelf] system bar tapped -> control panel");
            self.screen().framebuffer().open_control_panel();
            return;
        }
        let topbar = self.screen().widget_rect(0);
        let last = self.screen().widgets.len().saturating_sub(1);
        let pager = self.screen().widget_rect(last);

        if y >= topbar.y as i32 && y < topbar.y as i32 + topbar.h as i32 {
            self.tap_top_bar(x, y);
            return;
        }
        if y >= pager.y as i32 && y < pager.y as i32 + pager.h as i32 {
            self.tap_pager(x, y, pager);
            return;
        }
        // Download-folder picker first: it owns the page while open, even
        if self.dl_picker.is_some() {
            crate::local::tap_picker(self, x, y);
            return;
        }
        // Folder source: the browser owns the body (C eh_on_tap_browse).
        if self.source == Source::Folder && self.browser.open {
            crate::local::tap_browse(self, x, y);
            return;
        }
        if self.tab == Tab::Search {
            self.tap_search_body(x, y);
            return;
        }
        // Forward widget indices 2..last are the cover tiles (0 = top bar,
        // 1 = grid container, last = pager).  The C hit-test walks tiles
        // top-to-bottom; tap_cover maps the widget index to the entry.
        let hit = {
            let n = self.screen().widgets.len();
            let mut h: Option<usize> = None;
            for fwd in 2..n.saturating_sub(1) {
                if self.screen().widgets[fwd].hit(x, y) {
                    h = Some(fwd);
                    break;
                }
            }
            h
        };
        if let Some(i) = hit {
            self.tap_cover(i);
        }
    }

    /// Top bar zones (C eh_hit_top_bar + eh_hit_top_bar_right).  Left box:
    /// back (search / drilled) or no-op.  Source button opens the chooser.
    /// Right stack, in the C order from the corner: menu(3) / sync(2) /
    /// layout(7) / search(5).
    fn tap_top_bar(&mut self, x: i32, _y: i32) {
        use crate::appui::{BTN_PAD, BTN_SIZE, SOURCE_BTN_X, SOURCE_BTN_W};
        let r = self.screen().widget_rect(0);
        let w = r.w as i32;
        // Left button: back chevron (search / drilled) or house.
        if x >= BTN_PAD as i32 && x < (BTN_PAD + BTN_SIZE) as i32 {
            if self.tab == Tab::Search {
                self.leave_search();
            } else if self.drill > 0 {
                // While drilled the house is replaced by the back
                // chevron; tapping it pops one drill level (C eh_drill_back).
                self.drill_back();
            }
            return;
        }
        if self.tab == Tab::Search {
            return; // search bar has no other zones
        }
        // Source button.
        if (SOURCE_BTN_X..SOURCE_BTN_X + SOURCE_BTN_W).contains(&x) {
            self.set_overlay(Overlay::Source);
            return;
        }
        // Right stack (w - pad - k*btn for k=4,3,2,1 → search/layout/sync/menu).
        if x >= w - (BTN_PAD + 4 * BTN_SIZE) as i32 && x < w - (BTN_PAD + 3 * BTN_SIZE) as i32 {
            self.enter_search();
        } else if x >= w - (BTN_PAD + 3 * BTN_SIZE) as i32 && x < w - (BTN_PAD + 2 * BTN_SIZE) as i32 {
            self.toggle_layout();
        } else if x >= w - (BTN_PAD + 2 * BTN_SIZE) as i32 && x < w - (BTN_PAD + BTN_SIZE) as i32 {
            self.do_sync();
        } else if x >= w - (BTN_PAD + BTN_SIZE) as i32 && x < w - BTN_PAD as i32 {
            self.set_overlay(Overlay::More);
        }
    }

    /// Toggle grid / list view (C layout icon, which==7); resets to page 0.
    fn toggle_layout(&mut self) {
        self.view_mode = if self.view_mode == ViewMode::Grid { ViewMode::List } else { ViewMode::Grid };
        self.page = 0;
        self.refresh_shelf();
    }

    /// The pager's four buttons (C eh_hit_pager: -1/-3/-4/-2).  Box
    /// geometry mirrors appui::Pager's draw (x offsets from the band
    /// edges, 96×64).  Actions follow the C contract exactly:
    /// "<" prev / "<<" first / ">>" last / ">" next (eh_main.c
    /// eh_pu_handle_tail: -1/-3/-4/-2).
    fn tap_pager(&mut self, x: i32, y: i32, band: Rect) {
        let bw = 96i32;
        let bh = 64i32;
        let by = (band.y + (band.h - 64) / 2) as i32;
        let bx0 = band.x as i32;
        let bx1 = (band.x + band.w) as i32;
        let boxes = [
            (bx0 + 12, PageAction::Prev),     // "<" prev
            (bx0 + 116, PageAction::First),   // "<<" first
            (bx1 - 212, PageAction::Last),    // ">>" last
            (bx1 - 108, PageAction::Next),    // ">" next
        ];
        for (bx, action) in boxes {
            let b = Rect { x: bx as u32, y: by as u32, w: bw as u32, h: bh as u32 };
            if b.contains(x, y) {
                let target = match action {
                    PageAction::Prev => self.page.saturating_sub(1),
                    PageAction::First => 0,
                    PageAction::Last => self.pages.saturating_sub(1),
                    PageAction::Next => (self.page + 1).min(self.pages.saturating_sub(1)),
                };
                self.goto_page(target);
                return;
            }
        }
    }

    /// A cover tile tap (C eh_hit_thumbnail → eh_book_press_action).
    fn tap_cover(&mut self, idx: usize) {
        let pos = idx - 2; // [0]=top bar, [1]=grid container precede covers
        if pos < self.entries.len() {
            if self.entries[pos].stack {
                // Stack card: drill into the group (C eh_drill_card).
                let card = crate::store::ViewRow {
                    kind: 1,
                    book_id: self.entries[pos].book.id.clone(),
                    series_id: self.entries[pos].stack_scope.clone(),
                    series_name: self.entries[pos].stack_label.clone(),
                    series_count: self.entries[pos].stack_count,
                };
                self.drill_into_card(&card);
                return;
            }
            let book = self.entries[pos].book.clone();
            self.press_book(&book);
        }
    }
}
// ── overlay tap routing ───────────────────────────────────────────────

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
            Overlay::LogViewer | Overlay::Licenses | Overlay::LicenseDetail => crate::viewer::tap(x, y, self),
            Overlay::Download => {
                // The X button aborts every open download (C eh_main's
                // eh_dl_cancel_rect hit → eh_cancel_downloads); any other
                // tap dismisses only a drained popup (modal in flight).
                let scr = self.screen().framebuffer().screen();
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

    /// The More drawer: an outside tap dismisses (C behaviour), a row tap
    /// acts.  GroupBy / SortBy / DownloadAll are logged no-ops in this
    /// slice; Settings + Applications navigate.
    fn tap_more_menu(&mut self, x: i32, y: i32) {
        let scr = self.screen().framebuffer().screen();
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
