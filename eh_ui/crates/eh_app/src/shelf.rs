//! Shelf screen — the real library grid, bound to the store + cover layers.
//!
//! Builds the shell's [`Cover`] widgets from the persisted `books` rows and
//! the on-disk/cover cache, so the grid shows real titles/authors/art.  This
//! is the vertical slice that replaces the demo's placeholder tiles.
//!
//! Cover art is fetched BEFORE building the widgets (the caller passes books
//! + their decoded covers), so the shell stays free of image-lifetime
//! concerns — [`apply_cover_art`] mutates a `Cover` through a downcast the
//! caller drives once.

use std::path::Path;

use eh_hal::Framebuffer;
use eh_layout::taffy;
use eh_layout::taffy::{Dimension, Style};
use eh_render::Font;
use eh_shell::{Cover, Screen};

use crate::client::ApiClient;
use crate::cover;
use crate::store::{Book, Store};

/// A book paired with its decoded cover raw-RGB (if available).
pub struct ShelfEntry {
    pub book: Book,
    /// (rgb, width, height) cover art, or None for a placeholder tile.
    pub art: Option<(Vec<u8>, u32, u32)>,
}

/// Layout: the same wrap-flex grid as the demo, but each tile is a real book.
pub fn build_shelf<B: Framebuffer>(fb: B, entries: &[ShelfEntry]) -> Screen<B> {
    let font = load_font();
    let mut screen = Screen::new(fb, font);
    screen.layout_mut().root_flex_wrap();

    let cols = columns_for(screen.breakpoint);
    for e in entries {
        let mut c = Cover::new(e.book.title.clone());
        c.author = e.book.author.clone();
        c.title_size = 20.0;
        if let Some((rgb, w, h)) = &e.art {
            c.set_image(rgb.clone(), *w, *h);
        }
        let style = Style {
            size: taffy::geometry::Size {
                width: Dimension::percent(1.0 / cols as f32),
                height: Dimension::length(300.0),
            },
            ..Style::default()
        };
        screen.add_styled(Box::new(c), style);
    }
    screen
}

/// Load one page of books + their covers from the store/cache/API.
/// `count` = max books on the page; `offset` = page start.
/// Cover fetch is best-effort: an undecodable/missing cover → placeholder.
pub fn load_page(
    client: &ApiClient,
    store: &Store,
    covers_dir: &Path,
    count: usize,
    offset: usize,
) -> Vec<ShelfEntry> {
    let books = store.list_books(count, offset).unwrap_or_default();
    books
        .into_iter()
        .map(|book| {
            let art = fetch_cover(client, covers_dir, &book.id).ok().flatten();
            ShelfEntry { book, art }
        })
        .collect()
}

/// Fetch + decode a single book's cover; Ok(None) when none/undecodable.
fn fetch_cover(
    client: &ApiClient,
    covers_dir: &Path,
    id: &str,
) -> Result<Option<(Vec<u8>, u32, u32)>, String> {
    let bytes = cover::fetch(client, covers_dir, id)?;
    let (w, h, rgb) = cover::decode_rgb(&bytes)?;
    Ok(Some((rgb, w, h)))
}

/// Column count for the active breakpoint (same table as the demo).
pub fn columns_for(bp: eh_layout::Breakpoint) -> u32 {
    match bp {
        eh_layout::Breakpoint::Narrow => 2,
        eh_layout::Breakpoint::Std => 3,
        eh_layout::Breakpoint::Wide => 4,
    }
}

pub(crate) fn load_font() -> &'static Font {
    let f = Font::from_bytes(include_bytes!("../../../fonts/DejaVuSans.ttf"))
        .expect("embed font");
    Box::leak(Box::new(f))
}

/// The font the shelf uses (exposed for the status strip / top bar).
pub fn shelf_font() -> &'static Font {
    load_font()
}