//! Page construction + paging math for the shelf (split from `app.rs`):
//! the per-mode page size, one page of materialised-view entries with
//! their cover art resolved (server cache first, then LOCAL extraction),
//! and the Library / Search sub-page builders.  The SCREEN SWAP itself
//! (`App::refresh_shelf`) stays in `app.rs` — it owns the take-and-
//! rebuild framebuffer contract.
use eh_hal::Framebuffer;
use std::path::Path;

use crate::app::{App, Source, ViewMode};
use crate::appui::{PAGER_H, TOP_BAR_H};
use crate::shelf::{self, ShelfEntry};
use crate::store::Book;
use eh_shell::Screen;

impl<B: Framebuffer> App<B> {
    /// The shelf page size for the current view mode + panel width.  Grid
    /// uses the C mode-aware grid dims (3×2 on the standard panel); list is
    /// always 1 column of fixed-height rows that fit above the pager.
    pub(crate) fn page_size(&self, width: u32) -> usize {
        match self.view_mode {
            ViewMode::List => {
                let band = (self.content_bottom as i32
                    - TOP_BAR_H as i32
                    - crate::appui::TOP_BAR_PAD as i32
                    - PAGER_H as i32
                    - 8)
                .max(1) as u32;
                (band / shelf::LIST_ROW_H).max(1) as usize
            }
            ViewMode::Grid => {
                let g = shelf::grid_geom(width, self.content_bottom);
                (g.cols * g.rows) as usize
            }
        }
    }

    /// The library shelf (grid or list) at the current page.
    pub(crate) fn build_library_page(&mut self, fb: B, width: u32) -> Screen<B> {
        // Folder source: the directory browser IS the shelf body
        // (C BR_MODE_BROWSER); the top bar carries the current path.
        if self.source == Source::Folder && self.browser.open {
            self.pages = 1;
            self.entries.clear();
            let browser = std::mem::take(&mut self.browser);
            let screen = crate::local::build_browse_page(fb, &browser, self.content_bottom);
            self.browser = browser;
            return screen;
        }
        let per = self.page_size(width);
        let total = self.view_total_books();
        self.pages = if total == 0 { 1 } else { total.div_ceil(per) };
        if self.page >= self.pages {
            self.page = self.pages.saturating_sub(1);
        }
        self.entries = self.store_view_page(per, self.page * per);
        let page = self.page;
        let pages = self.pages;
        let content_bottom = self.content_bottom;
        let title = self.top_title().to_string();
        let (view_mode, source, syncing, drilled, sync_angle) = (
            self.view_mode,
            self.source,
            self.syncing,
            self.drill > 0,
            self.sync_angle,
        );
        shelf::build_shelf(
            fb,
            &title,
            page,
            pages,
            &self.entries,
            content_bottom,
            view_mode,
            drilled, // back chevron when drilled into a group
            source,  // source
            false,   // not the search tab
            syncing,
            sync_angle,
        )
    }

    /// Tile count the shelf pages over: the materialised view when one is
    /// present, else the library count (the C eh_view_total).
    pub(crate) fn view_total_books(&self) -> usize {
        let vt = self.store.view_total();
        if vt > 0 {
            vt
        } else {
            self.store.count().unwrap_or(0) as usize
        }
    }

    /// One page of shelf entries from the materialised view.  A stack card
    /// (kind 1) is paired with its representative book so covers/drills
    /// keep working; flat tiles map to their book.
    pub(crate) fn store_view_page(&mut self, per: usize, offset: usize) -> Vec<ShelfEntry> {
        let rows = self.store.view_page(per, offset).unwrap_or_default();
        let mut entries = Vec::with_capacity(rows.len());
        for v in rows {
            let book = self
                .store
                .get_book(&v.book_id)
                .ok()
                .flatten()
                .unwrap_or_default();
            let art = crate::cover::load_cached(&self.covers_dir, &book.id)
                .and_then(|bytes| crate::cover::decode_rgb(&bytes).ok())
                .map(|(w, h, rgb)| (rgb, w, h))
                .or_else(|| self.local_cover_art(&book));
            if art.is_some() {
                crate::logger::log(&format!("[bookshelf] cover_tick cache hit id={}", book.id));
            }
            let stack = v.kind == 1;
            let scope = if stack {
                v.series_id.clone()
            } else {
                String::new()
            };
            let progress = crate::progress::percent(&self.progress, &book.local_path);
            entries.push(ShelfEntry {
                book,
                art,
                stack,
                stack_label: v.series_name,
                stack_count: v.series_count,
                stack_scope: scope,
                progress,
            });
        }
        entries
    }

    /// Cover art for a LOCAL book (C cover_slot_fetch's local branch):
    /// no server cover exists, so extract the embedded image (EPUB),
    /// render the PDF's first page, or typeset a TXT's opening words —
    /// and cache the raw bytes next to the PNG cache so only the first
    /// view pays for the extraction.
    pub(crate) fn local_cover_art(&self, book: &Book) -> Option<(Vec<u8>, u32, u32)> {
        // Kavita rows get their art from the server cache; local_path
        // alone would re-extract every downloaded book needlessly.
        if self.source == Source::Kavita || book.local_path.is_empty() {
            return None;
        }
        let raw = crate::cover::raw_path(&self.covers_dir, &book.id);
        let bytes = match std::fs::read(&raw) {
            Ok(bytes) => bytes,
            Err(_) => {
                // Extraction failure caches an EMPTY tombstone so a
                // hopeless file is not re-opened (and a PDF not
                // re-rendered) on every later view.
                let Some(extracted) =
                    crate::extract::extract_book_cover(Path::new(&book.local_path), &book.ext)
                else {
                    let _ = crate::cover::store_raw(&self.covers_dir, &book.id, &[]);
                    return None;
                };
                crate::cover::store_raw(&self.covers_dir, &book.id, &extracted).ok()?;
                extracted
            }
        };
        if bytes.is_empty() {
            return None; // known-no-cover tombstone
        }
        crate::logger::log(&format!(
            "[bookshelf] cover_tick local extract id={}",
            book.id
        ));
        crate::cover::decode_rgb(&bytes)
            .ok()
            .map(|(w, h, rgb)| (rgb, w, h))
    }

    /// The Search sub-page at the current page (input row + history).
    pub(crate) fn build_search_page(&mut self, fb: B, width: u32) -> Screen<B> {
        let _ = width;
        // History rows per page: the C eh_history_pagesize formula.
        let rows_per = ((self.content_bottom as i32
            - PAGER_H as i32
            - TOP_BAR_H as i32
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
        let rows = if using_suggestions {
            &self.suggestions
        } else {
            &history
        };
        let (page, pages, query, content_bottom, syncing) = (
            self.page,
            self.pages,
            self.query.clone(),
            self.content_bottom,
            self.syncing,
        );
        shelf::build_search(
            fb,
            &query,
            page,
            pages,
            rows,
            content_bottom,
            syncing,
            self.search_kb,
        )
    }
}
