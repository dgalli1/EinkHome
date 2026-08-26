//! SQLite persistence — schema-compatible with the C app's eh_store.c.
//!
//! The `books`/`meta` tables match the C store's schema exactly, so an
//! existing device library (`bookshelf_lib.db` next to the config) carries
//! over unchanged.  A re-sync keeps an already-downloaded book's
//! `downloaded`/`local_path` state (the same INSERT OR REPLACE + lookup the
//! C app does).
//!
//! This module implements the row CRUD the shelf needs, plus FTS5 search,
//! suggest/rank prefix completion, and the materialised `view` projection.

use rusqlite::{params, Connection, OptionalExtension};

use crate::client::BookMeta;

pub const EH_LIB_DB_FILENAME: &str = "bookshelf_lib.db";
/// Max remembered search terms (C EH_SEARCH_HISTORY_MAX).
const EH_SEARCH_HISTORY_MAX: usize = 20;

/// Books the Rust-side grouped-view engine scans per rebuild.
///
/// Deliberate RSS guard, not an oversight: grouping materialises every
/// matching book in Rust (a HashMap of groups), so this is what keeps a
/// GROUPED browse of an outsized library from the ~100MB the flat shape
/// would need on device (the scale suite's budget).  The ungrouped shape
/// never hits the cap — it projects entirely in SQL straight from
/// `books` (see [`Store::view_rebuild`]) — so flat paging stays exact at
/// any library size; a grouped browse larger than the cap shows its
/// first VIEW_SCAN_CAP books instead of OOM-ing the reader.
pub(crate) const VIEW_SCAN_CAP: usize = 10_000;

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
    /// 1 when the SQLite build lacks the FTS5 module; routes committed
    /// search to the LIKE fallback (C g_no_fts).
    no_fts: bool,
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
        let no_fts = !init_fts(&conn);
        let store = Store { conn, no_fts };
        if let Some(parent) = path.parent() {
            store.import_legacy_once(parent);
        }
        // Backfill FTS index if it's empty and FTS is available.
        if !store.no_fts {
            store.fts_backfill();
        }
        Ok(store)
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
            .query_row("SELECT value FROM meta WHERE key='cursor'", [], |r| {
                r.get(0)
            })
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

    /// Upsert a book from server metadata, preserving downloaded/local_path.
    /// Stores search_text for folded diacritic matching (C eh_store_upsert_book).
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
        // search_text: server-folded search blob. Empty string when absent.
        let search_text = m.search_text.as_deref().unwrap_or("");

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
                search_text,
                genre,
            ],
        )?;

        // Sync FTS index if available (C store_fts_sync_row).
        if !self.no_fts {
            self.fts_sync_row(&m.id);
        }

        Ok(())
    }

    pub fn delete_book(&self, id: &str) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM books WHERE id=?1", [id])?;
        // Remove from FTS too.
        if !self.no_fts {
            let _ = self.conn.execute(
                "DELETE FROM search_fts WHERE rowid IN (SELECT rowid FROM books WHERE id=?1)",
                [id],
            );
        }
        Ok(())
    }

    /// Drop every book of one source (C eh_store_delete_source): local
    /// imports replace wholesale, so a re-scan never leaves stale entries
    /// behind.  FTS rows go first, while the books rows still exist.
    pub fn delete_source(&self, source: &str) -> rusqlite::Result<()> {
        if !self.no_fts {
            let _ = self.conn.execute(
                "DELETE FROM search_fts WHERE rowid IN \
                 (SELECT rowid FROM books WHERE source=?1)",
                [source],
            );
        }
        self.conn
            .execute("DELETE FROM books WHERE source=?1", [source])?;
        Ok(())
    }

    /// Extracted-metadata cache hit for a local book (C
    /// eh_store_local_meta_get): Some((title, author, series, series_idx))
    /// when known, so a rescan never re-parses a book whose metadata is
    /// already stored.
    pub fn local_meta_get(&self, id: &str) -> Option<(String, String, String, Option<f64>)> {
        self.conn
            .query_row(
                "SELECT title, author, series, series_idx FROM local_meta WHERE id=?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<f64>>(3)?,
                    ))
                },
            )
            .ok()
    }

    /// Row count for one source (the Local chooser consults this to
    /// decide between instant cached rows and a first import).
    pub fn count_source(&self, source: &str) -> rusqlite::Result<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM books WHERE source=?1",
            [source],
            |r| r.get(0),
        )
    }

    /// Every cached local-file metadata row.  The scan worker skips
    /// re-extraction for ids already here — opening each EPUB/PDF for
    /// its Info/OPF block is the dominant cost of a 20k-file scan, and
    /// the apply discards fresh values for cached ids anyway.
    pub fn local_meta_all(
        &self,
    ) -> rusqlite::Result<std::collections::HashMap<String, (String, String, String, Option<f64>)>>
    {
        let mut stmt = self
            .conn
            .prepare("SELECT id, title, author, series, series_idx FROM local_meta")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<f64>>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(i, t, a, s, si)| (i, (t, a, s, si)))
            .collect())
    }

    /// One source's row ids with their sizes — the local import's diff
    /// baseline (only new/size-changed files are rewritten, only
    /// vanished ids deleted).
    pub fn source_ids(
        &self,
        source: &str,
    ) -> rusqlite::Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, size FROM books WHERE source=?1")?;
        let rows = stmt
            .query_map([source], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    /// A book row's rowid (the FTS join key; the import-diff test reads
    /// it to prove untouched rows are not rewritten).
    pub fn book_rowid(&self, id: &str) -> rusqlite::Result<Option<i64>> {
        self.conn
            .query_row("SELECT rowid FROM books WHERE id=?1", [id], |r| r.get(0))
            .optional()
    }
    pub fn local_meta_put(
        &self,
        id: &str,
        title: &str,
        author: &str,
        series: &str,
        series_idx: Option<f64>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO local_meta(id, title, author, series, series_idx) \
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![id, title, author, series, series_idx],
        )?;
        Ok(())
    }

    /// Upsert a fully-built Book row (the local/folder import path: the
    /// caller has already resolved downloaded/local_path/source — C
    /// eh_store_upsert_book with the record filled by local_file_to_book).
    pub fn upsert_book_row(&self, b: &Book) -> rusqlite::Result<()> {
        self.conn.execute(
            concat!(
                "INSERT OR REPLACE INTO books(",
                "id,title,author,series,series_id,series_idx,",
                "ext,size,downloaded,local_path,added_at,",
                "filename,source,search_text,genre)",
                " VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"
            ),
            params![
                b.id,
                b.title,
                b.author,
                b.series,
                b.series_id,
                b.series_idx,
                b.ext,
                b.size,
                b.downloaded as i64,
                b.local_path,
                b.added_at,
                b.filename,
                if b.source.is_empty() {
                    "kavita"
                } else {
                    &b.source
                },
                b.search_text,
                b.genre,
            ],
        )?;
        if !self.no_fts {
            self.fts_sync_row(&b.id);
        }
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

    /// Mark a book as downloaded (or not), storing the local path.
    /// Same semantics as eh_store_set_downloaded: sets the flag
    /// plus the on-disk path when downloaded, "" otherwise.
    pub fn set_downloaded(
        &self,
        id: &str,
        downloaded: bool,
        local_path: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE books SET downloaded=?2, local_path=?3 WHERE id=?1",
            params![id, downloaded as i64, local_path],
        )?;
        Ok(())
    }

    /// Apply boot-reconciliation flips in ONE transaction: a moved
    /// downloads dir can flip every row, and per-row autocommit would
    /// fsync once per book on the device's flash.
    pub fn set_downloaded_flips(&self, flips: &[(String, bool, String)]) -> usize {
        let mut changed = 0usize;
        if self.conn.execute("BEGIN", []).is_err() {
            return 0;
        }
        for (id, dl, path) in flips {
            if self.set_downloaded(id, *dl, path).is_ok() {
                changed += 1;
            }
        }
        if self.conn.execute("COMMIT", []).is_err() {
            let _ = self.conn.execute("ROLLBACK", []);
            return 0;
        }
        changed
    }
    pub fn set_read(&self, id: &str, read: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO read_markers(book_id, is_read) VALUES(?1, ?2) \
             ON CONFLICT(book_id) DO UPDATE SET is_read = excluded.is_read",
            params![id, read as i64],
        )?;
        Ok(())
    }

    /// The local read marker (false when unset).
    pub fn is_read(&self, id: &str) -> bool {
        self.conn
            .query_row(
                "SELECT is_read FROM read_markers WHERE book_id = ?1",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .map(|v| v != 0)
            .unwrap_or(false)
    }

    /// Remove a book row outright (the "delete from cloud" landing: the
    /// server dropped it, so the local copy must go too).  Also clears
    /// the row's read marker.
    pub fn delete_book_row(&self, id: &str) -> rusqlite::Result<()> {
        // FTS first: the external-content index resolves rows by rowid,
        // so its entry must go while the books row still exists (the
        // same order delete_source's bulk delete uses).
        if !self.no_fts {
            self.conn.execute(
                "DELETE FROM search_fts WHERE rowid IN (SELECT rowid FROM books WHERE id=?1)",
                [id],
            )?;
        }
        self.conn
            .execute("DELETE FROM books WHERE id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM read_markers WHERE book_id = ?1", params![id])?;
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

    /// Stream EVERY book in [`list_books`] order through `f` without
    /// materialising the library: the boot reconciliation walks 100k-scale
    /// stores, and collecting them first would break the RSS budget.
    pub fn for_each_book(&self, mut f: impl FnMut(&Book)) -> rusqlite::Result<()> {
        let mut stmt = self.conn.prepare(concat!(
            "SELECT id,title,author,series,series_id,series_idx,",
            " ext,size,downloaded,local_path,added_at,",
            " filename,source,search_text,genre",
            " FROM books ORDER BY added_at DESC, title COLLATE NOCASE, id"
        ))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            f(&row_to_book(row)?);
        }
        Ok(())
    }

    /// Record a committed search term (idempotent, with timestamp).
    /// The C app's eh_store_search_add wraps INSERT OR REPLACE.
    pub fn search_add(&self, term: &str) -> rusqlite::Result<()> {
        if term.trim().is_empty() {
            return Ok(());
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT OR REPLACE INTO search_history(term, ts) VALUES(?1, ?2)",
            params![term.trim(), ts],
        )?;
        // Trim history to EH_SEARCH_HISTORY_MAX.
        self.conn.execute_batch(&format!(
            "DELETE FROM search_history WHERE rowid NOT IN \
             (SELECT rowid FROM search_history ORDER BY ts DESC LIMIT {EH_SEARCH_HISTORY_MAX})"
        ))?;
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
            .query_map(params![limit as i64, offset as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Filtered shelf page: books whose title/author/series/search_text
    /// match `query` (the C app's LIKE `view_where` fallback — ASCII
    /// case-insensitive substring, `%`/`_`/`\` escaped).  When FTS5 is
    /// available and the query is safe, uses FTS MATCH for better ranking.
    /// Empty query = the whole shelf.  Same column/order shape as `list_books`.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        source: &str,
    ) -> rusqlite::Result<Vec<Book>> {
        if query.trim().is_empty() {
            // '%%' matches everything: reuse the LIKE scan for the
            // source-filtered empty-query page.
            return self.search_like_sql("%%", source, limit, offset);
        }

        // FTS path — try when the index is available and the MATCH probe
        // confirms at least one result.
        if !self.no_fts {
            let fts_q = build_fts_query(query);
            if !fts_q.is_empty() {
                // Probe: does the MATCH return any row?
                let probe: bool = self
                    .conn
                    .prepare("SELECT 1 FROM search_fts WHERE search_fts MATCH ?1 LIMIT 1")
                    .and_then(|mut s| s.query_row(params![&fts_q], |r| r.get::<_, i64>(0)))
                    .unwrap_or(-1)
                    > 0;
                if probe {
                    return self.search_fts_sql(&fts_q, limit, offset, source);
                }
            }
        }

        // LIKE fallback.
        let pat = escape_like(query);
        let like_query = format!("%{pat}%");
        self.search_like_sql(&like_query, source, limit, offset)
    }

    fn search_fts_sql(
        &self,
        fts_q: &str,
        limit: usize,
        offset: usize,
        source: &str,
    ) -> rusqlite::Result<Vec<Book>> {
        let mut stmt = self.conn.prepare(concat!(
            "SELECT id,title,author,series,series_id,series_idx,",
            " ext,size,downloaded,local_path,added_at,",
            " filename,source,search_text,genre",
            " FROM books",
            " WHERE rowid IN (SELECT rowid FROM search_fts WHERE search_fts MATCH ?1)",
            " AND source = ?4",
            " ORDER BY added_at DESC, title COLLATE NOCASE, id",
            " LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = stmt
            .query_map(
                params![fts_q, limit as i64, offset as i64, source],
                row_to_book,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn search_like_sql(
        &self,
        like_query: &str,
        source: &str,
        limit: usize,
        offset: usize,
    ) -> rusqlite::Result<Vec<Book>> {
        let mut stmt = self.conn.prepare(concat!(
            "SELECT id,title,author,series,series_id,series_idx,",
            " ext,size,downloaded,local_path,added_at,",
            " filename,source,search_text,genre",
            " FROM books",
            " WHERE (title LIKE ?1 ESCAPE '\\' OR author LIKE ?1 ESCAPE '\\'",
            " OR series LIKE ?1 ESCAPE '\\' OR search_text LIKE ?1 ESCAPE '\\')",
            " AND source = ?4",
            " ORDER BY added_at DESC, title COLLATE NOCASE, id",
            " LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = stmt
            .query_map(
                params![like_query, limit as i64, offset as i64, source],
                row_to_book,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Replace the suggestion terms of one book (C eh_store_suggest_set).
    /// Deletes old edges, inserts new terms, refreshes rank counts.
    pub fn suggest_set(&self, book_id: &str, terms: &[String]) -> rusqlite::Result<()> {
        if book_id.is_empty() {
            return Ok(());
        }
        // Snapshot old terms for rank refresh.
        let mut old: Vec<String> = Vec::new();
        {
            let mut q = self
                .conn
                .prepare("SELECT term FROM suggest WHERE book_id=?1")?;
            let rows = q.query_map([book_id], |r| r.get::<_, String>(0))?;
            for t in rows.flatten() {
                old.push(t);
            }
        }
        // Delete old edges.
        self.conn
            .execute("DELETE FROM suggest WHERE book_id=?1", [book_id])?;
        // Insert new non-empty terms.
        for t in terms {
            if !t.is_empty() {
                self.conn.execute(
                    "INSERT OR IGNORE INTO suggest(term, book_id) VALUES(?1, ?2)",
                    params![t, book_id],
                )?;
            }
        }
        // Refresh ranks for touched terms.
        for t in &old {
            self.refresh_rank(t);
        }
        for t in terms {
            if !t.is_empty() && !old.contains(t) {
                self.refresh_rank(t);
            }
        }
        // Drop zero-count rank rows.
        self.conn
            .execute_batch("DELETE FROM suggest_rank WHERE cnt=0")?;
        Ok(())
    }

    fn refresh_rank(&self, term: &str) {
        let _ = self.conn.execute(
            "INSERT INTO suggest_rank(term, cnt) VALUES(?1, \
             (SELECT COUNT(*) FROM suggest WHERE term=?1)) \
             ON CONFLICT(term) DO UPDATE SET cnt=excluded.cnt",
            [term],
        );
    }

    /// Prefix search against the suggestion index (C eh_store_suggest_list).
    /// Returns suggestions ordered by rank descending, term ascending.
    pub fn suggest_list(&self, prefix: &str, limit: usize) -> rusqlite::Result<Vec<String>> {
        if prefix.len() < 2 || limit == 0 {
            return Ok(Vec::new());
        }
        let norm: String = prefix.to_ascii_lowercase();
        let bound = suggest_upper_bound(&norm);
        // Try ranked path first.
        if let Ok(rows) = self.suggest_list_ranked(&norm, &bound, limit) {
            if !rows.is_empty() {
                return Ok(rows);
            }
        }
        // Fallback: edge-table GROUP BY.
        let sql = "SELECT term FROM suggest WHERE term >= ?1 AND term < ?2 \
             GROUP BY term ORDER BY COUNT(*) DESC, term ASC LIMIT ?3"
            .to_string();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![norm, bound, limit as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn suggest_list_ranked(
        &self,
        norm: &str,
        bound: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT term FROM suggest_rank WHERE term >= ?1 AND term < ?2 \
             ORDER BY cnt DESC, term ASC LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![norm, bound, limit as i64], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Whether the store has any books with timestamps, series, etc.
    /// Returns (has_author_data, has_series_data, has_year_data, has_genre_data).
    pub fn dim_availability(&self) -> rusqlite::Result<(bool, bool, bool, bool)> {
        let a: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM books WHERE author IS NOT NULL AND author!='')",
            [],
            |r| r.get(0),
        )?;
        let y: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM books WHERE added_at IS NOT NULL AND added_at>0)",
            [],
            |r| r.get(0),
        )?;
        let s: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM books WHERE series_id IS NOT NULL AND series_id!='')",
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

    // ── FTS helpers ────────────────────────────────────────────────────

    fn fts_sync_row(&self, id: &str) {
        if self.no_fts {
            return;
        }
        // Drop stale FTS entry for this book's current rowid.
        let _ = self.conn.execute(
            "DELETE FROM search_fts WHERE rowid IN (SELECT rowid FROM books WHERE id=?1)",
            [id],
        );
        // Re-index the book's row at its (potentially new) rowid.
        let _ = self.conn.execute(
            "INSERT INTO search_fts(rowid, title, author, series, search_text) \
             SELECT rowid, title, author, series, COALESCE(search_text,'') FROM books WHERE id=?1",
            [id],
        );
    }

    fn fts_backfill(&self) {
        if self.no_fts {
            return;
        }
        let empty: bool = self
            .conn
            .query_row("SELECT 1 FROM search_fts LIMIT 1", [], |_| Ok(false))
            .unwrap_or(true);
        if empty {
            let _ = self.conn.execute_batch(
                "INSERT INTO search_fts(rowid, title, author, series, search_text) \
                 SELECT rowid, title, author, series, COALESCE(search_text,'') FROM books",
            );
        }
    }

    // ── view rebuild ───────────────────────────────────────────────────

    /// One row into the materialised `view` (kind 0 = book tile,
    /// 1 = stack card).
    fn insert_view_row(
        &self,
        pos: i64,
        kind: i64,
        book_id: &str,
        series_id: &str,
        name: &str,
        count: i64,
    ) -> rusqlite::Result<()> {
        self.conn
            .execute(
                "INSERT INTO view(pos,kind,book_id,series_id,series_name,series_count) VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![pos, kind, book_id, series_id, name, count],
            )
            .map(|_| ())
    }

    /// Every scanned book as an individual tile in sort order ("All
    /// books", or the leaf of a fully-drilled selection).
    fn append_flat(&self, all: &[Book], pos: &mut i64) -> rusqlite::Result<()> {
        for b in all {
            self.insert_view_row(*pos, 0, &b.id, &b.series_id, &b.title, 1)?;
            *pos += 1;
        }
        Ok(())
    }

    /// Multi-member groups become stack cards AT THE FIRST MEMBER'S SORT
    /// POSITION (so cards interleave with flat single-member tiles);
    /// single members stay lone tiles.  `key_of` picks the dimension:
    /// [`group_key`] at level 0 or [`dim_key`] at a drilled level.  `level`
    /// also picks the card label's dimension (see [`group_label`]).
    /// `group_empty`: when false, books with an EMPTY key never form a
    /// card — each stays a lone tile at its sort position (the Author >
    /// Series second level lists series-less books directly instead of
    /// hiding them behind an "unknown series" drill).
    fn append_grouped(
        &self,
        all: &[Book],
        group: GroupPreset,
        level: usize,
        group_empty: bool,
        key_of: &dyn Fn(&Book) -> String,
        pos: &mut i64,
    ) -> rusqlite::Result<()> {
        let mut groups: std::collections::HashMap<String, Vec<Book>> = Default::default();
        for b in all {
            groups.entry(key_of(b)).or_default().push(b.clone());
        }
        let mut seen: std::collections::HashSet<String> = Default::default();
        for b in all {
            let k = key_of(b);
            if k.is_empty() && !group_empty {
                self.insert_view_row(*pos, 0, &b.id, &b.series_id, &b.title, 1)?;
                *pos += 1;
                continue;
            }
            if !seen.insert(k.clone()) {
                continue; // covered by its card or lone tile
            }
            let members = &groups[&k];
            if members.len() > 1 {
                self.insert_view_row(
                    *pos,
                    1,
                    &members[0].id,
                    &k,
                    &group_label(members, group, level),
                    members.len() as i64,
                )?;
            } else {
                self.insert_view_row(*pos, 0, &b.id, &b.series_id, &b.title, 1)?;
            }
            *pos += 1;
        }
        Ok(())
    }

    /// Rebuild the materialised `view` table from the current books,
    /// grouped by `group` and ordered by `sort`, filtered by `query`
    /// within the pinned drill scopes (one per drilled level).  Returns
    /// the total rows (C view_rebuild returns n_stacks, stored in
    /// g_view_total).
    pub fn view_rebuild(
        &self,
        group: i64,
        sort: i64,
        drill: i64,
        query: &str,
        scopes: &[&str],
        source: &str,
    ) -> rusqlite::Result<i64> {
        let group = GroupPreset::from_i64(group);
        let sort = SortMode::from_i64(sort);
        let grouped = group != GroupPreset::None;
        let drilled = drill > 0;

        // Grouped/drilled shapes scan in Rust, bounded by VIEW_SCAN_CAP
        // (the RSS guard documented there).
        let mut all = self.list_books(VIEW_SCAN_CAP, 0)?;
        // The view shows ONLY the active source's rows (C view_source +
        // view_where): a local/folder import stays in the DB but must not
        // appear once the user switches back to Kavita.
        all.retain(|b| b.source == source);
        // Filter by query when present.
        if !query.is_empty() {
            let q = query.to_lowercase();
            all.retain(|b| {
                b.title.to_lowercase().contains(&q)
                    || b.author.to_lowercase().contains(&q)
                    || b.series.to_lowercase().contains(&q)
                    || b.search_text.to_lowercase().contains(&q)
            });
        }

        // Sort — the C view_order() comparators verbatim, including the
        // tie-breaks (NOCASE folds; the series sort deliberately has NO
        // title tie-break, so a series-less library orders by id).
        all.sort_by(|a, b| {
            let c = match sort {
                SortMode::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                SortMode::Author => a
                    .author
                    .to_lowercase()
                    .cmp(&b.author.to_lowercase())
                    .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
                SortMode::Series => a
                    .series
                    .to_lowercase()
                    .cmp(&b.series.to_lowercase())
                    .then_with(|| {
                        a.series_idx
                            .partial_cmp(&b.series_idx)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }),
                SortMode::Recent => b
                    .added_at
                    .cmp(&a.added_at)
                    .then_with(|| a.title.to_lowercase().cmp(&b.title.to_lowercase())),
            };
            c.then(a.id.cmp(&b.id))
        });

        self.conn.execute("DELETE FROM view", [])?;
        self.conn.execute("BEGIN", [])?;
        let result = (|| -> rusqlite::Result<i64> {
            // Flat ungrouped view (the 100k-scale shape): project
            // entirely in SQL so RSS stays flat at any library size —
            // materialising the books in Rust first (~100MB at 100k)
            // breaks the scale budget, and only this shape escapes
            // VIEW_SCAN_CAP.  C did this projection in SQL too.
            if !grouped && !drilled && query.is_empty() {
                // Mirror the in-memory comparator per SortMode (lowercased
                // key, then title, then id — Recent is descending recency).
                let order = match sort {
                    SortMode::Title => "title COLLATE NOCASE ASC, id",
                    SortMode::Author => "author COLLATE NOCASE ASC, title COLLATE NOCASE ASC, id",
                    SortMode::Series => "series COLLATE NOCASE ASC, series_idx ASC, id",
                    SortMode::Recent => "added_at DESC, title COLLATE NOCASE ASC, id",
                };
                self.conn.execute(
                    &format!(
                        "INSERT INTO view(pos,kind,book_id,series_id,series_name,series_count)
                         SELECT ROW_NUMBER() OVER (ORDER BY {order}) - 1,
                         0, id, series_id, title, 1
                         FROM books WHERE source = ?1"
                    ),
                    [source],
                )?;
                let n: i64 = self
                    .conn
                    .query_row("SELECT COUNT(*) FROM view", [], |r| r.get(0))?;
                return Ok(n);
            }
            let mut pos = 0i64;
            if drilled {
                // Pin every drilled level's scope against ITS OWN dimension
                // (C view_append_drill_conds + dim_at(group, L)): an empty
                // value matches books whose key is empty.
                let levels = (drill as usize).min(2);
                for l in 0..levels {
                    let want = scopes.get(l).copied().unwrap_or("");
                    all.retain(|b| {
                        let k = dim_key(b, group, l);
                        if want.is_empty() {
                            k.is_empty()
                        } else {
                            k == want
                        }
                    });
                }
                if levels < group_levels(group) {
                    // A deeper dimension still groups: stack cards + flat
                    // singles of the NEXT dimension among the survivors
                    // (C view_rebuild_group at drill level > 0).
                    // The drilled Author > Series level lists series-less
                    // books flat: they never form an "unknown series" card.
                    self.append_grouped(
                        &all,
                        group,
                        levels,
                        false,
                        &|b| dim_key(b, group, levels),
                        &mut pos,
                    )?;
                } else {
                    // Leaf level: flat books of the fully scoped selection.
                    self.append_flat(&all, &mut pos)?;
                }
            } else if grouped {
                self.append_grouped(&all, group, 0, true, &|b| group_key(b, group), &mut pos)?;
            } else {
                self.append_flat(&all, &mut pos)?;
            }
            Ok(pos)
        })();
        match result {
            Ok(n) => {
                self.conn.execute("COMMIT", [])?;
                Ok(n)
            }
            Err(e) => {
                self.conn.execute("ROLLBACK", [])?;
                Err(e)
            }
        }
    }

    pub fn view_count(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM view", [], |r| r.get(0))
    }

    pub fn view_page(&self, limit: usize, offset: usize) -> rusqlite::Result<Vec<ViewRow>> {
        let mut stmt = self.conn.prepare(concat!(
            "SELECT v.kind,v.book_id,v.series_id,v.series_name,v.series_count",
            " FROM view v ORDER BY v.pos LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |r| {
                Ok(ViewRow {
                    kind: r.get(0)?,
                    book_id: r.get(1)?,
                    series_id: r.get(2)?,
                    series_name: r.get(3)?,
                    series_count: r.get::<_, i64>(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn view_total(&self) -> usize {
        self.view_count().unwrap_or(0) as usize
    }

    /// List a series' books in reading order (series_idx, then title) —
    /// the C list_sorted helper download_series / delete_series call.
    pub fn list_series(&self, scope: &str) -> rusqlite::Result<Vec<Book>> {
        self.conn
            .prepare(concat!(
                "SELECT id,title,author,series,series_id,series_idx,",
                " ext,size,downloaded,local_path,added_at,",
                " filename,source,search_text,genre",
                " FROM books WHERE series_id=?1 ORDER BY series_idx, title"
            ))?
            .query_map([scope], |r| {
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
            .collect::<Result<Vec<_>, _>>()
    }
}

/// How many dimensions a grouping preset drills through (C group_levels):
/// only Author > Series nests two deep (C EH_GROUP_MAX_LEVELS).
fn group_levels(g: GroupPreset) -> usize {
    match g {
        GroupPreset::AuthorSeries => 2,
        GroupPreset::None => 0,
        _ => 1,
    }
}

/// The card label for one group: the DISPLAY value of the dimension the
/// members were keyed by (C view_rebuild_group labels each stack with
/// dim_at(group, level)'s value).  Author > Series is the only preset
/// whose label changes with the drill level: author cards at level 0,
/// series cards beneath an author.  An empty dimension value falls back
/// to the group.none.* i18n label ("Unknown author" / "No series" / …)
/// so the card never renders blank.
fn group_label(members: &[Book], g: GroupPreset, level: usize) -> String {
    let (raw, none_key) = match g {
        GroupPreset::Author => (members[0].author.clone(), "group.none.author"),
        GroupPreset::AuthorSeries if level >= 1 => (members[0].series.clone(), "group.none.series"),
        GroupPreset::AuthorSeries => (members[0].author.clone(), "group.none.author"),
        GroupPreset::Series => (members[0].series.clone(), "group.none.series"),
        GroupPreset::Year => (
            year_of(members[0].added_at).unwrap_or_default(),
            "group.none.year",
        ),
        GroupPreset::Genre => (members[0].genre.clone(), "group.none.genre"),
        GroupPreset::None => (String::new(), ""),
    };
    if raw.is_empty() && !none_key.is_empty() {
        crate::i18n::tr(none_key).to_string()
    } else {
        raw
    }
}

/// The group key of a book at drill LEVEL 0 (C dim_at(group, 0)): the
/// Author>Series preset's LEVEL-0 dimension is author alone (series only
/// at a deeper drill).
fn group_key(b: &Book, g: GroupPreset) -> String {
    match g {
        GroupPreset::Author | GroupPreset::AuthorSeries => b.author.trim().to_string(),
        GroupPreset::Series => b.series_id.trim().to_string(),
        GroupPreset::Year => year_of(b.added_at).unwrap_or_default(),
        GroupPreset::Genre => b.genre.trim().to_string(),
        GroupPreset::None => String::new(),
    }
}

/// Dimension key at a drill LEVEL (C dim_at): only Author>Series nests —
/// level 0 groups by author, level 1 by series_id.
fn dim_key(b: &Book, g: GroupPreset, level: usize) -> String {
    match g {
        GroupPreset::AuthorSeries if level >= 1 => b.series_id.trim().to_string(),
        _ => group_key(b, g),
    }
}

/// A row of the materialised `view` table (C's BsViewRow).
#[derive(Debug, Clone, Default)]
pub struct ViewRow {
    pub kind: i64,
    pub book_id: String,
    pub series_id: String,
    pub series_name: String,
    pub series_count: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupPreset {
    None = 0,
    AuthorSeries = 1,
    Author = 2,
    Year = 3,
    Genre = 4,
    Series = 5,
}
impl GroupPreset {
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::AuthorSeries,
            2 => Self::Author,
            3 => Self::Year,
            4 => Self::Genre,
            5 => Self::Series,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortMode {
    Title = 0,
    Author = 1,
    Series = 2,
    Recent = 3,
}
impl SortMode {
    pub fn from_i64(v: i64) -> Self {
        match v {
            1 => Self::Author,
            2 => Self::Series,
            3 => Self::Recent,
            _ => Self::Title,
        }
    }
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
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).take(14).collect();
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

/// Year from unix timestamp (Howard-Hinnant civil-from-days).
pub fn ymd_of(ts: i64) -> Option<(i64, i64, i64)> {
    if ts == 0 {
        return None;
    }
    // Howard-Hinnant civil_from_days: the input is days since 1970-01-01,
    // shifted into days since 0000-03-01 before the era math.
    let days = ts.div_euclid(86400) + 719_468;
    let era = days.div_euclid(146097);
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    Some((if m <= 2 { y + 1 } else { y }, m, d))
}

pub fn year_of(ts: i64) -> Option<String> {
    if ts == 0 {
        return None;
    }
    // Same civil_from_days shift as ymd_of (days since 1970 → 0000-03-01).
    let days = ts.div_euclid(86400) + 719_468;
    let era = days.div_euclid(146097);
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    // The March-based year runs Mar..Feb: January and February belong to
    // the following calendar year.
    let y = if mp >= 10 { y + 1 } else { y };
    Some(format!("{y}"))
}

/// Escape LIKE special chars (% _ \).
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Build an FTS5 MATCH query string from the raw user query.
/// Produces a phrase-prefix form: `"w1 w2" *` matching the C
/// `fts_query_from`.  Doubles embedded quotes, splits on whitespace.
fn build_fts_query(q: &str) -> String {
    let raw = q.trim();
    if raw.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(raw.len() + 4);
    out.push('"');
    let words = raw.split_whitespace().peekable();
    let mut first = true;
    for word in words {
        if !first {
            out.push(' ');
        }
        first = false;
        for c in word.chars() {
            if c == '"' {
                out.push('"');
                out.push('"');
            } else {
                out.push(c);
            }
        }
    }
    if out.len() <= 1 {
        return String::new();
    }
    out.push('"');
    out.push(' ');
    out.push('*');
    out
}

/// Exclusive upper bound for a prefix range scan (C suggest_upper_bound).
/// Always returns a valid bound for non-empty prefixes.
fn suggest_upper_bound(prefix: &str) -> String {
    if prefix.is_empty() {
        return String::new();
    }
    let mut bytes = prefix.as_bytes().to_vec();
    // Increment the last byte, walking back through 0xFF bytes.
    for i in (0..bytes.len()).rev() {
        if bytes[i] < 0xFF {
            bytes[i] += 1;
            bytes.truncate(i + 1);
            return String::from_utf8(bytes).unwrap_or_else(|_| format!("{prefix}\u{FFFF}"));
        }
    }
    // All bytes were 0xFF — append a high char.
    format!("{prefix}\u{FFFF}")
}

/// Map a rusqlite row to a Book (the C app's row_to_book pattern).
fn row_to_book(row: &rusqlite::Row<'_>) -> rusqlite::Result<Book> {
    Ok(Book {
        id: row.get(0)?,
        title: row.get(1)?,
        author: row.get(2)?,
        series: row.get(3)?,
        series_id: row.get(4)?,
        series_idx: row.get(5)?,
        ext: row.get(6)?,
        size: row.get(7)?,
        downloaded: row.get::<_, i64>(8)? != 0,
        local_path: row.get(9)?,
        added_at: row.get(10)?,
        filename: row.get(11)?,
        source: row.get(12)?,
        search_text: row.get(13)?,
        genre: row.get(14)?,
    })
}

fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(concat!(
        "CREATE TABLE IF NOT EXISTS books(",
        " id TEXT PRIMARY KEY,",
        " title TEXT, author TEXT, series TEXT, series_id TEXT,",
        " local_path TEXT, added_at INTEGER,",
        " filename TEXT, source TEXT, search_text TEXT, genre TEXT);",
        "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);",
        "CREATE TABLE IF NOT EXISTS search_history(term TEXT PRIMARY KEY, ts INTEGER);",
        "CREATE TABLE IF NOT EXISTS local_meta(",
        " id TEXT PRIMARY KEY,",
        " title TEXT, author TEXT, series TEXT, series_idx REAL);",
        "CREATE TABLE IF NOT EXISTS view(",
        " pos INTEGER PRIMARY KEY, kind INTEGER, book_id TEXT, series_id TEXT,",
        " series_name TEXT, series_count INTEGER);",
        // Suggest tables (C eh_store.c schema).
        "CREATE TABLE IF NOT EXISTS suggest(",
        " term TEXT NOT NULL, book_id TEXT NOT NULL,",
        " PRIMARY KEY(term, book_id)) WITHOUT ROWID;",
        "CREATE INDEX IF NOT EXISTS idx_suggest_book ON suggest(book_id);",
        "CREATE TABLE IF NOT EXISTS suggest_rank(",
        " term TEXT PRIMARY KEY, cnt INTEGER NOT NULL DEFAULT 0) WITHOUT ROWID;",
        // Local read markers (the long-press "mark as read"; the
        // firmware progress db is read-only for the app).
        "CREATE TABLE IF NOT EXISTS read_markers(",
        " book_id TEXT PRIMARY KEY, is_read INTEGER NOT NULL) WITHOUT ROWID;",
    ))?;
    // Add missing columns (migrations).
    for (col, kind) in MIGRATE_COLUMNS {
        let sql = format!("ALTER TABLE books ADD COLUMN {col} {kind}");
        let _ = conn.execute_batch(&sql);
    }
    // local_meta gained the series columns with the EPUB metadata
    // import; the ALTERs are ignored once the columns exist.
    for col in ["series TEXT", "series_idx REAL"] {
        let _ = conn.execute_batch(&format!("ALTER TABLE local_meta ADD COLUMN {col}"));
    }
    Ok(())
}

fn init_fts(conn: &Connection) -> bool {
    // Returns true when FTS5 is available.
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(title, author, series, search_text, content='books', content_rowid=rowid)"
    )
    .is_ok()
}

// ── One-time legacy JSON import ─────────────────────────────────────────

impl Store {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::BookMeta;

    #[test]
    fn upsert_preserves_downloaded_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let b = BookMeta {
            id: "k1".into(),
            title: "k1".into(),
            ..Default::default()
        };
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
            .query_row(
                "SELECT downloaded, local_path FROM books WHERE id='k1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(dl, 1);
        assert_eq!(lp, "/mnt/x/t.epub");
    }

    #[test]
    fn list_orders_by_added_desc() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        for (id, ts) in [
            ("older", "2026-01-01T00:00:00Z"),
            ("newer", "2026-06-01T00:00:00Z"),
        ] {
            store
                .upsert_book(&BookMeta {
                    id: id.into(),
                    title: id.into(),
                    added_at: Some(ts.into()),
                    ..Default::default()
                })
                .unwrap();
        }
        let list = store.list_books(10, 0).unwrap();
        assert_eq!(list[0].id, "newer");
        assert_eq!(list[1].id, "older");
    }

    #[test]
    fn ungrouped_view_lists_every_book_flat() {
        // Regression: the card-interleave rewrite collapsed the
        // no-grouping projection into a single group (group_key(None) is
        // "" for every book), so the whole library rendered as ONE card.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        for i in 0..4 {
            store
                .upsert_book(&BookMeta {
                    id: format!("k{i}"),
                    title: format!("Book {i}"),
                    ..Default::default()
                })
                .unwrap();
        }
        let n = store.view_rebuild(0, 0, 0, "", &[], "kavita").unwrap();
        assert_eq!(n, 4);
        let kinds: Vec<i64> = store
            .view_page(10, 0)
            .unwrap()
            .iter()
            .map(|r| r.kind)
            .collect();
        assert_eq!(kinds, vec![0, 0, 0, 0]);
    }

    #[test]
    fn grouped_view_makes_interleaved_cards() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        for (id, author) in [("a1", "Ann"), ("a2", "Ann"), ("b1", "Bob")] {
            store
                .upsert_book(&BookMeta {
                    id: id.into(),
                    title: id.into(),
                    authors: vec![author.into()],
                    ..Default::default()
                })
                .unwrap();
        }
        // group=2 = Author; cards at their first member's sort position.
        let n = store.view_rebuild(2, 0, 0, "", &[], "kavita").unwrap();
        assert_eq!(n, 2);
        let kinds: Vec<i64> = store
            .view_page(10, 0)
            .unwrap()
            .iter()
            .map(|r| r.kind)
            .collect();
        assert_eq!(kinds, vec![1, 0]);
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
        store
            .upsert_book(&BookMeta {
                id: "k1".into(),
                title: "Alpha".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .upsert_book(&BookMeta {
                id: "k2".into(),
                title: "Beta".into(),
                ..Default::default()
            })
            .unwrap();
        let r = store.search("alpha", 10, 0, "kavita").unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "k1");
    }

    #[test]
    fn view_and_search_filter_by_active_source() {
        // Regression: after a Local import, switching back to Kavita kept
        // showing the local rows (C views filter on view_source()).
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let mut k = BookMeta {
            id: "kav1".into(),
            title: "Kavita only".into(),
            ..Default::default()
        };
        k.authors = vec!["K Author".into()];
        store.upsert_book(&k).unwrap();
        let mut l = Book {
            id: "loc1".into(),
            title: "Local only".into(),
            author: "L Author".into(),
            source: "local".into(),
            ..Default::default()
        };
        l.search_text = "local only".into();
        store.upsert_book_row(&l).unwrap();

        let n = store.view_rebuild(0, 0, 0, "", &[], "kavita").unwrap();
        assert_eq!(n, 1, "local row leaked into the kavita view");
        assert!(store
            .view_page(10, 0)
            .unwrap()
            .iter()
            .all(|r| r.book_id != "loc1"));

        // Search spans the same filter (C search_fts_decide/view_where).
        let hits = store.search("only", 10, 0, "kavita").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "kav1");
        let hits = store.search("only", 10, 0, "local").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "loc1");
    }

    #[test]
    fn view_rebuild_collapses_single_author() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        for i in 0..3 {
            store
                .upsert_book(&BookMeta {
                    id: format!("k{i}"),
                    title: format!("Book {i}"),
                    authors: vec!["A".into()],
                    ..Default::default()
                })
                .unwrap();
        }
        let n = store.view_rebuild(2, 3, 0, "", &[], "kavita").unwrap(); // group=Author
        assert_eq!(n, 1, "single author collapses to 1 stack");
    }

    #[test]
    fn authorseries_drills_two_levels() {
        use crate::client::BookMeta;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        // Two series under author "Ann", one lone book by "Bob".
        for (id, author, series, sid) in [
            ("a1", "Ann", "Alpha", "s-alpha"),
            ("a2", "Ann", "Alpha", "s-alpha"),
            ("b1", "Ann", "Beta", "s-beta"),
            ("z1", "Bob", "", ""),
        ] {
            store
                .upsert_book(&BookMeta {
                    id: id.into(),
                    title: id.into(),
                    authors: vec![author.into()],
                    series: (!series.is_empty()).then(|| series.to_string()),
                    series_id: (!sid.is_empty()).then(|| sid.to_string()),
                    ..Default::default()
                })
                .unwrap();
        }
        // Level 0: author stacks (Ann card + Bob flat).  The card labels
        // the AUTHOR (the level-0 dimension), never the first member's
        // series — regression for the Author>Series cards showing a
        // series name at the top level.
        let n = store.view_rebuild(1, 0, 0, "", &[], "kavita").unwrap(); // group=AuthorSeries
        assert_eq!(n, 2);
        let rows = store.view_page(10, 0).unwrap();
        let ann = rows
            .iter()
            .find(|r| r.kind == 1 && r.series_id == "Ann")
            .expect("Ann author card");
        assert_eq!(ann.series_name, "Ann", "level-0 card labels the author");
        // Drill into Ann (level 1): series stacks WITHIN the author —
        // Bob's book must not leak through the level-0 scope.
        let n = store.view_rebuild(1, 0, 1, "", &["Ann"], "kavita").unwrap();
        assert_eq!(n, 2, "two series cards under Ann");
        let rows = store.view_page(10, 0).unwrap();
        assert!(
            rows.iter()
                .any(|r| r.kind == 1 && r.series_id == "s-alpha" && r.series_name == "Alpha"),
            "drilled card labels the series"
        );
        // Alpha is a 2-book stack; Beta has a single member so it stays a
        // flat tile at its own sort position.
        assert!(rows
            .iter()
            .any(|r| r.kind == 1 && r.series_id == "s-alpha" && r.series_count == 2));
        assert!(rows.iter().any(|r| r.kind == 0 && r.series_id == "s-beta"));
        let n = store
            .view_rebuild(1, 0, 2, "", &["Ann", "s-alpha"], "kavita")
            .unwrap();
        assert_eq!(n, 2);
        let rows = store.view_page(10, 0).unwrap();
        assert!(rows.iter().all(|r| r.kind == 0));
        assert!(rows.iter().all(|r| r.series_id == "s-alpha"));
    }

    #[test]
    fn authorseries_handles_empty_dims() {
        use crate::client::BookMeta;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        // Author "Ann": a 2-book series PLUS two series-less books; and a
        // 2-book series with NO author at all.
        for (id, author, series, sid) in [
            ("a1", "Ann", "Alpha", "s-al"),
            ("a2", "Ann", "Alpha", "s-al"),
            ("n1", "Ann", "", ""),
            ("n2", "Ann", "", ""),
            ("u1", "", "Seri", "s-u"),
            ("u2", "", "Seri", "s-u"),
        ] {
            store
                .upsert_book(&BookMeta {
                    id: id.into(),
                    title: id.into(),
                    authors: vec![author.into()],
                    series: (!series.is_empty()).then(|| series.to_string()),
                    series_id: (!sid.is_empty()).then(|| sid.to_string()),
                    ..Default::default()
                })
                .unwrap();
        }
        // Level 0: the author-less pair forms ONE card labelled with the
        // group.none.author i18n fallback (never a blank label).
        let n = store.view_rebuild(1, 0, 0, "", &[], "kavita").unwrap();
        assert_eq!(n, 2, "Ann card + unknown-author card");
        let rows = store.view_page(10, 0).unwrap();
        let unknown = rows
            .iter()
            .find(|r| r.kind == 1 && r.series_id.is_empty())
            .expect("unknown-author card");
        assert_eq!(unknown.series_name, "Unknown author");
        // Level 1 under Ann: the series card PLUS the series-less books
        // directly as flat tiles (no "unknown series" drill).
        let n = store.view_rebuild(1, 0, 1, "", &["Ann"], "kavita").unwrap();
        assert_eq!(n, 3, "Alpha card + two flat series-less tiles");
        let rows = store.view_page(10, 0).unwrap();
        assert!(rows
            .iter()
            .any(|r| r.kind == 1 && r.series_id == "s-al" && r.series_name == "Alpha"));
        assert_eq!(
            rows.iter().filter(|r| r.kind == 0).count(),
            2,
            "series-less books stay flat"
        );
        // Drilling into the unknown author groups by series as usual.
        let n = store.view_rebuild(1, 0, 1, "", &[""], "kavita").unwrap();
        assert_eq!(n, 1, "the pair forms one Seri card");
        let rows = store.view_page(10, 0).unwrap();
        assert!(rows.iter().any(|r| r.kind == 1 && r.series_name == "Seri"));
    }

    #[test]
    fn suggest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store
            .suggest_set(
                "b1",
                &["potter".into(), "harry".into(), "harry potter".into()],
            )
            .unwrap();
        let list = store.suggest_list("pott", 10).unwrap();
        assert!(
            list.contains(&"potter".into()),
            "suggest_list should find 'potter' from prefix 'pott'"
        );
    }

    #[test]
    fn suggest_folded_diacritic() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store.suggest_set("b1", &["songgong".into()]).unwrap();
        let list = store.suggest_list("songgong", 10).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], "songgong");
    }

    #[test]
    fn search_text_stored_and_searchable() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store
            .upsert_book(&BookMeta {
                id: "d1".into(),
                title: "Sŏnggong".into(),
                search_text: Some("songgong".into()),
                ..Default::default()
            })
            .unwrap();
        let r = store.search("songgong", 10, 0, "kavita").unwrap();
        assert_eq!(
            r.len(),
            1,
            "search_text should match folded diacritic query"
        );
        assert_eq!(r[0].id, "d1");
    }

    #[test]
    fn search_history_dedupes_and_trims() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        for _ in 0..5 {
            store.search_add("potter").unwrap();
        }
        assert_eq!(store.search_count().unwrap(), 1);
        // Add more than max (honour EH_SEARCH_HISTORY_MAX = 20).
        for i in 0..25 {
            store.search_add(&format!("term{i}")).unwrap();
        }
        assert_eq!(store.search_count().unwrap(), 20);
    }

    #[test]
    fn search_text_null_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store
            .upsert_book(&BookMeta {
                id: "n1".into(),
                title: "Normal".into(),
                search_text: None,
                ..Default::default()
            })
            .unwrap();
        let b = store.get_book("n1").unwrap().unwrap();
        assert!(
            b.search_text.is_empty(),
            "null search_text should store empty string"
        );
    }
    #[test]
    fn delete_source_and_local_meta_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store
            .upsert_book(&BookMeta {
                id: "k1".into(),
                title: "Kavita".into(),
                ..Default::default()
            })
            .unwrap();
        let local = Book {
            id: "fld_abc".into(),
            title: "Local".into(),
            ext: "epub".into(),
            downloaded: true,
            local_path: "/mnt/ext1/a.epub".into(),
            filename: "a.epub".into(),
            source: "local".into(),
            ..Default::default()
        };
        store.upsert_book_row(&local).unwrap();
        assert_eq!(store.count().unwrap(), 2);

        // Metadata cache roundtrip (C eh_store_local_meta_get/put).
        assert!(store.local_meta_get("fld_abc").is_none());
        store
            .local_meta_put("fld_abc", "T", "A", "S", Some(3.0))
            .unwrap();
        assert_eq!(
            store.local_meta_get("fld_abc"),
            Some(("T".into(), "A".into(), "S".into(), Some(3.0)))
        );

        // A local re-import replaces the source wholesale; Kavita survives.
        store.delete_source("local").unwrap();
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.get_book("k1").unwrap().unwrap().title, "Kavita");
        assert!(store.get_book("fld_abc").unwrap().is_none());
    }

    #[test]
    fn upsert_book_row_writes_full_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        let b = Book {
            id: "fld_x".into(),
            title: "Meta Title".into(),
            author: "Meta Author".into(),
            ext: "fb2".into(),
            size: 1234,
            downloaded: true,
            local_path: "/mnt/ext1/x.fb2".into(),
            filename: "x.fb2".into(),
            source: "local".into(),
            ..Default::default()
        };
        store.upsert_book_row(&b).unwrap();
        let got = store.get_book("fld_x").unwrap().unwrap();
        assert_eq!(got.title, "Meta Title");
        assert_eq!(got.author, "Meta Author");
        assert_eq!(got.ext, "fb2");
        assert_eq!(got.size, 1234);
        assert!(got.downloaded);
        assert_eq!(got.source, "local");
    }

    #[test]
    fn view_scan_cap_guards_grouped_but_sql_flat_bypasses_it() {
        // >VIEW_SCAN_CAP books in one source: the ungrouped shape
        // projects in SQL and stays complete at any size, while the
        // grouped engine's Rust-side scan is bounded by the cap (the RSS
        // guard documented on VIEW_SCAN_CAP — truncation beats OOM on a
        // reader).
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("cap.db")).unwrap();
        let n = VIEW_SCAN_CAP + 2;
        store.conn.execute("BEGIN", []).unwrap();
        {
            let mut ins = store
                .conn
                .prepare(concat!(
                    "INSERT INTO books(id,title,author,series,series_id,series_idx,",
                    " ext,size,downloaded,local_path,added_at,",
                    " filename,source,search_text,genre)",
                    " VALUES(?1,?2,?3,'','',0,'',0,0,'',0,'','kavita','','')"
                ))
                .unwrap();
            for i in 0..n {
                // Distinct authors => one lone tile per book when grouped,
                // so the grouped row count exposes the scan width.
                ins.execute(params![format!("k{i}"), format!("t{i}"), format!("a{i}")])
                    .unwrap();
            }
        }
        store.conn.execute("COMMIT", []).unwrap();

        // Flat: the SQL projection sees EVERY book — no cap.
        let flat = store
            .view_rebuild(
                GroupPreset::None as i64,
                SortMode::Title as i64,
                0,
                "",
                &[],
                "kavita",
            )
            .unwrap();
        assert_eq!(flat as usize, n);
        assert_eq!(store.view_total(), n);

        // Grouped by author: exactly the documented cap survives.
        let grouped = store
            .view_rebuild(
                GroupPreset::Author as i64,
                SortMode::Title as i64,
                0,
                "",
                &[],
                "kavita",
            )
            .unwrap();
        assert_eq!(grouped as usize, VIEW_SCAN_CAP);
    }
}

#[cfg(test)]
mod date_tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip_the_epoch_shift() {
        // 1970-01-01, 2023-01-01 (the mock corpus date), 2026-08-25 and a
        // leap-day: the +719468 shift must land the civil year exactly.
        // ts == 0 is the "unknown added_at" sentinel → None by contract.
        assert!(ymd_of(0).is_none());
        assert!(year_of(0).is_none());
        let cases = [
            (1_672_531_200, (2023, 1, 1)),
            (1_771_977_600, (2026, 2, 25)),
            (951_782_400, (2000, 2, 29)),
        ];
        for (ts, want) in cases {
            let (y, m, d) = ymd_of(ts).unwrap();
            assert_eq!((y, m, d), want, "ymd_of({ts})");
            assert_eq!(year_of(ts).unwrap(), format!("{y}"));
        }
    }
}
