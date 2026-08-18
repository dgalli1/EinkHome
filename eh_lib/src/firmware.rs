//! Firmware libc compatibility shims (ARM, EABI only).
//!
//! PocketBook's firmware glibc 2.23 does not export the plain 64-bit stat
//! aliases (`stat64`/`lstat64`/`fstat64`/`fstatat64`) that Rust `std`'s
//! filesystem code links against — only the legacy `__*stat64` family.  The
//! C app works around the same quirk for `stat` via a manual
//! `extern int __xstat(...)` (see app/data/eh_local.c).
//!
//! These shims forward the Rust-expected names to the firmware's
//! `__*stat64` entry points with kernel stat version 0 (the ARM value, per
//! the C workaround).  The `buf` pointer is passed through verbatim, so the
//! `struct stat64` layout must already match glibc's — it does, because Rust
//! `std` builds the struct from the same glibc ABI that the firmware fills.
//!
//! Gated to `target_arch = "arm"`: the host x86_64 build must NOT export
//! these (real glibc already provides `stat64`; defining it would be a
//! duplicate symbol at link time).

extern "C" {
    fn __xstat64(
        ver: std::ffi::c_int,
        path: *const std::ffi::c_char,
        buf: *mut std::ffi::c_void,
    ) -> std::ffi::c_int;
    fn __lxstat64(
        ver: std::ffi::c_int,
        path: *const std::ffi::c_char,
        buf: *mut std::ffi::c_void,
    ) -> std::ffi::c_int;
    fn __fxstat64(
        ver: std::ffi::c_int,
        fd: std::ffi::c_int,
        buf: *mut std::ffi::c_void,
    ) -> std::ffi::c_int;
    fn __fxstatat64(
        ver: std::ffi::c_int,
        fd: std::ffi::c_int,
        path: *const std::ffi::c_char,
        buf: *mut std::ffi::c_void,
        flags: std::ffi::c_int,
    ) -> std::ffi::c_int;
}

const STAT_VER_KERNEL: std::ffi::c_int = 0;

#[no_mangle]
pub unsafe extern "C" fn stat64(
    path: *const std::ffi::c_char,
    buf: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    unsafe { __xstat64(STAT_VER_KERNEL, path, buf) }
}

#[no_mangle]
pub unsafe extern "C" fn lstat64(
    path: *const std::ffi::c_char,
    buf: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    unsafe { __lxstat64(STAT_VER_KERNEL, path, buf) }
}

#[no_mangle]
pub unsafe extern "C" fn fstat64(
    fd: std::ffi::c_int,
    buf: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    unsafe { __fxstat64(STAT_VER_KERNEL, fd, buf) }
}

#[no_mangle]
pub unsafe extern "C" fn fstatat64(
    fd: std::ffi::c_int,
    path: *const std::ffi::c_char,
    buf: *mut std::ffi::c_void,
    flags: std::ffi::c_int,
) -> std::ffi::c_int {
    unsafe { __fxstatat64(STAT_VER_KERNEL, fd, path, buf, flags) }
}
