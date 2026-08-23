//! The long-press context menu flow (split from `app.rs`): hit-testing a
//! long press against the shelf widgets, opening the book/series menu,
//! routing its row taps, and the Delete/Download-all series actions
//! (C eh_long_press → eh_context → eh_context_item_handler).

use eh_hal::{Framebuffer, Rect};

use crate::app::{App, Overlay};
use crate::downloads::book_local_path;
use crate::store::Book;
use crate::widgets::context::ContextAction;

/// The long-press context menu's state (the C g_context* globals): which
/// rows are offered, their tap geometry, and the target — either a book
/// ([`ContextAction::Open`]/Download/Delete) or a series scope
/// (Download all / Delete series).
#[derive(Default)]
pub struct MenuState {
    /// Rows in tap order; parallel to [`MenuState::rects`].
    pub items: Vec<ContextAction>,
    /// Row rects rebuilt each draw so taps match the paint.
    pub rects: Vec<Rect>,
    /// 0 = book menu, 1 = series menu (the `context menu open series=N`
    /// log marker).
    pub series: u32,
    /// The book menu's target (None when dismissed / a series menu).
    pub book: Option<Book>,
    /// The series context's drill scope + label + member count
    /// (stack-card long-press).
    pub scope: String,
    pub label: String,
    pub count: i64,
}

impl MenuState {
    /// Dismiss the menu: clear rows, geometry and target (an outside tap
    /// or back navigation).
    pub fn dismiss(&mut self) {
        self.rects.clear();
        self.items.clear();
        self.book = None;
    }

    /// Take the series scope triple, clearing scope + label so a later
    /// book menu cannot inherit them (`count` is read-only).
    pub fn take_series(&mut self) -> (String, String, i64) {
        (
            std::mem::take(&mut self.scope),
            std::mem::take(&mut self.label),
            self.count,
        )
    }
}

impl<B: Framebuffer> App<B> {
    /// A long press on a shelf tile opens the book (or stack) context
    /// menu (C eh_long_press → eh_context).  Returns true when opened.
    pub(crate) fn long_press_at(&mut self, x: i32, y: i32) -> bool {
        let topbar = self.screen().widget_rect(0);
        let last = self.screen().widgets.len().saturating_sub(1);
        let pager = self.screen().widget_rect(last);
        if y < topbar.y as i32 || y >= pager.y as i32 {
            return false;
        }
        for (i, w) in self.screen().widgets.iter().enumerate().skip(1).take(last.saturating_sub(1)) {
            if w.hit(x, y) {
                let pos = i - 2; // widget 0 = topbar, 1 = grid container
                if pos < self.entries.len() {
                    if self.entries[pos].stack {
                        // A stack card long-press opens the SERIES context
                        // (Download all / Delete series).
                        let scope = self.entries[pos].stack_scope.clone();
                        let label = self.entries[pos].stack_label.clone();
                        let count = self.entries[pos].stack_count;
                        self.open_context_series(&scope, &label, count);
                        return true;
                    }
                    let book = self.entries[pos].book.clone();
                    self.open_context_book(&book);
                    return true;
                }
                return false;
            }
        }
        false
    }

    /// Open the series context menu (Download all / Delete series) for a
    /// stack card (C eh_context series branch).
    pub(crate) fn open_context_series(&mut self, scope: &str, label: &str, count: i64) {
        self.context.items = vec![ContextAction::DownloadAll, ContextAction::DeleteAll];
        self.context.series = 1;
        self.context.scope = scope.to_string();
        self.context.label = label.to_string();
        self.context.count = count;
        crate::logger::log("[bookshelf] context menu open series=1");
        self.set_overlay(Overlay::Context);
    }

    /// Open the book context menu (Open/Download/Delete).
    pub(crate) fn open_context_book(&mut self, book: &Book) {
        self.context.items = vec![ContextAction::Open, ContextAction::Download, ContextAction::Delete];
        self.context.series = 0;
        self.context.book = Some(book.clone());
        crate::logger::log("[bookshelf] context menu open series=0");
        self.set_overlay(Overlay::Context);
    }

    /// A context-menu row tap (C eh_context_item_handler).
    pub(crate) fn tap_context(&mut self, x: i32, y: i32) {
        for (i, r) in self.context.rects.iter().enumerate() {
            if r.contains(x, y) {
                if let Some(action) = self.context.items.get(i).copied() {
                    let book = self.context.book.take();
                    self.context.dismiss();
                    self.set_overlay(Overlay::None);
                    match action {
                        ContextAction::Open => {
                            if let Some(b) = book {
                                self.press_book(&b);
                            }
                        }
                        ContextAction::Download => {
                            if let Some(b) = book {
                                let cur = book_local_path(&b, &self.downloads_dir());
                                self.dl.reset();
                                self.enqueue_download(&b.id, &cur);
                            }
                        }
                        ContextAction::Delete => {
                            if let Some(b) = book {
                                self.delete_book(&b);
                            }
                        }
                        ContextAction::DownloadAll => {
                            let (scope, label, count) = self.context.take_series();
                            self.download_series(&scope, &label, count);
                        }
                        ContextAction::DeleteAll => {
                            let (scope, _, _) = self.context.take_series();
                            self.delete_series(&scope);
                        }
                    }
                    self.refresh_shelf();
                }
                return;
            }
        }
        // Tap outside the sheet → dismiss.
        self.context.rects.clear();
        self.context.items.clear();
        self.context.book = None;
        self.set_overlay(Overlay::None);
        self.refresh_shelf();
    }

    /// Remove a downloaded book's local file (C eh_context Delete).
    pub(crate) fn delete_book(&mut self, book: &Book) {
        let dl = self.downloads_dir();
        let cur = book_local_path(book, &dl);
        let removed = std::fs::remove_file(&cur).is_ok()
            || (!book.local_path.is_empty() && std::fs::remove_file(&book.local_path).is_ok());
        if let Err(e) = self.store.set_downloaded(&book.id, false, "") {
            crate::log(&format!("[eh_app] set_downloaded: {e}"));
        }
        if removed {
            crate::logger::log(&format!("[bookshelf] delete_book_file removed path={}", cur.display()));
        } else {
            crate::log(&format!("[eh_app] delete_book_file missing path={}", cur.display()));
        }
    }

    /// Download every book of a series (C eh_context Download all): queue
    /// the scope's books on the worker + open the modal popup.
    pub(crate) fn download_series(&mut self, scope: &str, _label: &str, _count: i64) {
        let books = self
            .store
            .list_sorted(crate::store::SortMode::Recent, "", 1, scope)
            .unwrap_or_default();
        crate::logger::log(&format!("[bookshelf] download_series scope={scope} queued={}", books.len()));
        // Fresh batch state BEFORE the popup draws: a previous batch's
        // tally must not mislabel this popup's status line.
        self.dl.reset();
        let dl = self.downloads_dir();
        for b in &books {
            let cur = book_local_path(b, &dl);
            // Via enqueue_download so an id already queued/in flight is a
            // dedup no-op (C eh_find_download guard), not a double fetch.
            self.enqueue_download(&b.id, &cur);
        }
        crate::logger::log("[bookshelf] draw_dl_popup");
        self.set_overlay(Overlay::Download);
    }

    /// Delete every downloaded file of a series (C eh_context Delete
    /// series).
    pub(crate) fn delete_series(&mut self, scope: &str) {
        let books = self
            .store
            .list_sorted(crate::store::SortMode::Recent, "", 1, scope)
            .unwrap_or_default();
        crate::logger::log(&format!("[bookshelf] delete_series scope={scope} books={}", books.len()));
        for b in &books {
            self.delete_book(b);
        }
    }
}
