//! SQLite persistence — schema-compatible with the C app's eh_store.c.
//!
//! The `books`/`meta` tables match the C store's schema exactly, so an
//! existing device library (`bookshelf_lib.db` next to the config) carries
//! over unchanged.  A re-sync keeps an already-downloaded book's
//! `downloaded`/`local_path` state (the same INSERT OR REPLACE + lookup the
//! C app does).
//!
//! This module implements the row CRUD the shelf needs.  FTS5 search,
//! suggest/rank, legacy-JSON import and the materialised `view` projection
//! are intentionally NOT ported yet — they're deferred to the slice that
//! uses them.

use rusqlite::{Connection, OptionalExtension, params};

use crate::client::BookMeta;

pub const EH_LIB_DB_FILENAME: &str = "bookshelf_lib.db";
/// Column names + types the C app's store_migrate_columns() adds to stores
/// created by older builds (CREATE TABLE IF NOT EXISTS leaves old shapes
/// untouched).  Mirrored verbatim for byte-compatible DBs.
const MIGRATE_COLUMNS: &[(&str, &str)] = &[
    ("series_idx", "REAL"),
    ("ext", "TEXT"),
    ("size", "INTEGER"),
    ("downloaded", "INTEGER"),
    ("local_path", "TEXT"),
    ("added_at", "INTEGER"),
    ("filename", "TEXT"),
    ("source", "TEXT"),
    ("genre", "TEXT"),
    ("search_text", "TEXT"),
];

/// A persisted book row (the slice of BsBook the shelf shows).
#[derive(Debug, Clone, Default)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: String,
    pub series: String,
    pub series_id: String,
    pub series_idx: f64,
    pub ext: String,
    pub size: i64,
    pub downloaded: bool,
    pub local_path: String,
    pub added_at: i64,
    pub filename: String,
    pub source: String,
    pub search_text: String,
    pub genre: String,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// The store DB filename next to the config (C: EH_LIB_DB_FILENAME).
    pub const LIB_DB_FILENAME: &'static str = EH_LIB_DB_FILENAME;
    /// Open (creating if needed) the store at `path`, applying the schema +
    /// column migrations.  Fails loudly on a corrupt/undecodable DB.
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Store> {
        let conn = Connection::open(path)?;
        // Same as the C app: one connection, journal mode untouched (WAL
        // hammers device flash), a transient lock holder should delay us not
        // fail with SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        apply_schema(&conn)?;
        Ok(Store { conn })
    }

    /// Number of books in the library.
    pub fn count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM books", [], |r| r.get(0))
    }

    /// The last-applied sync cursor (persisted in the `meta` table, same as
    /// the C app's eh_store_set_cursor).  0 = never synced.
    pub fn cursor(&self) -> rusqlite::Result<i64> {
        let raw: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key='cursor'", [], |r| r.get(0))
            .optional()?;
        match raw {
            None => Ok(0),
            Some(v) => Ok(v.parse::<i64>().unwrap_or(0)),
        }
    }

    pub fn set_cursor(&self, cursor: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES('cursor',?1)",
            [cursor.to_string()],
        )?;
        Ok(())
    }

    /// Begin a transaction (the sync applies each delta batch atomically).
    pub fn begin(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("BEGIN IMMEDIATE;")
    }
    pub fn commit(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("COMMIT;")
    }
    pub fn rollback(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("ROLLBACK;")
    }

    /// Insert or update one book.  An existing row keeps its
    /// `downloaded`/`local_path` (a re-sync must not lose the file flag),
    /// exactly like the C app's eh_store_upsert_book.
    pub fn upsert_book(&self, m: &BookMeta) -> rusqlite::Result<()> {
        // Preserve existing downloaded/local_path if already present.
        let (downloaded, local_path): (i64, String) = self
            .conn
            .query_row(
                "SELECT downloaded, local_path FROM books WHERE id=?1",
                [&m.id],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
            .unwrap_or((0, String::new()));

        let author = m.authors.first().cloned().unwrap_or_default();
        let filename = m.filename.as_deref().unwrap_or("");
        let genre = m.genre.as_deref().unwrap_or("");
        let added_at = parse_ts(m.added_at.as_deref());

        self.conn.execute(
            concat!(
                "INSERT OR REPLACE INTO books(",
                "id,title,author,series,series_id,series_idx,",
                "ext,size,downloaded,local_path,added_at,",
                "filename,source,search_text,genre)",
                " VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"
            ),
            params![
                m.id,
                m.title,
                author,
                m.series.as_deref().unwrap_or(""),
                m.series_id.as_deref().unwrap_or(""),
                m.series_idx.unwrap_or(0.0),
                m.format.as_deref().unwrap_or(""),
                m.size,
                downloaded,
                local_path,
                added_at,
                filename,
                "kavita",
                "",
                genre,
            ],
        )?;
        Ok(())
    }

    pub fn delete_book(&self, id: &str) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM books WHERE id=?1", [id])?;
        Ok(())
    }

    /// One book by id (the press action re-reads the row for the current
    /// `downloaded`/`local_path`/`filename` before acting).
    pub fn get_book(&self, id: &str) -> rusqlite::Result<Option<Book>> {
        let mut stmt = self.conn.prepare(concat!(
            "SELECT id,title,author,series,series_id,series_idx,",
            " ext,size,downloaded,local_path,added_at,",
            " filename,source,search_text,genre",
            " FROM books WHERE id=?1"
        ))?;
        let row = stmt
            .query_row([id], |r| {
                Ok(Book {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    author: r.get(2)?,
                    series: r.get(3)?,
                    series_id: r.get(4)?,
                    series_idx: r.get(5)?,
                    ext: r.get(6)?,
                    size: r.get(7)?,
                    downloaded: r.get::<_, i64>(8)? != 0,
                    local_path: r.get(9)?,
                    added_at: r.get(10)?,
                    filename: r.get(11)?,
                    source: r.get(12)?,
                    search_text: r.get(13)?,
                    genre: r.get(14)?,
                })
            })
            .optional()?;
        Ok(row)
    }

    /// Persist the download state (C `eh_store_set_downloaded`): the flag
    /// plus the on-disk path when downloaded, "" otherwise.
    pub fn set_downloaded(&self, id: &str, downloaded: bool, local_path: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE books SET downloaded=?2, local_path=?3 WHERE id=?1",
            params![id, downloaded as i64, local_path],
        )?;
        Ok(())
    }

    /// All books ordered for the shelf: by `added_at` desc, then title
    /// (the C app's default "Recent" grouping).  Returns a capped page.
    pub fn list_books(&self, limit: usize, offset: usize) -> rusqlite::Result<Vec<Book>> {
        let mut stmt = self.conn.prepare(concat!(
            "SELECT id,title,author,series,series_id,series_idx,",
            " ext,size,downloaded,local_path,added_at,",
            " filename,source,search_text,genre",
            " FROM books ORDER BY added_at DESC, title COLLATE NOCASE, id",
            " LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |r| {
                Ok(Book {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    author: r.get(2)?,
                    series: r.get(3)?,
                    series_id: r.get(4)?,
                    series_idx: r.get(5)?,
                    ext: r.get(6)?,
                    size: r.get(7)?,
                    downloaded: r.get::<_, i64>(8)? != 0,
                    local_path: r.get(9)?,
                    added_at: r.get(10)?,
                    filename: r.get(11)?,
                    source: r.get(12)?,
                    search_text: r.get(13)?,
                    genre: r.get(14)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// Apply CREATE TABLE IF NOT EXISTS + the additive column migrations, in the
/// same order/shape as the C app's SCHEMA_SQL + store_migrate_columns.
fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    // Base tables (no indexes yet — the C app's SCHEMA_SQL creates the
    // series_idx-referencing index AFTER migrating columns in).
    conn.execute_batch(concat!(
        "CREATE TABLE IF NOT EXISTS books(",
        " id TEXT PRIMARY KEY,",
        " title TEXT, author TEXT, series TEXT, series_id TEXT,",
        " local_path TEXT, added_at INTEGER,",
        " filename TEXT, source TEXT, search_text TEXT, genre TEXT);",
        "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);"
    ))?;

    // Additive columns for stores predating them (match C migration list),
    // so the series/added indexes below can reference series_idx.
    for (col, ty) in MIGRATE_COLUMNS {
        add_column_if_missing(conn, "books", col, ty)?;
    }

    // Indexes (must come after the column migrations).
    conn.execute_batch(concat!(
        "CREATE INDEX IF NOT EXISTS idx_books_title",
        " ON books(title COLLATE NOCASE, id);",
        "CREATE INDEX IF NOT EXISTS idx_books_author",
        " ON books(author COLLATE NOCASE, title COLLATE NOCASE, id);",
        "CREATE INDEX IF NOT EXISTS idx_books_series",
        " ON books(series_id, series_idx, title COLLATE NOCASE, id);",
        "CREATE INDEX IF NOT EXISTS idx_books_added",
        " ON books(added_at DESC, title COLLATE NOCASE, id);"
    ))?;
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    col: &str,
    ty: &str,
) -> rusqlite::Result<()> {
    let has: bool = conn
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name=?1"
            ),
            [col],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )?;
    if !has {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {ty};"))?;
    }
    Ok(())
}

/// Parse an ISO-8601 timestamp ("2026-06-19T12:34:56Z") into unix seconds.
/// Falls back to 0 on any malformed input (the C app writes added_at as a
/// unix int directly; the server string is only a convenience).
fn parse_ts(s: Option<&str>) -> i64 {
    let Some(s) = s else { return 0 };
    // "YYYY-MM-DDTHH:MM:SS" — strip the 'Z'/offset, treat as UTC.
    let digits: String = s
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(14)
        .collect();
    if digits.len() != 14 {
        return 0;
    }
    let y: i64 = digits[0..4].parse().unwrap_or(0);
    let mo: i64 = digits[4..6].parse().unwrap_or(1);
    let d: i64 = digits[6..8].parse().unwrap_or(1);
    let h: i64 = digits[8..10].parse().unwrap_or(0);
    let mi: i64 = digits[10..12].parse().unwrap_or(0);
    let se: i64 = digits[12..14].parse().unwrap_or(0);
    if y < 1970 {
        return 0;
    }
    // Days since epoch (civil algorithm), valid for 2000-2100.
    let y2 = y - if mo <= 2 { 1 } else { 0 };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    days * 86400 + h * 3600 + mi * 60 + se
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_preserves_downloaded_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let b = BookMeta { id: "k1".into(), title: "T".into(), ..Default::default() };
        store.upsert_book(&b).unwrap();
        // mark downloaded
        store
            .conn
            .execute(
                "UPDATE books SET downloaded=1, local_path='/mnt/x/t.epub' WHERE id='k1'",
                [],
            )
            .unwrap();
        // re-upsert same id — must keep downloaded/local_path
        store.upsert_book(&b).unwrap();
        let (dl, lp): (i64, String) = store
            .conn
            .query_row("SELECT downloaded, local_path FROM books WHERE id='k1'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(dl, 1);
        assert_eq!(lp, "/mnt/x/t.epub");
    }

    #[test]
    fn list_orders_by_added_desc() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        for (id, ts) in [("older", "2026-01-01T00:00:00Z"), ("newer", "2026-06-01T00:00:00Z")] {
            store
                .upsert_book(&BookMeta { id: id.into(), title: id.into(), added_at: Some(ts.into()), ..Default::default() })
                .unwrap();
        }
        let list = store.list_books(10, 0).unwrap();
        assert_eq!(list[0].id, "newer");
        assert_eq!(list[1].id, "older");
    }

    #[test]
    fn parse_iso_ts() {
        assert_eq!(parse_ts(Some("2026-06-19T12:34:56Z")), 1781872496);
        assert_eq!(parse_ts(None), 0);
        assert_eq!(parse_ts(Some("garbage")), 0);
    }
}