//! EPUB metadata + cover extraction.
//!
//! Replaces the hand-rolled ZIP reader and XML substring scraping in the
//! former `app/data/eh_extract.c`. Uses `zip` for the container and
//! `roxmltree`
//! for structured OPF/container parsing, while preserving the extraction
//! *semantics* of the C code (first `<dc:title>`, first `<dc:creator>`,
//! `<meta name="cover">` → `<item id>` → href).

use std::path::Path;

use crate::common::xml_unescape;

/// Read one entry from a ZIP archive into a Vec, or `Err(())` on any
/// open/read failure (mirrors `zip_entry_read`'s -1).
fn zip_read(zip_path: &Path, want: &str) -> Result<Vec<u8>, ()> {
    let file = std::fs::File::open(zip_path).map_err(|_| ())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| ())?;
    let mut entry = archive.by_name(want).map_err(|_| ())?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut buf).map_err(|_| ())?;
    Ok(buf)
}

/// Qualified tag's local name, e.g. `dc:title` → `title`. Falls back to the
/// full name when there is no prefix, so an unprefixed `<title>` still
/// matches `dc:title` (strictly more lenient than C, never less).
fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Text content (with entity unescaping + trimming) of the first element
/// whose local tag name matches `qname`. Returns `None` when absent —
/// mirroring `xml_tag_content` returning NULL.
fn element_text(doc: &roxmltree::Document, qname: &str) -> Option<String> {
    let want = local_name(qname);
    for node in doc.descendants().filter(|n| n.is_element()) {
        if local_name(node.tag_name().name()) == want {
            if let Some(t) = node.text() {
                let s = xml_unescape(t);
                let s = s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r' || c == '\n');
                return Some(s.to_string());
            }
            return Some(String::new());
        }
    }
    None
}

/// OPF full-path entry name from `META-INF/container.xml`. Returns `Err`
/// when the container is unreadable or has no `full-path` attribute
/// (mirrors `epub_opf_path` returning -1).
fn opf_path(zip_path: &Path) -> Result<String, ()> {
    let c = zip_read(zip_path, "META-INF/container.xml").map_err(|_| ())?;
    let s = String::from_utf8_lossy(&c);
    let doc = roxmltree::Document::parse(&s).map_err(|_| ())?;
    for node in doc.descendants().filter(|n| n.is_element()) {
        if let Some(v) = node.attribute("full-path") {
            let v = v.trim_start_matches('/');
            if !v.is_empty() {
                return Ok(v.to_string());
            }
        }
    }
    Err(())
}

/// Resolve an OPF `href` against the OPF's directory into a ZIP entry name
/// and strip any leading '/', mirroring `epub_entry_path`.
fn resolve_path(opf: &str, href: &str) -> String {
    let joined = match opf.rfind('/') {
        Some(idx) => format!("{}/{}", &opf[..idx], href),
        None => href.to_string(),
    };
    joined.trim_start_matches('/').to_string()
}

/// Pure metadata: read container → OPF, return (title, author).
/// Returns `Err` when container/OPF cannot be read (C's -1 path).
pub fn meta(path: &Path) -> Result<(Option<String>, Option<String>), ()> {
    let opf = opf_path(path)?;
    let xml = zip_read(path, &opf).map_err(|_| ())?;
    let s = String::from_utf8_lossy(&xml);
    let doc = roxmltree::Document::parse(&s).map_err(|_| ())?;
    let title = element_text(&doc, "dc:title");
    let author = element_text(&doc, "dc:creator");
    Ok((title, author))
}

/// Extract the embedded cover image to `out_path` (best-effort; the C
/// caller checks the file's existence). Returns `Ok(())` once the OPF
/// parses, even when no usable cover is found — matching `eh_extract_book_cover`
/// returning 0 once the EPUB structure reads.
pub fn cover(path: &Path, out_path: &Path) -> Result<(), ()> {
    let opf = opf_path(path)?;
    let xml = zip_read(path, &opf).map_err(|_| ())?;
    let s = String::from_utf8_lossy(&xml);
    let doc = roxmltree::Document::parse(&s).map_err(|_| ())?;

    // <meta name="cover" content="cid">
    let mut cid: Option<String> = None;
    for node in doc.descendants().filter(|n| n.is_element()) {
        if local_name(node.tag_name().name()) == "meta" && node.attribute("name") == Some("cover") {
            if let Some(c) = node.attribute("content") {
                cid = Some(c.to_string());
                break;
            }
        }
    }
    let Some(cid) = cid else {
        return Ok(());
    };

    // <item id="cid" href="...">
    let mut href: Option<String> = None;
    for node in doc.descendants().filter(|n| n.is_element()) {
        if local_name(node.tag_name().name()) == "item"
            && node.attribute("id").is_some()
            && node.attribute("id") == Some(cid.as_str())
        {
            href = node.attribute("href").map(|h| h.to_string());
            break;
        }
    }
    let Some(href) = href else {
        return Ok(());
    };

    let entry = resolve_path(&opf, &href);
    let img = match zip_read(path, &entry) {
        Ok(img) => img,
        Err(_) => return Ok(()),
    };

    // Only write JPEG (FF D8) or PNG (89 50) like the C code.
    let is_jpg = img.len() > 2 && img[0] == 0xff && img[1] == 0xd8;
    let is_png = img.len() > 4 && img[0] == 0x89 && img[1] == b'P';
    if (is_jpg || is_png) && std::fs::write(out_path, &img).is_err() {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgipc_meta() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bgipc.epub");
        let (title, author) = meta(&p).unwrap();
        assert_eq!(
            title.as_deref(),
            Some("Beej's Guide to Interprocess Communication")
        );
        assert_eq!(author.as_deref(), Some("Brian “Beej Jorgensen” Hall"));
    }

    #[test]
    fn bgipc_no_cover() {
        // bgipc has no `<meta name="cover">`; extraction must still succeed.
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bgipc.epub");
        assert!(cover(&p, Path::new("/tmp/eh_lib_should_not_exist.raw")).is_ok());
    }
}
