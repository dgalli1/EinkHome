//! eh_app — the real EinkHome application in Rust.
//!
//! This is the all-Rust replacement for the C bookshelf.  It talks to the
//! provider-neutral pbemu-api REST surface (the same `/api/v1` endpoints the
//! C app used) and persists to the same SQLite schema, so an existing device
//! library carries over unchanged.
//!
//! Layer order:
//!   config   — reads bookshelf.cfg (api_url/api_token)
//!   store    — SQLite persistence (schema-compatible with the C app)
//!   sync     — delta engine (sync/delta + sync/state)
//!   cover    — cover fetch + on-disk cache
//!   ui*      — shell widgets bound to real data
//!
//! The shelf screen renders real books (title/author/cover) instead of
//! placeholder tiles, driven by the sync + store layers.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]
pub mod app;
pub mod appui;
pub mod client;
pub mod choosers;
pub mod config;
pub mod context_menu;
pub mod cover;
pub mod downloads;
pub mod extract;
pub mod i18n;
pub mod input;
pub mod launcher;
pub mod logger;
pub mod menu;
pub mod pages;
pub mod progress;
pub mod local;
pub mod reader;
pub mod search;
pub mod settings;
pub mod shelf;
pub mod source;
pub mod store;
pub mod sync;
pub mod sysapp;
pub mod util;
pub mod widgets;
pub mod viewer;

/// Simple diagnostic logger.  On the device this writes to the same
/// guest-writable path the demo used; on host it prints to stderr.  Hook
/// point for the real logging backend later.
pub fn log(msg: &str) {
    // The e2e harness reads bookshelf.log (see `logger`); on the host we
    // also mirror to stderr.  Kept cheap + non-fatal.
    crate::logger::log(&format!("[eh_app] {msg}"));
    #[cfg(target_arch = "arm")]
    {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/eh_app.log")
        {
            use std::io::Write;
            let _ = writeln!(f, "{msg}");
        }
    }
}

#[cfg(test)]
pub mod testutil {
    use crate::client::BookMeta;

    /// A BookMeta fixture with just an id + title.
    pub fn book(id: &str, title: &str) -> BookMeta {
        BookMeta { id: id.into(), title: title.into(), ..Default::default() }
    }
}