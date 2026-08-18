//! PDF title/author extraction — faithful port of the C code's literal
//! `/Key (value)` scan of the uncompressed Info dict in the file tail.

use std::path::Path;

use crate::common::{find_sub, trim};

/// Find `/Key` followed by `(value)` in the buffer and write the unescaped
/// value into `out`, mirroring `pdf_find_string` (only the value for a
/// direct `(` after whitespace; `\n` escape → newline, other `\x` → x).
fn find_string(buf: &[u8], key: &[u8], out: &mut String) {
    out.clear();
    let mut p = 0usize;
    while let Some(idx) = find_sub(buf, key, p) {
        let mut q = idx + key.len();
        while q < buf.len() && matches!(buf[q], b' ' | b'\t' | b'\r' | b'\n') {
            q += 1;
        }
        if q < buf.len() && buf[q] == b'(' {
            q += 1;
            let mut val = Vec::new();
            let mut esc = false;
            while q < buf.len() {
                if esc {
                    val.push(if buf[q] == b'n' { b'\n' } else { buf[q] });
                    esc = false;
                    q += 1;
                    continue;
                }
                if buf[q] == b'\\' {
                    esc = true;
                    q += 1;
                    continue;
                }
                if buf[q] == b')' {
                    break;
                }
                val.push(buf[q]);
                q += 1;
            }
            let raw = String::from_utf8_lossy(&val);
            let s = trim(&raw);
            out.push_str(s);
            return;
        }
        p = q;
    }
}

pub fn meta(path: &Path) -> Result<(Option<String>, Option<String>), ()> {
    let f = std::fs::File::open(path).map_err(|_| ())?;
    let sz = f.metadata().map_err(|_| ())?.len();
    let want = if sz > 262144 {
        262144usize
    } else {
        sz as usize
    };
    if want == 0 {
        return Err(());
    }
    use std::io::{Read, Seek, SeekFrom};
    let mut f = f;
    f.seek(SeekFrom::Start(sz - want as u64)).map_err(|_| ())?;
    let mut buf = vec![0u8; want];
    f.read_exact(&mut buf).map_err(|_| ())?;

    let mut title = String::new();
    let mut author = String::new();
    find_string(&buf, b"/Title", &mut title);
    find_string(&buf, b"/Author", &mut author);

    let title = if title.is_empty() { None } else { Some(title) };
    let author = if author.is_empty() {
        None
    } else {
        Some(author)
    };
    Ok((title, author))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_literal_values() {
        let buf = b"/Title(My Book)\n/Author( Jane Doe )trailing()(".to_vec();
        let mut t = String::new();
        let mut a = String::new();
        find_string(&buf, b"/Title", &mut t);
        find_string(&buf, b"/Author", &mut a);
        assert_eq!(t, "My Book");
        assert_eq!(a, "Jane Doe");
    }

    #[test]
    fn handles_escapes_and_missing() {
        let buf = b"/Title(A \\n B \\/ C)then /Author(No)".to_vec();
        let mut t = String::new();
        let mut a = String::new();
        find_string(&buf, b"/Title", &mut t);
        find_string(&buf, b"/Author", &mut a);
        assert_eq!(t, "A \n B / C");
        assert_eq!(a, "No");
        let mut missing = String::new();
        find_string(&buf, b"/Missing", &mut missing);
        assert_eq!(missing, "");
    }
}
