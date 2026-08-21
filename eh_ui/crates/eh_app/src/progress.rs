//! Reading progress — percent-read per book from the firmware explorer db
//! (C eh_progress.c + eh_plat_progress_read).
//!
//! Percent-read comes from the firmware's `books_settings` table: the
//! integrated reader writes cpage/npage while reading, and the KOReader
//! pocketbooksync plugin writes into the very same rows — so one query
//! serves both.  The shelf renders the percent as a black bar at each
//! cover's bottom edge (see `shelf::draw_progress_bar`).
//!
//! Like the C app, the live db (+ `-wal` + `-shm`) is copied to a scratch
//! snapshot first: opening a live WAL set can block on a non-writable
//! guest, and the copy keeps the read off the live files entirely.
//! Every error is ignored — progress just stays empty.  On host/SDL
//! there is no explorer db; `PBEMU_EXPLORER_DB` points [`reload`] at a
//! fixture for tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Percent read (0..=100) keyed by local file path (C BsProgressEntry).
pub type ProgressMap = HashMap<String, u8>;

/// The firmware explorer db (C eh_plat_progress_db).
const EXPLORER_DB: &str = "/mnt/ext1/system/explorer-3/explorer-3.db";
/// Scratch snapshot the source is copied into before reading
/// (C eh_plat_progress_snap).
const SNAPSHOT_DB: &str = "/tmp/progress_import.db";
/// Upper bound for the fallback copy (C EH_PROGRESS_COPY_MAX): the
/// explorer db is a handful of MB at most; refusing anything pathological
/// keeps a huge source from stalling the caller.
const COPY_MAX: u64 = 64 * 1024 * 1024;

/// Host/test hook overriding the firmware db location.
const ENV_EXPLORER_DB: &str = "PBEMU_EXPLORER_DB";

/// The C eh_plat_progress_read query: books_settings ⋈ files ⋈ folders,
/// restricted to rows that actually carry page data.
const PROGRESS_SQL: &str = "SELECT fol.name, f.filename, bs.cpage, bs.npage \
     FROM books_settings bs \
     JOIN files f ON f.book_id = bs.bookid \
     JOIN folders fol ON fol.id = f.folder_id \
     WHERE bs.npage IS NOT NULL AND bs.npage > 0";

/// Source db path (`PBEMU_EXPLORER_DB` overrides the firmware location).
fn source_db() -> PathBuf {
    std::env::var_os(ENV_EXPLORER_DB)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(EXPLORER_DB))
}

/// Reload the progress map from the explorer db (C eh_progress_reload):
/// refresh the snapshot, then parse it.  Cheap enough to call at startup
/// and whenever the shelf is shown again after reading.  Returns an empty
/// map when the db is absent/unreadable (host, corrupt file).
pub fn reload() -> ProgressMap {
    let snap = PathBuf::from(SNAPSHOT_DB);
    snapshot(&source_db(), &snap);
    read_db(&snap)
}

/// Refresh the snapshot copy of `src` into `dst` (C progress_snapshot):
/// db + `-wal` + `-shm`, skipping a snapshot already at least as new as
/// the source, dropping a stale one when the source vanished, removing a
/// truncated copy so it is never read back as valid.
fn snapshot(src_root: &Path, dst_root: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut src = src_root.as_os_str().to_os_string();
        src.push(suffix);
        let src = PathBuf::from(src);
        let mut dst = dst_root.as_os_str().to_os_string();
        dst.push(suffix);
        let dst = PathBuf::from(dst);
        let md = match std::fs::metadata(&src) {
            Ok(md) => md,
            Err(_) => {
                let _ = std::fs::remove_file(&dst);
                continue;
            }
        };
        // Bound: refuse to copy a pathological (or non-file) source.
        if !md.is_file() || md.len() > COPY_MAX {
            continue;
        }
        // Skip when the snapshot is already at least as new as the source
        // (a fresh-enough copy from a prior reload).
        if let (Ok(sm), Ok(dm)) = (md.modified(), std::fs::metadata(&dst).and_then(|m| m.modified())) {
            if dm >= sm {
                continue;
            }
        }
        if std::fs::copy(&src, &dst).is_err() {
            let _ = std::fs::remove_file(&dst);
        }
    }
}

/// Parse one explorer db into the progress map.  Opens read-write without
/// create (like the C sqlite3_open_v2 READWRITE: recovering a WAL snapshot
/// may need to write) so a missing file fails instead of being created.
pub fn read_db(path: &Path) -> ProgressMap {
    let conn = match rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    ) {
        Ok(c) => c,
        Err(e) => {
            crate::log(&format!("[eh_app] progress: cannot open {}: {e}", path.display()));
            return ProgressMap::new();
        }
    };
    match read_progress(&conn) {
        Ok(map) => {
            crate::log(&format!("[eh_app] progress: {} entries", map.len()));
            map
        }
        Err(e) => {
            crate::log(&format!("[eh_app] progress: query failed: {e}"));
            ProgressMap::new()
        }
    }
}

/// The books_settings ⋈ files ⋈ folders walk (C eh_plat_progress_read):
/// `<folder>/<filename>` → integer percent clamped to 0..=100.
fn read_progress(conn: &rusqlite::Connection) -> rusqlite::Result<ProgressMap> {
    let mut stmt = conn.prepare(PROGRESS_SQL)?;
    let mut rows = stmt.query([])?;
    let mut map = ProgressMap::new();
    while let Some(row) = rows.next()? {
        let folder: Option<String> = row.get(0)?;
        let file: Option<String> = row.get(1)?;
        let (Some(folder), Some(file)) = (folder, file) else {
            continue;
        };
        let cpage: i64 = row.get(2)?;
        let npage: i64 = row.get(3)?;
        if npage <= 0 {
            continue;
        }
        // C: pct = cpage*100/npage; pct < 1 → 0; pct > 100 → 100.
        let pct = (cpage * 100 / npage).clamp(0, 100) as u8;
        map.insert(format!("{folder}/{file}"), pct);
    }
    Ok(map)
}

/// Percent read (0..=100) for a book file, 0 when unknown
/// (C eh_progress_percent).
pub fn percent(map: &ProgressMap, path: &str) -> u8 {
    if path.is_empty() {
        return 0;
    }
    map.get(path).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create an explorer-schema fixture: folders(id,name),
    /// files(book_id,filename,folder_id),
    /// books_settings(bookid,cpage,npage).
    fn make_fixture(path: &Path) {
        let conn = rusqlite::Connection::open(path).expect("create fixture");
        conn.execute_batch(
            "CREATE TABLE folders (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE files (book_id INTEGER, filename TEXT, folder_id INTEGER);
             CREATE TABLE books_settings (bookid INTEGER PRIMARY KEY, cpage INTEGER, npage INTEGER);
             INSERT INTO folders VALUES (1, '/mnt/ext1/books');
             INSERT INTO files VALUES (10, 'alpha.epub', 1);
             INSERT INTO files VALUES (11, 'beta.pdf', 1);
             INSERT INTO files VALUES (12, 'gamma.fb2', 1);
             INSERT INTO files VALUES (13, 'delta.epub', 1);
             -- half read
             INSERT INTO books_settings VALUES (10, 50, 100);
             -- clamps: past the end → 100, zero page → excluded
             INSERT INTO books_settings VALUES (11, 120, 100);
             INSERT INTO books_settings VALUES (12, 30, 0);
             -- rounding floors below 1% → 0
             INSERT INTO books_settings VALUES (13, 1, 250);",
        )
        .expect("seed fixture");
    }

    #[test]
    fn join_maps_folder_filename_paths_and_clamps() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let db = tmp.path().join("explorer-3.db");
        make_fixture(&db);
        let map = read_db(&db);
        assert_eq!(percent(&map, "/mnt/ext1/books/alpha.epub"), 50);
        assert_eq!(percent(&map, "/mnt/ext1/books/beta.pdf"), 100);
        // npage = 0 row is filtered out by the WHERE clause.
        assert_eq!(percent(&map, "/mnt/ext1/books/gamma.fb2"), 0);
        // 1*100/250 = 0 (integer division floors below 1%).
        assert_eq!(percent(&map, "/mnt/ext1/books/delta.epub"), 0);
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn negative_current_page_clamps_to_zero() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let db = tmp.path().join("explorer-3.db");
        let conn = rusqlite::Connection::open(&db).expect("create");
        conn.execute_batch(
            "CREATE TABLE folders (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE files (book_id INTEGER, filename TEXT, folder_id INTEGER);
             CREATE TABLE books_settings (bookid INTEGER PRIMARY KEY, cpage INTEGER, npage INTEGER);
             INSERT INTO folders VALUES (1, '/books');
             INSERT INTO files VALUES (1, 'x.epub', 1);
             INSERT INTO books_settings VALUES (1, -5, 100);",
        )
        .expect("seed");
        let map = read_db(&db);
        assert_eq!(percent(&map, "/books/x.epub"), 0);
    }

    #[test]
    fn null_columns_are_skipped() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let db = tmp.path().join("explorer-3.db");
        let conn = rusqlite::Connection::open(&db).expect("create");
        conn.execute_batch(
            "CREATE TABLE folders (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE files (book_id INTEGER, filename TEXT, folder_id INTEGER);
             CREATE TABLE books_settings (bookid INTEGER PRIMARY KEY, cpage INTEGER, npage INTEGER);
             INSERT INTO folders VALUES (1, '/books');
             INSERT INTO folders VALUES (2, NULL);
             INSERT INTO files VALUES (1, 'a.epub', 2);   -- NULL folder name
             INSERT INTO files VALUES (2, NULL, 1);       -- NULL filename
             INSERT INTO files VALUES (3, 'c.epub', 1);
             INSERT INTO books_settings VALUES (1, 10, 100);
             INSERT INTO books_settings VALUES (2, 10, 100);
             INSERT INTO books_settings VALUES (3, 25, 100);",
        )
        .expect("seed");
        let map = read_db(&db);
        assert_eq!(map.len(), 1);
        assert_eq!(percent(&map, "/books/c.epub"), 25);
    }

    #[test]
    fn missing_db_degrades_to_empty() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let map = read_db(&tmp.path().join("nope.db"));
        assert!(map.is_empty());
    }

    #[test]
    fn env_override_routes_reload_at_fixture() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let db = tmp.path().join("explorer-3.db");
        make_fixture(&db);
        // Also exercise the snapshot dance: reload copies db (+ sidecars,
        // here none) into the scratch snapshot before reading.
        std::env::set_var(ENV_EXPLORER_DB, &db);
        let map = reload();
        std::env::remove_var(ENV_EXPLORER_DB);
        assert_eq!(percent(&map, "/mnt/ext1/books/alpha.epub"), 50);
        assert_eq!(percent(&map, "/mnt/ext1/books/beta.pdf"), 100);
    }

    #[test]
    fn snapshot_copies_sidecars_and_survives_missing_source() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let src = tmp.path().join("live.db");
        make_fixture(&src);
        std::fs::write(tmp.path().join("live.db-wal"), b"wal-bytes").expect("wal");
        let dst = tmp.path().join("snap.db");
        snapshot(&src, &dst);
        assert!(dst.exists());
        assert!(tmp.path().join("snap.db-wal").exists());
        assert_eq!(read_db(&dst).len(), 3);
        // Source vanishes → stale snapshot files are dropped, not reused.
        std::fs::remove_file(&src).expect("remove src");
        snapshot(&src, &dst);
        assert!(!dst.exists());
    }

    #[test]
    fn percent_lookup_edges() {
        let mut map = ProgressMap::new();
        map.insert("/b/x.epub".to_string(), 42);
        assert_eq!(percent(&map, "/b/x.epub"), 42);
        assert_eq!(percent(&map, "/b/other.epub"), 0);
        assert_eq!(percent(&map, ""), 0);
    }
}
