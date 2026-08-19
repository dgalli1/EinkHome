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
/// Max remembered search terms (C EH_SEARCH_HISTORY_MAX).
const EH_SEARCH_HISTORY_MAX: usize = 20;
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
    pub const LIB_LEGACY_FILENAME: &'static str = "bookshelf_lib.json";
    /// Open (creating if needed) the store at `path`, applying the schema +
    /// column migrations.  Fails loudly on a corrupt/undecodable DB.
    pub fn open(path: &std::path::Path) -> rusqlite::Result<Store> {
        let conn = Connection::open(path)?;
        // Same as the C app: one connection, journal mode untouched (WAL
        // hammers device flash), a transient lock holder should delay us not
        // fail with SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        apply_schema(&conn)?;
        let store = Store { conn };
        if let Some(parent) = path.parent() {
            store.import_legacy_once(parent);
        }
        Ok(store)
    }

    /// One-time legacy JSON import (C store_import_legacy_once).
    fn import_legacy_once(&self, dir: &std::path::Path) {
        let legacy = dir.join(Self::LIB_LEGACY_FILENAME);
        if !legacy.exists() {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&legacy) else {
            return;
        };
        let Ok(items) = serde_json::from_str::<Vec<BookMeta>>(&text) else {
            crate::logger::log("[bookshelf] store: legacy import: JSON parse failed");
            return;
        };
        let Ok(()) = self.begin() else {
            return;
        };
        let mut count = 0;
        let mut failed = false;
        for item in &items {
            if self.upsert_book(item).is_ok() {
                count += 1;
            } else {
                failed = true;
                break;
            }
        }
        if failed || self.commit().is_err() {
            let _ = self.rollback();
            crate::logger::log(&format!(
                "[bookshelf] store: legacy import incomplete, keeping {}",
                legacy.display()
            ));
        } else {
            let migrated = dir.join(format!("{}.migrated", Self::LIB_LEGACY_FILENAME));
            let _ = std::fs::rename(&legacy, &migrated);
            crate::logger::log(&format!(
                "[bookshelf] store: migrated legacy JSON ({count} books)"
            ));
        }
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

    /// Record a search term in the history (C `eh_store_search_add`):
    /// dedupe by refreshing the timestamp, then trim to the newest
    /// `EH_SEARCH_HISTORY_MAX` rows.  Empty terms are ignored.
    pub fn search_add(&self, term: &str) -> rusqlite::Result<()> {
        if term.trim().is_empty() {
            return Ok(());
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT OR REPLACE INTO search_history(term,ts) VALUES(?1,?2)",
            params![term, now],
        )?;
        self.conn.execute(
            "DELETE FROM search_history WHERE rowid NOT IN \
             (SELECT rowid FROM search_history ORDER BY ts DESC, rowid DESC LIMIT ?1)",
            [EH_SEARCH_HISTORY_MAX as i64],
        )?;
        Ok(())
    }

    /// Number of remembered search terms (C `eh_store_search_count`).
    pub fn search_count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM search_history", [], |r| r.get(0))
    }

    /// Recent search terms, newest first (C `eh_store_search_list`).
    pub fn search_list(&self, limit: usize, offset: usize) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT term FROM search_history ORDER BY ts DESC, rowid DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Filtered shelf page: books whose title/author/series/search_text
    /// match `query` (the C app's LIKE `view_where` fallback — ASCII
    /// case-insensitive substring, `%`/`_`/`\` escaped).  Empty query =
    /// the whole shelf.  Same column/order shape as `list_books`.
    pub fn search(&self, query: &str, limit: usize, offset: usize) -> rusqlite::Result<Vec<Book>> {
        if query.trim().is_empty() {
            return self.list_books(limit, offset);
        }
        let pat = format!("%{}%", like_escape(query));
        let mut stmt = self.conn.prepare(concat!(
            "SELECT id,title,author,series,series_id,series_idx,",
            " ext,size,downloaded,local_path,added_at,",
            " filename,source,search_text,genre",
            " FROM books",
            " WHERE (title LIKE ?1 ESCAPE '\\' OR author LIKE ?1 ESCAPE '\\'",
            " OR series LIKE ?1 ESCAPE '\\' OR search_text LIKE ?1 ESCAPE '\\')",
            " ORDER BY added_at DESC, title COLLATE NOCASE, id",
            " LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = stmt
            .query_map(params![pat, limit as i64, offset as i64], |r| {
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

/// Escape SQL `LIKE` metacharacters (`%`, `_`, `\`) for a `ESCAPE '\'`
/// clause, matching the C app's `like_escape` so a literal `%` in a query
/// is treated as text, not a wildcard.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// One materialised `view` row (kind 0 = flat tile, 1 = stack card).
#[derive(Debug, Clone)]
pub struct ViewRow {
    pub kind: i64,
    pub book_id: String,
    pub series_id: String,
    pub series_name: String,
    pub series_count: i64,
}

/// Grouping presets (the C BsGroupPreset numeric codes, used in the
/// `view_rebuild: group=N` log).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupPreset {
    None = 0,
    Series = 1,
    Author = 2,
    Year = 3,
    Genre = 4,
    AuthorSeries = 5,
}

/// Sort modes (the C BsSortMode codes, used in the `sort=N` log).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortMode {
    Title = 0,
    Author = 1,
    Series = 2,
    Recent = 3,
}

impl Store {
    /// Rebuild the materialised `view` table (C eh_view_rebuild) for the
    /// active sort/group/drill + query.  A non-leaf grouped drill collapses
    /// multi-member groups into stack cards interleaved at their first
    /// member's sort position; everything else emits every book flat.
    /// Returns the tile count.
    pub fn view_rebuild(
        &self,
        group: GroupPreset,
        sort: SortMode,
        drill: u32,
        query: &str,
        scope: &str,
    ) -> rusqlite::Result<i64> {
        fn group_key(b: &Book, g: GroupPreset) -> String {
            match g {
                GroupPreset::Author => b.author.trim().to_string(),
                GroupPreset::Genre => b.genre.trim().to_string(),
                GroupPreset::Year => year_of(b.added_at).unwrap_or_default(),
                _ => b.series_id.trim().to_string(),
            }
        }
        fn label(b: &Book, g: GroupPreset) -> String {
            match g {
                GroupPreset::Author => b.author.trim().to_string(),
                GroupPreset::Genre => b.genre.trim().to_string(),
                GroupPreset::Year => group_key(b, g),
                _ => b.series.trim().to_string(),
            }
        }

        let books = self.list_sorted(sort, query, drill, scope)?;
        let total = books.len() as i64;
        let grouped = drill == 0 && group != GroupPreset::None;

        self.conn.execute("DELETE FROM view", [])?;
        self.conn.execute("BEGIN", [])?;
        let result = (|| -> rusqlite::Result<()> {
            let mut pos = 0i64;
            if grouped {
                use std::collections::HashMap;
                let mut groups: HashMap<String, Vec<&Book>> = HashMap::new();
                for b in &books {
                    let k = group_key(b, group);
                    if !k.is_empty() {
                        groups.entry(k).or_default().push(b);
                    }
                }
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                for b in &books {
                    let k = group_key(b, group);
                    if k.is_empty() || !seen.insert(k.clone()) {
                        continue;
                    }
                    let members = groups.get(&k).unwrap();
                    let kind = if members.len() > 1 { 1 } else { 0 };
                    let sid = if members.len() > 1 { k.clone() } else { String::new() };
                    self.conn.execute(
                        "INSERT INTO view(pos,kind,book_id,series_id,series_name,series_count) VALUES(?1,?2,?3,?4,?5,?6)",
                        rusqlite::params![pos, kind, members[0].id, sid, label(members[0], group), members.len() as i64],
                    )?;
                    pos += 1;
                }
            } else {
                for b in &books {
                    let sid = b.series_id.clone();
                    let name = b.series.clone();
                    self.conn.execute(
                        "INSERT INTO view(pos,kind,book_id,series_id,series_name,series_count) VALUES(?1,0,?2,?3,?4,1)",
                        rusqlite::params![pos, b.id, sid, name],
                    )?;
                    pos += 1;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                let _ = self.conn.execute("COMMIT", []);
                Ok(total)
            }
            Err(e) => {
                let _ = self.conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// Books in the active sort order (+ query / drill-scope filter), for
    /// `view_rebuild` and the flat-velocity paths.
    pub fn list_sorted(
        &self,
        sort: SortMode,
        query: &str,
        drill: u32,
        scope: &str,
    ) -> rusqlite::Result<Vec<Book>> {
        let order = match sort {
            SortMode::Title => "title COLLATE NOCASE, id",
            SortMode::Author => "author COLLATE NOCASE, title COLLATE NOCASE, id",
            SortMode::Series => "series COLLATE NOCASE, series_idx, id",
            SortMode::Recent => "added_at DESC, title COLLATE NOCASE, id",
        };
        let mut sql = String::from(concat!(
            "SELECT id,title,author,series,series_id,series_idx,",
            " ext,size,downloaded,local_path,added_at,",
            " filename,source,search_text,genre FROM books WHERE 1=1"
        ));
        let mut params: Vec<String> = Vec::new();
        let mut n = 0i32;
        if drill > 0 && !scope.is_empty() {
            n += 1;
            sql.push_str(&format!(
                " AND (author=?{n} OR series_id=?{n} OR genre=?{n})"
            ));
            params.push(scope.to_string());
        }
        if !query.trim().is_empty() {
            let pat = format!("%{}%", like_escape(query));
            n += 1;
            let p = n.to_string();
            sql.push_str(&format!(
                " AND (title LIKE ?{p} ESCAPE '\\' OR author LIKE ?{p} ESCAPE '\\' \
                 OR series LIKE ?{p} ESCAPE '\\' OR search_text LIKE ?{p} ESCAPE '\\')"
            ));
            params.push(pat);
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(order);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| row_to_book(r))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        Ok(rows)
    }

    /// One page of the materialised view (the shelf's source when grouped).
    pub fn view_page(&self, limit: usize, offset: usize) -> rusqlite::Result<Vec<ViewRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind,book_id,series_id,series_name,series_count FROM view \
             ORDER BY pos LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64, offset as i64], |r| {
                Ok(ViewRow {
                    kind: r.get(0)?,
                    book_id: r.get(1)?,
                    series_id: r.get(2)?,
                    series_name: r.get(3)?,
                    series_count: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Tile count of the materialised view.
    pub fn view_total(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM view", [], |r| r.get(0))
    }

    /// Which grouping dimensions the current data actually offers
    /// (author / series / year / genre present somewhere), C
    /// eh_view_dim_available — the group chooser omits empty dims so the
    /// harness's row indices line up.
    pub fn dim_availability(&self) -> rusqlite::Result<(bool, bool, bool, bool)> {
        let a: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM books WHERE author IS NOT NULL AND author!='')",
            [],
            |r| r.get(0),
        )?;
        let s: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM books WHERE series_id IS NOT NULL AND series_id!='')",
            [],
            |r| r.get(0),
        )?;
        let y: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM books WHERE added_at IS NOT NULL AND added_at>0)",
            [],
            |r| r.get(0),
        )?;
        let g: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM books WHERE genre IS NOT NULL AND genre!='')",
            [],
            |r| r.get(0),
        )?;
        Ok((a, s, y, g))
    }
}

/// UTC year of a unix timestamp (Howard Hinnant civil-from-days),
/// or None for 1970-01-01 (year 0 guard / invalid).
fn year_of(unix: i64) -> Option<String> {
    if unix <= 0 {
        return None;
    }
    let z = unix.div_euclid(86400) + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    Some(format!("{}", y))
}

/// Map a books row to a [`Book`] (shared by list/search/view reads).
fn row_to_book(r: &rusqlite::Row) -> rusqlite::Result<Book> {
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
}
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
        "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);",
        "CREATE TABLE IF NOT EXISTS search_history(term TEXT PRIMARY KEY, ts INTEGER);",
        "CREATE TABLE IF NOT EXISTS view(",
        " pos INTEGER PRIMARY KEY, kind INTEGER, book_id TEXT, series_id TEXT,",
        " series_name TEXT, series_count INTEGER);"
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
    if let Ok(ts) = s.parse::<i64>() {
        return ts;
    }
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

    #[test]
    fn search_filters_across_fields_case_insensitive() {
        use crate::client::BookMeta;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        // Authors aren't in BookMeta's first-author set — the store maps
        // authors[0] into author.  Insert a few titled/author books.
        let mut m = BookMeta::default();
        m.id = "a1".into();
        m.title = "The Last Leaf".into();
        m.authors = vec!["O. Henry".into()];
        store.upsert_book(&m).unwrap();
        m.id = "a2".into();
        m.title = "Moby Dick".into();
        m.authors = vec!["Herman Melville".into()];
        store.upsert_book(&m).unwrap();
        m.id = "a3".into();
        m.title = "Leaf Lovers".into();
        m.authors = Vec::new();
        store.upsert_book(&m).unwrap();

        // Title substring, case-insensitive (added_at ties → title asc).
        let hits = store.search("leaf", 10, 0).unwrap();
        let ids: Vec<&str> = hits.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, vec!["a3", "a1"]); // Leaf Lovers < The Last Leaf
        // Author match.
        let hits = store.search("melville", 10, 0).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a2");
        // Literal % is escaped, not a wildcard.
        let none = store.search("%", 10, 0).unwrap();
        assert!(none.is_empty());
        // Empty query = full shelf.
        assert_eq!(store.search("", 10, 0).unwrap().len(), 3);
    }

    #[test]
    fn view_rebuild_collapses_single_author() {
        use crate::client::BookMeta;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        for i in 0..3 {
            let mut m = BookMeta::default();
            m.id = format!("b{i}");
            m.title = format!("T{i}");
            m.authors = vec!["One Author".into()];
            store.upsert_book(&m).unwrap();
        }
        let total = store.view_rebuild(GroupPreset::Author, SortMode::Recent, 0, "", "").unwrap();
        assert_eq!(total, 3);
        let rows = store.view_page(10, 0).unwrap();
        assert_eq!(rows.len(), 1, "single-author library must collapse to one card");
        assert_eq!(rows[0].kind, 1);
        assert_eq!(rows[0].series_count, 3);
        // Drilled view: the group's books flat.
        let scope = rows[0].series_id.clone();
        let total2 = store.view_rebuild(GroupPreset::Author, SortMode::Recent, 1, "", &scope).unwrap();
        assert_eq!(total2, 3);
        let flat = store.view_page(10, 0).unwrap();
        assert_eq!(flat.len(), 3);
        assert!(flat.iter().all(|r| r.kind == 0));
    }

    #[test]
    fn search_history_dedupes_and_trims() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store.search_add("first").unwrap();
        store.search_add("second").unwrap();
        store.search_add("first").unwrap(); // dedupe → first again, bumps ts
        let list = store.search_list(100, 0).unwrap();
        assert_eq!(list, vec!["first", "second"]); // newest first
        assert_eq!(store.search_count().unwrap(), 2);
        store.search_add("").unwrap(); // ignored
        assert_eq!(store.search_count().unwrap(), 2);
    }
}