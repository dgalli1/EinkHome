//! Shelf paging math + data assembly (split from `app.rs`): the per-mode
//! page size, one page of materialised-view entries with their cover art
//! resolved (server cache first, then LOCAL extraction).  The page MODEL
//! sync into the Slint tree lives in `app/data.rs`.
use eh_hal::Framebuffer;
use std::path::Path;

use crate::app::{App, Source, ViewMode};
use crate::appui::{PAGER_H, TOP_BAR_H};
use crate::shelf::{self, ShelfEntry};
use crate::store::Book;

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
}
