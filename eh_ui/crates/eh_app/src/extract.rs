//! Book-file metadata + cover extraction (C eh_extract.h surface, pure
//! Rust): epub/fb2/pdf metadata scans, plus cover art from embedded epub
//! images, MuPDF first-page renders, or generated txt covers.
//!
//! Split out of `local.rs`: every function here is pure — a path in,
//! bytes/metadata out — with no App coupling, so both the Local import
//! and any other consumer can call it from any context.

use std::io::Read;
use std::path::Path;

/// Title-cap for filename-derived titles (C EH_MAX_TITLE_LEN).
pub(crate) const MAX_TITLE_LEN: usize = 96;

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
    pub(crate) fn is_empty(&self) -> bool {
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
    // Whitespace-only metadata counts as absent: the caller falls back to
    // the filename (without extension) instead of showing a blank title.
    let r = ExtractedMeta {
        title: r.title.trim().to_string(),
        author: r.author.trim().to_string(),
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
        "pdf" => pdf_first_page_png(path),
        "txt" => txt_word_cover(path),
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
    Some(ExtractedMeta {
        title,
        author,
        cover_hint: cover_hint(&opf),
    })
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
