//! The Search sub-page flow (split from `app.rs`): entering/leaving the
//! tab, committing a query through the store, body-tap routing over the
//! input row / history / suggestion rows, and the firmware keyboard open
//! (C top-bar search icon → eh_hit_search_input → eh_keyboard_handler).

use eh_hal::{Framebuffer, Rect};

use crate::app::{App, KbField, Tab};

impl<B: Framebuffer> App<B> {
    /// Open the Search sub-page (C top-bar search-icon tap, which==5).
    pub(crate) fn enter_search(&mut self) {
        self.tab = Tab::Search;
        self.page = 0;
        self.refresh_shelf();
    }

    /// Leave Search back to the library shelf, keeping the query filter.
    /// A still-open keyboard is cancelled first (C eh_evt_back_search_drill:
    /// CloseKeyboard, then the tab switch; the handler tears the band down).
    pub(crate) fn leave_search(&mut self) {
        if self.search_kb {
            self.screen()
                .framebuffer_mut()
                .cancel_keyboard();
            // The cancelled keyboard never delivers a commit, so drain the
            // band state here (the C handler's teardown).
            self.search_kb = false;
            self.suggestions.clear();
            self.suggest_q.clear();
        }
        self.tab = Tab::Library;
        self.page = 0;
        self.refresh_shelf();
    }

    /// Apply a committed search query (C eh_keyboard_handler non-empty
    /// branch): record it, filter the shelf, return to the library tab.
    /// Empty / unchanged text keeps the search page open (C outside-tap).
    pub(crate) fn commit_search(&mut self, term: &str) {
        let term = term.trim().to_string();
        if term.is_empty() || term == self.query {
            // Dismissed unedited: leave search, don't teleport home.
            return;
        }
        self.query = term.clone();
        if let Err(e) = self.store.search_add(&term) {
            crate::log(&format!("[eh_app] search_add: {e}"));
        }
        crate::logger::log(&format!("[bookshelf] search commit: query=`{term}`"));
        self.tab = Tab::Library;
        // The shelf reads the materialised view — the query must reach
        // view_rebuild, not just a widget refresh (C rebuilt the view on
        // every keyboard commit).
        self.rebuild_view();
    }

    /// Search-tab body taps: the input row opens the keyboard; a history
    /// row re-runs that stored query (C eh_hit_search_input / history tap).
    /// While the keyboard is open (C eh_pu_handle_search_kb) a suggestion
    /// or history row tap cancels the keyboard and commits the term —
    /// CloseKeyboard() delivers no commit, so the app performs it — and
    /// any other tap above the keyboard dismisses it.
    pub(crate) fn tap_search_body(&mut self, x: i32, y: i32) {
        let n = self.screen().widgets.len();
        let last = n.saturating_sub(1);
        // Input row is widget index 1 (bordered box inset like its draw).
        // With the keyboard already open a tap here dismisses it (C:
        // outside-band branch), it never re-opens.
        if !self.search_kb && n > 1 {
            let r = self.screen().widget_rect(1);
            if x >= r.x as i32 + 16
                && x < (r.x + r.w) as i32 - 16
                && y >= r.y as i32 + 10
                && y < (r.y + r.h) as i32 - 10
            {
                self.edit_search();
                return;
            }
        }
        // History ROWS are widget indices 3..last: index 2 is the body
        // CONTAINER (it spans the whole body, so treating it as a row
        // would swallow every tap below the input).  With the keyboard
        // open and suggestions showing, the rows parallel self.suggestions
        // (the band replaced the history list); otherwise the store's
        // newest-first history list.
        let mut hit: Option<usize> = None;
        let mut rects: Vec<Rect> = Vec::new();
        for i in 3..last {
            rects.push(self.screen().widget_rect(i));
        }
        for (i, r) in rects.iter().enumerate() {
            if r.contains(x, y) {
                hit = Some(i);
                break;
            }
        }
        if let Some(idx) = hit {
            let terms = if self.search_kb && !self.suggestions.is_empty() {
                crate::logger::log(&format!(
                    "[bookshelf] suggest tap: term=`{}`",
                    self.suggestions[idx]
                ));
                Some(self.suggestions[idx].clone())
            } else {
                self.store
                    .search_list(1000, 0)
                    .unwrap_or_default()
                    .get(idx)
                    .map(|t| {
                        crate::logger::log(&format!("[bookshelf] search history tap: query=`{t}`"));
                        t.clone()
                    })
            };
            if let Some(t) = terms {
                if self.search_kb {
                    // Cancel first: the firmware close must not deliver a
                    // commit racing ours (C CloseKeyboard + app-side commit).
                    self.search_kb = false;
                    self.suggestions.clear();
                    self.suggest_q.clear();
                    self.screen().framebuffer_mut().cancel_keyboard();
                }
                self.commit_search(&t);
            }
            return;
        }
        // Outside the rows with the keyboard open: dismiss it, staying on
        // the Search page (the bar returns to normal style).
        if self.search_kb {
            self.search_kb = false;
            self.suggestions.clear();
            self.suggest_q.clear();
            self.screen().framebuffer_mut().cancel_keyboard();
            self.refresh_shelf();
        }
    }

    /// Open the search keyboard with the current query as initial text.
    pub(crate) fn edit_search(&mut self) {
        use crate::app::{kb_arm, kb_commit, kb_take_pending};
        let initial = self.query.clone();
        let _ = kb_take_pending();
        kb_arm(KbField::Search);
        self.search_kb = true;
        self.suggestions.clear();
        // Reset the tick cache so the first poll acts even when the
        // pre-filled buffer matches the old query (C g_last_suggest_q[0]=0).
        self.suggest_q.clear();
        // Rebuild the search page to show the inverted input bar.
        self.refresh_shelf();
        self.screen()
            .framebuffer_mut()
            .open_keyboard("Search", &initial, kb_commit);
    }
}
