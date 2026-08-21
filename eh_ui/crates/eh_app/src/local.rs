//! Local + Folder book sources (C eh_local.c / eh_browser.c BR_MODE_BROWSER).
//!
//! Two filesystem-backed sources sit next to the remote Kavita library:
//!
//! * **Local** — a background walk of the storage root collects every book
//!   file into the store (`source='local'`, `downloaded=1` — the files ARE
//!   the books).  The walk runs on a plain [`std::thread`] and hands its
//!   result back over a channel; the apply (SQLite) happens on the UI
//!   thread, mirroring C's worker-walk → main-thread apply chain.
//! * **Folder** — the shelf body becomes a live directory browser
//!   (C `BR_MODE_BROWSER`): directory rows, `..` ascent, page keys, and a
//!   tap on a book file opens it through the same reader flow the Kavita
//!   library uses.
//!
//! Book-file metadata (title/author) is extracted in pure Rust from epub
//! (zip container + OPF), fb2 and PDF files, cached in the store's
//! `local_meta` table keyed by the stable `fld_<djb2>` id so a re-import
//! never re-parses a known book.

use std::io::Read;
use std::path::Path;

use eh_hal::{Framebuffer, Rect};
use eh_layout::taffy::{self, Dimension, Style};
use eh_shell::{DrawCtx, GRAY_LGRAY, GRAY_WHITE, Screen, Widget};

use crate::app::{App, Source, ViewMode};
use crate::appui::{TopBar, TopBarState, TOP_BAR_H, TOP_BAR_PAD};
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

/// Title-cap for filename-derived titles (C EH_MAX_TITLE_LEN).
const MAX_TITLE_LEN: usize = 96;

/// Extensions the shelf treats as book files (C BOOK_EXTS in
/// eh_browser.c; shared by the Local import and the Folder browser).
pub const BOOK_EXTS: [&str; 10] =
    ["epub", "pdf", "mobi", "azw", "azw3", "fb2", "djvu", "txt", "cbr", "cbz"];

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

/// The storage root for this run (env override first — the SDL/host test
/// path — else the device mount).
pub fn browse_root() -> String {
    std::env::var("EH_BROWSE_ROOT").unwrap_or_else(|_| DEVICE_BROWSE_ROOT.to_string())
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
        crate::log(&format!("[eh_app] local: scan cap {cap} reached, import truncated"));
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

// ── metadata extraction (C eh_extract.h surface, pure Rust) ─────────────

/// Title/author pulled out of a book file.  `cover_hint` is the zip member
/// path of an epub cover image when the OPF names one (consumers that do
/// not render local covers simply ignore it).
#[derive(Debug, Clone, Default)]
pub struct ExtractedMeta {
    pub title: String,
    pub author: String,
    pub cover_hint: Option<String>,
}

impl ExtractedMeta {
    fn is_empty(&self) -> bool {
        self.title.is_empty() && self.author.is_empty()
    }
}

/// Extract title/author from a book file (best-effort — any parse failure
/// returns empty fields and the caller keeps the filename-derived title).
pub fn extract_book_meta(path: &Path, ext: &str) -> ExtractedMeta {
    let r = match ext {
        "epub" => extract_epub(path).unwrap_or_default(),
        "fb2" => extract_fb2(path).unwrap_or_default(),
        "pdf" => extract_pdf(path).unwrap_or_default(),
        _ => ExtractedMeta::default(),
    };
    if r.is_empty() {
        crate::log(&format!("[eh_app] extract: no metadata in {}", path.display()));
    }
    r
}

/// The local-name (suffix after ':') of a possibly-qualified XML tag.
fn local_name(q: &[u8]) -> String {
    match q.iter().rposition(|&b| b == b':') {
        Some(i) => String::from_utf8_lossy(&q[i + 1..]).into_owned(),
        None => String::from_utf8_lossy(q).into_owned(),
    }
}

/// One field grabber: captures the folded text of the first matching
/// element, honouring nesting so an inner close does not end the capture.
struct Grab<'a> {
    names: &'a [&'a str],
    out: String,
    done: bool,
    /// Depth of nested open elements inside a captured one.
    depth: u32,
    capturing: bool,
}

impl<'a> Grab<'a> {
    fn new(names: &'a [&'a str]) -> Grab<'a> {
        Grab { names, out: String::new(), done: false, depth: 0, capturing: false }
    }

    fn start(&mut self, local: &str) {
        if self.done {
            return;
        }
        if self.capturing {
            self.depth += 1;
        } else if self.names.contains(&local) {
            self.capturing = true;
            self.depth = 0;
            self.out.clear();
        }
    }

    /// Element close: pops a nested level, ends the grab at the outermost.
    fn end(&mut self) {
        if !self.capturing || self.done {
            return;
        }
        if self.depth > 0 {
            self.depth -= 1;
            return;
        }
        self.capturing = false;
        if !self.out.is_empty() {
            self.done = true;
        }
    }

    fn text(&mut self, t: &str, limit: usize) {
        if !self.capturing || self.done {
            return;
        }
        let t = t.trim();
        if t.is_empty() {
            return;
        }
        if !self.out.is_empty() {
            self.out.push(' ');
        }
        self.out.push_str(t);
        self.out.truncate(limit);
    }
}

/// Pull the text of the first element named by `names` (local names,
/// namespace-prefix agnostic — `dc:title` matches "title") and the first
/// named by `authors`, each capped at `limit` bytes.
fn grab_two_fields(xml: &[u8], titles: &[&str], authors: &[&str], limit: usize) -> (String, String) {
    use quick_xml::events::Event;
    let mut rd = quick_xml::Reader::from_reader(xml);
    rd.config_mut().trim_text(true);
    let mut gt = Grab::new(titles);
    let mut ga = Grab::new(authors);
    loop {
        match rd.read_event_into(&mut Vec::new()) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local = local_name(e.name().as_ref());
                gt.start(&local);
                ga.start(&local);
            }
            Ok(Event::End(_)) => {
                gt.end();
                ga.end();
            }
            Ok(Event::Text(t)) => {
                let txt = t.unescape().unwrap_or_default();
                gt.text(&txt, limit);
                ga.text(&txt, limit);
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            // Decl/DocType/PI/Comment/CData carry no captured text.
            _ => {}
        }
    }
    (gt.out, ga.out)
}

/// Read a whole file, capped (metadata lives in headers; a huge PDF is
/// truncated to its first megabyte for the raw scan).
fn read_capped(path: &Path, cap: usize) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.by_ref().take(cap as u64).read_to_end(&mut buf);
    }
    buf
}

/// epub: zip container → META-INF/container.xml → rootfile OPF → first
/// dc:title / dc:creator.  The cover hint follows `meta[name=cover]` /
/// `properties=cover-image` when the OPF names one.
fn extract_epub(path: &Path) -> Option<ExtractedMeta> {
    let f = std::fs::File::open(path).ok()?;
    let mut ar = zip::ZipArchive::new(f).ok()?;

    // Locate the OPF via the container.
    let mut c = String::new();
    use std::io::Read as _;
    ar.by_name("META-INF/container.xml").ok()?.read_to_string(&mut c).ok()?;
    let opf_path = rootfile_path(&c)?;
    let mut opf = String::new();
    ar.by_name(&opf_path).ok()?.read_to_string(&mut opf).ok()?;
    let (title, author) =
        grab_two_fields(opf.as_bytes(), &["title"], &["creator"], MAX_TITLE_LEN);
    Some(ExtractedMeta { title, author, cover_hint: cover_hint(&opf) })
}

/// The full-path attribute of the container's first <rootfile> element.
fn rootfile_path(container_xml: &str) -> Option<String> {
    use quick_xml::events::Event;
    let mut rd = quick_xml::Reader::from_str(container_xml);
    rd.config_mut().trim_text(true);
    loop {
        match rd.read_event_into(&mut Vec::new()) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if local_name(e.name().as_ref()) == "rootfile" {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == b"full-path" {
                            return String::from_utf8(a.value.into_owned()).ok();
                        }
                    }
                }
            }
            Ok(Event::Eof) => return None,
            Err(_) => return None,
            _ => {}
        }
    }
}

/// The OPF's cover-image manifest href: an item with
/// properties~="cover-image", or the item id referenced by
/// meta[name="cover"] content (the two conventions in the wild).
fn cover_hint(opf: &str) -> Option<String> {
    // Convention 1: properties="cover-image" on a manifest item.
    for seg in opf.split("<item") {
        if let Some(href) = attr_in(seg, "href") {
            if let Some(props) = attr_in(seg, "properties") {
                if props.split_whitespace().any(|p| p == "cover-image") {
                    return Some(href);
                }
            }
        }
    }
    // Convention 2: <meta name="cover" content="<item-id>"/> then that id.
    for seg in opf.split("<meta") {
        if attr_in(seg, "name").as_deref() == Some("cover") {
            if let Some(id) = attr_in(seg, "content") {
                for item in opf.split("<item") {
                    if attr_in(item, "id").as_deref() == Some(id.as_str()) {
                        if let Some(href) = attr_in(item, "href") {
                            return Some(href);
                        }
                    }
                }
            }
        }
    }
    None
}

/// The value of `key="…"` / `key='…'` inside one element-source fragment.
fn attr_in(fragment: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=");
    let i = fragment.find(&pat)?;
    let rest = &fragment[i + pat.len()..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let end = rest[1..].find(quote)? + 1;
    Some(rest[1..end].to_string())
}

/// fb2: FictionBook title-info — book-title plus the author's
/// first/middle/last names joined with spaces.
fn extract_fb2(path: &Path) -> Option<ExtractedMeta> {
    let xml = read_capped(path, 1 << 20);
    let (title, _) = grab_two_fields(&xml, &["book-title"], &[], MAX_TITLE_LEN);
    let author = fb2_author(&xml);
    Some(ExtractedMeta { title, author, cover_hint: None })
}

/// The first title-info author's name parts folded into one line.
fn fb2_author(xml: &[u8]) -> String {
    use quick_xml::events::Event;
    let mut rd = quick_xml::Reader::from_reader(xml);
    rd.config_mut().trim_text(true);
    let mut parts: [String; 3] = Default::default();
    let mut in_title_info = false;
    // Depth of the first captured <author> element (0 = none yet).
    let mut author_depth = 0u32;
    let mut elem_depth = 0u32;
    let mut cur: Option<usize> = None;
    loop {
        match rd.read_event_into(&mut Vec::new()) {
            Ok(Event::Start(e)) => {
                let local = local_name(e.name().as_ref());
                elem_depth += 1;
                if author_depth == 0 {
                    if local == "title-info" {
                        in_title_info = true;
                    } else if in_title_info && local == "author" {
                        author_depth = elem_depth;
                    }
                } else {
                    match local.as_str() {
                        "first-name" => cur = Some(0),
                        "middle-name" => cur = Some(1),
                        "last-name" => cur = Some(2),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(_)) => {
                if author_depth > 0 && elem_depth == author_depth {
                    break; // the first author element is complete
                }
                elem_depth = elem_depth.saturating_sub(1);
            }
            Ok(Event::Text(t)) => {
                if let Some(idx) = cur {
                    let txt = t.unescape().unwrap_or_default();
                    let slot = &mut parts[idx];
                    if !slot.is_empty() {
                        slot.push(' ');
                    }
                    slot.push_str(txt.trim());
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

/// pdf: best-effort raw scan of the file bytes for a `/Title` / `/Author`
/// entry in an Info dictionary — literal `(...)` strings with the standard
/// escapes and UTF-16BE `<FEFF…>` hex strings are decoded.  Anything else
/// yields empty fields (the caller keeps the filename-derived title).
fn extract_pdf(path: &Path) -> Option<ExtractedMeta> {
    let buf = read_capped(path, 8 << 20);
    let title = pdf_dict_string(&buf, b"/Title", MAX_TITLE_LEN);
    let author = pdf_dict_string(&buf, b"/Author", 80);
    Some(ExtractedMeta { title, author, cover_hint: None })
}

/// The string value following `key` in raw PDF bytes, or "".
fn pdf_dict_string(buf: &[u8], key: &[u8], limit: usize) -> String {
    let mut i = 0;
    while let Some(pos) = find_at(buf, key, i) {
        i = pos + key.len();
        // Skip whitespace/comments between the key and its value.
        let mut j = i;
        while j < buf.len() && (buf[j] as char).is_whitespace() {
            j += 1;
        }
        match buf.get(j) {
            Some(b'(') => {
                if let Some((s, _end)) = pdf_literal_string(&buf[j + 1..]) {
                    return s.chars().take(limit).collect();
                }
            }
            Some(b'<') => {
                if let Some((s, _end)) = pdf_hex_string(&buf[j + 1..]) {
                    return s.chars().take(limit).collect();
                }
            }
            _ => {}
        }
    }
    String::new()
}

fn find_at(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Decode a PDF literal string body (after the opening paren): returns
/// (decoded, offset past the closing paren).
fn pdf_literal_string(body: &[u8]) -> Option<(String, usize)> {
    let mut out: Vec<u8> = Vec::new();
    let mut depth = 1usize;
    let mut i = 0;
    while i < body.len() {
        let b = body[i];
        match b {
            b'\\' => {
                i += 1;
                if i >= body.len() {
                    break;
                }
                match body[i] {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0c),
                    b'(' => out.push(b'('),
                    b')' => out.push(b')'),
                    b'\\' => out.push(b'\\'),
                    b'0'..=b'7' => {
                        // Up to three octal digits.
                        let mut v: u32 = (body[i] - b'0') as u32;
                        let mut k = 0;
                        while k < 2
                            && i + 1 < body.len()
                            && (b'0'..=b'7').contains(&body[i + 1])
                        {
                            i += 1;
                            k += 1;
                            v = v * 8 + (body[i] - b'0') as u32;
                        }
                        out.push(v as u8);
                    }
                    b'\n' => {} // line continuation
                    _ => out.push(body[i]),
                }
                i += 1;
            }
            b'(' => {
                depth += 1;
                out.push(b);
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((pdf_decode_bytes(&out), i + 1));
                }
                out.push(b);
                i += 1;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    None
}

/// Decode a PDF hex string body (after the opening '<'): returns
/// (decoded, offset past the closing '>').
fn pdf_hex_string(body: &[u8]) -> Option<(String, usize)> {
    let end = body.iter().position(|&b| b == b'>')?;
    let hex: String = body[..end]
        .iter()
        .filter(|b| b.is_ascii_hexdigit())
        .map(|b| *b as char)
        .collect();
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let cs: Vec<char> = hex.chars().collect();
    for pair in cs.chunks(2) {
        let hi = pair[0].to_digit(16).unwrap_or(0) as u8;
        let lo = pair.get(1).and_then(|c| c.to_digit(16)).unwrap_or(0) as u8;
        bytes.push(hi << 4 | lo);
    }
    Some((pdf_decode_bytes(&bytes), end + 1))
}

/// PDF text decoding: UTF-16BE when the BOM is present, else treat the
/// bytes as Latin-1-ish (PDFDocEncoding ≈ Latin-1 for our purposes).
fn pdf_decode_bytes(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xfe && bytes[1] == 0xff {
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
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

/// Kick the Local-source import (C eh_local_import_scanner): bump the
/// generation, spawn the scan thread, remember its receiver.  Safe to call
/// from the boot path and on every Local selection — a new kick invalidates
/// any in-flight result.
pub fn kick_import<B: Framebuffer>(app: &mut App<B>) {
    app.local_gen += 1;
    let gen = app.local_gen;
    let root = browse_root();
    crate::logger::log("[bookshelf] local: import scan started");
    let (tx, rx) = std::sync::mpsc::channel();
    app.local_scan = Some(rx);
    app.syncing = true;
    let _ = std::thread::Builder::new().name("local-scan".into()).spawn(move || {
        let files = scan(&root);
        let books: Vec<LocalBook> = files
            .iter()
            .map(|f| LocalBook { book: f.to_book(), meta: extract_book_meta(Path::new(&f.local_path), &f.ext) })
            .collect();
        crate::log(&format!("[eh_app] local: scanned {} books under {root}", books.len()));
        let _ = tx.send((gen, books));
    });
}

/// Drain a finished local scan into the store (C local_apply_slice's tail):
/// replace the whole 'local' source with the fresh results, cache unknown
/// metadata, then rebuild the view.  Stale generations drop their result.
pub fn poll_import<B: Framebuffer>(app: &mut App<B>) {
    let Some(rx) = &app.local_scan else { return };
    let Ok((gen, books)) = rx.try_recv() else { return };
    app.local_scan = None;
    if gen != app.local_gen {
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
                    app.store.local_meta_put(&b.id, &lb.meta.title, &lb.meta.author)?;
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

// ── folder browser (C eh_browser.c BR_MODE_BROWSER) ─────────────────────

/// One listed entry: `..` (when below the root), then subdirectories,
/// then book files — each group alphabetical (C browser_load's sort).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseEntry {
    pub name: String,
    pub is_dir: bool,
}

/// The browser state (C eh_g_browse_*).  `root` pins the storage root so
/// ascent stops there and display paths stay relative to it.
#[derive(Debug, Default)]
pub struct Browser {
    pub root: String,
    pub path: String,
    pub scroll: usize,
    pub entries: Vec<BrowseEntry>,
    pub open: bool,
}

impl Browser {
    /// Start browsing `dir` (C eh_browse_start).
    pub fn start(&mut self, dir: &str) {
        self.root = dir.to_string();
        self.path = dir.to_string();
        self.scroll = 0;
        self.load();
        self.open = true;
    }

    /// No ascent above the storage root (C browser_can_go_up).
    pub fn can_go_up(&self) -> bool {
        self.path != self.root
    }

    /// Refill `entries` from the current directory.
    pub fn load(&mut self) {
        self.entries.clear();
        if self.can_go_up() {
            self.entries.push(BrowseEntry { name: "..".into(), is_dir: true });
        }
        let Ok(rd) = std::fs::read_dir(&self.path) else {
            crate::log(&format!("[eh_app] browser: opendir {} failed", self.path));
            return;
        };
        let mut dirs: Vec<String> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        for e in rd.flatten() {
            if dirs.len() + files.len() >= BROWSE_MAX_ENTRIES {
                break;
            }
            let name = e.file_name();
            let name = name.to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            let (is_dir, is_reg) = if ft.is_symlink() {
                match std::fs::metadata(&path) {
                    Ok(m) => (m.is_dir(), m.is_file()),
                    Err(_) => (false, false),
                }
            } else {
                (ft.is_dir(), ft.is_file())
            };
            if is_dir {
                dirs.push(name);
            } else if is_reg && ext_of(&name).is_some_and(|x| is_book_ext(&x)) {
                files.push(name);
            }
        }
        dirs.sort();
        files.sort();
        self.entries
            .extend(dirs.into_iter().map(|name| BrowseEntry { name, is_dir: true }));
        self.entries
            .extend(files.into_iter().map(|name| BrowseEntry { name, is_dir: false }));
        crate::logger::log(&format!(
            "[bookshelf] browser: {} -> {} entries",
            self.path,
            self.entries.len()
        ));
    }

    /// Visible row count: the body runs from below the top bar to the
    /// content bottom (C browser_rows_visible, browser branch).
    pub fn rows_visible(content_bottom: u32) -> usize {
        let avail = (content_bottom as i32 - (TOP_BAR_H + TOP_BAR_PAD) as i32 - 8).max(1);
        (avail as u32 / FOLDER_ROW_H).max(1) as usize
    }

    /// Descend into a listed subdirectory (or ascend via `..`)
    /// (C browser_navigate).
    pub fn navigate(&mut self, name: &str) {
        if name == ".." {
            self.path = match std::path::Path::new(&self.path).parent() {
                Some(p) if self.path != self.root => p.to_string_lossy().into_owned(),
                _ => self.root.clone(),
            };
        } else {
            let next = format!("{}/{}", self.path, name);
            self.path = next;
        }
        self.scroll = 0;
        self.load();
    }

    /// Ascend one level; false when already at the browser root — the
    /// caller then decides what Back means (C eh_browse_up).
    pub fn up(&mut self) -> bool {
        if !self.can_go_up() {
            return false;
        }
        if let Some(p) = std::path::Path::new(&self.path).parent() {
            self.path = p.to_string_lossy().into_owned();
        }
        self.scroll = 0;
        self.load();
        true
    }

    /// Page the list one screen (C eh_browse_page); dir > 0 = forward.
    /// The draw path clamps, so the raw arithmetic mirrors the C app.
    pub fn page(&mut self, dir: i32, content_bottom: u32) {
        let rows = Self::rows_visible(content_bottom) as i32;
        let max = self.entries.len().saturating_sub(rows as usize) as i32;
        self.scroll = (self.scroll as i32 + dir * rows).clamp(0, max) as usize;
    }

    /// Display form of an absolute path: everything under the storage root
    /// shows relative to it; the root itself shows as "/" (C
    /// eh_user_path_display).
    pub fn user_display(path: &str, root: &str) -> String {
        if let Some(rest) = path.strip_prefix(root) {
            if rest.is_empty() {
                return "/".into();
            }
            if let Some(stripped) = rest.strip_prefix('/') {
                return stripped.to_string();
            }
        }
        path.to_string()
    }
}

/// The Book a folder-browser tap opens (C browser_open_book): the file IS
/// the book — filename-derived title, `fld_` id from the same hash as the
/// Local import, downloaded=1, source `folder`.
pub fn folder_book(path: &str, name: &str) -> Book {
    Book {
        id: format!("fld_{}", hash_hex(path)),
        title: stem_title(name),
        ext: ext_of(name).unwrap_or_default(),
        downloaded: true,
        local_path: path.to_string(),
        filename: name.to_string(),
        source: "folder".into(),
        ..Default::default()
    }
}

// ── browser page (the Folder source's shelf body) ───────────────────────

/// One full-width directory row (C browser_draw_row): white fill, a
/// separator line, the name in the row font with a trailing "/" for dirs.
struct BrowseRow {
    name: Option<String>,
    is_dir: bool,
    rect: Option<Rect>,
}

impl BrowseRow {
    fn blank() -> Self {
        Self { name: None, is_dir: false, rect: None }
    }
}

impl Widget for BrowseRow {
    fn draw(&mut self, ctx: &mut DrawCtx, rect: Rect) {
        self.rect = Some(rect);
        ctx.fill(rect, GRAY_WHITE);
        let w = ctx.surf.width();
        ctx.hline(0, rect.y + rect.h - 1, w, 1, GRAY_LGRAY);
        let Some(name) = &self.name else { return };
        let label = if self.is_dir { format!("{name}/") } else { name.clone() };
        // Pixel-fit truncation to w - 64 (C eh_utf8_fit_width).
        let mut label = label;
        while label.len() > 1 && ctx.font.width(&label, 28.0) as i32 > w as i32 - 64 {
            label.pop();
        }
        let baseline = rect.y as i32 + (FOLDER_ROW_H as i32 - 28) / 2 + 20;
        ctx.text(32, baseline, 28.0, &label, eh_shell::GRAY_BLACK);
    }
    fn dirty(&self, out: &mut Vec<Rect>) {
        if let Some(r) = self.rect {
            out.push(r);
        }
    }
    fn hit(&self, x: i32, y: i32) -> bool {
        matches!(self.rect, Some(r) if r.contains(x, y))
    }
}

/// Build the browser screen: the top bar carries the current directory as
/// its title; the body lists the visible rows (C eh_draw_browse — body
/// only, the caller owns the chrome).
pub fn build_browse_page<B: Framebuffer>(fb: B, browser: &Browser, content_bottom: u32) -> Screen<B> {
    let font = crate::shelf::shelf_font();
    let mut screen = Screen::new(fb, font);
    screen.layout_mut().root_flex_column();
    let tb = TopBar::new(TopBarState {
        back: false,
        source: Source::Folder,
        view_mode: ViewMode::Grid,
        search: false,
        syncing: false,
        sync_angle: 0,
        title: Browser::user_display(&browser.path, &browser.root),
    });
    screen.add_styled(
        Box::new(tb),
        Style {
            flex_shrink: 0.0,
            size: taffy::geometry::Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(TOP_BAR_H as f32),
            },
            ..Style::default()
        },
    );
    let body = screen.add_container(Style { flex_grow: 1.0, ..Style::default() });
    let rows = Browser::rows_visible(content_bottom);
    for i in 0..rows {
        let idx = browser.scroll + i;
        let row = match browser.entries.get(idx) {
            Some(e) => BrowseRow { name: Some(e.name.clone()), is_dir: e.is_dir, rect: None },
            None => BrowseRow::blank(),
        };
        screen.add_to(
            body,
            Box::new(row),
            Style {
                flex_shrink: 0.0,
                size: taffy::geometry::Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(FOLDER_ROW_H as f32),
                },
                ..Style::default()
            },
        );
    }
    screen
}

/// Open the folder browser at the storage root (C source-tap →
/// eh_browse_start): the browser becomes the shelf body.
pub fn start_browse<B: Framebuffer>(app: &mut App<B>) {
    let root = browse_root();
    app.browser.start(&root);
    app.refresh_shelf();
}

/// Body tap in browser mode (C eh_on_tap_browse, below the top bar): a
/// directory row navigates, a book file opens through the reader flow.
pub fn tap_browse<B: Framebuffer>(app: &mut App<B>, x: i32, y: i32) {
    let _ = x; // rows span the full width; only y matters
    let top = TOP_BAR_H + TOP_BAR_PAD;
    if (y as u32) < top {
        return;
    }
    let idx = ((y as u32 - top - 8) / FOLDER_ROW_H) as usize + app.browser.scroll;
    let Some(entry) = app.browser.entries.get(idx).cloned() else { return };
    if entry.is_dir {
        app.browser.navigate(&entry.name);
        app.refresh_shelf();
    } else {
        let path = format!("{}/{}", app.browser.path, entry.name);
        let book = folder_book(&path, &entry.name);
        crate::logger::log(&format!("[bookshelf] browse: opening {path}"));
        app.open_reader(Path::new(&path), &book.title);
    }
}

/// Page key in browser mode: scroll the listing one screen and rebuild.
pub fn browse_page<B: Framebuffer>(app: &mut App<B>, dir: i32) {
    app.browser.page(dir, app.content_bottom);
    app.refresh_shelf();
}

/// Back key in browser mode: ascend one level; false at the root (the
/// caller falls through) (C eh_browse_up).
pub fn browse_up<B: Framebuffer>(app: &mut App<B>) -> bool {
    if !app.browser.up() {
        return false;
    }
    app.refresh_shelf();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

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
        z.start_file("mimetype", SimpleFileOptions::default()).unwrap();
        z.write_all(b"application/epub+zip").unwrap();
        z.start_file("META-INF/container.xml", SimpleFileOptions::default()).unwrap();
        z.write_all(
            br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles>
</container>"#,
        )
        .unwrap();
        z.start_file("OEBPS/content.opf", SimpleFileOptions::default()).unwrap();
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

    #[test]
    fn browser_navigation_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("beta")).unwrap();
        std::fs::create_dir_all(root.join("alpha")).unwrap();
        touch(root, "zbook.epub");
        touch(root, "abook.epub");
        std::fs::create_dir_all(root.join("beta").join("inner")).unwrap();

        let root_str = root.to_string_lossy().into_owned();
        let mut b = Browser::default();
        b.start(&root_str);
        assert!(b.open);
        // At the root there is no ".."; dirs first (alpha, beta), then files.
        assert!(!b.can_go_up());
        let names: Vec<&str> = b.entries.iter().map(|e| e.name.as_str()).collect();
        // Directories first (alpha, beta), then files (abook, zbook).
        assert_eq!(names, ["alpha", "beta", "abook.epub", "zbook.epub"]);

        // Descend: ".." leads the list.
        b.navigate("beta");
        assert_eq!(b.path, format!("{root_str}/beta"));
        assert_eq!(b.entries[0].name, "..");
        assert!(b.entries[0].is_dir);
        assert_eq!(b.entries[1].name, "inner");

        // Ascend back to the root; one more up stays at the root.
        assert!(b.up());
        assert_eq!(b.path, root_str);
        assert!(!b.up());

        // Paging clamps at both ends.
        b.page(5, 480); // forward past the end
        let maxed = b.scroll;
        assert!(maxed <= b.entries.len().saturating_sub(Browser::rows_visible(480)));
        b.page(-50, 480); // backward before the start
        assert_eq!(b.scroll, 0);

        // Display paths strip the root; the root itself shows as "/".
        assert_eq!(Browser::user_display(&root_str, &root_str), "/");
        assert_eq!(
            Browser::user_display(&format!("{root_str}/beta/inner"), &root_str),
            "beta/inner"
        );
        assert_eq!(Browser::user_display("/elsewhere", &root_str), "/elsewhere");
    }

    #[test]
    fn folder_book_derives_fld_id_like_local_scan() {
        let path = "/mnt/ext1/Books/x.epub";
        let b = folder_book(path, "x.epub");
        assert_eq!(b.id, format!("fld_{}", hash_hex(path)));
        assert_eq!(b.source, "folder");
        assert!(b.downloaded);
        assert_eq!(b.local_path, path);
        // Same id derivation as the Local import for identical paths.
        let f = LocalFile {
            id: format!("fld_{}", hash_hex(path)),
            title: String::new(),
            filename: "x.epub".into(),
            local_path: path.into(),
            ext: "epub".into(),
            size: 0,
        };
        assert_eq!(f.to_book().id, b.id);
        assert!(f.to_book().downloaded);
        assert_eq!(f.to_book().source, "local");
    }
}
