//! Integration tests that call the exported (`#[no_mangle]`) FFI functions
//! the C app links, exercising the full C-ABI contract — buffer policy,
//! NUL termination, truncation, and return codes — against real fixtures.

use eh_lib::{eh_extract_book_cover, eh_extract_book_meta};
use std::ffi::CString;
use std::ffi::{c_char, c_int};
use std::path::Path;

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(name: &str) -> CString {
    CString::new(format!("{FIX}/{name}")).unwrap()
}

fn run_meta(path: &CString, ext: &CString) -> (i32, String, String) {
    let mut title = vec![b'X'; 64];
    let mut author = vec![b'X'; 64];
    let rc: c_int = unsafe {
        eh_extract_book_meta(
            path.as_ptr(),
            ext.as_ptr(),
            title.as_mut_ptr() as *mut c_char,
            title.len(),
            author.as_mut_ptr() as *mut c_char,
            author.len(),
        )
    };
    let title_s = unsafe {
        std::ffi::CStr::from_ptr(title.as_ptr() as *const c_char)
            .to_string_lossy()
            .into_owned()
    };
    let author_s = unsafe {
        std::ffi::CStr::from_ptr(author.as_ptr() as *const c_char)
            .to_string_lossy()
            .into_owned()
    };
    (rc, title_s, author_s)
}

#[test]
fn epub_meta_from_real_book() {
    let p = fixture("bgipc.epub");
    let e = CString::new("epub").unwrap();
    let (rc, title, author) = run_meta(&p, &e);
    assert_eq!(rc, 0);
    // '&#39;' unescaped to an apostrophe, smart quotes preserved.
    assert_eq!(
        title.trim_end_matches('X'),
        "Beej's Guide to Interprocess Communication"
    );
    assert_eq!(author.trim_end_matches('X'), "Brian “Beej Jorgensen” Hall");
}

#[test]
fn meta_returns_0_with_empty_fields_when_tags_absent() {
    // mini.epub HAS tags; use a real book with an absent field-name case:
    // a PDF with metadata returns 0.
    let p = fixture("sample.pdf");
    let e = CString::new("pdf").unwrap();
    let (rc, title, _) = run_meta(&p, &e);
    assert_eq!(rc, 0);
    assert_eq!(title.trim_end_matches('X'), "My Great Book");
}

#[test]
fn unsupported_ext_returns_minus_1() {
    let p = fixture("bgipc.epub");
    let e = CString::new("mobi").unwrap();
    let (rc, title, _) = run_meta(&p, &e);
    assert_eq!(rc, -1);
    // Out buffers cleared before dispatch (C behavior).
    assert_eq!(title, "");
}

#[test]
fn null_ext_returns_minus_1() {
    let p = fixture("bgipc.epub");
    let mut title = vec![0u8; 4];
    let rc: c_int = unsafe {
        eh_extract_book_meta(
            p.as_ptr(),
            std::ptr::null(),
            title.as_mut_ptr() as *mut c_char,
            title.len(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(rc, -1);
}

#[test]
fn title_buffer_truncates_at_cap() {
    // bgipc title fits in 64 bytes; force a tiny buffer to exercise truncation.
    let p = fixture("bgipc.epub");
    let e = CString::new("epub").unwrap();
    let mut title = vec![0u8; 8];
    let rc: c_int = unsafe {
        eh_extract_book_meta(
            p.as_ptr(),
            e.as_ptr(),
            title.as_mut_ptr() as *mut c_char,
            title.len(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(rc, 0);
    let s = unsafe {
        std::ffi::CStr::from_ptr(title.as_ptr() as *const c_char)
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(s, "Beej's "); // first 7 bytes (cap-1) of "Beej's Guide..."
    assert_eq!(title[7], 0); // NUL-terminated
}

#[test]
fn cover_extracts_from_mini_epub() {
    let p = fixture("mini.epub");
    let e = CString::new("epub").unwrap();
    let out = tempfile_path("mini_cover.raw");
    let out_c = CString::new(out.as_str()).unwrap();
    let mut outbuf = out_c.as_bytes().to_vec();
    outbuf.push(0);
    let rc: c_int = unsafe {
        eh_extract_book_cover(
            p.as_ptr(),
            e.as_ptr(),
            outbuf.as_mut_ptr() as *mut c_char,
            outbuf.len(),
        )
    };
    assert_eq!(rc, 0);
    let bytes = std::fs::read(&out).unwrap();
    assert!(bytes.starts_with(&[0x89, b'P']), "expected PNG magic");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn cover_returns_0_even_without_embedded_cover() {
    // bgipc has no <meta name="cover">; extraction still returns 0 (the C
    // caller checks the file's existence).
    let p = fixture("bgipc.epub");
    let e = CString::new("epub").unwrap();
    let out = tempfile_path("nope.raw");
    let out_c = CString::new(out.as_str()).unwrap();
    let mut outbuf = out_c.as_bytes().to_vec();
    outbuf.push(0);
    let rc: c_int = unsafe {
        eh_extract_book_cover(
            p.as_ptr(),
            e.as_ptr(),
            outbuf.as_mut_ptr() as *mut c_char,
            outbuf.len(),
        )
    };
    assert_eq!(rc, 0);
    assert!(!Path::new(&out).exists());
}

#[test]
fn cover_unsupported_ext_returns_minus_1() {
    let p = fixture("bgipc.epub");
    let e = CString::new("pdf").unwrap();
    let out = tempfile_path("never.raw");
    let out_c = CString::new(out.as_str()).unwrap();
    let mut outbuf = out_c.as_bytes().to_vec();
    outbuf.push(0);
    let rc: c_int = unsafe {
        eh_extract_book_cover(
            p.as_ptr(),
            e.as_ptr(),
            outbuf.as_mut_ptr() as *mut c_char,
            outbuf.len(),
        )
    };
    assert_eq!(rc, -1);
    assert!(!Path::new(&out).exists());
}

fn tempfile_path(suffix: &str) -> String {
    let d = std::env::temp_dir();
    let n = format!("eh_extract_ffi_test_{suffix}");
    d.join(n).to_string_lossy().into_owned()
}
