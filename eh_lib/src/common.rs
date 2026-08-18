//! Shared helpers for the book-metadata extraction.
//!
//! Semantics mirror the former C implementation (`app/data/eh_extract.h`)
//! so the Rust library is a behavioral drop-in for it.

use std::ffi::c_char;

/// Trim leading/trailing whitespace (space/tab/CR/LF) in place, like C's
/// `trim_str`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t' || c == '\r' || c == '\n')
}

/// Basic XML entity unescape (named + decimal numeric), mirroring C's
/// `xml_unescape` / `xml_numeric_entity`. Only decimal `&#NN;` is decoded
/// (the C code uses `strtol(...,10)`); hex `&#x...;` is left verbatim.
pub fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes.get(i) {
            Some(b'&') => {
                if let Some(remain) = bytes.get(i + 1..) {
                    match remain {
                        _ if remain.starts_with(b"lt;") => {
                            out.push('<');
                            i += 4;
                        }
                        _ if remain.starts_with(b"gt;") => {
                            out.push('>');
                            i += 4;
                        }
                        _ if remain.starts_with(b"amp;") => {
                            out.push('&');
                            i += 5;
                        }
                        _ if remain.starts_with(b"quot;") => {
                            out.push('"');
                            i += 6;
                        }
                        _ if remain.starts_with(b"apos;") => {
                            out.push('\'');
                            i += 6;
                        }
                        _ if remain.starts_with(b"#") => {
                            // `&#NN;` decimal; next char must be a digit (not `x`).
                            if let Some(d) = remain.get(2) {
                                if d.is_ascii_digit() {
                                    let mut j = 2;
                                    let mut code: u32 = 0;
                                    while let Some(c) = remain.get(j) {
                                        if !c.is_ascii_digit() {
                                            break;
                                        }
                                        code = code
                                            .saturating_mul(10)
                                            .saturating_add((c - b'0') as u32);
                                        j += 1;
                                    }
                                    if remain.get(j) == Some(&b';') && code != 0 && code < 0x110000
                                    {
                                        let cp = if (0xD800..=0xDFFF).contains(&code) {
                                            0xFFFD
                                        } else {
                                            code
                                        };
                                        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                                        i += 1 + 1 + j + 1; // &# + digits + ;
                                        continue;
                                    }
                                }
                            }
                            out.push('&');
                            i += 1;
                        }
                        _ => {
                            out.push('&');
                            i += 1;
                        }
                    }
                } else {
                    out.push('&');
                    i += 1;
                }
            }
            Some(&_c) => {
                // Copy the full UTF-8 sequence, not just one byte.
                let ch = unsafe { s.get_unchecked(i..) }.chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
            None => unreachable!(),
        }
    }
    out
}

/// Copy `src` into a caller-provided C buffer as a NUL-terminated byte
/// string, truncating at a UTF-8 boundary so at most `cap-1` bytes are
/// written (like C's `snprintf` into a fixed array). No-op for NULL/0 cap.
///
/// # Safety
/// `out` must be a valid write pointer to `cap` bytes.
pub unsafe fn write_cstr(src: &str, out: *mut c_char, cap: usize) {
    if out.is_null() || cap == 0 {
        return;
    }
    let max = cap - 1;
    let mut n = src.len();
    if n > max {
        n = max;
        while n > 0 && !src.is_char_boundary(n) {
            n -= 1;
        }
    }
    if n > 0 {
        let bytes = src.as_bytes().as_ptr().cast::<c_char>();
        std::ptr::copy_nonoverlapping(bytes, out, n);
    }
    *out.add(n) = 0;
}

/// Byte-level substring search (needle not empty). Returns index or None.
pub fn find_sub(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from >= hay.len() {
        return None;
    }
    let first = needle[0];
    let mut i = from;
    let max = hay.len() - needle.len();
    while i <= max {
        if hay[i] == first && &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}
