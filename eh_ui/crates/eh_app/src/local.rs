//! Local + Folder book sources' shared facts, the Local import scanner
//! (C eh_local.c) and its async walk → apply chain.  The Folder source's
//! live directory browser lives in `browser` (C eh_browser.c).
//!
//! **Local** — a background walk of the storage root collects every book
//! file into the store (`source='local'`, `downloaded=1` — the files ARE
//! the books).  The walk runs on a plain [`std::thread`] and hands its
//! result back over a channel; the apply (SQLite) happens on the UI
//! thread, mirroring C's worker-walk → main-thread apply chain.
//!
//! Book-file metadata (title/author) is extracted in pure Rust from epub
//! (zip container + OPF), fb2 and PDF files, cached in the store's
//! `local_meta` table keyed by the stable `fld_<djb2>` id so a re-import
//! never re-parses a known book.

mod browser;

pub use browser::{
    browse_page, browse_up, build_browse_page, folder_book, start_browse, tap_browse, tap_picker,
    BrowseEntry, Browser,
};
use std::path::Path;

use eh_hal::Framebuffer;

use crate::app::App;
use crate::extract::{extract_book_meta, ExtractedMeta, MAX_TITLE_LEN};
use crate::store::Book;

// ── shared facts ─────────────────────────────────────────────────────────

/// The on-device storage root (C eh_plat_browse_root).  Host/SDL tests
/// override it with EH_BROWSE_ROOT.
pub const DEVICE_BROWSE_ROOT: &str = "/mnt/ext1";

/// The directory walk's caps (C EH_LOCAL_SCAN_DEPTH / EH_LOCAL_SCAN_CAP):
/// recursion depth and total books per import.
pub const SCAN_DEPTH: u32 = 8;
pub const SCAN_CAP: usize = 20_000;

/// Browser list cap + row height (C EH_BROWSE_MAX_ENTRIES /
/// EH_FOLDER_ROW_H).
pub const BROWSE_MAX_ENTRIES: usize = 512;
pub const FOLDER_ROW_H: u32 = 96;

/// Extensions the shelf treats as book files (C BOOK_EXTS in
/// eh_browser.c; shared by the Local import and the Folder browser).
pub const BOOK_EXTS: [&str; 10] = [
    "epub", "pdf", "mobi", "azw", "azw3", "fb2", "djvu", "txt", "cbr", "cbz",
];

/// True when `ext` (already lowercase) is a book extension (C eh_is_book_ext).
pub fn is_book_ext(ext: &str) -> bool {
    BOOK_EXTS.contains(&ext)
}

/// The lowercase extension of `name`, or None when there is none
/// (C local_scan_is_book's ext normalization).
pub fn ext_of(name: &str) -> Option<String> {
    let dot = name.rfind('.')?;
    if dot + 1 >= name.len() {
        return None;
    }
    Some(name[dot + 1..].to_ascii_lowercase())
}

/// djb2 hash → 8 hex chars: the stable opaque `fld_` ids both the Local
/// import and the Folder browser derive from file paths (C eh_hash_hex).
pub fn hash_hex(s: &str) -> String {
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u64);
    }
    format!("{h:08x}")
}

/// Filename without its extension, capped like the C title field.
fn stem_title(name: &str) -> String {
    let stem = match name.rfind('.') {
        Some(d) if d > 0 => &name[..d],
        _ => name,
    };
    stem.chars().take(MAX_TITLE_LEN - 1).collect()
}

/// True when running on PocketBook hardware (the ext1 mount exists).
/// Platform seam for the path defaults: device builds keep the firmware
/// layout, PC hosts (SDL / linuxfb desktop) get useful $HOME-based ones.
fn on_device() -> bool {
    Path::new(DEVICE_BROWSE_ROOT).is_dir()
}

/// Fallback storage root on PC hosts.
fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
}

/// The storage root for this run (env override first — the SDL/host test
/// path), then the device mount on hardware, else the PC home directory
/// (browsing /mnt/ext1 on a desktop can never list anything).
pub fn browse_root() -> String {
    match std::env::var("EH_BROWSE_ROOT") {
        Ok(d) if !d.is_empty() => d,
        _ if on_device() => DEVICE_BROWSE_ROOT.to_string(),
        _ => home_dir(),
    }
}

/// The default downloads directory per platform (C eh_plat_downloads_dir):
/// the device's ext1 Downloads mount on hardware, $HOME/Downloads on PC
/// hosts.  App::new resolves + creates it and falls back to /tmp when
/// unwritable, so this stays a pure default.
pub fn default_downloads_dir() -> String {
    if on_device() {
        format!("{DEVICE_BROWSE_ROOT}/Downloads")
    } else {
        format!("{}/Downloads", home_dir())
    }
}

// ── scanner (C eh_local.c) ───────────────────────────────────────────────

/// One collected file record — the lean subset of Book the walk fills
/// without the metadata cache (author/title come from extraction during
/// the apply).
#[derive(Debug, Clone)]
pub struct LocalFile {
    pub id: String,
    pub title: String,
    pub filename: String,
    pub local_path: String,
    pub ext: String,
    pub size: i64,
}

impl LocalFile {
    /// The full Book record the apply writes (C local_file_to_book's leaf
    /// fields: downloaded=1 — the files ARE the books; added_at stays 0
    /// like the C memset).
    pub fn to_book(&self) -> Book {
        Book {
            id: self.id.clone(),
            title: self.title.clone(),
            ext: self.ext.clone(),
            size: self.size,
            downloaded: true,
            local_path: self.local_path.clone(),
            filename: self.filename.clone(),
            source: "local".into(),
            ..Default::default()
        }
    }
}

/// Walk `root` collecting every book file under the scan caps.  Hidden
/// entries (leading '.') are skipped at every level; symlinks resolve to
/// their real type so FAT/FUSE filesystems behave like the C stat fallback.
pub fn scan(root: &str) -> Vec<LocalFile> {
    scan_limited(root, SCAN_CAP)
}

/// `scan` with an explicit cap (the unit tests shrink it).
fn scan_limited(root: &str, cap: usize) -> Vec<LocalFile> {
    let mut out = Vec::new();
    let mut truncated = false;
    collect(Path::new(root), 0, cap, &mut out, &mut truncated);
    if truncated {
        crate::log(&format!(
            "[eh_app] local: scan cap {cap} reached, import truncated"
        ));
    }
    out
}

fn collect(dir: &Path, depth: u32, cap: usize, out: &mut Vec<LocalFile>, truncated: &mut bool) {
    if depth > SCAN_DEPTH {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if out.len() >= cap {
            *truncated = true;
            break;
        }
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        // d_type is only a hint: resolve symlinks (and DT_UNKNOWN-style
        // filesystems) through metadata like C local_scan_classify.
        let (is_dir, is_reg) = if ft.is_symlink() || (!ft.is_dir() && !ft.is_file()) {
            match std::fs::metadata(&path) {
                Ok(m) => (m.is_dir(), m.is_file()),
                Err(_) => (false, false),
            }
        } else {
            (ft.is_dir(), ft.is_file())
        };
        if is_dir {
            collect(&path, depth + 1, cap, out, truncated);
            continue;
        }
        if !is_reg {
            continue;
        }
        let Some(ext) = ext_of(&name) else { continue };
        if !is_book_ext(&ext) {
            continue;
        }
        let size = e.metadata().map(|m| m.len() as i64).unwrap_or(0);
        let path_str = path.to_string_lossy().into_owned();
        out.push(LocalFile {
            id: format!("fld_{}", hash_hex(&path_str)),
            title: stem_title(&name),
            filename: name.into_owned(),
            local_path: path_str,
            ext,
            size,
        });
    }
}

// ── async import chain (C worker walk → main-thread apply) ──────────────

/// One scanned book plus its freshly extracted metadata; the apply step
/// prefers the store's local_meta cache over re-extraction.
#[derive(Debug, Clone)]
pub struct LocalBook {
    pub book: Book,
    pub meta: ExtractedMeta,
}

/// The in-flight Local import scan job (the C g_local_scan* globals):
/// the worker → main-thread receiver plus the chain generation — a new
/// kick or a source switch bumps the generation, and a landed result
/// whose generation no longer matches is discarded as stale.
#[derive(Default)]
pub(crate) struct ScanJob {
    /// Scan results arrive here once the worker finishes.
    pub rx: Option<std::sync::mpsc::Receiver<(u32, Vec<LocalBook>)>>,
    /// Bumped on every kick/cancel; pollers compare before applying.
    pub gen: u32,
}

/// Kick the Local-source import (C eh_local_import_scanner): bump the
/// generation, spawn the scan thread, remember its receiver.  Safe to call
/// from the boot path and on every Local selection — a new kick invalidates
/// any in-flight result.
pub fn kick_import<B: Framebuffer>(app: &mut App<B>) {
    app.scan_job.gen += 1;
    let gen = app.scan_job.gen;
    let root = browse_root();
    crate::logger::log("[bookshelf] local: import scan started");
    let (tx, rx) = std::sync::mpsc::channel();
    app.scan_job.rx = Some(rx);
    app.syncing = true;
    let _ = std::thread::Builder::new()
        .name("local-scan".into())
        .spawn(move || {
            let files = scan(&root);
            let books: Vec<LocalBook> = files
                .iter()
                .map(|f| LocalBook {
                    book: f.to_book(),
                    meta: extract_book_meta(Path::new(&f.local_path), &f.ext),
                })
                .collect();
            crate::log(&format!(
                "[eh_app] local: scanned {} books under {root}",
                books.len()
            ));
            let _ = tx.send((gen, books));
        });
}

/// Drop an in-flight local import scan: bump the generation so a landed
/// result is discarded as stale by [`poll_import`] (the C scanner's gen
/// guard; a source switch must not apply a scan under the new source).
pub fn cancel_scan<B: Framebuffer>(app: &mut App<B>) {
    app.scan_job.gen += 1;
    app.scan_job.rx = None;
}

/// Drain a finished local scan into the store (C local_apply_slice's tail):
/// replace the whole 'local' source with the fresh results, cache unknown
/// metadata, then rebuild the view.  Stale generations drop their result.
pub fn poll_import<B: Framebuffer>(app: &mut App<B>) {
    let Some(rx) = &app.scan_job.rx else { return };
    let Ok((gen, books)) = rx.try_recv() else {
        return;
    };
    app.scan_job.rx = None;
    if gen != app.scan_job.gen {
        return; // stale chain (source switch / settings change): drop
    }
    app.syncing = false;
    let applied = (|| -> rusqlite::Result<()> {
        app.store.begin()?;
        app.store.delete_source("local")?;
        for lb in &books {
            let mut b = lb.book.clone();
            match app.store.local_meta_get(&b.id) {
                Some((t, a)) => {
                    if !t.is_empty() {
                        b.title = t;
                    }
                    if !a.is_empty() {
                        b.author = a;
                    }
                }
                None => {
                    if !lb.meta.title.is_empty() {
                        b.title = lb.meta.title.clone();
                    }
                    if !lb.meta.author.is_empty() {
                        b.author = lb.meta.author.clone();
                    }
                    app.store
                        .local_meta_put(&b.id, &lb.meta.title, &lb.meta.author)?;
                }
            }
            app.store.upsert_book_row(&b)?;
        }
        app.store.commit()
    })();
    match applied {
        Ok(()) => {
            crate::logger::log(&format!(
                "[bookshelf] local: imported {} books (local) from {}",
                books.len(),
                browse_root()
            ));
            app.rebuild_view();
            app.refresh_shelf();
        }
        Err(e) => {
            let _ = app.store.rollback();
            crate::log(&format!("[eh_app] local: import aborted: {e}"));
            app.syncing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::extract_book_cover;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn scan_filters_extensions_and_skips_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "a.epub");
        touch(root, "b.EPUB"); // case-insensitive extension
        touch(root, "c.xyz"); // not a book extension
        touch(root, "d"); // no extension
        touch(root, ".hidden.epub"); // hidden file
        std::fs::create_dir_all(root.join(".h")).unwrap(); // hidden dir
        touch(&root.join(".h"), "nested.epub");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        touch(&root.join("sub"), "s.fb2");

        let mut found = scan_limited(root.to_str().unwrap(), 100);
        found.sort_by(|a, b| a.filename.cmp(&b.filename));
        let names: Vec<&str> = found.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(names, ["a.epub", "b.EPUB", "s.fb2"]);
        let a = found.iter().find(|f| f.filename == "a.epub").unwrap();
        assert_eq!(a.title, "a");
        assert_eq!(a.ext, "epub");
        assert!(a.id.starts_with("fld_"));
    }

    #[test]
    fn scan_stops_at_count_cap() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            touch(dir.path(), &format!("book{i}.epub"));
        }
        let found = scan_limited(dir.path().to_str().unwrap(), 5);
        assert_eq!(found.len(), 5);
    }

    /// A minimal but structurally valid epub: zip container, container.xml
    /// pointing at an OPF with dc:title/dc:creator and a cover manifest.
    fn write_epub(path: &Path) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.start_file("mimetype", SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"application/epub+zip").unwrap();
        z.start_file("META-INF/container.xml", SimpleFileOptions::default())
            .unwrap();
        z.write_all(
            br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles>
</container>"#,
        )
        .unwrap();
        z.start_file("OEBPS/content.opf", SimpleFileOptions::default())
            .unwrap();
        z.write_all(
            br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <metadata>
    <dc:title>The Rust Book</dc:title>
    <dc:creator>A. Coder</dc:creator>
    <meta name="cover" content="cover-img"/>
  </metadata>
  <manifest><item id="cover-img" href="cover.png" media-type="image/png"/></manifest>
</package>"#,
        )
        .unwrap();
        z.finish().unwrap();
    }

    /// A 4x4 white Grayscale8 PNG (built with the same encoder the txt
    /// cover uses, so no hand-rolled bytes to rot).
    fn tiny_png() -> Vec<u8> {
        let px = vec![0xFFu8; 16];
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut enc = png::Encoder::new(&mut out, 4, 4);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&px).unwrap();
        }
        out.into_inner()
    }

    #[test]
    fn epub_cover_extraction_reads_the_named_member() {
        // The OPF names OEBPS/cover.png via meta[name=cover]; the href is
        // OPF-dir relative, so resolution must find it inside the zip.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.epub");
        {
            use std::io::Write as _;
            use zip::write::SimpleFileOptions;
            let f = std::fs::File::create(&path).unwrap();
            let mut z = zip::ZipWriter::new(f);
            z.start_file("META-INF/container.xml", SimpleFileOptions::default())
                .unwrap();
            z.write_all(br#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#).unwrap();
            z.start_file("OEBPS/content.opf", SimpleFileOptions::default())
                .unwrap();
            z.write_all(br#"<package xmlns="http://www.idpf.org/2007/opf"><metadata><meta name="cover" content="cover-img"/></metadata><manifest><item id="cover-img" href="cover.png"/></manifest></package>"#).unwrap();
            z.start_file("OEBPS/cover.png", SimpleFileOptions::default())
                .unwrap();
            z.write_all(&tiny_png()).unwrap();
            z.finish().unwrap();
        }
        let bytes = extract_book_cover(&path, "epub").expect("epub cover extracted");
        assert!(bytes.starts_with(b"\x89PNG"));
        assert!(
            crate::cover::decode_rgb(&bytes).is_ok(),
            "extracted cover must decode"
        );
    }

    #[test]
    fn broken_epub_yields_no_meta_and_filename_fallback_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("My Great Novel.epub");
        std::fs::write(&path, b"this is not a zip file").unwrap();
        // Metadata extraction fails cleanly -> poll_import keeps the
        // to_book() title, i.e. the filename WITHOUT the extension.
        assert!(extract_book_meta(&path, "epub").is_empty());
        assert_eq!(
            stem_title(path.file_name().unwrap().to_str().unwrap()),
            "My Great Novel"
        );
    }

    #[test]
    fn txt_cover_typesets_the_opening_words() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(
            &path,
            "The Hobbit\n\nIn a hole in the ground there lived a hobbit...\n",
        )
        .unwrap();
        let bytes = extract_book_cover(&path, "txt").expect("txt cover generated");
        assert!(bytes.starts_with(b"\x89PNG"));
        let decoded = crate::cover::decode_rgb(&bytes).unwrap();
        // Mostly white sheet with SOME dark text pixels.
        let dark = decoded.2.iter().filter(|&&v| v < 100).count();
        assert!(dark > 20, "typeset words missing, dark={dark}");
        assert!(dark < decoded.2.len() / 2, "sheet should stay mostly white");
        // A blank text file has nothing to catch: placeholder instead.
        let empty = dir.path().join("empty.txt");
        std::fs::write(&empty, b"   \n\t\n").unwrap();
        assert!(extract_book_cover(&empty, "txt").is_none());
    }

    #[test]
    fn pdf_first_page_renders_via_bundled_mupdf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.pdf");
        // Minimal single-page PDF (no metadata at all — the point of the
        // first-page fallback).
        std::fs::write(
            &path,
            concat!(
                "%PDF-1.4\n",
                "1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n",
                "2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n",
                "3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 280] >> endobj\n",
                "trailer << /Root 1 0 R /Size 4 >>\n",
                "%%EOF\n"
            ),
        )
        .unwrap();
        // Metadata: none -> title falls back to the filename stem.
        assert!(extract_book_meta(&path, "pdf").is_empty());
        let bytes = extract_book_cover(&path, "pdf").expect("mupdf must render page 1");
        assert!(bytes.starts_with(b"\x89PNG"));
        let (w, h, rgb) = crate::cover::decode_rgb(&bytes).unwrap();
        assert!((w, h) == (300, 420), "fit-to-card render, got {w}x{h}");
        // A blank white page: samples stay bright.
        let dark = rgb.iter().filter(|&&v| v < 100).count();
        assert_eq!(dark, 0, "blank page should have no dark pixels");
    }

    #[test]
    fn epub_title_author_cover_hint() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("book.epub");
        write_epub(&p);
        let m = extract_book_meta(&p, "epub");
        assert_eq!(m.title, "The Rust Book");
        assert_eq!(m.author, "A. Coder");
        assert_eq!(m.cover_hint.as_deref(), Some("cover.png"));
    }

    #[test]
    fn fb2_title_author() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("book.fb2");
        std::fs::write(
            &p,
            br#"<FictionBook>
  <description>
    <title-info>
      <author><first-name>Ivan</first-name><middle-name>P.</middle-name><last-name>Petrov</last-name></author>
      <book-title>War and Peace</book-title>
    </title-info>
  </description>
  <body/>
</FictionBook>"#,
        )
        .unwrap();
        let m = extract_book_meta(&p, "fb2");
        assert_eq!(m.title, "War and Peace");
        assert_eq!(m.author, "Ivan P. Petrov");
    }

    #[test]
    fn pdf_info_dict_strings() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("doc.pdf");
        // Literal string title + UTF-16BE hex author (FEFF BOM + "деж").
        let mut raw = b"%PDF-1.4\n1 0 obj\n<< /Title (My \\(Great\\) Doc) /Author <FEFF043404350436>>\nendobj\n".to_vec();
        raw.extend_from_slice(b"trailer\n<< /Root 1 0 R >>\n%%EOF\n");
        std::fs::write(&p, &raw).unwrap();
        let m = extract_book_meta(&p, "pdf");
        assert_eq!(m.title, "My (Great) Doc");
        assert_eq!(m.author, "деж");
    }

    #[test]
    fn unknown_ext_yields_empty_meta() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.txt");
        std::fs::write(&p, b"hello").unwrap();
        let m = extract_book_meta(&p, "txt");
        assert!(m.title.is_empty());
        assert!(m.author.is_empty());
    }
    #[test]
    fn hash_is_djb2_8hex() {
        // djb2("a") = 5381*33 + 97 = 177670 → 0002b606
        assert_eq!(hash_hex("a"), "0002b606");
        assert_eq!(hash_hex("").len(), 8);
    }
}
