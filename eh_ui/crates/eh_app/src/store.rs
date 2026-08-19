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
             (SELECT rowid FROM search_history ORDER BY ts DESC LIMIT {})",
            EH_SEARCH_HISTORY_MAX
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
            .query_map(params![limit as i64, offset as i64], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Filtered shelf page: books whose title/author/series/search_text
    /// match `query` (the C app's LIKE `view_where` fallback — ASCII
    /// case-insensitive substring, `%`/`_`/`\` escaped).  When FTS5 is
    /// available and the query is safe, uses FTS MATCH for better ranking.
    /// Empty query = the whole shelf.  Same column/order shape as `list_books`.
    pub fn search(&self, query: &str, limit: usize, offset: usize) -> rusqlite::Result<Vec<Book>> {
        if query.trim().is_empty() {
            return self.list_books(limit, offset);
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
                    return self.search_fts_sql(&fts_q, limit, offset);
                }
            }
        }

        // LIKE fallback.
        let pat = escape_like(query);
        let like_query = format!("%{}%", pat);
        self.search_like_sql(&like_query, limit, offset)
    }

    fn search_fts_sql(&self, fts_q: &str, limit: usize, offset: usize) -> rusqlite::Result<Vec<Book>> {
        let mut stmt = self.conn.prepare(concat!(
            "SELECT id,title,author,series,series_id,series_idx,",
            " ext,size,downloaded,local_path,added_at,",
            " filename,source,search_text,genre",
            " FROM books",
            " WHERE rowid IN (SELECT rowid FROM search_fts WHERE search_fts MATCH ?1)",
            " ORDER BY added_at DESC, title COLLATE NOCASE, id",
            " LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = stmt
            .query_map(params![fts_q, limit as i64, offset as i64], |r| row_to_book(r))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn search_like_sql(&self, like_query: &str, limit: usize, offset: usize) -> rusqlite::Result<Vec<Book>> {
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
            .query_map(params![like_query, limit as i64, offset as i64], row_to_book)?
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
            let mut q = self.conn.prepare("SELECT term FROM suggest WHERE book_id=?1")?;
            let rows = q.query_map([book_id], |r| r.get::<_, String>(0))?;
            for row in rows {
                if let Ok(t) = row {
                    old.push(t);
                }
            }
        }
        // Delete old edges.
        self.conn.execute("DELETE FROM suggest WHERE book_id=?1", [book_id])?;
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
        self.conn.execute_batch("DELETE FROM suggest_rank WHERE cnt=0")?;
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
        let sql = format!(
            "SELECT term FROM suggest WHERE term >= ?1 AND term < ?2 \
             GROUP BY term ORDER BY COUNT(*) DESC, term ASC LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![norm, bound, limit as i64], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn suggest_list_ranked(&self, norm: &str, bound: &str, limit: usize) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT term FROM suggest_rank WHERE term >= ?1 AND term < ?2 \
             ORDER BY cnt DESC, term ASC LIMIT ?3"
        )?;
        let rows = stmt
            .query_map(params![norm, bound, limit as i64], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Whether the store has any books with timestamps, series, etc.
    /// Returns (has_author_data, has_series_data, has_year_data, has_genre_data).
    pub fn dim_availability(&self) -> rusqlite::Result<(bool, bool, bool, bool)> {
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
        Ok((true, s, y, true))
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
                 SELECT rowid, title, author, series, COALESCE(search_text,'') FROM books"
            );
        }
    }

    // ── view rebuild ───────────────────────────────────────────────────

    /// Rebuild the materialised `view` table from the current books,
    /// grouped by `group` and ordered by `sort`, filtered by `query`
    /// within `scope`.  Returns the total rows (C view_rebuild returns
    /// n_stacks, stored in g_view_total).
    pub fn view_rebuild(
        &self,
        group: i64,
        sort: i64,
        drill: i64,
        query: &str,
        _scope: &str,
    ) -> rusqlite::Result<i64> {
        fn group_key(b: &Book, g: GroupPreset) -> String {
            match g {
                GroupPreset::Author => b.author.trim().to_string(),
                GroupPreset::AuthorSeries => {
                    format!("{}|{}", b.author.trim(), b.series_id.trim())
                }
                GroupPreset::Series => b.series_id.trim().to_string(),
                GroupPreset::Year => year_of(b.added_at).unwrap_or_default(),
                GroupPreset::Genre => b.genre.trim().to_string(),
                GroupPreset::None => String::new(),
            }
        }

        let group = GroupPreset::from_i64(group);
        let sort = SortMode::from_i64(sort);
        let grouped = group != GroupPreset::None;
        let drilled = drill > 0;

        let mut all = self.list_books(10_000, 0)?;
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

        // Sort.
        all.sort_by(|a, b| {
            let c = match sort {
                SortMode::Title => a.title.cmp(&b.title),
                SortMode::Author => a.author.cmp(&b.author).then(a.title.cmp(&b.title)),
                SortMode::Series => {
                    a.series
                        .cmp(&b.series)
                        .then(a.series_idx.partial_cmp(&b.series_idx).unwrap_or(std::cmp::Ordering::Equal))
                        .then(a.title.cmp(&b.title))
                }
                SortMode::Recent => b.added_at.cmp(&a.added_at).then(a.title.cmp(&b.title)),
            };
            c.then(a.id.cmp(&b.id))
        });

        self.conn.execute("DELETE FROM view", [])?;
        self.conn.execute("BEGIN", [])?;
        let result = (|| -> rusqlite::Result<i64> {
            let mut pos = 0i64;
            if grouped {
                use std::collections::HashMap;
                let mut groups: HashMap<String, Vec<Book>> = HashMap::new();
                if drilled {
                    // When drilled, show flat books of the matching group.
                    let key = group_key(&all.first().cloned().unwrap_or_default(), group);
                    all.retain(|b| group_key(b, group) == key);
                    for b in &all {
                        self.conn.execute(
                            "INSERT INTO view(pos,kind,book_id,series_id,series_name,series_count) VALUES(?1,?2,?3,?4,?5,?6)",
                            rusqlite::params![pos, 0, b.id, b.series_id, b.title, 1],
                        )?;
                        pos += 1;
                    }
                } else {
                    for b in &all {
                        let k = group_key(b, group);
                        groups.entry(k).or_default().push(b.clone());
                    }
                    for (_k, members) in &groups {
                        let kind = if members.len() > 1 { 1 } else { 0 };
                        let sid = members[0].series_id.clone();
                        let label = group_label(members, group);
                        self.conn.execute(
                            "INSERT INTO view(pos,kind,book_id,series_id,series_name,series_count) VALUES(?1,?2,?3,?4,?5,?6)",
                            rusqlite::params![pos, kind, members[0].id, sid, label, members.len() as i64],
                        )?;
                        pos += 1;
                    }
                }
            } else {
                for b in &all {
                    self.conn.execute(
                        "INSERT INTO view(pos,kind,book_id,series_id,series_name,series_count) VALUES(?1,?2,?3,?4,?5,?6)",
                        rusqlite::params![pos, 0, b.id, b.series_id, b.title, 1],
                    )?;
                    pos += 1;
                }
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

    pub fn view_page(
        &self,
        limit: usize,
        offset: usize,
    ) -> rusqlite::Result<Vec<ViewRow>> {
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

    /// List books matching a series_id scope (C list_sorted helper for
    /// download_series / delete_series).
    pub fn list_sorted(&self, _sort: SortMode, _query: &str, _limit: usize, scope: &str) -> rusqlite::Result<Vec<Book>> {
        self.conn
            .prepare(concat!(
                "SELECT id,title,author,series,series_id,series_idx,",
                " ext,size,downloaded,local_path,added_at,",
                " filename,source,search_text,genre",
                " FROM books WHERE series_id=?1 ORDER BY series_idx, title"
            ))?
            .query_map([scope], |r| {
                Ok(Book {
                    id: r.get(0)?, title: r.get(1)?, author: r.get(2)?,
                    series: r.get(3)?, series_id: r.get(4)?, series_idx: r.get(5)?,
                    ext: r.get(6)?, size: r.get(7)?,
                    downloaded: r.get::<_, i64>(8)? != 0,
                    local_path: r.get(9)?, added_at: r.get(10)?,
                    filename: r.get(11)?, source: r.get(12)?,
                    search_text: r.get(13)?, genre: r.get(14)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
    }
}

fn group_label(members: &[Book], g: GroupPreset) -> String {
    match g {
        GroupPreset::Author => members[0].author.clone(),
        GroupPreset::AuthorSeries => members[0].series.clone(),
        GroupPreset::Series => members[0].series.clone(),
        GroupPreset::Year => year_of(members[0].added_at).unwrap_or_default(),
        GroupPreset::Genre => members[0].genre.clone(),
        GroupPreset::None => String::new(),
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

/// Year from unix timestamp (Howard-Hinnant civil-from-days).
pub fn year_of(ts: i64) -> Option<String> {
    if ts == 0 {
        return None;
    }
    let days = ts.div_euclid(86400);
    let era = if days >= -719468 {
        days.div_euclid(146097)
    } else {
        (days - 146096).div_euclid(146097)
    };
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    Some(format!("{}", y))
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
    let mut words = raw.split_whitespace().peekable();
    let mut first = true;
    while let Some(word) = words.next() {
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
    ))?;
    // Add missing columns (migrations).
    for (col, kind) in MIGRATE_COLUMNS {
        let sql = format!("ALTER TABLE books ADD COLUMN {col} {kind}");
        let _ = conn.execute_batch(&sql);
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
        let b = BookMeta { id: "k1".into(), title: "k1".into(), ..Default::default() };
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
        store
            .upsert_book(&BookMeta { id: "k1".into(), title: "Alpha".into(), ..Default::default() })
            .unwrap();
        store
            .upsert_book(&BookMeta { id: "k2".into(), title: "Beta".into(), ..Default::default() })
            .unwrap();
        let r = store.search("alpha", 10, 0).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "k1");
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
        let n = store.view_rebuild(2, 3, 0, "", "").unwrap(); // group=Author
        assert_eq!(n, 1, "single author collapses to 1 stack");
    }

    #[test]
    fn suggest_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("t.db")).unwrap();
        store.suggest_set("b1", &["potter".into(), "harry".into(), "harry potter".into()]).unwrap();
        let list = store.suggest_list("pott", 10).unwrap();
        assert!(list.contains(&"potter".into()), "suggest_list should find 'potter' from prefix 'pott'");
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
        store.upsert_book(&BookMeta {
            id: "d1".into(),
            title: "Sŏnggong".into(),
            search_text: Some("songgong".into()),
            ..Default::default()
        }).unwrap();
        let r = store.search("songgong", 10, 0).unwrap();
        assert_eq!(r.len(), 1, "search_text should match folded diacritic query");
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
        store.upsert_book(&BookMeta {
            id: "n1".into(),
            title: "Normal".into(),
            search_text: None,
            ..Default::default()
        }).unwrap();
        let b = store.get_book("n1").unwrap().unwrap();
        assert!(b.search_text.is_empty(), "null search_text should store empty string");
    }
}
