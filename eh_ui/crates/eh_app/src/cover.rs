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

/// Resolve the covers dir (next to the store/config file), creating it.
pub fn resolve_covers_dir(app_dir: &Path) -> PathBuf {
    let dir = app_dir.join(COVERS_SUBDIR);
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777));
    dir
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
    let rgb: Vec<u8> = match ct.samples() {
        4 => out.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]).collect(),
        3 => out,
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
        n if n == (w as usize) * (h as usize) * 4 => {
            pixels.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]).collect()
        }
        _ => {
            return Err(format!("jpeg: unexpected pixel count"));
        }
    };
    Ok((w, h, rgb))
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
        assert_eq!(p.file_name().and_then(|n| n.to_str()), Some(format!("{safe}.png").as_str()));
        assert_eq!(p.parent().unwrap().file_name().and_then(|n| n.to_str()), Some(b.as_str()));
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
}