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

pub mod client;
pub mod config;
pub mod cover;
pub mod store;
pub mod sync;
pub mod util;