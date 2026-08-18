//! `eh_lib` — Rust native library for the EinkHome app, compiled as a
//! C-ABI staticlib and linked into the C binary.
//!
//! Currently exposes the book metadata/cover extraction API, a drop-in for
//! the former hand-rolled C parser (see `app/data/eh_extract.h`):
//!   - `eh_extract_book_meta(path, ext, title, title_cap, author, author_cap)`
//!   - `eh_extract_book_cover(path, ext, out_path, out_cap)`
//!
//! The return-value contract is preserved: `0` = format parsed (fields may
//! be empty → caller falls back to the filename), `-1` = unsupported/
//! unreadable. Further Rust-backed APIs will live here as they are added.

mod common;
mod epub;
mod fb2;
mod pdf;

#[cfg(target_arch = "arm")]
mod firmware;

use std::ffi::{c_char, c_int, CStr};
use std::path::Path;

/// Dispatch on the lowercase extension (C uses `strcmp(ext, "epub")`, so
/// matching is exact). Returns `true` when the format was handled and the
/// out buffers were populated (or left empty — the caller falls back to the
/// filename).
fn dispatch_meta(
    path: &Path,
    ext: &str,
    set_title: &mut dyn FnMut(&str),
    set_author: &mut dyn FnMut(&str),
) -> bool {
    let parsed = match ext {
        "epub" => epub::meta(path),
        "pdf" => pdf::meta(path),
        "fb2" => fb2::meta(path),
        _ => return false,
    };
    match parsed {
        Ok((title, author)) => {
            if let Some(t) = title {
                set_title(&t);
            }
            if let Some(a) = author {
                set_author(&a);
            }
            true
        }
        Err(_) => false,
    }
}

/// Extract title/author. Mirrors `eh_extract_book_meta`.
///
/// # Safety
/// `path`/`ext` must be valid NUL-terminated C strings; `title`/`author`
/// must be writable buffers of `title_cap`/`author_cap` bytes (either
/// pointer may be NULL when the corresponding cap is 0).
#[no_mangle]
pub unsafe extern "C" fn eh_extract_book_meta(
    path: *const c_char,
    ext: *const c_char,
    title: *mut c_char,
    title_cap: usize,
    author: *mut c_char,
    author_cap: usize,
) -> c_int {
    // The C function clears the out buffers before any dispatch.
    if !title.is_null() && title_cap > 0 {
        *title = 0;
    }
    if !author.is_null() && author_cap > 0 {
        *author = 0;
    }

    if path.is_null() || ext.is_null() {
        return -1;
    }
    let path = match CStr::from_ptr(path).to_str() {
        Ok(p) => p,
        Err(_) => return -1,
    };
    let ext = match CStr::from_ptr(ext).to_str() {
        Ok(e) => e,
        Err(_) => return -1,
    };

    let mut set_title = |s: &str| common::write_cstr(s, title, title_cap);
    let mut set_author = |s: &str| common::write_cstr(s, author, author_cap);

    if dispatch_meta(Path::new(path), ext, &mut set_title, &mut set_author) {
        0
    } else {
        -1
    }
}

/// Extract the embedded cover to `out_path`. Mirrors `eh_extract_book_cover`:
/// returns 0 when the EPUB structure reads (best-effort; the C caller checks
/// the written file), -1 for unsupported ext or an unreadable EPUB.
///
/// # Safety
/// `path`/`ext` must be valid NUL-terminated C strings; `out_path` must be a
/// writable NUL-terminated C string (`out_cap` bounds the buffer) already
/// containing the target file path (never cleared, per contract).
#[no_mangle]
pub unsafe extern "C" fn eh_extract_book_cover(
    path: *const c_char,
    ext: *const c_char,
    out_path: *mut c_char,
    out_cap: usize,
) -> c_int {
    if path.is_null() || ext.is_null() {
        return -1;
    }
    let path = match CStr::from_ptr(path).to_str() {
        Ok(p) => p,
        Err(_) => return -1,
    };
    let ext = match CStr::from_ptr(ext).to_str() {
        Ok(e) => e,
        Err(_) => return -1,
    };
    if ext != "epub" {
        return -1;
    }
    // The caller wrote the output path into this buffer already; read it.
    if out_path.is_null() || out_cap == 0 {
        return -1;
    }
    let out = match CStr::from_ptr(out_path).to_str() {
        Ok(p) if !p.is_empty() => p.to_string(),
        _ => return -1,
    };
    match epub::cover(Path::new(path), Path::new(&out)) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}
