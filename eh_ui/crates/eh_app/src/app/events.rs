//! Input routing & the event-loop pump: tap/long-press dispatch by
//! overlay (C eh_input.c), async keyboard draining, background-worker
//! ticks, and back navigation.  Split out of app.rs by concern.
use super::*;

impl<B: Framebuffer> App<B> {
    pub fn on_event(&mut self, ev: &InputEvent) {
        if self.drain_keyboard() {
            return; // a keyboard commit consumed this event (C draws in it)
        }
        match ev {
            InputEvent::KeyDown { key: KeyCode::Back } => self.back(),
            // Home is a no-op while foregrounded (C eh_evt_keypress: the
            // taskmanager handles it; closing here would read as a crash).
            InputEvent::KeyDown { key: KeyCode::Home } => {}
            // Page-turn buttons paginate the shelf; with an overlay open
            // they fall through to the Back logic (close the topmost
            // sheet), matching the stock bookshelf (C eh_evt_keypress).
            InputEvent::KeyDown {
                key: key @ (KeyCode::PrevPage | KeyCode::NextPage),
            } => {
                if self.overlay == Overlay::None {
                    // Folder source: the browser body pages its listing
                    // (C eh_evt_keypress → eh_browse_page).
                    if self.source == Source::Folder && self.browser.open {
                        let dir = match key {
                            KeyCode::NextPage => 1,
                            _ => -1,
                        };
                        crate::local::browse_page(self, dir);
                        return;
                    }
                    let target = match key {
                        KeyCode::NextPage => self.page + 1,
                        _ => self.page.saturating_sub(1),
                    };
                    if target < self.pages {
                        self.goto_page(target);
                    }
                } else {
                    self.back();
                }
            }
            InputEvent::PointerDown { x, y } => {
                self.press_pos = Some((*x, *y));
                self.press_start = Some(std::time::Instant::now());
                self.drag_y = Some(*y);
                self.drag_total = 0;
                // Slint hit-tests the press (TouchArea grab pairing); the
                // release reports the semantic target.
                self.ui.dispatch(slint::platform::WindowEvent::PointerPressed {
                    position: slint::LogicalPosition::new(*x as f32, *y as f32),
                    button: slint::platform::PointerEventButton::Left,
                });
            }
            InputEvent::PointerMove { x, y } => {
                // Launcher vertical drag (C eh_main.c drag_scroll_move):
                // travel below DRAG_SLOP leaves the list alone (a
                // stationary hold must not jitter it), and once dragging,
                // launcher::drag_move clamps the offset against the same
                // geometry the painter uses and reports a change only when
                // the visible scroll moved — so a held pointer produces at
                // most one dirty transition per real scroll step, never a
                // repaint loop.
                if self.overlay == Overlay::Launcher {
                    if let (Some(prev), Some(_)) = (self.drag_y, self.press_start) {
                        let dy = prev - *y;
                        self.drag_total += dy;
                        if self.drag_total.abs() >= crate::launcher::DRAG_SLOP
                            && crate::launcher::drag_move(self, dy)
                        {
                            self.dirty = true;
                        }
                    }
                }
                self.drag_y = Some(*y);
                self.ui.dispatch(slint::platform::WindowEvent::PointerMoved {
                    position: slint::LogicalPosition::new(*x as f32, *y as f32),
                });
            }
            InputEvent::PointerUp { x, y } => {
                let (x, y) = (*x, *y);
                // Long-press on the shelf → context menu (C eh_long_press).
                let is_long = match (self.press_pos, self.press_start) {
                    (Some((px, py)), Some(t0)) => {
                        let moved = (x - px).abs() > 24 || (y - py).abs() > 24;
                        let held = t0.elapsed() >= std::time::Duration::from_millis(450);
                        !moved && held
                    }
                    _ => false,
                };
                self.press_pos = None;
                self.press_start = None;
                // A drag (moved > 48px) is not a tap.
                let dragged = self.drag_total.abs() > 48;
                self.drag_total = 0;
                self.drag_y = None;
                // Slint hit-tests the release against the pressed
                // TouchArea and reports the semantic target; tap vs
                // long-press classification stays App-side (above).
                self.ui.dispatch(slint::platform::WindowEvent::PointerReleased {
                    position: slint::LogicalPosition::new(x as f32, y as f32),
                    button: slint::platform::PointerEventButton::Left,
                });
                if self.overlay == Overlay::None {
                    self.pending_long = is_long && self.tab == Tab::Library;
                    self.apply_actions();
                } else {
                    // Interim: overlays still hit-test their draw-time
                    // rect caches (ported screen by screen); base-page
                    // intents under an overlay are dropped.
                    let _ = crate::ui::drain_actions();
                    if !dragged {
                        self.tap_overlay(x, y);
                    }
                }
            }
            // EVT_SHOW / EVT_FOREGROUND (C eh_evt_show): a full redraw —
            // the user may have been reading with the integrated reader
            // or KOReader while we were away, so refresh their progress
            // first, then repaint everything.
            InputEvent::WidgetShown => self.reload_progress(),
            _ => {}
        }
    }

    /// Consume a committed keyboard edit.  Returns true when the event that
    /// triggered this drain came from the keyboard commit and must not also
    /// be routed (the C app's commit handler draws immediately, so the tap
    /// that closed the keyboard never reaches the screen).
    pub(crate) fn drain_keyboard(&mut self) -> bool {
        match kb_take_pending() {
            None => false,
            Some((KbField::ApiHost, text)) => {
                self.config.api_url = normalize_host(&text);
                self.client = ApiClient::new(&self.config.api_url, &self.config.api_token);
                self.kb_editing = None;
                self.save_config();
                self.dirty = true;
                true
            }
            Some((KbField::ApiKey, text)) => {
                self.config.api_token = text;
                self.client = ApiClient::new(&self.config.api_url, &self.config.api_token);
                self.kb_editing = None;
                self.save_config();
                self.dirty = true;
                true
            }
            Some((KbField::Search, text)) => {
                let changed = text != self.query;
                // The keyboard is closing: tear the live suggestion band
                // down (C eh_keyboard_handler: ClearTimerByName + nsuggest=0).
                self.search_kb = false;
                self.suggestions.clear();
                self.suggest_q.clear();
                if changed {
                    self.commit_search(&text);
                } else if self.tab == Tab::Search {
                    // Keyboard dismissed unchanged: redraw the bar in normal style.
                    self.refresh_shelf();
                }
                true
            }
        }
    }

    /// The 200 ms suggest tick (C suggest_debounce_tick): while the search
    /// keyboard is open, poll the live keyboard buffer — the firmware's
    /// text-change callback never fires on this build — and re-query the
    /// suggestion index only when the buffer moved.  Returns true when the
    /// band changed and a repaint is due.  The caller owns the cadence
    /// (the facade's weak timer; the C app re-arms SetWeakTimerEx here).
    pub fn tick(&mut self) -> bool {
        // Background full-library cover-warm pass (one fetch per tick).
        self.cover_warm_tick();
        // Drain a finished local-source import (C apply chain's main-thread
        // slice): replaces the 'local' source and rebuilds the view.
        crate::local::poll_import(self);
        // Drain the async sync worker (C's wkr done-callbacks + bsyncp
        // close tick): applies events to the popup state machine and
        // lands the terminal rebuild on the main thread.
        if self.sync_poll() {
            self.dirty = true; // the present() skip would swallow the update
        }
        let due = self.sync_spin_tick();
        if due {
            // The glyph rotated: the top bar needs a repaint (the facade
            // presents every tick; present() skips when not dirty).
            self.dirty = true;
        }
        if !self.search_kb || self.tab != Tab::Search {
            return due;
        }
        let Some(text) = self.fb().live_keyboard_text() else {
            return false;
        };
        if text == self.suggest_q {
            return false; // nothing typed since the last tick
        }
        self.suggest_q = text;
        let rows = self
            .store
            .suggest_list(&self.suggest_q, crate::app::SUGGEST_MAX_HITS)
            .unwrap_or_default();
        if rows == self.suggestions {
            return false; // buffer moved but the hits did not (C `changed` check)
        }
        self.suggestions = rows;
        // Rebuild so the band shows the new rows (or restores the history
        // list when the hits emptied); present() flushes from `dirty`.
        self.refresh_shelf();
        true
    }

    /// Back (hardware key): close the topmost overlay; on the search tab
    /// leave search keeping the active query filter (C: 'the grid stays
    /// filtered').
    fn back(&mut self) {
        if self.overlay != Overlay::None {
            self.set_overlay(Overlay::None);
            self.menu_rows.clear();
            self.settings_rows.clear();
            self.launcher_rects.clear();
            self.source_rows.clear();
            self.context.rects.clear();
            self.context.items.clear();
            self.context.book = None;
            return;
        }
        // Drilled into a group: pop the drill level first.
        if self.drill > 0 {
            self.drill_back();
            return;
        }
        // The download-folder picker closes on Back and returns to the
        // Settings page it was opened from (C eh_folder_close).
        if self.dl_picker.take().is_some() {
            self.set_overlay(Overlay::Settings);
            return;
        }
        // Folder source: Back ascends one level; at the browser root it
        // falls through (C eh_browse_up's "caller decides" contract).
        if self.source == Source::Folder && self.browser.open && crate::local::browse_up(self) {
            return;
        }
        if self.tab == Tab::Search {
            self.leave_search();
        }
    }

    /// The active downloads dir (C eh_resolve_downloads_dir default).
    pub(crate) fn downloads_dir(&self) -> String {
        self.config
            .downloads_dir
            .clone()
            .unwrap_or_else(crate::local::default_downloads_dir)
    }

    // ── shelf state ───────────────────────────────────────────────────

    /// The offered group-chooser presets (C eh_view_dim_available), in the
    /// harness's row order: None, Author>Series, Series, Author, Year,
    /// Genre, minus dims the store has no values for.
    pub(crate) fn group_offer(&self) -> Vec<crate::store::GroupPreset> {
        let (a, s, y, g) = self
            .store
            .dim_availability()
            .unwrap_or((true, false, true, true));
        use crate::store::GroupPreset;
        let mut out = vec![GroupPreset::None];
        if a && s {
            out.push(GroupPreset::AuthorSeries);
        }
        if s {
            out.push(GroupPreset::Series);
        }
        if a {
            out.push(GroupPreset::Author);
        }
        if y {
            out.push(GroupPreset::Year);
        }
        if g {
            out.push(GroupPreset::Genre);
        }
        out
    }

    /// Rebuild the materialised view for the active group/sort/drill and
    /// log the C `view_rebuild: view=… sort=… group=… drill=…` marker.
    pub(crate) fn rebuild_view(&mut self) {
        let (group, sort, drill, q) = (self.group, self.sort, self.drill, self.query.clone());
        let total = {
            let scopes = self.drill_scopes();
            let src = self.source.config_value();
            self.store
                .view_rebuild(group as i64, sort as i64, drill as i64, &q, &scopes, &src)
                .unwrap_or(0)
        };
        crate::logger::log(&format!(
            "[bookshelf] view_rebuild: view={} sort={} group={} drill={}",
            total, sort as i64, group as i64, drill
        ));
        self.dirty = true;
        self.refresh_shelf();
    }

    /// Change the active overlay, marking the frame dirty (the present
    /// skip must repaint when the overlay changes).
    pub(crate) fn set_overlay(&mut self, o: Overlay) {
        if o != self.overlay {
            // Leaving the sync sheet retires its state machine (the C
            // popup flag lives in eh_g_state; ours rides the overlay).
            if self.overlay == Overlay::Sync {
                self.sync_popup.open = false;
            }
            self.dirty = true;
        }
        self.overlay = o;
    }

    /// The centered top-bar title (C top_bar_title): the deepest drilled
    /// series/group name, the query on a filtered shelf, else nothing.
    pub(crate) fn top_title(&self) -> &str {
        for name in self.drill_names[..self.drill as usize].iter().rev() {
            if !name.is_empty() {
                return name;
            }
        }
        if self.query.is_empty() {
            ""
        } else {
            &self.query
        }
    }
}
