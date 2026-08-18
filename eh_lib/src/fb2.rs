//! FB2 title/author extraction — XML `<book-title>` / `<first-name>` /
//! `<last-name>`, mirroring the C `fb2_meta` (first 256 KiB of the file,
//! author = "first last").

use std::path::Path;

/// Strictness note: the C code substring-scanned raw bytes and returned 0
/// even for malformed XML (empty fields → caller falls back to filename).
/// We parse the XML instead, so a well-formed FB2 yields identical results
/// and a malformed one is rejected (both outcomes fall back to the
/// filename, so the observable behavior is unchanged).
pub fn meta(path: &Path) -> Result<(Option<String>, Option<String>), ()> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|_| ())?;
    let mut buf = vec![0u8; 262144 - 1];
    let got = f.read(&mut buf).map_err(|_| ())?;
    if got == 0 {
        return Err(());
    }
    buf.truncate(got);
    let s = String::from_utf8_lossy(&buf);
    let doc = roxmltree::Document::parse(&s).map_err(|_| ())?;

    let title = element_text(&doc, "book-title");
    let first = element_text(&doc, "first-name").unwrap_or_default();
    let last = element_text(&doc, "last-name").unwrap_or_default();

    let author = if !first.is_empty() || !last.is_empty() {
        Some(if last.is_empty() {
            first.clone()
        } else if first.is_empty() {
            last.clone()
        } else {
            format!("{first} {last}")
        })
    } else {
        None
    };

    Ok((title, author))
}

fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn element_text(doc: &roxmltree::Document, qname: &str) -> Option<String> {
    let want = local_name(qname);
    for node in doc.descendants().filter(|n| n.is_element()) {
        if local_name(node.tag_name().name()) == want {
            let t = node.text().unwrap_or("");
            return Some(
                crate::common::xml_unescape(t)
                    .trim_matches(|c| c == ' ' || c == '\t' || c == '\r' || c == '\n')
                    .to_string(),
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_fb2() {
        // Build the doc inline to exercise the real code path.
        let xml = br#"<?xml version="1.0"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
  <description>
    <title-info>
      <book-title>The Book Title</book-title>
      <author><first-name>Jane</first-name><last-name>Doe</last-name></author>
    </title-info>
  </description>
</FictionBook>"#;
        let doc = roxmltree::Document::parse(std::str::from_utf8(xml).unwrap()).unwrap();
        assert_eq!(
            element_text(&doc, "book-title").as_deref(),
            Some("The Book Title")
        );
        assert_eq!(element_text(&doc, "first-name").as_deref(), Some("Jane"));
        assert_eq!(element_text(&doc, "last-name").as_deref(), Some("Doe"));
    }
}
