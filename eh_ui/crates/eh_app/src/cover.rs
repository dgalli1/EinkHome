//! Cover fetch + on-disk cache, layout-compatible with the C app.
//!
//! Cache paths match the C app's eh_cover_cache_path: `covers/<bucket>/
//! <sanitized-id>.png` under the app dir (next to the config/store), where
//! `<bucket>` is the low byte of a FNV-1a hash written as 2 hex chars
//! (256 sharded dirs so a large library never piles one directory, matching
//! the C sharding), and `<sanitized-id>` is the book id with `/` → `_`.
//!
//! A Rust app can therefore read covers already cached by the C app, and
//! vice-versa.

use std::path::{Path, PathBuf};

use crate::client::ApiClient;

use eh_hal::Framebuffer;

use crate::app::{App, Source};

/// Cache subdir name (the C app's EH_COVERS_SUBDIR).
pub const COVERS_SUBDIR: &str = "covers";

/// Sanitise an id for use as a filename: `/` → `_` (the only char that would
/// cross a path boundary), mirroring the C app's cover_sanitize.
pub fn sanitize(id: &str) -> String {
    id.replace('/', "_")
}

/// FNV-1a 32-bit hash (same constants as the C app's cover_bucket_of).
pub fn bucket_of(safe: &str) -> String {
    let mut h: u32 = 2166136261;
    for b in safe.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    // Low byte as 2 lowercase hex chars.
    format!("{:02x}", h & 0xff)
}

/// The on-disk PNG path for a book id, under `covers_dir`.
pub fn cache_path(covers_dir: &Path, id: &str) -> PathBuf {
    let safe = sanitize(id);
    let bucket = bucket_of(&safe);
    covers_dir.join(bucket).join(format!("{safe}.png"))
}

/// Resolve the covers dir (next to the store/config file), creating it,
/// and run the one-time raw-cache migration ([`validate_raw_cache`]).
pub fn resolve_covers_dir(app_dir: &Path) -> PathBuf {
    let dir = app_dir.join(COVERS_SUBDIR);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777));
    validate_raw_cache(&dir);
    dir
}

/// Path of the RAW extracted cover for a local book — the exact bytes
/// pulled from the file (PNG or JPEG), sharded like [`cache_path`] but
/// with a `.raw` suffix (C eh_cover_raw_path).  Persisting them makes a
/// later view skip re-opening the book file.
pub fn raw_path(covers_dir: &Path, id: &str) -> PathBuf {
    let safe = sanitize(id);
    let bucket = bucket_of(&safe);
    covers_dir.join(bucket).join(format!("{safe}.raw"))
}

/// Atomically install raw extracted-cover bytes for `id`.
pub fn store_raw(covers_dir: &Path, id: &str, bytes: &[u8]) -> std::io::Result<()> {
    let path = raw_path(covers_dir, id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("raw.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

/// Cache-format stamp of the RAW extracted-cover cache (`.raw` files).
/// Bump whenever extraction output changes meaningfully: a mismatch
/// wipes the whole raw cache once at boot, so stale bytes never outlive
/// the fix that invalidates them — old no-cover tombstones from builds
/// without a decoder, first-page renders from before a renderer fix.
pub const RAW_CACHE_VERSION: u32 = 2;

const RAW_VER_FILE: &str = ".raw_ver";

/// Drop every cached `.raw` when the stamp is missing or from another
/// version, then write the current stamp.  The `.png` cache (server
/// covers) is untouched — only local extraction output resets.
pub fn validate_raw_cache(covers_dir: &Path) {
    let stamp = covers_dir.join(RAW_VER_FILE);
    if std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == RAW_CACHE_VERSION.to_string()) {
        return;
    }
    if let Ok(buckets) = std::fs::read_dir(covers_dir) {
        for bucket in buckets.flatten() {
            let p = bucket.path();
            if !p.is_dir() {
                continue; // the stamp file itself lives at the root
            }
            if let Ok(files) = std::fs::read_dir(&p) {
                for f in files.flatten() {
                    let fp = f.path();
                    if fp.extension().is_some_and(|x| x == "raw" || x == "tmp") {
                        let _ = std::fs::remove_file(&fp);
                    }
                }
            }
        }
    }
    let _ = std::fs::write(&stamp, format!("{RAW_CACHE_VERSION}\n"));
}

/// Return the cached cover PNG bytes for `id`, if present.
pub fn load_cached(covers_dir: &Path, id: &str) -> Option<Vec<u8>> {
    let p = cache_path(covers_dir, id);
    std::fs::read(p).ok()
}

/// Fetch a cover from the API and persist it to the cache.  Returns the
/// PNG bytes (also written to disk).  Idempotent: an existing cache entry is
/// returned without a network round-trip.
pub fn fetch(client: &ApiClient, covers_dir: &Path, id: &str) -> Result<Vec<u8>, String> {
    if let Some(cached) = load_cached(covers_dir, id) {
        return Ok(cached);
    }
    let bytes = client.cover(id)?;
    if bytes.is_empty() {
        return Err("empty cover".into());
    }
    let path = cache_path(covers_dir, id);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Atomic install: a reader on another thread (the warm pass fetches
    // off-thread while the UI decodes) must never see a truncated file —
    // a partial image aborts the whole process under panic=abort.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(bytes)
}

use std::os::unix::fs::PermissionsExt;

use std::io::Cursor;

/// Decode PNG or JPEG bytes to 8-bit RGB (w, h, rgb-triples).  The raw
/// `/cover` endpoint serves JPEGs (240x360 for Kavita); the on-disk cache
/// stores a normalized copy (PNG when re-encoded, or the source bytes).
/// The shell's Cover consumes raw RGB.
pub fn decode_rgb(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    // The PNG/JPEG decoders can panic on malformed input (a truncated
    // cache file read mid-write); a panic here would abort the whole
    // process, so fence them and degrade to a decode error.
    let res = std::panic::catch_unwind(|| {
        if bytes.starts_with(b"\xff\xd8") {
            decode_jpeg(bytes)
        } else {
            decode_png(bytes)
        }
    });
    res.unwrap_or_else(|_| Err("decoder panicked on malformed input".into()))
}

fn decode_png(png: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(Cursor::new(png));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png header: {e}"))?;
    let mut out = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut out)
        .map_err(|e| format!("png frame: {e}"))?;
    let (ct, _bd) = reader.output_color_type();
    // EXPAND+STRIP_16 give 8-bit samples; 3 samples = RGB, 4 = RGBA.
    // Forgiving on grayscale covers (1 sample: our generated TXT covers
    // and real-world scans): replicate the sample to RGB.
    let rgb: Vec<u8> = match ct.samples() {
        4 => out
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|px| [px[0], px[1], px[2]])
            .collect(),
        3 => out,
        2 => out
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|px| [px[0], px[0], px[0]])
            .collect(),
        1 => out.iter().flat_map(|&v| [v, v, v]).collect(),
        n => return Err(format!("unsupported decoded samples={n}")),
    };
    Ok((info.width, info.height, rgb))
}

fn decode_jpeg(jpeg: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(jpeg));
    let pixels = decoder.decode().map_err(|e| format!("jpeg decode: {e}"))?;
    let info = decoder.info().ok_or_else(|| "jpeg: no info".to_string())?;
    // decode() returns RGB or RGBA depending on the source; normalize to RGB.
    let w = info.width as u32;
    let h = info.height as u32;
    let rgb = match pixels.len() {
        n if n == (w as usize) * (h as usize) * 3 => pixels,
        n if n == (w as usize) * (h as usize) * 4 => pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|px| [px[0], px[1], px[2]])
            .collect(),
        // Grayscale JPEGs (1 byte per pixel — Kavita serves some covers
        // this way) replicate to RGB.
        n if n == (w as usize) * (h as usize) => pixels.iter().flat_map(|&g| [g, g, g]).collect(),
        _ => {
            return Err("jpeg: unexpected pixel count".to_string());
        }
    };
    Ok((w, h, rgb))
}

/// Shared state of the full-library cover-warm pass (the C
/// eh_cover_warm_* globals): the atomics live behind Arcs because the
/// persistent worker thread mutates them off the UI thread.
/// (C's bcov weak timer fetched one cover per main-loop tick; a
/// spawn-per-cover model here retained ~180MB of glibc arena at 100k
/// books, so the pass runs on one long-lived drainer.)
pub(crate) struct WarmHandle {
    /// Ids queued by the current pass (progress denominator).
    pub total: usize,
    /// Remote ids still to fetch.
    pub remaining: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Worker alive (draining or paused offline).
    pub active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Live network gate the worker polls between fetches.
    pub online: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Default for WarmHandle {
    fn default() -> Self {
        WarmHandle {
            total: 0,
            remaining: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            // Assume online until the first tick probes the hal gate.
            online: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
}

impl WarmHandle {
    /// Covers already fetched by the current pass (progress numerator).
    pub fn done(&self) -> u32 {
        self.total
            .saturating_sub(self.remaining.load(std::sync::atomic::Ordering::Relaxed))
            as u32
    }
}

impl<B: Framebuffer> App<B> {
    /// Start the background full-library cover-warm pass (C
    /// eh_cover_warm_start, run after a remote sync on the Kavita
    /// source): every server book's cover lands in the on-disk cache so
    /// offline launches still show real covers — not just the pages the
    /// user happened to view.
    pub(crate) fn cover_warm_start(&mut self) {
        if self.source != Source::Kavita {
            return;
        }
        let ids: Vec<String> = self
            .store
            .list_books(1_000_000, 0)
            .unwrap_or_default()
            .into_iter()
            .map(|b| b.id)
            .collect();
        self.warm.total = ids.len();
        self.warm
            .remaining
            .store(ids.len(), std::sync::atomic::Ordering::Relaxed);
        if ids.is_empty() {
            return;
        }
        self.warm
            .active
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // One persistent worker drains the queue (C: one fetch per bcov
        // tick).  A spawn-per-cover model created 100k threads for a
        // full-library pass; glibc arena retention put RSS at ~200MB.
        let remaining = self.warm.remaining.clone();
        let active = self.warm.active.clone();
        let online = self.warm.online.clone();
        let client = self.client.clone();
        let covers_dir = self.covers_dir.clone();
        let _ = std::thread::Builder::new()
            .name("cover-warm".into())
            .spawn(move || {
                let mut ids = ids;
                while let Some(id) = ids.pop() {
                    if !online.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(200));
                        continue;
                    }
                    remaining.store(ids.len(), std::sync::atomic::Ordering::Relaxed);
                    if load_cached(&covers_dir, &id).is_some() {
                        continue;
                    }
                    let _ = fetch(&client, &covers_dir, &id);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                remaining.store(0, std::sync::atomic::Ordering::Relaxed);
                active.store(false, std::sync::atomic::Ordering::Relaxed);
            });
    }
    /// Drain the warm pass: at most one fetch handed to a background
    /// thread per call (the C pass arms its bcov weak timer per fetch),
    /// skipped entirely offline.  The network call MUST NOT run on the
    /// UI thread — a blocking fetch here stalls event processing for the
    /// whole request duration and the shell feels dead after boot.
    pub(crate) fn cover_warm_tick(&mut self) {
        // The worker thread polls this gate between fetches.
        let online = self.fb().net_active();
        self.warm
            .online
            .store(online, std::sync::atomic::Ordering::Relaxed);
    }
    /// True while the full-library warm pass still has covers to fetch
    /// (C eh_cover_warm_active); offline counts as drained — the pass is
    /// gated off offline and would otherwise pin the sheet forever.
    pub(crate) fn cover_warm_active(&mut self) -> bool {
        // Safe from overlay draws (screen take()n during present): use
        // the live probe when available, else the cached value.  Offline
        // counts as drained (the worker pauses, C gated it the same way).
        let online = if let Some(fb) = self.fb.as_mut() {
            let net = fb.net_active();
            self.fb_net_active = net;
            net
        } else {
            self.fb_net_active
        };
        online
            && self
                .warm
                .remaining
                .load(std::sync::atomic::Ordering::Relaxed)
                > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_matches_c_layout() {
        // Sanitize + bucket are pure functions; verify they produce
        // deterministic 2-hex buckets and the expected path shape.
        let id = "kavita_ch_1";
        let safe = sanitize(id);
        let b = bucket_of(&safe);
        assert_eq!(safe, id); // no slashes
        assert_eq!(b.len(), 2);
        assert!(b.chars().all(|c| c.is_ascii_hexdigit()));
        let dir = tempfile::tempdir().unwrap();
        let p = cache_path(dir.path(), id);
        assert_eq!(
            p.file_name().and_then(|n| n.to_str()),
            Some(format!("{safe}.png").as_str())
        );
        assert_eq!(
            p.parent().unwrap().file_name().and_then(|n| n.to_str()),
            Some(b.as_str())
        );
    }

    #[test]
    fn cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let covers = resolve_covers_dir(dir.path());
        let p = cache_path(&covers, "abc/def");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"PNG").unwrap();
        assert_eq!(load_cached(&covers, "abc/def").unwrap(), b"PNG");
        // sanitize path: slug must not contain '/'
        assert!(!sanitize("abc/def").contains('/'));
    }

    #[test]
    fn warm_handle_starts_online_and_progress_tracks_total_minus_remaining() {
        use std::sync::atomic::Ordering;
        let mut w = WarmHandle::default();
        // The gate assumes online until the first tick probes the hal.
        assert!(w.online.load(Ordering::Relaxed));
        assert_eq!(w.done(), 0);

        w.total = 10;
        w.remaining.store(3, Ordering::Relaxed);
        assert_eq!(w.done(), 7);

        // A re-armed pass with a lagging counter saturates at zero
        // instead of underflowing the progress bar.
        w.total = 5;
        w.remaining.store(9, Ordering::Relaxed);
        assert_eq!(w.done(), 0);
    }
    #[test]
    fn raw_cache_invalidation_wipes_stale_covers_once() {
        let dir = tempfile::tempdir().unwrap();
        let covers = resolve_covers_dir(dir.path());
        // A good render, a poisoned tombstone, and a server PNG that
        // must survive the wipe.
        store_raw(&covers, "bookA", b"\x89PNG-bytes").unwrap();
        store_raw(&covers, "bookB", &[]).unwrap();
        let png = cache_path(&covers, "srv");
        std::fs::create_dir_all(png.parent().unwrap()).unwrap();
        std::fs::write(&png, b"PNG").unwrap();

        // Simulate the previous cache generation.
        let stamp = covers.join(".raw_ver");
        std::fs::write(&stamp, "1\n").unwrap();

        validate_raw_cache(&covers);
        assert!(!raw_path(&covers, "bookA").exists(), "stale render kept");
        assert!(!raw_path(&covers, "bookB").exists(), "tombstone kept");
        assert_eq!(std::fs::read(&png).unwrap(), b"PNG", "server cache hit");
        assert_eq!(
            std::fs::read_to_string(&stamp).unwrap().trim(),
            RAW_CACHE_VERSION.to_string()
        );

        // Current stamp: a re-run is a no-op (fresh writes survive).
        store_raw(&covers, "bookC", b"new").unwrap();
        validate_raw_cache(&covers);
        assert_eq!(std::fs::read(raw_path(&covers, "bookC")).unwrap(), b"new");
    }
}
