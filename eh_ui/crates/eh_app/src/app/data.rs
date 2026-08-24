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

    /// Rebuild the shelf at the current page (the caller presents):
    /// recompute page math, fetch the page's entries, and push the page
    /// model into the Slint tree.
    pub fn refresh_shelf(&mut self) {
        self.dirty = true;
        if self.dl_picker.is_some() {
            // The download-folder picker owns the whole page (C
            // BR_MODE_PICKER draws over the settings screen).
            self.pages = 1;
            self.entries.clear();
            let mut b = self.dl_picker.take().expect("picker");
            self.sync_browser_model(&b);
            self.dl_picker = Some(b);
            return;
        }
        if self.tab == Tab::Search {
            self.refresh_search_page();
            return;
        }
        if self.source == Source::Folder && self.browser.open {
            // Folder source: the directory browser IS the shelf body
            // (C BR_MODE_BROWSER); the top bar carries the current path.
            self.pages = 1;
            self.entries.clear();
            let browser = std::mem::take(&mut self.browser);
            self.sync_browser_model(&browser);
            self.browser = browser;
        } else {
            let width = self.screen_width();
            let per = self.page_size(width);
            let total = self.view_total_books();
            self.pages = if total == 0 { 1 } else { total.div_ceil(per) };
            if self.page >= self.pages {
                self.page = self.pages.saturating_sub(1);
            }
            self.entries = self.store_view_page(per, self.page * per);
            self.sync_shelf_model();
        }
        // C draw_grid marker (the e2e harness's wait-for-grid token) with
        // the projected tile total — LIBRARY only: the C Search page logs
        // draw_search_tab instead, and the harness reads a draw_grid in a
        // search-invocation slice as "jumped to the library".
        if self.tab == Tab::Library && !self.browse_active() {
            let view = self.view_total_books();
            crate::logger::log(&format!(
                "[bookshelf] draw_grid view={view} page={} top={} bot={}",
                self.page,
                crate::appui::TOP_BAR_H,
                self.content_bottom
            ));
        }
        crate::log(&format!(
            "[eh_app] shelf page={}/{} entries={}",
            self.page + 1,
            self.pages,
            self.entries.len()
        ));
    }

    /// Push the library page's tiles into the Slint entries model.
    fn sync_shelf_model(&mut self) {
        let tiles: Vec<crate::ui::ShelfTile> = self
            .entries
            .iter()
            .map(|e| {
                let (art, has_art) = match &e.art {
                    Some((rgb, w, h)) => (
                        slint::Image::from_rgb8(slint::SharedPixelBuffer::<
                            slint::Rgb8Pixel,
                        >::clone_from_slice(rgb, *w, *h)),
                        true,
                    ),
                    None => (slint::Image::default(), false),
                };
                crate::ui::ShelfTile {
                    title: e.book.title.clone().into(),
                    author: e.book.author.clone().into(),
                    art,
                    has_art,
                    stack: e.stack,
                    stack_label: e.stack_label.clone().into(),
                    stack_count: e.stack_count as i32,
                    progress: e.progress as i32,
                }
            })
            .collect();
        let model = slint::VecModel::from(tiles);
        self.ui.comp().set_entries(slint::ModelRc::new(model));
    }

    /// Push a browser listing (Folder source body or the download-dir
    /// picker) into the Slint browse model: `rows_visible` fixed rows
    /// starting at the scroll offset, blank-padded.
    fn sync_browser_model(&mut self, browser: &crate::local::browser::Browser) {
        let rows_visible = crate::local::browser::Browser::rows_visible(self.content_bottom);
        let mut rows: Vec<crate::ui::BrowseRow> = Vec::with_capacity(rows_visible);
        for i in 0..rows_visible {
            match browser.entries.get(browser.scroll + i) {
                Some(e) => rows.push(crate::ui::BrowseRow {
                    label: if e.is_dir {
                        format!("{}/", e.name).into()
                    } else {
                        e.name.clone().into()
                    },
                    blank: false,
                }),
                None => rows.push(crate::ui::BrowseRow {
                    label: slint::SharedString::new(),
                    blank: true,
                }),
            }
        }
        let scroll = browser.scroll as i32;
        let model = slint::VecModel::from(rows);
        let c = self.ui.comp();
        c.set_browse_rows(slint::ModelRc::new(model));
        c.set_browse_scroll(scroll);
    }

    /// The Search sub-page: history (or live suggestions) rows + paging.
    fn refresh_search_page(&mut self) {
        // History rows per page: the C eh_history_pagesize formula.
        let rows_per = ((self.content_bottom as i32
            - crate::appui::PAGER_H as i32
            - crate::appui::TOP_BAR_H as i32
            - crate::appui::TOP_BAR_PAD as i32
            - 88)
            / 96)
            .max(1) as usize;
        let total = self.store.search_count().unwrap_or(0) as usize;
        self.pages = if total == 0 {
            1
        } else {
            total.div_ceil(rows_per)
        };
        if self.page >= self.pages {
            self.page = self.pages.saturating_sub(1);
        }
        let offset = self.page * rows_per;
        crate::logger::log("[bookshelf] draw_search_tab");
        let history = self.store.search_list(rows_per, offset).unwrap_or_default();
        // While the keyboard is open with hits, the suggestion band
        // replaces the history list (C suggest_debounce_tick →
        // eh_draw_suggestions); empty hits keep the history visible.
        let using_suggestions = self.search_kb && !self.suggestions.is_empty();
        let rows: Vec<String> = if using_suggestions {
            self.suggestions.clone()
        } else {
            history
        };
        let hint = rows.is_empty();
        let list: Vec<String> = if hint {
            vec![crate::i18n::tr("search.empty").to_string()]
        } else {
            rows
        };
        let model = slint::VecModel::from(list.into_iter().map(slint::SharedString::from).collect::<Vec<_>>());
        let c = self.ui.comp();
        c.set_history(slint::ModelRc::new(model));
        c.set_history_hint(hint);
    }

    /// Flip to `page` (clamped): fetch the page's covers into the cache
    /// first (C cover-warm pass), then rebuild.
    pub fn goto_page(&mut self, page: usize) {
        if page >= self.pages || page == self.page {
            return;
        }
        self.page = page;
        let width = self.screen_width();
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
        if self.fb().net_active() {
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
                    self.reader_pref, self.reader_path,
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
