//! Shelf data operations: page loading/rebuilds, drill paging, progress
//! reloads, config persistence and settings application (the C model-
//! side helpers eh_refresh*/eh_save_config/eh_settings_apply).
use super::*;

impl<B: Framebuffer> App<B> {
    /// Re-read the reading-progress map from the firmware explorer db and
    /// repaint the shelf (the C eh_evt_show → eh_progress_reload flow).
    /// Public so lifecycle plumbing (EVT_SHOW/FOREGROUND delivery) can
    /// drive it too.
    pub fn reload_progress(&mut self) {
        self.progress = crate::progress::reload();
        self.refresh_shelf();
    }

    /// Rebuild the shelf at the current page (the caller presents).
    pub fn refresh_shelf(&mut self) {
        self.dirty = true;
        // Take the framebuffer out first: the new screen is built from the
        // same canvas (the C app's full-redraw navigation).
        let fb = self.screen.take().expect("screen present").into_framebuffer();
        if let Some(b) = self.dl_picker.as_mut() {
            // The download-folder picker owns the whole page (C
            // BR_MODE_PICKER draws over the settings screen).
            let mut screen = crate::local::build_browse_page(fb, b, self.content_bottom);
            screen.content_h = self.content_bottom;
            self.screen = Some(screen);
            return;
        }
        let width = fb.screen().width;
        let mut screen = if self.tab == Tab::Search {
            self.build_search_page(fb, width)
        } else {
            self.build_library_page(fb, width)
        };
        screen.content_h = self.content_bottom;
        self.screen = Some(screen);
        // C draw_grid marker (the e2e harness's wait-for-grid token) with
        // the projected tile total — LIBRARY only: the C Search page logs
        // draw_search_tab instead, and the harness reads a draw_grid in a
        // search-invocation slice as "jumped to the library".
        if self.tab == Tab::Library {
            let sw = self.screen().framebuffer().screen().width;
            let view = self.view_total_books();
            crate::logger::log(&format!(
                "[bookshelf] draw_grid view={view} page={} cell={}x0 top=96 bot={}",
                self.page, sw, self.content_bottom
            ));
        }
        crate::log(&format!(
            "[eh_app] shelf page={}/{} entries={}",
            self.page + 1,
            self.pages,
            self.entries.len()
        ));
    }

    /// Flip to `page` (clamped): fetch the page's covers into the cache
    /// first (C cover-warm pass), then rebuild.
    pub fn goto_page(&mut self, page: usize) {
        if page >= self.pages || page == self.page {
            return;
        }
        self.page = page;
        let width = self.screen().framebuffer().screen().width;
        let per = self.page_size(width);
        let books = if self.query.is_empty() {
            self.store.list_books(per, page * per).unwrap_or_default()
        } else {
            self.store
                .search(&self.query, per, page * per, &self.source.config_value())
                .unwrap_or_default()
        };
        // C cover-warm pass — network-gated: an offline flip renders the
        // cached covers only (no remote fetches, C eh_plat_net_active).
        if self.screen().framebuffer().net_active() {
            for b in &books {
                let _ = cover::fetch(&self.client, &self.covers_dir, &b.id);
            }
        }
        self.refresh_shelf();
    }

    /// Picker commit (C folder_commit + eh_settings_apply's dir
    /// re-resolve): store the chosen downloads dir, persist it, log the
    /// saved marker and repaint.  Back returns to Settings.
    pub(crate) fn commit_downloads_dir(&mut self, path: &str) {
        ensure_writable_dir(path);
        self.config.downloads_dir = Some(path.to_string());
        self.save_config();
        crate::logger::log("[bookshelf] settings: saved");
        self.dl_picker = None;
        self.set_overlay(Overlay::Settings);
    }

    /// Save the settings screen's edits to the config file (C
    /// eh_save_config_file after the Save button / a keyboard commit).
    pub fn save_config(&mut self) {
        if let Some(p) = &self.cfg_path {
            if let Err(e) = self.config.save(p) {
                crate::log(&format!("[eh_app] config save failed: {e}"));
            } else {
                crate::log(&format!("[eh_app] settings: saved {}", p.display()));
                crate::logger::log(&format!(
                    "[bookshelf] settings: reader_pref={} (cfg `{}`)",
                    self.reader_pref,
                    self.reader_path,
                ));
            }
        }
    }

    /// The Save button's full side-effect chain (C eh_settings_apply):
    /// persist, rebuild the endpoint URLs from the (possibly edited)
    /// api_base/api_token, then re-sync so the shelf reflects the new
    /// server immediately.
    pub fn settings_apply(&mut self) {
        // C aborts any in-flight sync chain BEFORE the endpoints are
        // rebuilt (eh_sync_abort): the worker stops between rounds — and
        // drops a fetched-but-unapplied round — so it never fetches from
        // the new URL with the old cursor nor applies a stale response on
        // top of the new configuration.
        self.sync_abort();
        self.save_config();
        self.client = ApiClient::new(&self.config.api_url, &self.config.api_token);
        if self.source != Source::Folder {
            self.resync();
        }

        self.set_overlay(Overlay::None);
    }
}
