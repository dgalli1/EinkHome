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
            // Decoded-art memory cache first: a shelf rebuild (tab
            // switch, page flip) must not re-decode every visible cover
            // — the PNG/JPEG decode dominated the switch otherwise.
            let art = match self.art_cached(&book.id) {
                Some(a) => Some(a),
                None => {
                    let art = crate::cover::load_cached(&self.covers_dir, &book.id)
                        .and_then(|bytes| crate::cover::decode_rgb(&bytes).ok())
                        .map(|(w, h, rgb)| (rgb, w, h))
                        .or_else(|| self.local_cover_art(&book));
                    if let Some((rgb, w, h)) = &art {
                        self.art_store(&book.id, rgb.clone(), *w, *h);
                    }
                    art
                }
            };
            if art.is_some() {
                crate::logger::log(&format!("[bookshelf] cover_tick cache hit id={}", book.id));
            }
            let stack = v.kind == 1;
            let scope = if stack {
                v.series_id.clone()
            } else {
                String::new()
            };
            let progress = if self.store.is_read(&book.id) {
                100 // marked read: the tile ring shows a finished book
            } else {
                crate::progress::percent(&self.progress, &book.local_path)
            };
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
                crate::logger::log(&format!(
                    "[bookshelf] cover_tick local extract id={}",
                    book.id
                ));
                extracted
            }
        };
        if bytes.is_empty() {
            return None; // known-no-cover tombstone
        }
        crate::cover::decode_rgb(&bytes)
            .ok()
            .map(|(w, h, rgb)| (rgb, w, h))
    }

    /// Decoded-art memo look-up (FIFO-capped; see [`App::art_store`]).
    fn art_cached(&self, id: &str) -> Option<(Vec<u8>, u32, u32)> {
        self.art_cache.map.get(id).cloned()
    }

    /// Memoize a decoded cover: the working set of a page or two fits
    /// easily, and a tab switch then pays plain clones instead of a
    /// PNG/JPEG decode per visible tile.
    fn art_store(&mut self, id: &str, rgb: Vec<u8>, w: u32, h: u32) {
        const ART_CACHE_CAP: usize = 64;
        if self.art_cache.order.contains(&id.to_string()) {
            return;
        }
        if self.art_cache.order.len() >= ART_CACHE_CAP {
            if let Some(oldest) = self.art_cache.order.pop_front() {
                self.art_cache.map.remove(&oldest);
            }
        }
        self.art_cache.order.push_back(id.to_string());
        self.art_cache.map.insert(id.to_string(), (rgb, w, h));
    }
}

/// Decoded cover memo (book id → RGB + dims) with FIFO eviction order.
#[derive(Default)]
pub(crate) struct ArtCache {
    map: std::collections::HashMap<String, (Vec<u8>, u32, u32)>,
    order: std::collections::VecDeque<String>,
}
