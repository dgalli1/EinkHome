//! Book-file metadata + cover extraction (C eh_extract.h surface, pure
//! Rust) across KOReader's DocumentRegistry formats:
//!
//! | format | cover source |
//! |---|---|
//! | EPUB | the OPF-named embedded image (two wild conventions resolved) |
//! | PDF | MuPDF first-page render (`pdf-mupdf` feature) |
//! | CBZ / CBT / ZIP | first archive image in natural name order |
//! | FB2 | the `<coverpage>`-named base64 `<binary>`, else the first image binary |
//! | MOBI / AZW / AZW3 | EXTH cover record, else the first image record (PalmDOC records decompressed) |
//! | PDB | first record with an image magic (PeanutPress covers) |
//! | RTF | the first `\pict` blip group |
//! | XPS / OXPS | the first fixed page's image resource |
//! | TXT | generated first-words cover |
//! | DjVu, CHM, DOC | not extracted — they need vendor codecs (djvulibre / chmlib / OLE+Word); tiles use the placeholder |
//! | HTML | no embedded art — placeholder, like KOReader's generated covers |
//!
//! Everything here is bytes-in/bytes-out with no App coupling, so both
//! the Local import and any other consumer can call it from any context.
//!
//! Split out of `local.rs`: every function here is pure — a path in,
//! bytes/metadata out — with no App coupling, so both the Local import
//! and any other consumer can call it from any context.

use std::io::{Read, Seek};
use std::path::Path;

/// Title-cap for filename-derived titles (C EH_MAX_TITLE_LEN).
pub(crate) const MAX_TITLE_LEN: usize = 96;

/// Title/author/series pulled out of a book file.  `cover_hint` is the
/// zip member path of an epub cover image when the OPF names one
/// (consumers that do not render local covers simply ignore it).  The
/// series fields mirror what Kavita reads from the OPF: the calibre
/// `calibre:series`/`calibre:series_index` meta pair, else the EPUB3
/// `belongs-to-collection` + `group-position` pair.
#[derive(Debug, Clone, Default)]
pub struct ExtractedMeta {
    pub title: String,
    pub author: String,
    pub series: String,
    pub series_index: Option<f64>,
    pub cover_hint: Option<String>,
}

impl ExtractedMeta {
    pub(crate) fn is_empty(&self) -> bool {
        self.title.is_empty() && self.author.is_empty()
    }
}
/// Extract title/author/series from a book file (best-effort — any parse
/// failure returns empty fields and the caller keeps the filename-derived
/// title).
pub fn extract_book_meta(path: &Path, ext: &str) -> ExtractedMeta {
    let r = match ext {
        "epub" => extract_epub(path).unwrap_or_default(),
        "fb2" => extract_fb2(path).unwrap_or_default(),
        "pdf" => extract_pdf(path).unwrap_or_default(),
        _ => ExtractedMeta::default(),
    };
    // Whitespace-only metadata counts as absent: the caller falls back to
    // the filename (without extension) instead of showing a blank title.
    let r = ExtractedMeta {
        title: r.title.trim().to_string(),
        author: r.author.trim().to_string(),
        series: r.series.trim().to_string(),
        series_index: r.series_index,
        cover_hint: r.cover_hint,
    };
    if r.is_empty() {
        crate::log(&format!(
            "[eh_app] extract: no metadata in {}",
            path.display()
        ));
    }
    r
}

/// Extract the cover image bytes of a local book (C eh_extract_book_
/// cover), forgiving on broken files:
///
/// * EPUB — the embedded cover image the OPF names (the two wild
///   conventions `extract_epub` already resolves).
/// * PDF — no embedded-image concept we can rely on: render the FIRST
///   PAGE instead (the "screenshot" fallback — even a metadata-less PDF
///   then shows its first page as the tile art).
/// * TXT — render the file's first few WORDS onto a generated cover;
///   plain-text exports usually open with the title, so it gets caught
///   by accident.
///
/// Returns raw PNG/JPEG bytes, decodable by [`crate::cover::decode_rgb`].
pub fn extract_book_cover(path: &Path, ext: &str) -> Option<Vec<u8>> {
    let bytes = match ext {
        "epub" => extract_epub_cover(path),
        "pdf" => pdf_first_page_png_or_none(path),
        "txt" => txt_word_cover(path),
        // Comic/text archives: the first image member in natural filename
        // order is the cover (KOReader picdocument lists archive images
        // the same way).
        "cbz" | "zip" => zip_image_cover(path),
        "cbt" => tar_image_cover(path),
        // FB2 carries its cover as a base64 <binary> named by <coverpage>.
        "fb2" => fb2_cover(path),
        // MOBI/AZW(+KF8): the EXTH cover record, else the first image
        // record.  PDB containers: first record that starts with an
        // image magic (PeanutPress covers).
        "mobi" | "azw" | "azw3" => mobi_cover(path),
        "pdb" => pdb_cover(path),
        // RTF: the first \pict blip group.
        "rtf" => rtf_cover(path),
        // XPS/OXPS: the first fixed page's image resource.
        "xps" | "oxps" => xps_cover(path),
        // The rest of KOReader's registry needs vendor codecs this port
        // does not carry: DjVu (IW44/JB2 page renders via djvulibre),
        // CHM (LZX-compressed streams via chmlib), DOC (OLE compound +
        // Word binary).  HTML/TXT carry no embedded art — TXT still gets
        // the generated word cover above; HTML tiles use the
        // placeholder, like KOReader's generated covers.
        _ => None,
    };
    if bytes.is_none() {
        crate::log(&format!("[eh_app] extract: no cover in {}", path.display()));
    }
    bytes
}

/// The EPUB's embedded cover image bytes: the OPF cover member, else a
/// zip member conventionally named cover.<img-ext>.
fn extract_epub_cover(path: &Path) -> Option<Vec<u8>> {
    let hint = extract_epub(path)?.cover_hint;
    let f = std::fs::File::open(path).ok()?;
    let mut ar = zip::ZipArchive::new(f).ok()?;
    let member = hint.or_else(|| find_cover_member(&mut ar))?;
    let name = resolve_member(&mut ar, &member)?;
    let mut out = Vec::new();
    use std::io::Read as _;
    ar.by_name(&name).ok()?.read_to_end(&mut out).ok()?;
    (!out.is_empty()).then_some(out)
}

/// A zip member whose base name looks like a cover image.
fn find_cover_member(ar: &mut zip::ZipArchive<std::fs::File>) -> Option<String> {
    (0..ar.len()).find_map(|i| {
        let n = ar.by_index(i).ok()?.name().to_string();
        let base = n.rsplit('/').next().unwrap_or(&n).to_ascii_lowercase();
        (base.starts_with("cover.") && img_ext(&base)).then_some(n)
    })
}

/// Match an OPF href against actual zip members: exact, `./`-stripped,
/// percent-decoded, then base-name only (hrefs are OPF-dir relative).
fn resolve_member(ar: &mut zip::ZipArchive<std::fs::File>, member: &str) -> Option<String> {
    for cand in [
        member.to_string(),
        member.trim_start_matches("./").to_string(),
        member.replace("%20", " "),
    ] {
        if ar.by_name(&cand).is_ok() {
            return Some(cand);
        }
        let want = cand
            .rsplit('/')
            .next()
            .unwrap_or(&cand)
            .to_ascii_lowercase();
        let hit = (0..ar.len()).find_map(|i| {
            let n = ar.by_index(i).ok()?.name().to_string();
            let base = n.rsplit('/').next().unwrap_or(&n).to_ascii_lowercase();
            (base == want).then_some(n)
        });
        if let Some(hit) = hit {
            return Some(hit);
        }
    }
    None
}

fn img_ext(base_lower: &str) -> bool {
    [".png", ".jpg", ".jpeg"]
        .iter()
        .any(|e| base_lower.ends_with(e))
}

/// Render the PDF's FIRST page to PNG with the statically linked MuPDF
/// (the `mupdf` crate vendors and cross-builds the C sources — the same
/// cargo-zigbuild path as libsqlite3-sys), so the fallback works on the
/// PocketBook itself: even a metadata-less PDF shows its first page as
/// the tile art.
/// Without the MuPDF feature the page-render cover fallback is
/// unavailable (Android): PDF metadata extraction stays pure-Rust.
#[cfg(not(feature = "pdf-mupdf"))]
fn pdf_first_page_png_or_none(_path: &Path) -> Option<Vec<u8>> {
    None
}

#[cfg(feature = "pdf-mupdf")]
fn pdf_first_page_png_or_none(path: &Path) -> Option<Vec<u8>> {
    pdf_first_page_png(path)
}

#[cfg(feature = "pdf-mupdf")]
fn pdf_first_page_png(path: &Path) -> Option<Vec<u8>> {
    let doc = mupdf::Document::open(path.to_str()?).ok()?;
    let page = doc.load_page(0).ok()?;
    let bounds = page.bounds().ok()?;
    let bw = (bounds.x1 - bounds.x0).max(1.0);
    let bh = (bounds.y1 - bounds.y0).max(1.0);
    // Fit inside the cover card, preserve aspect, keep it sane.
    let s = ((300.0 / bw).min(450.0 / bh)).clamp(0.05, 8.0);
    let ctm = mupdf::Matrix::new_scale(s, s);
    let pixmap = page
        .to_pixmap(&ctm, &mupdf::Colorspace::device_gray(), false, false)
        .ok()?;
    let mut png = Vec::new();
    pixmap.write_to(&mut png, mupdf::ImageFormat::PNG).ok()?;
    (!png.is_empty()).then_some(png)
}

/// Render the text file's opening words onto a generated cover (a blank
/// 2:3 sheet, the words set like a half-title page).  The first words of
/// a plain-text export are usually its title, so the tile reads as the
/// book rather than a placeholder.
fn txt_word_cover(path: &Path) -> Option<Vec<u8>> {
    let buf = read_capped(path, 2048);
    let text = String::from_utf8_lossy(&buf);
    let words: Vec<&str> = text
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .take(6)
        .collect();
    if words.is_empty() {
        return None; // empty file — nothing to catch
    }

    const W: u32 = 300;
    const H: u32 = 450; // 2:3, the C cover-card ratio
    const LINE_H: i32 = 34;
    let font = crate::shelf::shelf_font();
    let max_w = (W - 48) as f32;

    // Greedy wrap into at most 5 lines.
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for w in &words {
        let cand = if cur.is_empty() {
            (*w).into()
        } else {
            format!("{cur} {w}")
        };
        if font.width(&cand, 26.0) <= max_w || cur.is_empty() {
            cur = cand;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = (*w).into();
        }
    }
    if !cur.is_empty() && lines.len() < 5 {
        lines.push(cur);
    }

    let mut px = vec![0xFFu8; (W * H) as usize];
    {
        let mut surf =
            eh_render::Surface::new(&mut px, W, H, W as usize, eh_hal::PixelFormat::Grayscale8);
        let asc = font.line_h(26.0).0 as i32;
        let top = ((H as i32 - LINE_H * lines.len() as i32) / 2).max(24) + asc;
        let mut g = eh_render::Glyph::new();
        for (i, line) in lines.iter().enumerate() {
            let lw = font.width(line, 26.0) as i32;
            let lx = ((W as i32 - lw) / 2).max(24);
            eh_render::draw_text(
                &mut surf,
                font,
                26.0,
                line,
                lx,
                top + i as i32 * LINE_H,
                crate::appui::GRAY_BLACK,
                &mut g,
            );
        }
    }

    // Encode Grayscale8 PNG (decode_rgb re-expands to RGB).
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut enc = png::Encoder::new(&mut out, W, H);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().ok()?;
        wr.write_image_data(&px).ok()?;
    }
    Some(out.into_inner())
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
        Grab {
            names,
            out: String::new(),
            done: false,
            depth: 0,
            capturing: false,
        }
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
        // Byte-cap on a char boundary: String::truncate panics when the
        // cut splits a multibyte char, and titles are untrusted device
        // input (an EPUB title with one straddling byte 96 would crash
        // the import scan).
        if self.out.len() > limit {
            let mut end = limit;
            while !self.out.is_char_boundary(end) {
                end -= 1;
            }
            self.out.truncate(end);
        }
    }
}

/// Pull the text of the first element named by `names` (local names,
/// namespace-prefix agnostic — `dc:title` matches "title") and the first
/// named by `authors`, each capped at `limit` bytes.
fn grab_two_fields(
    xml: &[u8],
    titles: &[&str],
    authors: &[&str],
    limit: usize,
) -> (String, String) {
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
    ar.by_name("META-INF/container.xml")
        .ok()?
        .read_to_string(&mut c)
        .ok()?;
    let opf_path = rootfile_path(&c)?;
    let mut opf = String::new();
    ar.by_name(&opf_path).ok()?.read_to_string(&mut opf).ok()?;
    let (title, author) = grab_two_fields(opf.as_bytes(), &["title"], &["creator"], MAX_TITLE_LEN);
    let (series, series_index) = epub_series(&opf);
    Some(ExtractedMeta {
        title,
        author,
        series,
        series_index,
        cover_hint: cover_hint(&opf),
    })
}

/// The EPUB's series + position from the OPF metadata block, mirroring
/// Kavita's parsing: the calibre pair
/// `<meta name="calibre:series" content="…"/>` /
/// `<meta name="calibre:series_index" content="…"/>`, else the EPUB3
/// collection pair `<meta property="belongs-to-collection">…</meta>` /
/// `<meta property="group-position">…</meta>`.  Calibre wins when both
/// are present (it is the explicit, indexed form).  No series meta →
/// ("", None).
fn epub_series(opf: &str) -> (String, Option<f64>) {
    use quick_xml::events::Event;
    let mut rd = quick_xml::Reader::from_str(opf);
    rd.config_mut().trim_text(true);
    let mut calibre_series = String::new();
    let mut calibre_idx: Option<f64> = None;
    let mut collection = String::new();
    let mut group_pos: Option<f64> = None;
    // Text capture for the EPUB3 Start/End form: the collection name.
    let mut cap: Option<(&str, String)> = None;
    loop {
        match rd.read_event_into(&mut Vec::new()) {
            Ok(Event::Empty(e)) => {
                if local_name(e.name().as_ref()) != "meta" {
                    continue;
                }
                let mut name = None;
                let mut property = None;
                let mut content = None;
                for a in e.attributes().flatten() {
                    match a.key.as_ref() {
                        b"name" => name = String::from_utf8(a.value.into_owned()).ok(),
                        b"property" => property = String::from_utf8(a.value.into_owned()).ok(),
                        b"content" => content = String::from_utf8(a.value.into_owned()).ok(),
                        _ => {}
                    };
                }
                match (name.as_deref(), content.as_deref()) {
                    (Some("calibre:series"), Some(v)) => {
                        calibre_series = v.trim().to_string();
                    }
                    (Some("calibre:series_index"), Some(v)) => {
                        calibre_idx = v.trim().parse().ok();
                    }
                    _ => {}
                }
                // A self-closing belongs-to-collection carries no name.
                if property.as_deref() == Some("group-position") {
                    if let Some(v) = content.as_deref() {
                        group_pos = v.trim().parse().ok();
                    }
                }
            }
            Ok(Event::Start(e)) => {
                if local_name(e.name().as_ref()) != "meta" {
                    continue;
                }
                let property = e.attributes().flatten().find_map(|a| {
                    (a.key.as_ref() == b"property")
                        .then(|| String::from_utf8(a.value.into_owned()).ok())
                        .flatten()
                });
                match property.as_deref() {
                    Some("belongs-to-collection") if collection.is_empty() => {
                        cap = Some(("collection", String::new()));
                    }
                    Some("group-position") if group_pos.is_none() => {
                        cap = Some(("position", String::new()));
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if let Some((which, buf)) = cap.as_mut() {
                    if let Ok(txt) = t.unescape() {
                        buf.push_str(txt.trim());
                    }
                    let _ = which;
                }
            }
            Ok(Event::End(_)) => {
                if let Some((which, buf)) = cap.take() {
                    match which {
                        "collection" => collection = buf,
                        "position" => group_pos = buf.trim().parse().ok(),
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    if !calibre_series.is_empty() {
        calibre_series.truncate(MAX_TITLE_LEN - 1);
        (calibre_series, calibre_idx)
    } else if !collection.is_empty() {
        collection.truncate(MAX_TITLE_LEN - 1);
        (collection, group_pos)
    } else {
        (String::new(), None)
    }
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
    Some(ExtractedMeta {
        title,
        author,
        series: String::new(),
        series_index: None,
        cover_hint: None,
    })
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
    Some(ExtractedMeta {
        title,
        author,
        series: String::new(),
        series_index: None,
        cover_hint: None,
    })
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
                        while k < 2 && i + 1 < body.len() && (b'0'..=b'7').contains(&body[i + 1]) {
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

// ── KOReader-registry covers: CBZ/CBT/ZIP, FB2, MOBI/PDB, RTF, XPS ──────

/// True when bytes start with a PNG or JPEG magic — the two codecs
/// [`crate::cover::decode_rgb`] speaks, and the only members worth
/// returning from an archive scan.
fn is_png_or_jpeg(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG") || b.starts_with(b"\xff\xd8")
}

/// Natural-order key: digit runs compare numerically so `page2` sorts
/// before `page10` (KOReader picdocument sorts the archive's image list).
fn natural_key(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut key = String::with_capacity(lower.len() + 8);
    let mut digits = String::new();
    for c in lower.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if !digits.is_empty() {
                key.push_str(&format!("{digits:0>16}"));
                digits.clear();
            }
            key.push(c);
        }
    }
    if !digits.is_empty() {
        key.push_str(&format!("{digits:0>16}"));
    }
    key
}

/// CBZ/ZIP cover: the first image member in natural filename order whose
/// bytes carry a decodable magic.  Entries that fail the magic check are
/// skipped, not fatal (archives mix thumbnails and pages).
fn zip_image_cover(path: &Path) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let mut ar = zip::ZipArchive::new(f).ok()?;
    let mut names: Vec<String> = (0..ar.len())
        .filter_map(|i| {
            let n = ar.by_index(i).ok()?.name().to_string();
            let base = n.rsplit('/').next()?.to_ascii_lowercase();
            img_ext(&base).then_some(n)
        })
        .collect();
    names.sort_by_key(|n| natural_key(n));
    for n in names {
        let mut out = Vec::new();
        if ar.by_name(&n).ok()?.read_to_end(&mut out).is_ok()
            && is_png_or_jpeg(&out)
            && crate::cover::decode_rgb(&out).is_ok()
        {
            return Some(out);
        }
    }
    None
}

/// CBT cover: same rule over an uncompressed tar stream.  Handles ustar
/// and the GNU `L` long-name convention; sparse/pax entries are skipped
/// by their size fields like any other payload.
fn tar_image_cover(path: &Path) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut pending_name: Option<String> = None;
    loop {
        let mut header = [0u8; 512];
        if f.read_exact(&mut header).is_err() {
            return None; // clean EOF: no image member found
        }
        if header.iter().all(|&b| b == 0) {
            return None; // end-of-archive block
        }
        let name = pending_name.take().unwrap_or(tar_str(&header[0..100])?);
        let size = tar_size(&header[124..136])?;
        let typeflag = header[156];
        if typeflag == b'L' {
            // GNU long name: the next blocks carry the real name for the
            // entry that follows; the payload is padded like any other.
            let mut long = vec![0u8; size as usize];
            f.read_exact(&mut long).ok()?;
            pending_name = Some(
                String::from_utf8_lossy(&long)
                    .trim_end_matches('\0')
                    .to_string(),
            );
            // The payload was just consumed: only the padding remains.
            let pad = (512 - (size % 512)) % 512;
            f.seek(std::io::SeekFrom::Current(pad as i64)).ok()?;
            continue;
        }
        let base = name
            .rsplit('/')
            .next()
            .unwrap_or(&name)
            .to_ascii_lowercase();
        if (typeflag == b'0' || typeflag == 0) && img_ext(&base) {
            let mut out = vec![0u8; size as usize];
            if f.read_exact(&mut out).is_ok()
                && is_png_or_jpeg(&out)
                && crate::cover::decode_rgb(&out).is_ok()
            {
                return Some(out);
            }
        }
        // Payload is padded to a 512-byte boundary.
        let pad = (512 - (size % 512)) % 512;
        f.seek(std::io::SeekFrom::Current(size as i64 + pad as i64))
            .ok()?;
    }
}

fn tar_str(field: &[u8]) -> Option<String> {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    Some(String::from_utf8_lossy(&field[..end]).into_owned())
}

fn tar_size(octal: &[u8]) -> Option<u64> {
    let s = std::str::from_utf8(octal).ok()?;
    let s = s.trim_matches(|c: char| c == '\0' || c == ' ');
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

/// Minimal base64 (standard alphabet, whitespace and padding ignored) —
/// FB2 binaries are the only consumer, and a hand-rolled decoder keeps
/// the dependency surface at `zip` alone.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (i, &c) in T.iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut nbits = 0u32;
    for &b in s.as_bytes() {
        let v = table[b as usize];
        if v == 255 {
            continue; // whitespace / padding / junk
        }
        acc = (acc << 6) | u32::from(v);
        nbits += 6;
        if nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
        }
    }
    (!out.is_empty()).then_some(out)
}

/// FB2 cover: `<coverpage><image l:href="#id"/>` names a `<binary
/// id="id" content-type="image/…">` payload; without a coverpage the
/// first image-typed binary wins (the FB2 spec's own fallback order).
fn fb2_cover(path: &Path) -> Option<Vec<u8>> {
    let buf = read_capped(path, 64 << 20);

    // Optional <coverpage><image l:href="#id"/>: when present, only that
    // binary qualifies; when absent the first image-typed binary wins
    // (the FB2 spec's own fallback order).
    let cover_id: Option<Vec<u8>> = find_at(&buf, b"<coverpage", 0).and_then(|href_at| {
        let hash = find_at(&buf, b"#", href_at)?;
        let id_end = buf[hash..]
            .iter()
            .position(|&b| b == b'"' || b == b'>' || b == b' ' || b == b'?')?
            + hash;
        Some(buf[hash + 1..id_end].to_vec())
    });

    // <binary id="<id>" ...> … </binary>
    let mut from = 0;
    while let Some(b) = find_at(&buf, b"<binary", from) {
        let tag_end = find_at(&buf, b">", b)?;
        let body_start = tag_end + 1;
        let close = find_at(&buf, b"</binary>", body_start)?;
        let tag = &buf[b..tag_end];
        let id_at = find_at(tag, b"id", 0)
            .map(|p| find_at(tag, b"\"", p).unwrap_or(0))
            .map(|q| q + 1)?;
        let id_stop = tag[id_at..]
            .iter()
            .position(|&b| b == b'"')
            .map(|p| p + id_at)?;
        let id_match = cover_id
            .as_ref()
            .is_some_and(|id| &tag[id_at..id_stop] == id);
        let image_typed = find_at(tag, b"image/", 0).is_some();
        if (cover_id.is_some() && id_match) || (cover_id.is_none() && image_typed) {
            let b64 = std::str::from_utf8(&buf[body_start..close]).ok()?;
            let bytes = base64_decode(b64)?;
            return (is_png_or_jpeg(&bytes) && crate::cover::decode_rgb(&bytes).is_ok())
                .then_some(bytes);
        }
        from = close + 1;
    }

    // Fallback: the first image-typed binary.
    from = 0;
    while let Some(b) = find_at(&buf, b"<binary", from) {
        let tag_end = find_at(&buf, b">", b)?;
        let body_start = tag_end + 1;
        let close = find_at(&buf, b"</binary>", body_start)?;
        let tag = &buf[b..tag_end];
        if find_at(tag, b"image/", 0).is_some() {
            let b64 = std::str::from_utf8(&buf[body_start..close]).ok()?;
            let bytes = base64_decode(b64)?;
            return (is_png_or_jpeg(&bytes) && crate::cover::decode_rgb(&bytes).is_ok())
                .then_some(bytes);
        }
        from = close + 1;
    }
    None
}

/// PDB container: record offset table after the 78-byte header.
struct PdbRecords {
    /// Records as (offset, length) in file order.
    records: Vec<(u64, u32)>,
    /// PalmDOC compression of record 0 (1 none, 2 PalmDOC, 17480 HUFF).
    compression: u16,
    /// Record 0's raw bytes (PalmDOC + MOBI + EXTH headers).
    record0: Vec<u8>,
}

fn pdb_open(path: &Path) -> Option<PdbRecords> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 78];
    f.read_exact(&mut head).ok()?;
    let num = u16::from_be_bytes([head[76], head[77]]) as usize;
    if num == 0 {
        return None;
    }
    let mut table = vec![0u8; num * 8];
    f.read_exact(&mut table).ok()?;
    let mut offsets = Vec::with_capacity(num);
    for r in table.as_chunks::<8>().0 {
        offsets.push(u32::from_be_bytes([r[0], r[1], r[2], r[3]]) as u64);
    }
    let file_len = f.metadata().ok()?.len();
    let mut records = Vec::with_capacity(num);
    for (i, &off) in offsets.iter().enumerate() {
        let end = offsets.get(i + 1).copied().unwrap_or(file_len);
        records.push((off, (end.saturating_sub(off)).min(u32::MAX as u64) as u32));
    }
    let (off0, len0) = records[0];
    let mut record0 = vec![0u8; len0 as usize];
    f.seek(std::io::SeekFrom::Start(off0)).ok()?;
    f.read_exact(&mut record0).ok()?;
    let compression = u16::from_be_bytes([record0[0], record0[1]]);
    Some(PdbRecords {
        records,
        compression,
        record0,
    })
}

fn pdb_read_record(pdb: &PdbRecords, path: &Path, idx: usize) -> Option<Vec<u8>> {
    let (off, len) = *pdb.records.get(idx)?;
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(std::io::SeekFrom::Start(off)).ok()?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn be32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// PalmDOC LZ77 (the classic 4096-byte window pair decoder).  HUFF/CDIC
/// (compression 17480) is deliberately unsupported — those Amazon
/// packings never wrap the cover record, and the codec is a project of
/// its own.
fn palmdoc_decompress(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 8);
    let mut i = 0;
    while i < data.len() {
        // The flag byte nominally gates literal-vs-pair per bit; real
        // streams are decodable positionally because the compressor
        // escapes literals that would alias a pair prefix (values
        // 0x00..=0x08 never appear as pair heads).
        let _flags = data[i];
        i += 1;
        for bit in 0..8 {
            if i >= data.len() {
                break;
            }
            let b = data[i];
            i += 1;
            match b {
                0x00..=0x08 => out.push(b),
                0x09..=0x7f => {
                    let b2 = *data.get(i)?;
                    i += 1;
                    let distance = (((b & 0x3f) << 4) | (b2 >> 4)) as usize + 1;
                    let length = (b2 & 0x0f) as usize + 3;
                    let start = out.len().checked_sub(distance)?;
                    for k in 0..length {
                        let byte = out[start + k];
                        out.push(byte);
                    }
                }
                0x80..=0xbf => {
                    out.push(b' ');
                    out.push(b & 0x7f);
                }
                _ => {
                    let b2 = *data.get(i)?;
                    i += 1;
                    out.push(b' ');
                    out.push(((b & 0x7f) << 1) | (b2 >> 7));
                    out.push(b2 & 0x7f);
                }
            }
            let _ = bit;
        }
    }
    Some(out)
}

/// MOBI/AZW cover: the EXTH CoverRec Index (type 201) names the image
/// record relative to `firstImageIndex`; without EXTH the first record
/// from `firstImageIndex` that starts with an image magic wins
/// (KOReader/crengine's `MOBIgetCoverPage`, narrowed to raw records —
/// HUFF/CDIC packings are not decoded).
fn mobi_cover(path: &Path) -> Option<Vec<u8>> {
    let pdb = pdb_open(path)?;
    if pdb.records.len() < 2 || &pdb.record0[16..20] != b"MOBI" {
        // Not actually a MOBI payload (PalmDOC text in a .mobi name).
        return pdb_cover(path);
    }
    let header_len = be32(&pdb.record0, 20)? as usize;
    // MobileRead MOBI header layout: First Image index @108, EXTH flags
    // @128 (both relative to record 0's start).
    let first_image = be32(&pdb.record0, 108).filter(|&v| v != 0xffff_ffff);
    let exth_flags = be32(&pdb.record0, 128).unwrap_or(0);

    // EXTH walk: records are (type, length, payload) from record0 +
    // 16 + header_len.
    let mut cover_index: Option<u32> = None;
    if exth_flags & 0x40 != 0 {
        let mut p = 16 + header_len;
        if pdb.record0.get(p..p + 4) == Some(&b"EXTH"[..]) {
            let count = be32(&pdb.record0, p + 8)? as usize;
            p += 12;
            for _ in 0..count {
                let rtype = be32(&pdb.record0, p)?;
                let rlen = be32(&pdb.record0, p + 4)? as usize;
                if rlen < 8 || p + rlen > pdb.record0.len() {
                    break;
                }
                if rtype == 201 && rlen >= 12 {
                    cover_index = be32(&pdb.record0, p + 8);
                }
                p += rlen;
            }
        }
    }

    // EXTH 201 is relative to the first image record (KindleUnpack's
    // coverRecord = exth201 + firstImageIndex).
    if let (Some(fii), Some(ci)) = (first_image, cover_index) {
        let idx = fii.saturating_add(ci) as usize;
        if idx < pdb.records.len() {
            if let Some(mut bytes) = pdb_read_record(&pdb, path, idx) {
                if !is_png_or_jpeg(&bytes) && pdb.compression == 2 {
                    if let Some(dec) = palmdoc_decompress(&bytes) {
                        bytes = dec;
                    }
                }
                if is_png_or_jpeg(&bytes) && crate::cover::decode_rgb(&bytes).is_ok() {
                    return Some(bytes);
                }
            }
        }
    }

    // Fallback: the first image-magic record from firstImageIndex (raw —
    // kindlegen and calibre store image records uncompressed).
    let start = first_image.unwrap_or(1) as usize;
    for idx in start..pdb.records.len() {
        if let Some(bytes) = pdb_read_record(&pdb, path, idx) {
            if is_png_or_jpeg(&bytes) && crate::cover::decode_rgb(&bytes).is_ok() {
                return Some(bytes);
            }
        }
    }
    None
}

/// Generic PDB: the first record starting with an image magic (PeanutPress
/// covers; PalmDOC text yields None).
fn pdb_cover(path: &Path) -> Option<Vec<u8>> {
    let pdb = pdb_open(path)?;
    for idx in 1..pdb.records.len() {
        if let Some(bytes) = pdb_read_record(&pdb, path, idx) {
            if is_png_or_jpeg(&bytes) && crate::cover::decode_rgb(&bytes).is_ok() {
                return Some(bytes);
            }
        }
    }
    None
}

/// RTF cover: the first `{\pict…\pngblip|\jpegblip <hex>}` group.  Nested
/// destinations (`{\*\blipuid …}`) are skipped with brace matching, and
/// control words inside the data are stepped over.
fn rtf_cover(path: &Path) -> Option<Vec<u8>> {
    let buf = read_capped(path, 16 << 20);
    let mut from = 0;
    while let Some(pict) = find_at(&buf, b"\\pict", from) {
        let png = find_at(&buf, b"\\pngblip", pict);
        let jpeg = find_at(&buf, b"\\jpegblip", pict);
        let (blip, is_jpeg) = match (png, jpeg) {
            (Some(a), Some(b)) => {
                if a < b {
                    (a, false)
                } else {
                    (b, true)
                }
            }
            (Some(a), None) => (a, false),
            (None, Some(b)) => (b, true),
            _ => {
                from = pict + 5;
                continue;
            }
        };
        let kw_len = if is_jpeg { 9 } else { 8 };
        let bytes = rtf_hex_after(&buf[blip + kw_len..])?;
        if is_png_or_jpeg(&bytes) && crate::cover::decode_rgb(&bytes).is_ok() {
            return Some(bytes);
        }
        from = blip + 1;
    }
    None
}

/// Hex payload of a `\pict` group: hex digits accumulate, control words
/// are stepped over, nested `{…}` destinations are skipped whole, and the
/// group's closing brace ends the data.
fn rtf_hex_after(data: &[u8]) -> Option<Vec<u8>> {
    let mut hex = Vec::new();
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'{' => {
                // Skip a whole nested group (brace-aware, escapes honoured).
                let mut depth = 1usize;
                i += 1;
                while i < data.len() && depth > 0 {
                    match data[i] {
                        b'\\'
                            if i + 1 < data.len()
                                && (data[i + 1] == b'{' || data[i + 1] == b'}') =>
                        {
                            i += 2;
                        }
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            b'}' => break,
            b'\\' => {
                let mut j = i + 1;
                while j < data.len() && data[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j == i + 1 && j < data.len() {
                    j += 1; // escaped symbol like \{ or \\
                }
                while j < data.len() && (data[j].is_ascii_digit() || data[j] == b'-') {
                    j += 1;
                }
                if j < data.len() && data[j] == b' ' {
                    j += 1;
                }
                i = j;
            }
            b if b.is_ascii_hexdigit() => {
                hex.push(b);
                i += 1;
                if hex.len() > 8 << 20 {
                    return None; // absurd blip: bail out
                }
            }
            _ => i += 1,
        }
    }
    if hex.len() < 8 || hex.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_chunks::<2>().0 {
        out.push(u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?);
    }
    Some(out)
}

/// XPS/OXPS cover: the first fixed page's first `ImageSource` resource
/// (the OPC walk KOReader's xps engine does: FixedDocSeq → FixedDocument
/// → page), falling back to the first `.fpage` member.
fn xps_cover(path: &Path) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let mut ar = zip::ZipArchive::new(f).ok()?;

    let mut page: Option<String> = None;
    if let Some(seq) = read_member(&mut ar, "FixedDocSeq.fdseq") {
        if let Some(src) = quoted_value(&seq, "Source") {
            if let Some(fdoc) = read_member(&mut ar, &src) {
                page = quoted_value(&fdoc, "Source");
            }
        }
    }
    let page = match page {
        Some(p) => p,
        None => {
            let mut pages: Vec<String> = (0..ar.len())
                .filter_map(|i| {
                    let n = ar.by_index(i).ok()?.name().to_string();
                    n.ends_with(".fpage").then_some(n)
                })
                .collect();
            pages.sort();
            pages.into_iter().next()?
        }
    };

    let xml = read_member(&mut ar, &page)?;
    let mut from = 0;
    while let Some(p) = find_at(xml.as_bytes(), b"ImageSource", from) {
        let src = quoted_value(&xml[p..], "ImageSource")?;
        let member = src.trim_start_matches('/');
        if let Some(name) = resolve_member(&mut ar, member) {
            let mut out = Vec::new();
            if ar.by_name(&name).ok()?.read_to_end(&mut out).is_ok()
                && is_png_or_jpeg(&out)
                && crate::cover::decode_rgb(&out).is_ok()
            {
                return Some(out);
            }
        }
        from = p + 11;
    }
    None
}

fn read_member(ar: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    let name = resolve_member(ar, name)?;
    let mut s = String::new();
    ar.by_name(&name).ok()?.read_to_string(&mut s).ok()?;
    Some(s)
}

/// First `Attr="value"` occurrence in an XML-ish buffer.
fn quoted_value(xml: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = xml.find(&needle)? + needle.len();
    let end = xml[start..].find('"')? + start;
    Some(xml[start..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── PDF Info-dictionary strings ─────────────────────────────────────

    #[test]
    fn pdf_literal_string_decodes_escapes_nesting_and_octal() {
        // Standard escapes, an unescaped nested paren pair, and an octal
        // escape; the returned offset lands past the closing paren.
        let body = br#"a\(b\)c(d)e\101f)"#;
        let (s, end) = pdf_literal_string(body).unwrap();
        assert_eq!(s, "a(b)c(d)eAf");
        assert_eq!(end, body.len());

        // Octal runs take up to three digits: \1010 is 'A' then '0'.
        assert_eq!(pdf_literal_string(br"\1010)").unwrap().0, "A0");

        // A backslash-newline continues the line; unknown escapes keep
        // the escaped char.
        assert_eq!(pdf_literal_string(b"a\\\nb\\xy)").unwrap().0, "abxy");
    }

    #[test]
    fn pdf_literal_string_unterminated_is_none() {
        assert!(pdf_literal_string(b"no closing paren").is_none());
    }

    #[test]
    fn pdf_hex_string_skips_whitespace_and_pads_odd_digit() {
        // Whitespace between hex digits is ignored; a FEFF BOM decodes
        // the rest as UTF-16BE.
        let body = b"FEFF 0434 0435 > rest";
        let (s, end) = pdf_hex_string(body).unwrap();
        assert_eq!(s, "де");
        assert_eq!(end, b"FEFF 0434 0435 >".len());
        // An odd trailing digit pairs its high nibble with 0:
        // "414" -> 0x41 'A', 0x40 '@'.
        assert_eq!(pdf_hex_string(b"414>").unwrap().0, "A@");
    }

    #[test]
    fn pdf_dict_string_scans_past_nonstring_values_and_caps_chars() {
        // Whitespace between key and value; a non-string value (`true`)
        // keeps the scan going to the next occurrence of the key.
        assert_eq!(
            pdf_dict_string(b"/Title\n true /Title (Real)", b"/Title", 96),
            "Real"
        );
        // The cap counts characters, not bytes: multibyte text survives
        // whole (a byte truncate would split a char).
        assert_eq!(
            pdf_dict_string(b"/Title <FEFF043404350436>", b"/Title", 2),
            "де"
        );
        // No usable value anywhere: empty string.
        assert_eq!(pdf_dict_string(b"/Author null", b"/Author", 8), "");
    }

    #[test]
    fn pdf_decode_bytes_switches_on_the_utf16_bom() {
        // No BOM: bytes map through PDFDocEncoding (≈ Latin-1).
        assert_eq!(pdf_decode_bytes(&[0x41, 0xE9]), "Aé");
        // FEFF BOM: the remainder reads as UTF-16BE units.
        assert_eq!(
            pdf_decode_bytes(&[0xFE, 0xFF, 0x00, 0x64, 0x04, 0x34]),
            "dд"
        );
    }

    // ── XML field grabbing ──────────────────────────────────────────────

    #[test]
    fn grab_two_fields_matches_local_names_across_nesting() {
        let xml = br#"<metadata><dc:title>A<x>B</x> C</dc:title><dc:creator>Z &amp; W</dc:creator></metadata>"#;
        let (t, a) = grab_two_fields(xml, &["title"], &["creator"], MAX_TITLE_LEN);
        // Namespace prefixes are stripped for matching, an inner close
        // does not end the capture, and entities arrive unescaped.
        assert_eq!(t, "A B C");
        assert_eq!(a, "Z & W");
    }

    #[test]
    fn grab_falls_through_an_empty_first_match() {
        // `done` only latches on non-empty text: an empty first <title>
        // must not swallow the real one.
        let xml = br#"<p><title></title><title>Fallback</title></p>"#;
        let (t, _) = grab_two_fields(xml, &["title"], &[], MAX_TITLE_LEN);
        assert_eq!(t, "Fallback");
    }

    #[test]
    fn grab_byte_cap_never_splits_a_multibyte_char() {
        // Regression: a title whose byte cap landed mid-char used to hit
        // String::truncate's char-boundary assertion — a device crash in
        // the middle of the import scan.
        let xml = format!("<t>{}é</t>", "a".repeat(95)).into_bytes();
        let (t, _) = grab_two_fields(&xml, &["t"], &[], MAX_TITLE_LEN);
        assert_eq!(t.len(), 95);
        assert_eq!(t.chars().count(), 95);

        // When the cap DOES fall on a boundary, the full budget is used.
        let xml = format!("<t>{}é</t>", "a".repeat(94)).into_bytes();
        let (t, _) = grab_two_fields(&xml, &["t"], &[], MAX_TITLE_LEN);
        assert_eq!(t.len(), MAX_TITLE_LEN);
        assert!(t.ends_with('é'));
    }

    // ── epub plumbing ───────────────────────────────────────────────────

    #[test]
    fn rootfile_path_reads_the_first_rootfile() {
        let xml = r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/><rootfile full-path="second.opf"/></rootfiles></container>"#;
        assert_eq!(rootfile_path(xml).as_deref(), Some("OEBPS/content.opf"));
        assert_eq!(rootfile_path("<container/>"), None);
    }

    #[test]
    fn cover_hint_resolves_both_wild_conventions() {
        // Convention 1 wins even when a plain item precedes it, and only
        // the exact "cover-image" token matches.
        let props = r#"<manifest><item id="c2" href="plain.png"/><item id="c1" href="props.png" properties="cover-image image-count-2"/></manifest>"#;
        assert_eq!(cover_hint(props).as_deref(), Some("props.png"));
        // Convention 2: meta[name=cover] content=<id> → that item's href.
        let meta = r#"<metadata><meta name="cover" content="my-img"/></metadata><manifest><item id="my-img" href="meta.png"/></manifest>"#;
        assert_eq!(cover_hint(meta).as_deref(), Some("meta.png"));
        // Neither convention: no hint.
        assert_eq!(
            cover_hint(r#"<manifest><item id="x" href="a.png"/></manifest>"#),
            None
        );
    }

    #[test]
    fn attr_in_reads_quoted_values_only() {
        assert_eq!(attr_in(r#"href="a.png""#, "href").as_deref(), Some("a.png"));
        assert_eq!(attr_in("href='a.png'", "href").as_deref(), Some("a.png"));
        assert_eq!(attr_in("href=a.png", "href"), None);
        assert_eq!(attr_in("id=x", "href"), None);
    }

    #[test]
    fn resolve_member_exact_dot_percent_and_basename() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.epub");
        let f = std::fs::File::create(&path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.start_file("OEBPS/plain.png", SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"x").unwrap();
        z.start_file("OEBPS/imgs/my cover.png", SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"x").unwrap();
        z.finish().unwrap();
        let mut ar = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();

        // Exact member name.
        assert_eq!(
            resolve_member(&mut ar, "OEBPS/plain.png").as_deref(),
            Some("OEBPS/plain.png")
        );
        // OPF-dir relative href with a ./ prefix resolves by base name.
        assert_eq!(
            resolve_member(&mut ar, "./plain.png").as_deref(),
            Some("OEBPS/plain.png")
        );
        // Percent-encoded space decodes to the stored member.
        assert_eq!(
            resolve_member(&mut ar, "OEBPS/imgs/my%20cover.png").as_deref(),
            Some("OEBPS/imgs/my cover.png")
        );
        // A wrong directory still falls back to the base name,
        // case-insensitively.
        assert_eq!(
            resolve_member(&mut ar, "elsewhere/MY COVER.PNG").as_deref(),
            Some("OEBPS/imgs/my cover.png")
        );
        // Nothing matches: None.
        assert_eq!(resolve_member(&mut ar, "nope.png"), None);
    }

    // ── KOReader-registry covers ─────────────────────────────────────────

    /// A valid 4×4 PNG (the shared tiny_png helper lives in local.rs's
    /// test module; this one is local so both suites stay independent).
    fn art_png() -> Vec<u8> {
        let px = vec![0xEEu8; 16];
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

    /// A real 4×4 JPEG (PIL-encoded) — decode_rgb must accept every art
    /// payload these tests feed through the extractors.
    fn art_jpeg() -> Vec<u8> {
        const TINY_JPEG: [u8; 632] = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43, 0x00, 0x10, 0x0b, 0x0c,
            0x0e, 0x0c, 0x0a, 0x10, 0x0e, 0x0d, 0x0e, 0x12, 0x11, 0x10, 0x13, 0x18, 0x28, 0x1a,
            0x18, 0x16, 0x16, 0x18, 0x31, 0x23, 0x25, 0x1d, 0x28, 0x3a, 0x33, 0x3d, 0x3c, 0x39,
            0x33, 0x38, 0x37, 0x40, 0x48, 0x5c, 0x4e, 0x40, 0x44, 0x57, 0x45, 0x37, 0x38, 0x50,
            0x6d, 0x51, 0x57, 0x5f, 0x62, 0x67, 0x68, 0x67, 0x3e, 0x4d, 0x71, 0x79, 0x70, 0x64,
            0x78, 0x5c, 0x65, 0x67, 0x63, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x11, 0x12, 0x12, 0x18,
            0x15, 0x18, 0x2f, 0x1a, 0x1a, 0x2f, 0x63, 0x42, 0x38, 0x42, 0x63, 0x63, 0x63, 0x63,
            0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63,
            0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63,
            0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63, 0x63,
            0x63, 0x63, 0x63, 0x63, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x04, 0x00, 0x04, 0x03,
            0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xff, 0xc4, 0x00, 0x1f, 0x00,
            0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b,
            0xff, 0xc4, 0x00, 0xb5, 0x10, 0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05,
            0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7d, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05,
            0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81,
            0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62,
            0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29,
            0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
            0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66,
            0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84,
            0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
            0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5,
            0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
            0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5,
            0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9,
            0xfa, 0xff, 0xc4, 0x00, 0x1f, 0x01, 0x00, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05,
            0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x11, 0x00, 0x02, 0x01,
            0x02, 0x04, 0x04, 0x03, 0x04, 0x07, 0x05, 0x04, 0x04, 0x00, 0x01, 0x02, 0x77, 0x00,
            0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
            0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23,
            0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1,
            0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39,
            0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57,
            0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75,
            0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a,
            0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6,
            0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2,
            0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7,
            0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3,
            0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xda, 0x00, 0x0c, 0x03, 0x01, 0x00,
            0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00, 0x6d, 0x14, 0x51, 0x5e, 0x59, 0xec, 0x9f,
            0xff, 0xd9,
        ];
        TINY_JPEG.to_vec()
    }

    fn write_zip(path: &std::path::Path, entries: &[(&str, Vec<u8>)]) {
        use std::io::Write as _;
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        for (name, data) in entries {
            z.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn cbz_cover_is_first_image_in_natural_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbz");
        write_zip(
            &path,
            &[
                ("pages/page10.png", art_png()),
                ("pages/page2.jpg", art_jpeg()),
                ("pages/page1.png", art_png()),
                ("cover.txt", b"not an image".to_vec()),
            ],
        );
        let bytes = extract_book_cover(&path, "cbz").expect("cbz cover");
        // Natural order: page1 < page2 < page10 (lexicographic would pick
        // page10).
        assert!(bytes.starts_with(b"\x89PNG"));
        assert!(crate::cover::decode_rgb(&bytes).is_ok());
    }

    #[test]
    fn zip_cover_skips_undecodable_members() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.zip");
        write_zip(
            &path,
            &[
                ("a/first.png", b"\x89PNG truncated garbage".to_vec()),
                ("a/second.jpg", art_jpeg()),
            ],
        );
        let bytes = extract_book_cover(&path, "zip").expect("zip cover");
        assert!(bytes.starts_with(b"\xff\xd8"), "skipped to the jpeg");
    }

    #[test]
    fn cbt_cover_reads_ustar_and_gnu_longnames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comic.cbt");

        // Hand-build a tar: a GNU long-name entry whose name only fits
        // via the 'L' payload, then the image, then EOF blocks.
        let png = art_png();
        let long_name = "very-long-directory-path/".repeat(6) + "page-001.png";
        let mut tar = Vec::new();
        let entry = |name: &str, typeflag: u8, payload: &[u8], out: &mut Vec<u8>| {
            let mut h = [0u8; 512];
            let nb = name.as_bytes();
            h[..nb.len().min(100)].copy_from_slice(&nb[..nb.len().min(100)]);
            h[156] = typeflag;
            let size = payload.len();
            let oct = format!("{:011o}", size);
            h[124..124 + oct.len()].copy_from_slice(oct.as_bytes());
            // ustar checksum: spaces while summing, then the real one.
            h[148..156].fill(b' ');
            let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
            let chk = format!("{:06o}\0 ", sum);
            h[148..156].copy_from_slice(chk.as_bytes());
            out.extend_from_slice(&h);
            out.extend_from_slice(payload);
            let pad = (512 - (size % 512)) % 512;
            out.extend(std::iter::repeat_n(0u8, pad));
        };
        let long_payload = {
            let mut v = long_name.as_bytes().to_vec();
            v.push(0);
            v
        };
        entry("././@LongLink", b'L', &long_payload, &mut tar);
        // The short header for the long-named entry carries a placeholder
        // name and the real size.
        {
            let mut h = [0u8; 512];
            h[..10].copy_from_slice(b"short.png\0");
            h[156] = b'0';
            let oct = format!("{:011o}", png.len());
            h[124..124 + oct.len()].copy_from_slice(oct.as_bytes());
            h[148..156].fill(b' ');
            let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
            let chk = format!("{:06o}\0 ", sum);
            h[148..156].copy_from_slice(chk.as_bytes());
            tar.extend_from_slice(&h);
            tar.extend_from_slice(&png);
            let pad = (512 - (png.len() % 512)) % 512;
            tar.extend(std::iter::repeat_n(0u8, pad));
        }
        tar.extend(std::iter::repeat_n(0u8, 1024)); // EOF blocks
        std::fs::write(&path, &tar).unwrap();

        let bytes = extract_book_cover(&path, "cbt").expect("cbt cover");
        assert!(bytes.starts_with(b"\x89PNG"));
        assert_eq!(bytes, png);
    }

    #[test]
    fn fb2_cover_prefers_the_coverpage_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.fb2");
        let b64 = |bytes: &[u8]| -> String {
            const T: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            let mut acc = 0u32;
            let mut n = 0;
            for &b in bytes {
                acc = (acc << 8) | u32::from(b);
                n += 8;
                while n >= 6 {
                    n -= 6;
                    out.push(T[((acc >> n) & 0x3f) as usize] as char);
                }
            }
            if n > 0 {
                out.push(T[((acc << (6 - n)) & 0x3f) as usize] as char);
            }
            while !out.len().is_multiple_of(4) {
                out.push('=');
            }
            out
        };
        let xml = format!(
            r##"<FictionBook><description><title-info><coverpage><image l:href="#cover.jpg"/></coverpage></title-info></description><body/><binary id="other.jpg" content-type="image/jpeg">{}</binary><binary id="cover.jpg" content-type="image/jpeg">{}</binary></FictionBook>"##,
            b64(b"\xff\xd8\xff\xe0 other"),
            b64(&art_jpeg()),
        );
        std::fs::write(&path, &xml).unwrap();
        let bytes = extract_book_cover(&path, "fb2").expect("fb2 cover");
        assert_eq!(bytes, art_jpeg(), "the coverpage binary, not the first");
    }

    #[test]
    fn fb2_cover_falls_back_to_the_first_image_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nofb2cover.fb2");
        let png = art_png();
        let xml = format!(
            r#"<FictionBook><binary id="p1" content-type="image/png">{}</binary></FictionBook>"#,
            {
                const T: &[u8; 64] =
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
                let mut out = String::new();
                let mut acc = 0u32;
                let mut n = 0;
                for &b in &png {
                    acc = (acc << 8) | u32::from(b);
                    n += 8;
                    while n >= 6 {
                        n -= 6;
                        out.push(T[((acc >> n) & 0x3f) as usize] as char);
                    }
                }
                if n > 0 {
                    out.push(T[((acc << (6 - n)) & 0x3f) as usize] as char);
                }
                while !out.len().is_multiple_of(4) {
                    out.push('=');
                }
                out
            }
        );
        std::fs::write(&path, &xml).unwrap();
        let bytes = extract_book_cover(&path, "fb2").expect("fb2 fallback");
        assert_eq!(bytes, png);
    }

    /// A minimal MOBI: PDB header + record 0 (PalmDOC + MOBI + EXTH with
    /// a 201 cover index) + an uncompressed image record.  `first_image =
    /// None` writes a text-only container (no image record).
    fn write_mobi(path: &std::path::Path, with_exth: bool, first_image: Option<u32>) {
        let art = art_jpeg();
        let has_art = first_image.is_some();
        let image_rec_index = 2u32; // record 0 header, record 1 text, record 2 image

        // record 0: PalmDOC (16) + MOBI header + optional EXTH
        let mut r0 = Vec::new();
        r0.extend_from_slice(&2u16.to_be_bytes()); // PalmDOC compression
        r0.extend_from_slice(&0u16.to_be_bytes());
        r0.extend_from_slice(&0u32.to_be_bytes()); // text length
        r0.extend_from_slice(&1u16.to_be_bytes()); // text record count
        r0.extend_from_slice(&4096u16.to_be_bytes());
        r0.extend_from_slice(&0u16.to_be_bytes()); // no encryption
        r0.extend_from_slice(&0u16.to_be_bytes());
        let mobi_start = r0.len() as u32;
        r0.extend_from_slice(b"MOBI");
        let mut body = Vec::new();
        body.extend_from_slice(&248u32.to_be_bytes()); // header length
        body.extend_from_slice(&2u32.to_be_bytes()); // mobi type
        body.extend_from_slice(&65001u32.to_be_bytes()); // utf-8
        body.extend_from_slice(&0u32.to_be_bytes()); // unique id
        body.extend_from_slice(&0u32.to_be_bytes()); // file version
        for _ in 0..10 {
            body.extend_from_slice(&0xffff_ffffu32.to_be_bytes()); // indices
        }
        body.extend_from_slice(&1u32.to_be_bytes()); // first non-book index
        body.extend_from_slice(&0u32.to_be_bytes()); // full name offset
        body.extend_from_slice(&0u32.to_be_bytes()); // full name length
        body.extend_from_slice(&9u32.to_be_bytes()); // locale
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&6u32.to_be_bytes()); // min version
        body.extend_from_slice(&first_image.unwrap_or(0xffff_ffff).to_be_bytes());
        for _ in 0..4 {
            body.extend_from_slice(&0u32.to_be_bytes()); // huffman
        }
        let exth_flag: u32 = if with_exth { 0x40 } else { 0 };
        body.extend_from_slice(&exth_flag.to_be_bytes()); // exth flags
        r0.extend_from_slice(&body);
        if with_exth && has_art {
            let mut exth = Vec::new();
            exth.extend_from_slice(b"EXTH");
            let mut recs = Vec::new();
            recs.extend_from_slice(&201u32.to_be_bytes());
            recs.extend_from_slice(&12u32.to_be_bytes());
            recs.extend_from_slice(&(image_rec_index - first_image.unwrap_or(0)).to_be_bytes());
            exth.extend_from_slice(&(recs.len() as u32 + 12).to_be_bytes());
            exth.extend_from_slice(&1u32.to_be_bytes()); // record count
            exth.extend_from_slice(&recs);
            r0.extend_from_slice(&exth);
        }
        // Patch the MOBI header length to cover the EXTH too (the spec
        // counts from "MOBI" through the end of the EXTH).
        let hl = (r0.len() as u32) - mobi_start;
        r0[mobi_start as usize + 4..mobi_start as usize + 8].copy_from_slice(&hl.to_be_bytes());

        let text_rec = vec![b'x'; 16]; // one PalmDOC text record

        // PDB container
        let nrec: u16 = if has_art { 3 } else { 2 };
        let mut out = Vec::new();
        out.extend_from_slice(b"BOOKMOBI\0"); // name field head
        out.resize(32, 0); // name (32)
        out.extend_from_slice(&0u16.to_be_bytes()); // attributes
        out.extend_from_slice(&0u16.to_be_bytes()); // version
        out.extend_from_slice(&[0u8; 12]); // ctime/mtime/btime
        out.extend_from_slice(&0u32.to_be_bytes()); // modnum
        out.extend_from_slice(&0u32.to_be_bytes()); // app info
        out.extend_from_slice(&0u32.to_be_bytes()); // sort info
        out.extend_from_slice(b"BOOK"); // type
        out.extend_from_slice(b"MOBI"); // creator
        out.extend_from_slice(&0u32.to_be_bytes()); // unique seed
        out.extend_from_slice(&0u32.to_be_bytes()); // next record list
        out.extend_from_slice(&nrec.to_be_bytes());
        let rec0_len = r0.len() as u32;
        let mut table_off = 78 + nrec as usize * 8;
        // record 0
        // Each entry: offset u32 + attributes u8 + unique id [u8; 3].
        out.extend_from_slice(&(table_off as u32).to_be_bytes());
        out.push(0);
        out.extend_from_slice(&[0, 0, 0]);
        // record 1 (text)
        table_off += rec0_len as usize;
        out.extend_from_slice(&(table_off as u32).to_be_bytes());
        out.push(0);
        out.extend_from_slice(&[0, 0, 1]);
        // record 2 (image)
        if has_art {
            table_off += text_rec.len();
            out.extend_from_slice(&(table_off as u32).to_be_bytes());
            out.push(0);
            out.extend_from_slice(&[0, 0, 2]);
        }
        out.extend_from_slice(&r0);
        out.extend_from_slice(&text_rec);
        if has_art {
            out.extend_from_slice(&art);
        }
        std::fs::write(path, &out).unwrap();
    }

    #[test]
    fn mobi_cover_reads_the_exth_cover_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.mobi");
        write_mobi(&path, true, Some(2));
        let bytes = extract_book_cover(&path, "mobi").expect("mobi cover via EXTH");
        assert!(bytes.starts_with(b"\xff\xd8"));
    }

    #[test]
    fn mobi_cover_scans_image_records_without_exth() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noexth.mobi");
        write_mobi(&path, false, Some(2));
        let bytes = extract_book_cover(&path, "mobi").expect("mobi cover via scan");
        assert!(bytes.starts_with(b"\xff\xd8"));
    }

    #[test]
    fn pdb_cover_scans_records_for_image_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.pdb");
        // A PalmDOC-ish PDB: record 0 header, record 1 text, record 2 art.
        write_mobi(&path, false, Some(2));
        let bytes = extract_book_cover(&path, "pdb").expect("pdb cover via scan");
        assert!(bytes.starts_with(b"\xff\xd8"));
    }

    #[test]
    fn rtf_cover_decodes_the_first_png_blip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.rtf");
        let png = art_png();
        let hex: String = png.iter().map(|b| format!("{:02x}", b)).collect();
        let rtf = format!(
            r#"{{\rtf1\ansi {{\pict\pngblip\picw4\pich4\picwgoal1000\pichgoal1000 {{\*\blipuid 123456}}{hex}}}}}"#
        );
        std::fs::write(&path, &rtf).unwrap();
        let bytes = extract_book_cover(&path, "rtf").expect("rtf cover");
        assert_eq!(bytes, png, "hex payload decoded past the blipuid group");
    }

    #[test]
    fn rtf_cover_decodes_jpeg_blips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.rtf");
        let jpg = art_jpeg();
        let hex: String = jpg.iter().map(|b| format!("{:02X}", b)).collect();
        let rtf = format!(r#"{{\rtf1 {{\pict\jpegblip\picw4{hex}}}}}"#);
        std::fs::write(&path, &rtf).unwrap();
        let bytes = extract_book_cover(&path, "rtf").expect("rtf jpeg cover");
        assert!(bytes.starts_with(b"\xff\xd8"));
    }

    #[test]
    fn xps_cover_walks_fdseq_to_the_page_resource() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.xps");
        write_zip(
            &path,
            &[
                (
                    "FixedDocSeq.fdseq",
                    br#"<FixedDocumentSequence><DocumentReference Source="/Documents/1/FixedDoc.fdoc"/></FixedDocumentSequence>"#.to_vec(),
                ),
                (
                    "Documents/1/FixedDoc.fdoc",
                    br#"<FixedDocument><PageContent Source="/Documents/1/Pages/1.fpage"/></FixedDocument>"#.to_vec(),
                ),
                (
                    "Documents/1/Pages/1.fpage",
                    br#"<FixedPage><Canvas><Path Fill="{ImageBrush ImageSource="/Documents/1/Pages/1.image-1.png"}"/></Canvas></FixedPage>"#.to_vec(),
                ),
                ("Documents/1/Pages/1.image-1.png", art_png()),
            ],
        );
        let bytes = extract_book_cover(&path, "xps").expect("xps cover");
        assert!(bytes.starts_with(b"\x89PNG"));
    }

    #[test]
    fn formats_without_extractable_covers_return_none() {
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("book.html");
        std::fs::write(&txt, b"<html><body>no art</body></html>").unwrap();
        assert!(extract_book_cover(&txt, "html").is_none(), "html");
        assert!(extract_book_cover(&txt, "djvu").is_none(), "djvu");
        assert!(extract_book_cover(&txt, "chm").is_none(), "chm");
        assert!(extract_book_cover(&txt, "doc").is_none(), "doc");
        // A text-only PalmDOC pdb: records exist but none is an image.
        let pdb = dir.path().join("text.pdb");
        write_mobi(&pdb, false, None);
        // first_image = None -> the scan starts at record 1 (text) and
        // finds nothing decodable.
        assert!(extract_book_cover(&pdb, "pdb").is_none(), "text pdb");
    }

    #[test]
    fn base64_decoder_handles_whitespace_and_padding() {
        // "hello" -> aGVsbG8= (with newlines injected).
        assert_eq!(
            base64_decode("aGVs\nbG8=  ").as_deref(),
            Some(b"hello".as_slice())
        );
        assert_eq!(base64_decode("").as_deref(), None);
    }

    #[test]
    fn natural_key_orders_pages_numerically() {
        assert!(natural_key("page2.png") < natural_key("page10.png"));
        assert!(natural_key("p1.jpg") < natural_key("p2.jpg"));
        assert_eq!(natural_key("cover.png"), natural_key("Cover.PNG"));
    }

    // ── misc helpers ────────────────────────────────────────────────────

    #[test]
    fn local_name_strips_the_namespace_prefix() {
        assert_eq!(local_name(b"dc:title"), "title");
        assert_eq!(local_name(b"title"), "title");
        assert_eq!(local_name(b"a:b:c"), "c");
    }

    #[test]
    fn read_capped_truncates_and_tolerates_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.bin");
        std::fs::write(&p, vec![0u8; 4096]).unwrap();
        assert_eq!(read_capped(&p, 1000).len(), 1000);
        assert_eq!(read_capped(&p, 1 << 20).len(), 4096);
        // Missing file: empty buffer, no panic (best-effort extraction).
        assert!(read_capped(&dir.path().join("nope"), 10).is_empty());
    }
}
