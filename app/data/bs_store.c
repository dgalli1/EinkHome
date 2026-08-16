/* bs_store.c - part of the bookshelf app (see bs_core.h)
 *
 * On-device library persistence and view projection, backed by the
 * firmware's own SQLite (libsqlite3.so.0 in /ebrmain/lib, loaded via
 * LD_LIBRARY_PATH).
 *
 * The books table is the single source of truth; the old in-memory
 * g_lib[] master array is gone.  A 100k-book library never fits in
 * device RAM, so every consumer pages through the store instead:
 *
 *  - sync writes one batch of rows per /sync/delta round inside a
 *    single transaction (store_upsert_book), resuming via a persisted
 *    cursor;
 *  - the grid/list renders from a materialised `view` table that
 *    encodes the active filter/sort/group/drill as a pos-ordered row
 *    list; view_rebuild() fills it in SQL, view_fetch_page() reads one
 *    screenful;
 *  - downloads and the context menu look rows up by id
 *    (store_get_book).
 *
 * RAM cost is O(page rows) + O(cover slots), independent of library
 * size.  Why SQLite over the old hand-rolled JSON file: atomic
 * transactions (a power cut mid-sync can never leave a half-written
 * store), indexed lookups instead of full-file reparses, and a paged
 * b-tree beats a 100k-element array on e-ink hardware.
 */

#include "bs_core.h"
#include <stdlib.h>
#include "cJSON.h"
#include "bs_config.h"
#include "bs_downloads.h"
#include "bs_launcher.h"
#include "bs_model.h"
#include "bs_net.h"
#include "bs_store.h"
#include "bs_ui.h"

#include "sqlite3.h"

static sqlite3 *g_db;

/* Prepared statements reused across sync rounds — a 200-round sync
 * would otherwise prepare thousands.  Prepared lazily on first use;
 * every call resets + clears bindings.  Finalized in store_close (a
 * NULL finalize is a no-op), so a close/reopen re-prepares cleanly. */
static sqlite3_stmt *g_st_upsert_lookup; /* SELECT downloaded, local_path */
static sqlite3_stmt *g_st_upsert;        /* INSERT OR REPLACE INTO books */
static sqlite3_stmt *g_st_suggest_del;   /* DELETE FROM suggest */
static sqlite3_stmt *g_st_suggest_ins;   /* INSERT OR IGNORE INTO suggest */
static sqlite3_stmt *g_st_get_book;      /* SELECT ... FROM books WHERE id */
static sqlite3_stmt *g_st_set_downloaded; /* UPDATE books SET downloaded ... */
static sqlite3_stmt *g_st_next_dl_probes;      /* rowid-keyset id scan */
static sqlite3_stmt *g_st_suggest_rank_refresh; /* recompute one term's rank */
static sqlite3_stmt *g_st_suggest_rank_zero;    /* drop zero-count rank rows */
static sqlite3_stmt *g_st_fts_rowid;  /* SELECT rowid FROM books WHERE id */
static sqlite3_stmt *g_st_fts_del;    /* DELETE FROM search_fts WHERE rowid */
static sqlite3_stmt *g_st_fts_ins;    /* INSERT INTO search_fts(...) */
/* 1 when the firmware SQLite lacks the FTS5 module; routes committed
 * search to the LIKE fallback and makes every FTS write a no-op. */
static int g_no_fts;
/* Lazily probed: 1 once suggest_rank holds any term (see
 * suggest_rank_ready). */
static int g_rank_ready = -1;
/* Committed-search decision cache (see search_fts_decide): the query
 * the decision was made for, the FTS MATCH string to bind, and whether
 * the FTS index or the LIKE scan serves it. */
static char g_search_q_cache[BS_MAX_QUERY_LEN];
static char g_fts_query[BS_MAX_QUERY_LEN * 4 + 16];
static int g_search_use_fts;

/* Prepare `sql` into *slot once; returns the statement, or NULL when
 * the db is closed or the prepare failed (callers degrade exactly as
 * the old per-call prepare did). */
static sqlite3_stmt *
st_prep_once(sqlite3_stmt **slot, const char *sql)
{
  if (*slot == NULL && g_db != NULL)
    sqlite3_prepare_v2(g_db, sql, -1, slot, NULL);
  return *slot;
}

/* ── schema ------------------------------------------------------------- */

static const char *const SCHEMA_SQL =
    "CREATE TABLE IF NOT EXISTS books("
    " id TEXT PRIMARY KEY,"
    " title TEXT, author TEXT, series TEXT, series_id TEXT,"
    " local_path TEXT, added_at INTEGER,"
    " filename TEXT, source TEXT, search_text TEXT, genre TEXT);"
    "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);"
    "CREATE TABLE IF NOT EXISTS view("
    " pos INTEGER PRIMARY KEY,"
    " kind INTEGER, book_id TEXT, series_id TEXT,"
    " series_name TEXT, series_count INTEGER);"
    "CREATE TABLE IF NOT EXISTS search_history("
    " term TEXT PRIMARY KEY, ts INTEGER);"
    "CREATE TABLE IF NOT EXISTS local_meta("
    " id TEXT PRIMARY KEY,"
    " title TEXT, author TEXT);"
    "CREATE INDEX IF NOT EXISTS idx_books_title"
    " ON books(title COLLATE NOCASE, id);"
    "CREATE INDEX IF NOT EXISTS idx_books_author"
    " ON books(author COLLATE NOCASE, title COLLATE NOCASE, id);"
    "CREATE INDEX IF NOT EXISTS idx_books_series"
    " ON books(series_id, series_idx, title COLLATE NOCASE, id);"
    "CREATE INDEX IF NOT EXISTS idx_books_added"
    " ON books(added_at DESC, title COLLATE NOCASE, id);"
    "CREATE INDEX IF NOT EXISTS idx_books_dl"
    " ON books(downloaded, title COLLATE NOCASE, id);"
    /* Search-completion term index: one (term, book_id) edge per
     * suggestion term the server derived from the book's title,
     * authors and series.  WITHOUT ROWID makes the table b-tree the
     * prefix-lookup index (range scan on term); the book_id index
     * serves the per-book DELETE on sync.  No schema_version bump:
     * an empty table just yields no suggestions until the first
     * sync populates it. */
    "CREATE TABLE IF NOT EXISTS suggest("
    " term TEXT NOT NULL, book_id TEXT NOT NULL,"
    " PRIMARY KEY(term, book_id)) WITHOUT ROWID;"
    "CREATE INDEX IF NOT EXISTS idx_suggest_book ON suggest(book_id);"
    /* Aggregated suggestion-rank table: one row per term holding how
     * many books contain it.  store_suggest_set recomputes the rows
     * for a book's affected terms straight from the `suggest` edge
     * table, so store_suggest_list is a pure ordered range scan (no
     * per-keystroke GROUP BY over the whole prefix).  WITHOUT ROWID
     * makes the b-tree the prefix-lookup index.  No schema_version
     * bump: an older DB has edges but no rank yet, and
     * store_suggest_list falls back to the edge-table GROUP BY until
     * the next sync populates it. */
    "CREATE TABLE IF NOT EXISTS suggest_rank("
    " term TEXT PRIMARY KEY, cnt INTEGER NOT NULL DEFAULT 0)"
    " WITHOUT ROWID;";

/* Build the absolute store path next to the config file. */
static void store_path(char *out, size_t cap) {
  char dir[BS_MAX_PATH_LEN];
  bs_dirname_of(bs_g_config_path, dir, sizeof dir);
  snprintf(out, cap, "%s/%s", dir, BS_LIB_DB_FILENAME);
}

/* 1 when `table` has a column named `col` (per PRAGMA table_info). */
static int store_has_column(const char *table, const char *col) {
  char sql[96];
  sqlite3_stmt *st = NULL;
  int found = 0;
  snprintf(sql, sizeof sql, "PRAGMA table_info(%s)", table);
  if (sqlite3_prepare_v2(g_db, sql, -1, &st, NULL) != SQLITE_OK)
    return 1; /* cannot introspect; assume present */
  while (sqlite3_step(st) == SQLITE_ROW) {
    const char *name = (const char *)sqlite3_column_text(st, 1);
    if (name != NULL && strcmp(name, col) == 0) {
      found = 1;
      break;
    }
  }
  sqlite3_finalize(st);
  return found;
}

/* Stores created by older app builds predate some columns; CREATE TABLE
 * IF NOT EXISTS leaves the old shape untouched, so add whatever is
 * missing instead of failing the schema step. */
static void store_migrate_columns(void) {
  static const struct {
    const char *col;
    const char *type;
  } mig[] = {
      {"series_idx", "REAL"}, {"ext", "TEXT"},
      {"size", "INTEGER"},    {"downloaded", "INTEGER"},
      {"local_path", "TEXT"}, {"added_at", "INTEGER"},
      {"filename", "TEXT"},   {"source", "TEXT"},
      {"genre", "TEXT"},
      {"search_text", "TEXT"},
  };
  int changed = 0;
  for (size_t i = 0; i < sizeof mig / sizeof mig[0]; i++) {
    int has = store_has_column("books", mig[i].col);
    bs_LOG("[bookshelf] store: dbg col=%s has=%d err=%s\n", mig[i].col, has,
        g_db ? sqlite3_errmsg(g_db) : "?");
    if (has)
      continue;
    char sql[128];
    snprintf(sql, sizeof sql, "ALTER TABLE books ADD COLUMN %s %s", mig[i].col,
             mig[i].type);
    if (sqlite3_exec(g_db, sql, NULL, NULL, NULL) != SQLITE_OK)
      bs_LOG("[bookshelf] store: migrate %s failed: %s\n", mig[i].col,
          sqlite3_errmsg(g_db));
    else
      changed = 1;
  }
  if (changed) {
    /* Rows written before the migration carry no data in the new
     * columns; a full re-sync repopulates them.  The marker makes
     * the reset one-shot: otherwise every boot would reset the
     * cursor and re-sync the whole library. */
    bs_store_set_cursor(0);
    bs_store_set_meta("schema_version", "2");
    bs_LOG("[bookshelf] store: schema migrated; sync cursor reset\n");
  }
}

/* Persist one meta key/value pair (used for one-shot migration markers). */
static int bind_text_trunc(sqlite3_stmt *st, int i, const char *s);

void bs_store_set_meta(const char *key, const char *value) {
  if (g_db == NULL)
    return;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(
          g_db, "INSERT OR REPLACE INTO meta(key, value) VALUES(?1, ?2)", -1,
          &st, NULL) != SQLITE_OK)
    return;
  bind_text_trunc(st, 1, key);
  bind_text_trunc(st, 2, value);
  sqlite3_step(st);
  sqlite3_finalize(st);
}

int bs_store_meta_value(const char *key, char *out, size_t cap) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db, "SELECT value FROM meta WHERE key=?1", -1, &st,
                         NULL) != SQLITE_OK)
    return 0;
  bind_text_trunc(st, 1, key);
  int found = 0;
  if (sqlite3_step(st) == SQLITE_ROW) {
    snprintf(out, cap, "%s", (const char *)sqlite3_column_text(st, 0));
    found = 1;
  }
  sqlite3_finalize(st);
  return found;
}

/* ── open / close -------------------------------------------------------- */

/* Import a legacy JSON store into an open db.  The JSON text is the old
 * on-disk format (a bare array of book objects), parsed with cJSON so
 * no in-memory library array is needed.  Returns the number of books
 * imported. */
static int store_import_legacy(const char *legacy_path) {
  char *txt = bs_read_text_file(legacy_path);
  if (txt == NULL)
    return 0;
  cJSON *root = cJSON_Parse(txt);
  free(txt);
  if (root == NULL) {
    bs_LOG("[bookshelf] store: legacy import: JSON parse failed\n");
    return -1;
  }
  int count = 0;
  int failed = 0;
  BsBook tmp;
  if (sqlite3_exec(g_db, "BEGIN", NULL, NULL, NULL) != SQLITE_OK) {
    bs_LOG("[bookshelf] store: legacy import BEGIN failed: %s\n",
        sqlite3_errmsg(g_db));
    cJSON_Delete(root);
    return -1;
  }
  if (cJSON_IsArray(root)) {
    const cJSON *it;
    cJSON_ArrayForEach(it, root) {
      if (!cJSON_IsObject(it)) {
        failed = 1;
        continue;
      }
      if (bs_parse_book_obj(it, &tmp, 1) == 0 && bs_store_upsert_book(&tmp) == 0)
        count++;
      else
        failed = 1;
    }
  } else {
    failed = 1;
  }
  cJSON_Delete(root);
  if (failed || sqlite3_exec(g_db, "COMMIT", NULL, NULL, NULL) != SQLITE_OK) {
    /* Any parse/upsert error or a failed COMMIT aborts the whole
     * import: roll back and report failure so the caller keeps the
     * JSON and the import re-runs next boot. */
    sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, NULL);
    return -1;
  }
  return count;
}

/* Open (and create on first use) the library database.  When no db
 * exists but a legacy JSON store does, import it once and rename the
 * JSON out of the way.  Falls back gracefully (g_db stays NULL) when
 * the directory is not writable; the app then runs online-only. */
void bs_store_open(void) {
  char path[BS_MAX_PATH_LEN * 2];
  store_path(path, sizeof path);

  if (sqlite3_open(path, &g_db) != SQLITE_OK) {
    bs_LOG("[bookshelf] store: open failed %s: %s\n", path,
        g_db ? sqlite3_errmsg(g_db) : "?");
    sqlite3_close(g_db);
    g_db = NULL;
    return;
  }

  /* One connection, journal mode untouched (WAL would hammer the
   * device flash): a transient lock holder (another process, or a
   * crash-recovery pass) should delay us, not fail with SQLITE_BUSY. */
  sqlite3_busy_timeout(g_db, 2000);

  /* Stores from older builds predate some columns; the index in
   * SCHEMA_SQL would fail on them, so add missing columns first.
   * `id` is present in every schema version, so it doubles as the
   * table-exists probe (PRAGMA table_info is reliable here, a
   * sqlite_master SELECT is not on the guest's sqlite).  The marker
   * makes the migration one-shot: the cursor reset that follows a
   * real schema change must not repeat on every boot. */
  {
    char ver[8] = "";
    if (bs_store_meta_value("schema_version", ver, sizeof ver) != 1 ||
        strcmp(ver, "2") != 0) {
      if (store_has_column("books", "id"))
        store_migrate_columns();
      bs_store_set_meta("schema_version", "2");
    }
    /* v3: rebuild the series index as a covering index
     * (series_id, series_idx, title COLLATE NOCASE, id) so that
     * store_series_ids' ORDER BY ... title is served by the index
     * instead of a temp sort of the whole matching set.  v2 stores
     * carry the old non-covering index and CREATE INDEX IF NOT EXISTS
     * would no-op on them, so drop + recreate explicitly.  Fresh
     * stores get the new shape straight from SCHEMA_SQL, so on a
     * brand-new db (no books table yet, SCHEMA_SQL runs next) this
     * is just a marker stamp. */
    if (bs_store_meta_value("schema_version", ver, sizeof ver) != 1 ||
        strcmp(ver, "3") != 0) {
      if (store_has_column("books", "id") == 1) {
        sqlite3_exec(g_db, "DROP INDEX IF EXISTS idx_books_series", NULL,
                     NULL, NULL);
        sqlite3_exec(g_db, "CREATE INDEX IF NOT EXISTS idx_books_series"
                           " ON books(series_id, series_idx,"
                           " title COLLATE NOCASE, id)",
                     NULL, NULL, NULL);
      }
      bs_store_set_meta("schema_version", "3");
    }
    /* Column-driven backstop: builds that stamped the v2 marker
     * before filename/source joined the migration list would
     * otherwise skip them forever (the marker check above is
     * satisfied).  The migration only alters genuinely missing
     * columns, so this is a no-op on a healthy store. */
    if (store_has_column("books", "id") == 1 &&
        (store_has_column("books", "filename") != 1 ||
         store_has_column("books", "source") != 1))
      store_migrate_columns();
  }
  if (sqlite3_exec(g_db, SCHEMA_SQL, NULL, NULL, NULL) != SQLITE_OK) {
    /* Introspection can miss a pre-existing table (e.g. a locked or
     * partially-created db); migrate whatever is missing and retry. */
    store_migrate_columns();
    if (sqlite3_exec(g_db, SCHEMA_SQL, NULL, NULL, NULL) != SQLITE_OK) {
      bs_LOG("[bookshelf] store: schema failed: %s\n", sqlite3_errmsg(g_db));
      sqlite3_close(g_db);
      g_db = NULL;
      return;
    }
  }

  /* Committed-search index.  FTS5 may be absent from the firmware
   * build; when it is, CREATE VIRTUAL TABLE errors and g_no_fts routes
   * search to the byte-identical LIKE path (view_where) and skips every
   * FTS write.  External content: the index stores only rowid + tokens,
   * the source text lives in `books`, so the two never drift unless a
   * book write is missed (store_upsert_book / view_rebuild keep it in
   * step). */
  g_no_fts = 0;
  if (sqlite3_exec(g_db,
                   "CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5("
                   "title, author, series, search_text,"
                   " content='books', content_rowid=rowid)",
                   NULL, NULL, NULL) != SQLITE_OK) {
    g_no_fts = 1;
    bs_LOG("[bookshelf] store: FTS5 unavailable (%s); search falls back to LIKE\n",
        sqlite3_errmsg(g_db));
  }

  /* One-time legacy JSON import. */
  char legacy[BS_MAX_PATH_LEN * 2];
  char dir[BS_MAX_PATH_LEN];
  bs_dirname_of(bs_g_config_path, dir, sizeof dir);
  snprintf(legacy, sizeof legacy, "%s/%s", dir, BS_LIB_LEGACY_FILENAME);
  FILE *f = fopen(legacy, "r");
  if (f != NULL) {
    fclose(f);
    int n = store_import_legacy(legacy);
    if (n < 0) {
      /* Import failed midway (or could not commit): keep the JSON in
       * place so the import re-runs on the next boot. */
      bs_LOG("[bookshelf] store: legacy import incomplete, keeping %s\n",
          legacy);
    } else {
      char migrated[BS_MAX_PATH_LEN * 2 + 16];
      snprintf(migrated, sizeof migrated, "%s.migrated", legacy);
      rename(legacy, migrated);
      bs_LOG("[bookshelf] store: migrated legacy JSON (%d books)\n", n);
    }
  }
}

void bs_store_close(void) {
  sqlite3_finalize(g_st_upsert_lookup);
  sqlite3_finalize(g_st_upsert);
  sqlite3_finalize(g_st_suggest_del);
  sqlite3_finalize(g_st_suggest_ins);
  sqlite3_finalize(g_st_get_book);
  sqlite3_finalize(g_st_set_downloaded);
  sqlite3_finalize(g_st_next_dl_probes);
  sqlite3_finalize(g_st_suggest_rank_refresh);
  sqlite3_finalize(g_st_suggest_rank_zero);
  sqlite3_finalize(g_st_fts_rowid);
  sqlite3_finalize(g_st_fts_del);
  sqlite3_finalize(g_st_fts_ins);
  g_st_upsert_lookup = NULL;
  g_st_upsert = NULL;
  g_st_suggest_del = NULL;
  g_st_suggest_ins = NULL;
  g_st_get_book = NULL;
  g_st_set_downloaded = NULL;
  g_st_next_dl_probes = NULL;
  g_st_suggest_rank_refresh = NULL;
  g_st_suggest_rank_zero = NULL;
  g_st_fts_rowid = NULL;
  g_st_fts_del = NULL;
  g_st_fts_ins = NULL;
  /* A close/reopen re-probes FTS availability and re-decides the
   * search cache from scratch. */
  g_no_fts = 0;
  g_rank_ready = -1;
  g_search_q_cache[0] = '\0';
  g_search_use_fts = 0;
  if (g_db != NULL) {
    sqlite3_close(g_db);
    g_db = NULL;
  }
}

/* ── row CRUD ------------------------------------------------------------- */

static int bind_text_trunc(sqlite3_stmt *st, int i, const char *s) {
  return sqlite3_bind_text(st, i, s ? s : "", -1, SQLITE_TRANSIENT);
}

/* Keep the FTS index in step with one book row: drop any index entry
 * left at the book's previous rowid (INSERT OR REPLACE renumbers the
 * row), then index the fresh row at its new rowid.  The title/author/
 * series columns are always indexed, so a NULL search_text (local
 * import) stays searchable.  No-op when FTS is unavailable. */
static void store_fts_sync_row(const BsBook *b, long long old_rowid) {
  if (g_no_fts || g_db == NULL)
    return;
  sqlite3_stmt *rid = st_prep_once(&g_st_fts_rowid,
                                   "SELECT rowid FROM books WHERE id=?1");
  if (rid == NULL)
    return;
  sqlite3_reset(rid);
  sqlite3_clear_bindings(rid);
  bind_text_trunc(rid, 1, b->id);
  long long new_rowid = 0;
  if (sqlite3_step(rid) == SQLITE_ROW)
    new_rowid = sqlite3_column_int64(rid, 0);
  sqlite3_reset(rid);
  if (new_rowid == 0)
    return;
  if (old_rowid != 0) {
    sqlite3_stmt *d = st_prep_once(&g_st_fts_del,
                                   "DELETE FROM search_fts WHERE rowid=?1");
    if (d != NULL) {
      sqlite3_reset(d);
      sqlite3_clear_bindings(d);
      sqlite3_bind_int64(d, 1, old_rowid);
      sqlite3_step(d);
    }
  }
  sqlite3_stmt *ins = st_prep_once(
      &g_st_fts_ins,
      "INSERT INTO search_fts(rowid, title, author, series, search_text)"
      " VALUES(?1,?2,?3,?4,?5)");
  if (ins != NULL) {
    sqlite3_reset(ins);
    sqlite3_clear_bindings(ins);
    sqlite3_bind_int64(ins, 1, new_rowid);
    bind_text_trunc(ins, 2, b->title);
    bind_text_trunc(ins, 3, b->author);
    bind_text_trunc(ins, 4, b->series);
    bind_text_trunc(ins, 5, b->search_text);
    sqlite3_step(ins);
  }
}

/* Populate the FTS index from books when it is empty but books exist
 * (an upgraded store predating the index).  Cheap no-op once the index
 * holds any row. */
static void store_fts_backfill_if_empty(void) {
  if (g_no_fts || g_db == NULL)
    return;
  int fts_empty = 1;
  sqlite3_stmt *ck = NULL;
  if (sqlite3_prepare_v2(g_db, "SELECT 1 FROM search_fts LIMIT 1", -1, &ck,
                         NULL) == SQLITE_OK) {
    fts_empty = sqlite3_step(ck) != SQLITE_ROW;
    sqlite3_finalize(ck);
  }
  if (!fts_empty)
    return;
  if (sqlite3_exec(g_db,
                   "INSERT INTO search_fts(rowid, title, author, series,"
                   " search_text)"
                   " SELECT rowid, title, author, series,"
                   "        COALESCE(search_text,'') FROM books",
                   NULL, NULL, NULL) != SQLITE_OK)
    bs_LOG("[bookshelf] store: FTS backfill failed: %s\n", sqlite3_errmsg(g_db));
}

/* Insert or update one book row.  An existing row keeps its
 * downloaded/local_path state (file removal goes through
 * store_set_downloaded); a fresh row inherits whatever the caller
 * probed.  Returns 0 on success. */
int bs_store_upsert_book(const BsBook *b) {
  if (g_db == NULL)
    return -1;

  int downloaded = b->downloaded;
  /* Copy the existing row's local_path into a local buffer BEFORE the
   * lookup statement is reused: sqlite3_column_text() pointers die
   * with the statement's next step/reset, and the value is re-bound
   * below.  Default to the caller's path so fresh rows (and rows
   * whose downloaded flag is 0) keep the exact pre-fix semantics. */
  char lp[BS_MAX_PATH_LEN];
  long long old_rowid = 0; /* the row's pre-OR-REPLACE rowid (0 = fresh) */
  snprintf(lp, sizeof lp, "%s", b->local_path);
  sqlite3_stmt *q = st_prep_once(
      &g_st_upsert_lookup,
      "SELECT downloaded, local_path, rowid FROM books WHERE id=?1");
  if (q != NULL) {
    sqlite3_reset(q);
    sqlite3_clear_bindings(q);
    bind_text_trunc(q, 1, b->id);
    if (sqlite3_step(q) == SQLITE_ROW) {
      old_rowid = sqlite3_column_int64(q, 2);
      if (sqlite3_column_int(q, 0) == 1) {
        downloaded = 1;
        const char *t = (const char *)sqlite3_column_text(q, 1);
        snprintf(lp, sizeof lp, "%s", t ? t : "");
      }
    }
    /* The lookup SELECT is left at ROW (hit) or DONE (miss); either
     * way it may still hold a read cursor on books.  Reset before the
     * INSERT below or the write fails with SQLITE_LOCKED. */
    sqlite3_reset(q);
  }

  sqlite3_stmt *st = st_prep_once(
      &g_st_upsert,
      "INSERT OR REPLACE INTO books("
      "id,title,author,series,series_id,series_idx,"
      "ext,size,downloaded,local_path,added_at,"
      "filename,source,search_text,genre)"
      " VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)");
  if (st == NULL)
    return -1;
  sqlite3_reset(st);
  sqlite3_clear_bindings(st);
  bind_text_trunc(st, 1, b->id);
  bind_text_trunc(st, 2, b->title);
  bind_text_trunc(st, 3, b->author);
  bind_text_trunc(st, 4, b->series);
  bind_text_trunc(st, 5, b->series_id);
  sqlite3_bind_double(st, 6, b->series_idx);
  bind_text_trunc(st, 7, b->ext);
  sqlite3_bind_int(st, 8, b->size);
  sqlite3_bind_int(st, 9, downloaded);
  bind_text_trunc(st, 10, lp);
  sqlite3_bind_int64(st, 11, b->added_at);
  bind_text_trunc(st, 12, b->filename);
  bind_text_trunc(st, 13, b->source[0] ? b->source : "kavita");
  bind_text_trunc(st, 14, b->search_text);
  bind_text_trunc(st, 15, b->genre);
  int rc = sqlite3_step(st);
  if (rc != SQLITE_DONE)
    bs_LOG("[bookshelf] upsert FAILED id=%s rc=%d: %s\n", b->id, rc,
        sqlite3_errmsg(g_db));
  else
    store_fts_sync_row(b, old_rowid);
  return rc == SQLITE_DONE ? 0 : -1;
}

void bs_store_delete_book(const char *id) {
  if (g_db == NULL)
    return;
  /* Drop the FTS entry first, while the books row still exists to
   * resolve its rowid. */
  if (!g_no_fts) {
    sqlite3_stmt *f = NULL;
    if (sqlite3_prepare_v2(g_db,
                           "DELETE FROM search_fts WHERE rowid IN"
                           " (SELECT rowid FROM books WHERE id=?1)",
                           -1, &f, NULL) == SQLITE_OK) {
      bind_text_trunc(f, 1, id);
      sqlite3_step(f);
      sqlite3_finalize(f);
    }
  }
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db, "DELETE FROM books WHERE id=?1", -1, &st,
                         NULL) != SQLITE_OK)
    return;
  bind_text_trunc(st, 1, id);
  sqlite3_step(st);
  sqlite3_finalize(st);
}

/* Drop every book of one source (local imports replace wholesale, so a
 * re-scan never leaves stale entries behind). */
void bs_store_delete_source(const char *source) {
  if (g_db == NULL)
    return;
  if (!g_no_fts) {
    sqlite3_stmt *f = NULL;
    if (sqlite3_prepare_v2(g_db,
                           "DELETE FROM search_fts WHERE rowid IN"
                           " (SELECT rowid FROM books WHERE source=?1)",
                           -1, &f, NULL) == SQLITE_OK) {
      bind_text_trunc(f, 1, source);
      sqlite3_step(f);
      sqlite3_finalize(f);
    }
  }
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db, "DELETE FROM books WHERE source=?1", -1, &st,
                         NULL) != SQLITE_OK)
    return;
  bind_text_trunc(st, 1, source);
  sqlite3_step(st);
  sqlite3_finalize(st);
}

/* Extracted-metadata cache for local books, keyed by the stable
 * fld_<hash> id.  Survives re-imports so a rescan never re-parses a
 * book whose metadata is already known.  Returns 1 on hit. */
int bs_store_local_meta_get(const char *id, char *title, size_t title_cap,
                         char *author, size_t author_cap) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db,
                         "SELECT title, author FROM local_meta WHERE id=?1", -1,
                         &st, NULL) != SQLITE_OK)
    return 0;
  bind_text_trunc(st, 1, id);
  int hit = sqlite3_step(st) == SQLITE_ROW;
  if (hit) {
    const char *t = (const char *)sqlite3_column_text(st, 0);
    const char *a = (const char *)sqlite3_column_text(st, 1);
    if (title != NULL && title_cap > 0)
      snprintf(title, title_cap, "%s", t != NULL ? t : "");
    if (author != NULL && author_cap > 0)
      snprintf(author, author_cap, "%s", a != NULL ? a : "");
  }
  sqlite3_finalize(st);
  return hit;
}

void bs_store_local_meta_put(const char *id, const char *title,
                          const char *author) {
  if (g_db == NULL)
    return;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db,
                         "INSERT OR REPLACE INTO local_meta(id, title, author)"
                         " VALUES(?1, ?2, ?3)",
                         -1, &st, NULL) != SQLITE_OK)
    return;
  bind_text_trunc(st, 1, id);
  bind_text_trunc(st, 2, title != NULL ? title : "");
  bind_text_trunc(st, 3, author != NULL ? author : "");
  sqlite3_step(st);
  sqlite3_finalize(st);
}

void bs_store_set_downloaded(const char *id, int downloaded,
                          const char *local_path) {
  if (g_db == NULL)
    return;
  sqlite3_stmt *st = st_prep_once(
      &g_st_set_downloaded,
      "UPDATE books SET downloaded=?2, local_path=?3 WHERE id=?1");
  if (st == NULL)
    return;
  sqlite3_reset(st);
  sqlite3_clear_bindings(st);
  bind_text_trunc(st, 1, id);
  sqlite3_bind_int(st, 2, downloaded);
  bind_text_trunc(st, 3, local_path);
  int rc = sqlite3_step(st);
  if (rc != SQLITE_DONE)
    bs_LOG("[bookshelf] store_set_downloaded failed: %s\n", sqlite3_errmsg(g_db));
}

static void fill_book_from_stmt(sqlite3_stmt *st, BsBook *b) {
  memset(b, 0, sizeof *b);
  const char *t = (const char *)sqlite3_column_text(st, 0);
  snprintf(b->id, sizeof b->id, "%s", t ? t : "");
  t = (const char *)sqlite3_column_text(st, 1);
  snprintf(b->title, sizeof b->title, "%s", t ? t : "");
  t = (const char *)sqlite3_column_text(st, 2);
  snprintf(b->author, sizeof b->author, "%s", t ? t : "");
  t = (const char *)sqlite3_column_text(st, 3);
  snprintf(b->series, sizeof b->series, "%s", t ? t : "");
  t = (const char *)sqlite3_column_text(st, 4);
  snprintf(b->series_id, sizeof b->series_id, "%s", t ? t : "");
  b->series_idx = (float)sqlite3_column_double(st, 5);
  t = (const char *)sqlite3_column_text(st, 6);
  snprintf(b->ext, sizeof b->ext, "%s", t ? t : "");
  b->size = sqlite3_column_int(st, 7);
  b->downloaded = sqlite3_column_int(st, 8);
  t = (const char *)sqlite3_column_text(st, 9);
  snprintf(b->local_path, sizeof b->local_path, "%s", t ? t : "");
  b->added_at = (long)sqlite3_column_int64(st, 10);
  t = (const char *)sqlite3_column_text(st, 11);
  snprintf(b->filename, sizeof b->filename, "%s", t ? t : "");
  t = (const char *)sqlite3_column_text(st, 12);
  snprintf(b->source, sizeof b->source, "%s", t ? t : "");
  t = (const char *)sqlite3_column_text(st, 13);
  snprintf(b->genre, sizeof b->genre, "%s", t ? t : "");
}

#define BS_BOOK_COLS                                                              \
  "id,title,author,series,series_id,series_idx,ext,size,downloaded,local_"     \
  "path,added_at,"                                                             \
  "filename,source,genre"
/* books columns qualified for the view JOIN (bare BOOK_COLS would leave
 * every column after the first unqualified and ambiguous). */
#define BS_BOOK_COLS_Q                                                            \
  "b.id,b.title,b.author,b.series,b.series_id,b.series_idx,b.ext,b.size,b."    \
  "downloaded,"                                                                \
  "b.local_path,b.added_at,b.filename,b.source,b.genre"

/* Fetch one book row by id.  Returns 1 when found, 0 otherwise. */
int bs_store_get_book(const char *id, BsBook *out) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = st_prep_once(
      &g_st_get_book, "SELECT " BS_BOOK_COLS " FROM books WHERE id=?1");
  if (st == NULL)
    return 0;
  sqlite3_reset(st);
  sqlite3_clear_bindings(st);
  bind_text_trunc(st, 1, id);
  int found = sqlite3_step(st) == SQLITE_ROW;
  if (found)
    fill_book_from_stmt(st, out);
  /* Data is copied out above; reset releases the statement's read
   * cursor so a later write to books on this connection cannot hit
   * SQLITE_LOCKED. */
  sqlite3_reset(st);
  return found;
}

void bs_store_series_name(const char *series_id, char *out, size_t cap) {
  out[0] = '\0';
  if (g_db == NULL)
    return;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(
          g_db,
          "SELECT series FROM books WHERE series_id=?1 AND series!=''"
          " LIMIT 1",
          -1, &st, NULL) != SQLITE_OK)
    return;
  bind_text_trunc(st, 1, series_id);
  if (sqlite3_step(st) == SQLITE_ROW) {
    const char *t = (const char *)sqlite3_column_text(st, 0);
    if (t != NULL)
      snprintf(out, cap, "%s", t);
  }
  sqlite3_finalize(st);
}

int bs_store_count(void) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db, "SELECT COUNT(*) FROM books", -1, &st, NULL) !=
      SQLITE_OK)
    return 0;
  int n = 0;
  if (sqlite3_step(st) == SQLITE_ROW)
    n = sqlite3_column_int(st, 0);
  sqlite3_finalize(st);
  return n;
}

int bs_store_count_undownloaded(void) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db, "SELECT COUNT(*) FROM books WHERE downloaded=0",
                         -1, &st, NULL) != SQLITE_OK)
    return 0;
  int n = 0;
  if (sqlite3_step(st) == SQLITE_ROW)
    n = sqlite3_column_int(st, 0);
  sqlite3_finalize(st);
  return n;
}

/* First slice of not-downloaded ids in a stable order, for batch
 * download-all.  No offset: completed downloads shrink the
 * "downloaded=0" set, so callers page by finishing items, not by
 * skipping.  Returns the number of ids written (< cap = done). */
int bs_store_next_undownloaded(char ids[][BS_MAX_ID_LEN], int cap) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db,
                         "SELECT id FROM books WHERE downloaded=0"
                         " ORDER BY title COLLATE NOCASE, id LIMIT ?1",
                         -1, &st, NULL) != SQLITE_OK)
    return 0;
  sqlite3_bind_int(st, 1, cap);
  int n = 0;
  while (n < cap && sqlite3_step(st) == SQLITE_ROW) {
    const char *id = (const char *)sqlite3_column_text(st, 0);
    snprintf(ids[n], BS_MAX_ID_LEN, "%s", id ? id : "");
    n++;
  }
  sqlite3_finalize(st);
  return n;
}

/* Paged scan over every book in rowid order (downloaded or not), for
 * the startup flag refresh.  Rowid keyset pagination: the caller keeps
 * the last rowid seen (*after_rowid, 0 to start), so each page is one
 * b-tree scan of the rowid index with no OFFSET re-walk.  Returns only
 * the probe fields — the boot scan must not pay a per-book SELECT (the
 * old loop's store_get_book per id).  Results are copied into out[]
 * before the statement is reused.  Returns the number of rows written
 * (< cap = done); *after_rowid advances to the last rowid read and
 * stays unchanged when no rows are returned. */
int bs_store_next_dl_probes(BsDownloadProbe *out, int cap,
                         long long *after_rowid) {
  if (g_db == NULL || after_rowid == NULL || out == NULL)
    return 0;
  sqlite3_stmt *st = st_prep_once(
      &g_st_next_dl_probes,
      "SELECT rowid, id, filename, local_path, downloaded, ext FROM books"
      " WHERE rowid > ?1 ORDER BY rowid LIMIT ?2");
  if (st == NULL)
    return 0;
  sqlite3_reset(st);
  sqlite3_clear_bindings(st);
  sqlite3_bind_int64(st, 1, *after_rowid);
  sqlite3_bind_int(st, 2, cap);
  int n = 0;
  while (n < cap && sqlite3_step(st) == SQLITE_ROW) {
    BsDownloadProbe *p = &out[n];
    snprintf(p->id, sizeof p->id, "%s",
             (const char *)sqlite3_column_text(st, 1));
    snprintf(p->filename, sizeof p->filename, "%s",
             (const char *)sqlite3_column_text(st, 2));
    snprintf(p->local_path, sizeof p->local_path, "%s",
             (const char *)sqlite3_column_text(st, 3));
    p->downloaded = sqlite3_column_int(st, 4);
    snprintf(p->ext, sizeof p->ext, "%s",
             (const char *)sqlite3_column_text(st, 5));
    *after_rowid = sqlite3_column_int64(st, 0);
    n++;
  }
  /* A page that filled exactly cap leaves the statement at SQLITE_ROW
   * (the LIMIT was reached before the scan exhausted); reset releases
   * its read cursor so a later write to books is not locked out. */
  sqlite3_reset(st);
  return n;
}

/* Delete a book's local file and mark it not downloaded.  The metadata
 * row stays — the server remains the source of truth for the library. */
void bs_store_delete_book_file(const char *id) {
  BsBook b;
  if (!bs_store_get_book(id, &b))
    return;
  char path[BS_MAX_PATH_LEN];
  /* Remove the file where it actually lives (the stored location may
   * predate a downloads-folder change). */
  bs_book_existing_path(&b, path, sizeof path);
  if (unlink(path) == 0)
    bs_LOG("[bookshelf] delete_book_file removed %s\n", path);
  else
    bs_LOG("[bookshelf] delete_book_file unlink failed %s\n", path);
  bs_store_set_downloaded(id, 0, "");
  BsDownloadItem *d = bs_find_download(id);
  if (d != NULL)
    d->state = 3;
}

/* Slice of one series' member ids in volume order. */
int bs_store_series_ids(const char *series_id, char ids[][BS_MAX_ID_LEN], int cap,
                     int offset) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db,
                         "SELECT id FROM books WHERE series_id=?1"
                         " ORDER BY series_idx, title COLLATE NOCASE, id"
                         " LIMIT ?2 OFFSET ?3",
                         -1, &st, NULL) != SQLITE_OK)
    return 0;
  bind_text_trunc(st, 1, series_id);
  sqlite3_bind_int(st, 2, cap);
  sqlite3_bind_int(st, 3, offset);
  int n = 0;
  while (n < cap && sqlite3_step(st) == SQLITE_ROW) {
    const char *id = (const char *)sqlite3_column_text(st, 0);
    snprintf(ids[n], BS_MAX_ID_LEN, "%s", id ? id : "");
    n++;
  }
  sqlite3_finalize(st);
  return n;
}

/* ── search history ------------------------------------------------------- */

/* Record a committed search term: dedupe on the term itself (re-adding a
 * known term refreshes its timestamp, moving it to the front of the
 * list), then trim the table to the newest SEARCH_HISTORY_MAX rows.
 * Empty terms are ignored. */
void bs_store_search_add(const char *term) {
  if (g_db == NULL || term == NULL || term[0] == '\0')
    return;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(
          g_db,
          "INSERT OR REPLACE INTO search_history(term, ts) VALUES(?1, ?2)", -1,
          &st, NULL) == SQLITE_OK) {
    bind_text_trunc(st, 1, term);
    sqlite3_bind_int64(st, 2, (sqlite3_int64)time(NULL));
    sqlite3_step(st);
    sqlite3_finalize(st);
  }
  /* Keep only the newest SEARCH_HISTORY_MAX rows (newest first by
   * timestamp, ties broken by insert order). */
  sqlite3_stmt *trim = NULL;
  if (sqlite3_prepare_v2(g_db,
                         "DELETE FROM search_history WHERE rowid NOT IN"
                         " (SELECT rowid FROM search_history"
                         "  ORDER BY ts DESC, rowid DESC LIMIT ?1)",
                         -1, &trim, NULL) == SQLITE_OK) {
    sqlite3_bind_int(trim, 1, BS_SEARCH_HISTORY_MAX);
    sqlite3_step(trim);
    sqlite3_finalize(trim);
  }
}

int bs_store_search_count(void) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db, "SELECT COUNT(*) FROM search_history", -1, &st,
                         NULL) != SQLITE_OK)
    return 0;
  int n = 0;
  if (sqlite3_step(st) == SQLITE_ROW)
    n = sqlite3_column_int(st, 0);
  sqlite3_finalize(st);
  return n;
}

/* Slice of recent search terms, newest first.  Returns the number of
 * terms written (< cap = no more history). */
int bs_store_search_list(char terms[][BS_MAX_QUERY_LEN], int cap, int offset) {
  if (g_db == NULL || cap <= 0)
    return 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db,
                         "SELECT term FROM search_history"
                         " ORDER BY ts DESC, rowid DESC LIMIT ?1 OFFSET ?2",
                         -1, &st, NULL) != SQLITE_OK)
    return 0;
  sqlite3_bind_int(st, 1, cap);
  sqlite3_bind_int(st, 2, offset);
  int n = 0;
  while (n < cap && sqlite3_step(st) == SQLITE_ROW) {
    const char *t = (const char *)sqlite3_column_text(st, 0);
    snprintf(terms[n], BS_MAX_QUERY_LEN, "%s", t ? t : "");
    n++;
  }
  sqlite3_finalize(st);
  return n;
}

/* ── suggestion index ----------------------------------------------------- */

/* Exclusive upper bound for a prefix range scan: the smallest string
 * greater than every string starting with `prefix` (increment the last
 * code point).  Returns 1 with the bound NUL-terminated in out, or 0
 * when the bound overflows U+10FFFF (caller omits the `<` clause). */
static int suggest_upper_bound(const char *prefix, size_t len, char *out,
                               size_t cap) {
  if (len == 0 || len + 4 >= cap)
    return 0;
  unsigned char last = (unsigned char)prefix[len - 1];
  if (last < 0x7F) { /* ASCII fast path: bump the last byte */
    memcpy(out, prefix, len);
    out[len - 1] = (char)(last + 1);
    out[len] = '\0';
    return 1;
  }
  /* General path: decode the trailing code point, +1, re-encode.
   * Scan back to its lead byte. */
  size_t start = len - 1;
  while (start > 0 && (prefix[start] & 0xC0) == 0x80)
    start--;
  unsigned char lead = (unsigned char)prefix[start];
  size_t clen = 1;
  if ((lead & 0xE0) == 0xC0)
    clen = 2;
  else if ((lead & 0xF0) == 0xE0)
    clen = 3;
  else if ((lead & 0xF8) == 0xF0)
    clen = 4;
  uint32_t cp;
  if (start + clen != len) { /* malformed tail: last byte alone */
    start = len - 1;
    cp = (uint32_t)(unsigned char)prefix[start];
  } else if (clen == 1) {
    cp = lead;
  } else {
    static const uint8_t masks[5] = {0, 0, 0x1F, 0x0F, 0x07};
    cp = lead & masks[clen];
    for (size_t k = 1; k < clen; k++)
      cp = (cp << 6) | ((unsigned char)prefix[start + k] & 0x3F);
  }
  if (cp == 0x10FFFF)
    return 0;
  cp++;
  memcpy(out, prefix, start);
  if (cp < 0x80) {
    out[start] = (char)cp;
    out[start + 1] = '\0';
  } else if (cp < 0x800) {
    out[start] = (char)(0xC0 | (cp >> 6));
    out[start + 1] = (char)(0x80 | (cp & 0x3F));
    out[start + 2] = '\0';
  } else if (cp < 0x10000) {
    out[start] = (char)(0xE0 | (cp >> 12));
    out[start + 1] = (char)(0x80 | ((cp >> 6) & 0x3F));
    out[start + 2] = (char)(0x80 | (cp & 0x3F));
    out[start + 3] = '\0';
  } else {
    out[start] = (char)(0xF0 | (cp >> 18));
    out[start + 1] = (char)(0x80 | ((cp >> 12) & 0x3F));
    out[start + 2] = (char)(0x80 | ((cp >> 6) & 0x3F));
    out[start + 3] = (char)(0x80 | (cp & 0x3F));
    out[start + 4] = '\0';
  }
  return 1;
}

/* Replace the suggestion terms of one book.  The caller (do_sync)
 * holds the batch transaction.  n == 0 (or terms == NULL) deletes
 * every edge of the book — used for removed books and re-syncs of
 * books whose server sent no terms (old server). */
void bs_store_suggest_set(const char *book_id,
                       const char terms[][BS_SUGGEST_TERM_MAX], int n) {
  if (g_db == NULL || book_id == NULL)
    return;
  /* Snapshot the book's current edge terms before the delete so the
   * rank refresh below can correct both the removed and the added
   * sides of this book's change.  The server caps terms per book at
   * SUGGEST_MAX_TERMS, so a fixed buffer is safe; a store whose edges
   * exceed it self-heals on the next sync of those books. */
  char old[BS_SUGGEST_MAX_TERMS][BS_SUGGEST_TERM_MAX];
  int n_old = 0;
  {
    sqlite3_stmt *q = NULL;
    if (sqlite3_prepare_v2(g_db, "SELECT term FROM suggest WHERE book_id=?1",
                           -1, &q, NULL) == SQLITE_OK) {
      bind_text_trunc(q, 1, book_id);
      while (n_old < BS_SUGGEST_MAX_TERMS && sqlite3_step(q) == SQLITE_ROW) {
        const char *t = (const char *)sqlite3_column_text(q, 0);
        snprintf(old[n_old], BS_SUGGEST_TERM_MAX, "%s", t ? t : "");
        n_old++;
      }
      sqlite3_finalize(q);
    }
  }

  sqlite3_stmt *del = st_prep_once(&g_st_suggest_del,
                                   "DELETE FROM suggest WHERE book_id=?1");
  if (del == NULL)
    return;
  sqlite3_reset(del);
  sqlite3_clear_bindings(del);
  bind_text_trunc(del, 1, book_id);
  sqlite3_step(del);
  if (n > 0 && terms != NULL) {
    /* One cached single-row statement, bound once per term: a sync
     * round runs this per book, and a fresh prepare per term would cost
     * ~100k prepares on a full sync.  The slot is prepared on first use
     * and finalized in store_close. */
    sqlite3_stmt *ins = st_prep_once(
        &g_st_suggest_ins,
        "INSERT OR IGNORE INTO suggest(term, book_id) VALUES(?1, ?2)");
    if (ins != NULL) {
      for (int i = 0; i < n; i++) {
        if (terms[i][0] == '\0')
          continue; /* skip empty terms exactly as before */
        sqlite3_reset(ins);
        sqlite3_clear_bindings(ins);
        bind_text_trunc(ins, 1, terms[i]);
        bind_text_trunc(ins, 2, book_id);
        sqlite3_step(ins);
      }
    }
  }
  /* Recompute the rank count for every term this book touches (old
   * edges + new terms) straight from the edge table, so adds and
   * removes both settle in one pass; then drop any rank term that now
   * has no edges at all. */
  sqlite3_stmt *rf = st_prep_once(
      &g_st_suggest_rank_refresh,
      "INSERT INTO suggest_rank(term, cnt) VALUES(?1,"
      " (SELECT COUNT(*) FROM suggest WHERE term=?1))"
      " ON CONFLICT(term) DO UPDATE SET cnt=excluded.cnt");
  if (rf != NULL) {
    for (int i = 0; i < n_old; i++) {
      sqlite3_reset(rf);
      sqlite3_clear_bindings(rf);
      bind_text_trunc(rf, 1, old[i]);
      sqlite3_step(rf);
    }
    for (int i = 0; i < n; i++) {
      if (terms[i][0] == '\0')
        continue;
      int dup = 0;
      for (int j = 0; j < n_old; j++)
        if (strcmp(terms[i], old[j]) == 0) {
          dup = 1;
          break;
        }
      if (dup)
        continue;
      sqlite3_reset(rf);
      sqlite3_clear_bindings(rf);
      bind_text_trunc(rf, 1, terms[i]);
      sqlite3_step(rf);
    }
  }
  sqlite3_stmt *z = st_prep_once(&g_st_suggest_rank_zero,
                                 "DELETE FROM suggest_rank WHERE cnt=0");
  if (z != NULL) {
    sqlite3_reset(z);
    sqlite3_clear_bindings(z);
    sqlite3_step(z);
  }
}

/* Lazily probe whether the rank table holds any term.  Once it does,
 * sync keeps it populated, so the result is cached; while it is still
 * empty (an older DB upgraded in place), the caller falls back to the
 * edge-table GROUP BY until the next sync fills it. */
static int suggest_rank_ready(void) {
  if (g_rank_ready == 1)
    return 1;
  int has = 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db, "SELECT 1 FROM suggest_rank LIMIT 1", -1, &st,
                         NULL) == SQLITE_OK) {
    has = sqlite3_step(st) == SQLITE_ROW;
    sqlite3_finalize(st);
  }
  if (has)
    g_rank_ready = 1;
  return has;
}

/* Prefix lookup over the term index, most-popular first (a term's
 * count = number of books containing it; ties by term).  Only
 * ASCII-lowercasing happens here — the server folded terms, so a
 * folded-ASCII term matches typed ASCII input; non-ASCII typed input
 * may miss (accepted).  Empty and single-char prefixes return 0: the
 * range is useless at 100k books and the UI keeps showing history.
 * Bounded output: LIMIT keeps RAM O(cap), never the whole index. */
int bs_store_suggest_list(const char *prefix, char out[][BS_SUGGEST_TERM_MAX],
                       int cap) {
  if (g_db == NULL || prefix == NULL || cap <= 0)
    return 0;
  size_t len = strlen(prefix);
  if (len < 2 || len >= BS_SUGGEST_TERM_MAX)
    return 0;
  char norm[BS_SUGGEST_TERM_MAX];
  for (size_t i = 0; i <= len; i++)
    norm[i] = (prefix[i] >= 'A' && prefix[i] <= 'Z') ? (char)(prefix[i] + 32)
                                                     : prefix[i];
  char bound[BS_SUGGEST_TERM_MAX + 4];
  int has_bound = suggest_upper_bound(norm, len, bound, sizeof bound);

  const char *sql;
  if (suggest_rank_ready()) {
    /* Rank path: a pure ordered range scan over the aggregated table
     * (cnt is the b-tree key after term) — no per-keystroke GROUP BY
     * over the whole prefix range. */
    sql = has_bound
              ? "SELECT term FROM suggest_rank WHERE term >= ?1 AND term < ?2"
                " ORDER BY cnt DESC, term ASC LIMIT ?3"
              : "SELECT term FROM suggest_rank WHERE term >= ?1"
                " ORDER BY cnt DESC, term ASC LIMIT ?3";
  } else {
    /* Upgrade fallback: rank table empty but edges may exist; the
     * grouped edge scan still yields correct suggestions until the
     * next sync fills the rank table (and if there are no edges at
     * all, it naturally returns nothing). */
    sql = has_bound
              ? "SELECT term FROM suggest WHERE term >= ?1 AND term < ?2"
                " GROUP BY term ORDER BY COUNT(*) DESC, term ASC LIMIT ?3"
              : "SELECT term FROM suggest WHERE term >= ?1"
                " GROUP BY term ORDER BY COUNT(*) DESC, term ASC LIMIT ?3";
  }
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db, sql, -1, &st, NULL) != SQLITE_OK)
    return 0;
  bind_text_trunc(st, 1, norm);
  if (has_bound)
    bind_text_trunc(st, 2, bound);
  sqlite3_bind_int(st, has_bound ? 3 : 2, cap);
  int n = 0;
  while (n < cap && sqlite3_step(st) == SQLITE_ROW) {
    const char *t = (const char *)sqlite3_column_text(st, 0);
    snprintf(out[n], BS_SUGGEST_TERM_MAX, "%s", t ? t : "");
    n++;
  }
  sqlite3_finalize(st);
  return n;
}

/* ── sync cursor + batch transaction -------------------------------------- */

long long bs_store_get_cursor(void) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(g_db, "SELECT value FROM meta WHERE key='cursor'", -1,
                         &st, NULL) != SQLITE_OK)
    return 0;
  long long c = 0;
  if (sqlite3_step(st) == SQLITE_ROW)
    c = sqlite3_column_int64(st, 0);
  sqlite3_finalize(st);
  return c;
}

void bs_store_set_cursor(long long cursor) {
  if (g_db == NULL)
    return;
  sqlite3_stmt *st = NULL;
  /* meta.value is TEXT: cast explicitly so the cursor is stored as a
   * text value (a raw int64 bind would rely on affinity); reads parse
   * it back via column_int64 either way. */
  if (sqlite3_prepare_v2(
          g_db,
          "INSERT OR REPLACE INTO meta(key,value)"
          " VALUES('cursor', CAST(?1 AS TEXT))",
          -1, &st, NULL) != SQLITE_OK)
    return;
  sqlite3_bind_int64(st, 1, cursor);
  sqlite3_step(st);
  sqlite3_finalize(st);
}

int bs_store_begin(void) {
  if (g_db == NULL)
    return -1;
  char *msg = NULL;
  if (sqlite3_exec(g_db, "BEGIN", NULL, NULL, &msg) != SQLITE_OK) {
    bs_LOG("[bookshelf] store_begin FAILED: %s\n",
        msg ? msg : sqlite3_errmsg(g_db));
    if (msg)
      sqlite3_free(msg);
    return -1;
  }
  return 0;
}

void bs_store_commit(void) {
  if (g_db != NULL) {
    char *msg = NULL;
    if (sqlite3_exec(g_db, "COMMIT", NULL, NULL, &msg) != SQLITE_OK) {
      bs_LOG("[bookshelf] store_commit FAILED: %s\n",
          msg ? msg : sqlite3_errmsg(g_db));
      /* A failed COMMIT leaves the transaction open; abort it so the
       * next store_begin starts from a clean state. */
      sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, NULL);
    }
    if (msg)
      sqlite3_free(msg);
  }
}

/* Abort the current transaction, discarding every write since the
 * matching store_begin.  Used by the sync engine when a batch cannot
 * be applied cleanly (see sync_apply_round). */
void bs_store_rollback(void) {
  if (g_db != NULL) {
    char *msg = NULL;
    if (sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, &msg) != SQLITE_OK) {
      bs_LOG("[bookshelf] store_rollback FAILED: %s\n",
          msg ? msg : sqlite3_errmsg(g_db));
      if (msg)
        sqlite3_free(msg);
    }
  }
}
/* ── view projection ------------------------------------------------------- */

/* Largest k <= n such that s[0..k) ends on a UTF-8 boundary: when the
 * last kept byte is a continuation, walk back to its lead byte and
 * drop the whole (now split) character; a bare lead byte at the cut
 * is dropped as well.  Malformed input degrades to a safe shorter cut,
 * never to a mid-sequence byte. */
static size_t utf8_cut_back(const char *s, size_t n) {
  if (s == NULL || n == 0)
    return 0;
  size_t k = n;
  unsigned char last = (unsigned char)s[k - 1];
  if ((last & 0xC0) == 0x80) {
    /* Continuation byte: the character it belongs to started earlier
     * and is cut in half; step back to the lead byte and drop the
     * whole sequence (lead included). */
    while (k > 0 && ((unsigned char)s[k - 1] & 0xC0) == 0x80)
      k--;
    if (k > 0)
      k--; /* the lead byte itself is part of the split char */
  } else if ((last & 0xE0) == 0xC0 || (last & 0xF0) == 0xE0 ||
             (last & 0xF8) == 0xF0) {
    k--; /* truncated lead byte with no continuation bytes */
  }
  return k;
}

/* Escape LIKE metacharacters so a user query is matched literally. */
static void like_escape(const char *in, char *out, size_t cap) {
  size_t o = 0;
  for (size_t i = 0; in[i] != '\0' && o + 3 < cap; i++) {
    char c = in[i];
    if (c == '%' || c == '_' || c == '\\') {
      if (o + 2 >= cap)
        break;
      out[o++] = '\\';
    }
    out[o++] = c;
  }
  /* The capacity bound may have cut a multibyte character in half;
   * back the cut off to a UTF-8 boundary. */
  o = utf8_cut_back(out, o);
  out[o] = '\0';
}

/* The active source's storage value; views (view_where) and the FTS
 * probe (search_fts_decide) both filter on it. */
static const char *view_source(void) {
  return bs_g_state.source == BS_SOURCE_LOCAL    ? "local"
         : bs_g_state.source == BS_SOURCE_FOLDER ? "folder"
                                           : "kavita";
}

/* Build a safe FTS5 MATCH query from the raw user query: emit the
 * whole query as one quoted phrase with a prefix marker ("w1 w2" *),
 * doubling any embedded quotes.  A phrase requires the words to appear
 * adjacent, the closest analogue of LIKE's %substring% semantics, and
 * quoting neutralises operators/punctuation so the query can't change
 * FTS query shape.  Returns the query length, or 0 when nothing usable
 * remains (caller falls back to LIKE). */
static int fts_query_from(const char *raw, char *out, size_t cap) {
  if (raw == NULL || cap < 4)
    return 0;
  size_t o = 0;
  const char *p = raw;
  int any = 0;
  out[o++] = '"';
  while (*p != '\0') {
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')
      p++;
    if (*p == '\0')
      break;
    if (any) { /* one space between words */
      if (o + 1 >= cap)
        return 0;
      out[o++] = ' ';
    }
    while (*p != '\0' && *p != ' ' && *p != '\t' && *p != '\n' && *p != '\r') {
      char c = *p++;
      if (c == '"') { /* double the quote to escape it inside a phrase */
        if (o + 2 >= cap)
          return 0;
        out[o++] = '"';
        out[o++] = '"';
      } else {
        if (o + 1 >= cap)
          return 0;
        out[o++] = c;
      }
    }
    any = 1;
  }
  if (!any)
    return 0;
  if (o + 3 >= cap)
    return 0;
  out[o++] = '"';
  out[o++] = ' '; /* FTS5 phrase-prefix form: "w1 w2" * */
  out[o++] = '*';
  out[o] = '\0';
  return (int)o;
}

/* Decide (and cache) the search strategy for the current query.  Uses
 * the FTS index only when all of: the module is available, the MATCH
 * query parses, and the index demonstrably matches at least one book
 * in the active source.  Anything else — module absent, parse error,
 * or a substring FTS can't match (probe finds no row) — keeps the
 * byte-identical LIKE %query% path, so search never goes wrong or
 * empty.  Re-decided whenever g_state.query changes. */
static void search_fts_decide(void) {
  if (strcmp(g_search_q_cache, bs_g_state.query) == 0)
    return;
  snprintf(g_search_q_cache, sizeof g_search_q_cache, "%s", bs_g_state.query);
  g_search_use_fts = 0;
  g_fts_query[0] = '\0';
  if (g_no_fts || bs_g_state.query[0] == '\0' || g_db == NULL)
    return;
  if (fts_query_from(bs_g_state.query, g_fts_query, sizeof g_fts_query) <= 0)
    return;
  /* Probe: does the MATCH serve at least one row in the active
   * source?  A clean prepare proves the query parses; a hit proves
   * FTS can answer it (a substring FTS can't match yields no row and
   * falls back to LIKE).  The source filter mirrors view_where so the
   * decision matches the real scoping. */
  sqlite3_stmt *st = NULL;
  if (sqlite3_prepare_v2(
          g_db,
          "SELECT 1 FROM search_fts f JOIN books b ON b.rowid=f.rowid"
          " WHERE search_fts MATCH ?1 AND COALESCE(b.source,'kavita')=?2"
          " LIMIT 1",
          -1, &st, NULL) != SQLITE_OK)
    return; /* MATCH parse failed: keep LIKE */
  bind_text_trunc(st, 1, g_fts_query);
  bind_text_trunc(st, 2, view_source());
  if (sqlite3_step(st) == SQLITE_ROW)
    g_search_use_fts = 1;
  sqlite3_finalize(st);
}

/* Append the active filter/query WHERE clause (AND-joined) to sql.
 * qbind is the parameter index the query pattern will be bound at
 * (0 = no query parameter). */
static void view_where(char *sql, size_t cap, int qbind) {
  /* The downloaded/remote filter was removed; every caller builds
   * "WHERE" + this clause, so a constant true keeps the pattern. */
  snprintf(sql + strlen(sql), cap - strlen(sql), " 1=1");
  if (qbind > 0 && bs_g_state.query[0] != '\0') {
    search_fts_decide();
    if (g_search_use_fts) {
      /* FTS index path: restrict to rows the index matches; the
       * source filter below still applies.  `rowid` is books.rowid
       * in every view_where caller (all are FROM books). */
      snprintf(sql + strlen(sql), cap - strlen(sql),
               " AND rowid IN (SELECT rowid FROM search_fts"
               " WHERE search_fts MATCH ?%d)",
               qbind);
    } else {
      /* LIKE fallback — byte-identical to the pre-FTS behaviour.  One
       * bound pattern serves all LIKEs (same ?qbind index).  The
       * folded search_text column joins the raw fields so folded
       * suggestions (e.g. "songgong" from "sŏnggong") and diacritic
       * queries actually find books; it is NULL for local imports,
       * where the raw fields still match.  Series joined so
       * series-word suggestions produce results. */
      snprintf(sql + strlen(sql), cap - strlen(sql),
               " AND (title LIKE ?%d ESCAPE '\\' OR author LIKE ?%d ESCAPE '\\'"
               " OR series LIKE ?%d ESCAPE '\\'"
               " OR search_text LIKE ?%d ESCAPE '\\')",
               qbind, qbind, qbind, qbind);
    }
  }
  /* Only the active source's books are visible; rows written before
   * the source column existed are kavita books.  The value comes from
   * a fixed enum, so no quoting concerns. */
  snprintf(sql + strlen(sql), cap - strlen(sql),
           " AND COALESCE(source,'kavita')='%s'", view_source());
}

static const char *view_order(void) {
  switch (bs_g_state.sort) {
  case BS_SORT_AUTHOR:
    return "author COLLATE NOCASE, title COLLATE NOCASE, id";
  case BS_SORT_SERIES:
    return "series COLLATE NOCASE, series_idx, id";
  case BS_SORT_RECENT:
    return "added_at DESC, title COLLATE NOCASE, id";
  default:
    return "title COLLATE NOCASE, id";
  }
}

/* Bind the query pattern at qbind (no-op when the query is empty or
 * qbind is 0). */
static void view_bind_query(sqlite3_stmt *st, int qbind) {
  if (qbind <= 0 || bs_g_state.query[0] == '\0')
    return;
  search_fts_decide();
  if (g_search_use_fts) {
    bind_text_trunc(st, qbind, g_fts_query);
    return;
  }
  char pat[BS_MAX_QUERY_LEN * 2 + 4];
  char esc[BS_MAX_QUERY_LEN * 2];
  like_escape(bs_g_state.query, esc, sizeof esc);
  snprintf(pat, sizeof pat, "%%%s%%", esc);
  bind_text_trunc(st, qbind, pat);
}

/* ── dimension grouping support ──────────────────────────────────────── *
 * A chosen grouping dimension (bs_g_group_dim) collapses the view into
 * "stack" cards by the active dimension's value — the same card
 * rendering the series stacks use — so a group is one card you tap to
 * drill into (regroup by the next dimension, or flat at the leaf). */

/* Level count of a grouping preset. */
static int group_levels(BsGroupPreset g) {
  switch (g) {
  case BS_GROUP_AUTHOR_SERIES: return 2;
  case BS_GROUP_NONE:          return 0;
  default:                     return 1;
  }
}

/* The grouping dimension in effect at drill level *lvl* of a preset. */
static BsGroupDim dim_at(BsGroupPreset g, int lvl) {
  switch (g) {
  case BS_GROUP_SERIES:        return BS_GROUP_BY_SERIES;
  case BS_GROUP_AUTHOR:        return BS_GROUP_BY_AUTHOR;
  case BS_GROUP_YEAR:          return BS_GROUP_BY_YEAR;
  case BS_GROUP_GENRE:         return BS_GROUP_BY_GENRE;
  case BS_GROUP_AUTHOR_SERIES: return lvl == 0 ? BS_GROUP_BY_AUTHOR
                                               : BS_GROUP_BY_SERIES;
  default:                     return BS_GROUP_ALL;
  }
}

/* 1 = a dimension grouping is active (drilling beneath the top). */
static int grouped_active(void) {
  return bs_g_group != BS_GROUP_NONE &&
         bs_g_drill_level < group_levels(bs_g_group);
}

/* SQL for a grouping dimension; q selects the view-JOIN alias (`b.`). */
static const char *dim_sql(BsGroupDim dim, int q) {
  switch (dim) {
  case BS_GROUP_BY_SERIES:
    /* Series grouping is the remote API's own identity (series_id), not
     * an app-side derivation, so it keys on series_id. */
    return q ? "b.series_id COLLATE NOCASE" : "series_id COLLATE NOCASE";
  case BS_GROUP_BY_AUTHOR:
    return q ? "b.author COLLATE NOCASE" : "author COLLATE NOCASE";
  case BS_GROUP_BY_YEAR:
    return q ? "strftime('%Y', b.added_at, 'unixepoch')"
             : "strftime('%Y', added_at, 'unixepoch')";
  case BS_GROUP_BY_GENRE:
    return q ? "b.genre COLLATE NOCASE" : "genre COLLATE NOCASE";
  default:
    return NULL; /* BS_GROUP_ALL */
  }
}

/* Human label of a stack card (empty value → "No series" etc). */
static void group_label_for(BsGroupDim dim, const char *value, char *out,
                            size_t cap) {
  if (value[0] == '\0') {
    const char *kl = NULL;
    switch (dim) {
    case BS_GROUP_BY_SERIES: kl = "group.none.series"; break;
    case BS_GROUP_BY_AUTHOR: kl = "group.none.author"; break;
    case BS_GROUP_BY_YEAR:   kl = "group.none.year";   break;
    case BS_GROUP_BY_GENRE:  kl = "group.none.genre";  break;
    default: break;
    }
    snprintf(out, cap, "%s", kl ? bs_i18n(kl) : "...");
  } else {
    snprintf(out, cap, "%s", value);
  }
}

/* 1 when any book in the current source has a non-empty value for this
 * grouping dimension (drives which options the group chooser offers). */
int bs_view_dim_available(BsGroupDim dim) {
  if (dim == BS_GROUP_ALL || g_db == NULL)
    return 1;
  const char *e = dim_sql(dim, 0);
  if (e == NULL)
    return 1;
  /* Series grouping is only acceptable when the remote API supplies a
     series identity — i.e. on the Kavita (remote) source.  The
     local/folder sources derive series from filenames, which never
     count, so the option is hidden there entirely. */
  if (dim == BS_GROUP_BY_SERIES && bs_g_state.source != BS_SOURCE_KAVITA)
    return 0;
  const char *src = bs_g_state.source == BS_SOURCE_LOCAL    ? "local"
                  : bs_g_state.source == BS_SOURCE_FOLDER ? "folder"
                                                          : "kavita";
  sqlite3_stmt *st = NULL;
  char sql[256];
  snprintf(sql, sizeof sql,
           "SELECT COUNT(*) FROM books WHERE %s IS NOT NULL AND %s!=''"
           " AND source=?1", e, e);
  if (sqlite3_prepare_v2(g_db, sql, -1, &st, NULL) != SQLITE_OK)
    return 0;
  bind_text_trunc(st, 1, src);
  int n = 0;
  if (sqlite3_step(st) == SQLITE_ROW)
    n = sqlite3_column_int(st, 0);
  sqlite3_finalize(st);
  return n > 0;
}

/* ── view rebuild ───────────────────────────────────────────────────── */

/* Rebuild the materialised view table for the current filter/sort/
 * group/drill.  Collapse modes (All books) emit standalone books as
 * flat tiles and multi-book series as single cards, interleaved in
 * first-seen order of the active sort; single-member series stay flat.
 * Dimension groupings (series/author/year/genre) order the books by the
 * current dimension and materialise a group index for header rendering
 * and drill-in.  All ordering and grouping happens in SQL so RAM never
 * holds the whole library. */
void bs_view_rebuild(void) {
  if (g_db == NULL)
    return;

  /* A rebuilt view renumbers every tile: disarm any in-flight
   * long-press so it cannot fire on a row that no longer maps to the
   * same book (or is out of range). */
  bs_g_lp_vi = -1;
  bs_g_lp_armed = 0;

  /* One-time backfill: an upgraded store has books but no index yet
   * (FTS was just created).  Fill it once so search runs on FTS; a
   * populated index makes this a cheap no-op thereafter. */
  store_fts_backfill_if_empty();

  /* Keep the count the previous view had: the rollback paths below
   * restore the old view rows, so the cached total must go back to
   * that value instead of being recounted. */
  int prev_total = bs_g_view_total;

  if (sqlite3_exec(g_db, "BEGIN", NULL, NULL, NULL) != SQLITE_OK) {
    bs_LOG("[bookshelf] view_rebuild: BEGIN failed: %s\n",
        sqlite3_errmsg(g_db));
    return; /* previous view and g_view_total stay intact */
  }
  if (sqlite3_exec(g_db, "DELETE FROM view", NULL, NULL, NULL) != SQLITE_OK) {
    bs_LOG("[bookshelf] view_rebuild: DELETE failed: %s\n",
        sqlite3_errmsg(g_db));
    sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, NULL);
    return; /* previous view and g_view_total stay intact */
  }
  bs_g_view_total = 0;

  char sql[2048];
  int rc = SQLITE_OK;
  /* Rows written to view by the INSERT below; becomes g_view_total
   * after COMMIT (sqlite3_changes must be read before any DROP TABLE
   * or the COMMIT — both reset the counter). */
  int inserted = 0;

  if (bs_g_drilled_series[0] != '\0') {
    /* Drill-down: the series' members under the active filter/sort. */
    snprintf(
        sql, sizeof sql,
        "INSERT INTO view(kind, book_id, series_id, series_name, series_count)"
        " SELECT 0, id, series_id, series, 0 FROM books WHERE series_id=?1 "
        "AND");
    view_where(sql, sizeof sql, 2);
    snprintf(sql + strlen(sql), sizeof sql - strlen(sql), " ORDER BY %s",
             view_order());
    sqlite3_stmt *st = NULL;
    rc = sqlite3_prepare_v2(g_db, sql, -1, &st, NULL);
    if (rc == SQLITE_OK) {
      bind_text_trunc(st, 1, bs_g_drilled_series);
      view_bind_query(st, 2);
      rc = sqlite3_step(st);
      inserted = sqlite3_changes(g_db);
      sqlite3_finalize(st);
    }
  } else if (bs_g_group == BS_GROUP_NONE) {
    /* idempotent: a failed earlier rebuild must not wedge the collapse
     * (a leaked t_sorted makes the CREATE TEMP TABLE below fail). */
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_sorted", NULL, NULL, NULL);
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_grp", NULL, NULL, NULL);
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_out", NULL, NULL, NULL);
    /* Collapse mode: order the filtered set once into a temp table
     * (rowid = sort position), then emit flats and series cards
     * keyed by first-seen position. */
    rc = sqlite3_exec(g_db,
                      "CREATE TEMP TABLE t_sorted(id TEXT, series_id TEXT,"
                      " series_idx REAL, series TEXT)",
                      NULL, NULL, NULL);
    if (rc != SQLITE_OK)
      bs_LOG("[bookshelf] view_rebuild: t_sorted create rc=%d: %s\n", rc,
          sqlite3_errmsg(g_db));
    if (rc == SQLITE_OK) {
      snprintf(sql, sizeof sql,
               "INSERT INTO t_sorted SELECT id, series_id, series_idx, series"
               " FROM books WHERE");
      view_where(sql, sizeof sql, 1);
      snprintf(sql + strlen(sql), sizeof sql - strlen(sql), " ORDER BY %s",
               view_order());
      sqlite3_stmt *st = NULL;
      rc = sqlite3_prepare_v2(g_db, sql, -1, &st, NULL);
      if (rc == SQLITE_OK) {
        view_bind_query(st, 1);
        rc = sqlite3_step(st);
        sqlite3_finalize(st);
        /* step reports SQLITE_DONE on success; the exec chain
         * below gates on SQLITE_OK, so normalise. */
        if (rc == SQLITE_DONE)
          rc = SQLITE_OK;
      }
    }
    if (rc == SQLITE_OK) {
      rc = sqlite3_exec(
          g_db,
          "CREATE TEMP TABLE t_out(fk INTEGER, kind INTEGER, book_id TEXT,"
          " series_id TEXT, series_name TEXT, series_count INTEGER)",
          NULL, NULL, NULL);
    }
    if (rc == SQLITE_OK) {
      /* Per-series aggregates plus an index on the temp sort
       * table keep the collapse linear in the library size: every
       * correlated lookup below hits t_sorted_sid instead of
       * scanning t_sorted once per output row. */
      rc = sqlite3_exec(g_db,
                        "CREATE TEMP TABLE t_grp AS"
                        " SELECT series_id AS sid, COUNT(*) AS c,"
                        "        MAX(series_idx) AS mx"
                        " FROM t_sorted WHERE series_id IS NOT NULL"
                        "  AND series_id!='' GROUP BY series_id;"
                        "CREATE INDEX t_sorted_sid ON t_sorted(series_id)",
                        NULL, NULL, NULL);
    }
    if (rc == SQLITE_OK) {
      /* Flat tiles: standalone books and single-member series,
       * each at its own sort position. */
      rc = sqlite3_exec(g_db,
                        "INSERT INTO t_out"
                        " SELECT s.rowid, 0, s.id, s.series_id, s.series,"
                        "        COALESCE(g.c, 1)"
                        " FROM t_sorted s LEFT JOIN t_grp g"
                        "  ON g.sid=s.series_id"
                        " WHERE g.c IS NULL OR g.c=1",
                        NULL, NULL, NULL);
    }
    if (rc == SQLITE_OK) {
      /* One card per multi-book series, after all flat tiles,
       * ordered by first-seen position.  Representative = highest
       * volume (ties: earliest sort position). */
      rc = sqlite3_exec(g_db,
                        "INSERT INTO t_out"
                        " SELECT 1000000000 +"
                        "        (SELECT MIN(s2.rowid) FROM t_sorted s2"
                        "          WHERE s2.series_id=g.sid),"
                        "        1, rep.id, g.sid, rep.series, g.c"
                        " FROM t_grp g"
                        " JOIN t_sorted rep ON rep.series_id=g.sid"
                        "  AND rep.series_idx=g.mx"
                        "  AND rep.rowid=(SELECT MIN(s3.rowid) FROM t_sorted s3"
                        "                  WHERE s3.series_id=g.sid"
                        "                    AND s3.series_idx=g.mx)"
                        " WHERE g.c>1",
                        NULL, NULL, NULL);
    }
    if (rc == SQLITE_OK) {
      rc = sqlite3_exec(
          g_db,
          "INSERT INTO view(kind, book_id, series_id, series_name,"
          " series_count)"
          " SELECT kind, book_id, series_id, series_name, series_count"
          " FROM t_out ORDER BY fk, kind",
          NULL, NULL, NULL);
      /* Read the insert count before the DROP TABLEs below (any
       * non-SELECT statement resets sqlite3_changes). */
      inserted = sqlite3_changes(g_db);
    }
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_sorted", NULL, NULL, NULL);
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_grp", NULL, NULL, NULL);
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_out", NULL, NULL, NULL);
  } else if (grouped_active()) {
    /* Dimension grouping collapses into stack cards (like the series
     * stacks) keyed by the active dimension's value; single-member
     * groups stay flat tiles.  Tapping a card drills into it (regroups
     * by the next dimension, or flat at the leaf). */
    const char *dim = dim_sql(dim_at(bs_g_group, bs_g_drill_level), 0);
    /* The card label is the display value (series name for series,
     * which groups by the API series_id). */
    const char *lbl = (dim_at(bs_g_group, bs_g_drill_level) == BS_GROUP_BY_SERIES)
                          ? "series"
                          : dim;
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_sorted", NULL, NULL, NULL);
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_grp", NULL, NULL, NULL);
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_out", NULL, NULL, NULL);
    rc = sqlite3_exec(g_db,
                      "CREATE TEMP TABLE t_sorted(id TEXT NOT NULL, g TEXT,"
                      " lbl TEXT)",
                      NULL, NULL, NULL);
    if (rc != SQLITE_OK)
      bs_LOG("[bookshelf] view_rebuild: t_sorted create rc=%d: %s\n", rc,
          sqlite3_errmsg(g_db));
    if (rc == SQLITE_OK) {
      snprintf(sql, sizeof sql,
               "INSERT INTO t_sorted SELECT id, %s, %s FROM books WHERE", dim,
               lbl);
      view_where(sql, sizeof sql, bs_g_drill_level + 1);
      for (int L = 0; L < bs_g_drill_level; L++) {
        const char *e = dim_sql(dim_at(bs_g_group, L), 0);
        if (bs_g_drill_values[L][0])
          snprintf(sql + strlen(sql), sizeof sql - strlen(sql),
                   " AND (%s=?%d)", e, L + 1);
        else
          snprintf(sql + strlen(sql), sizeof sql - strlen(sql),
                   " AND (%s IS NULL OR %s='')", e, e);
      }
      snprintf(sql + strlen(sql), sizeof sql - strlen(sql), " ORDER BY %s",
               view_order());
      sqlite3_stmt *st = NULL;
      rc = sqlite3_prepare_v2(g_db, sql, -1, &st, NULL);
      if (rc == SQLITE_OK) {
        for (int L = 0; L < bs_g_drill_level; L++)
          if (bs_g_drill_values[L][0])
            bind_text_trunc(st, L + 1, bs_g_drill_values[L]);
        view_bind_query(st, bs_g_drill_level + 1);
        rc = sqlite3_step(st);
        if (rc == SQLITE_DONE)
          rc = SQLITE_OK; /* exec chain below gates on SQLITE_OK */
        sqlite3_finalize(st);
      }
    }
    if (rc == SQLITE_OK) {
      rc = sqlite3_exec(g_db,
                        "CREATE TEMP TABLE t_out(fk INTEGER, kind INTEGER,"
                        " book_id TEXT, series_id TEXT, series_name TEXT,"
                        " series_count INTEGER)",
                        NULL, NULL, NULL);
    }
    if (rc == SQLITE_OK) {
      rc = sqlite3_exec(g_db,
                        "CREATE TEMP TABLE t_grp AS"
                        " SELECT g AS sid, COUNT(*) AS c FROM t_sorted"
                        " GROUP BY g;"
                        "CREATE INDEX t_sorted_g ON t_sorted(g)",
                        NULL, NULL, NULL);
    }
    if (rc == SQLITE_OK) {
      /* Flat tiles: standalone books and single-member groups. */
      rc = sqlite3_exec(g_db,
                        "INSERT INTO t_out"
                        " SELECT s.rowid, 0, s.id, '', s.lbl, COALESCE(g.c, 1)"
                        " FROM t_sorted s LEFT JOIN t_grp g ON g.sid=s.g"
                        " WHERE g.c IS NULL OR g.c=1",
                        NULL, NULL, NULL);
    }
    if (rc == SQLITE_OK) {
      /* One stack card per multi-book group, after all flat tiles, in
       * first-seen order.  Representative = the group's first book in
       * the active sort.  series_id carries the raw group value so a
       * card tap can drill into scope. */
      rc = sqlite3_exec(g_db,
                        "INSERT INTO t_out"
                        " SELECT 1000000000 +"
                        "        (SELECT MIN(s2.rowid) FROM t_sorted s2"
                        "          WHERE s2.g=g.sid),"
                        "        1, rep.id, g.sid, rep.lbl, g.c"
                        " FROM t_grp g"
                        " JOIN t_sorted rep ON rep.g=g.sid AND rep.rowid="
                        "      (SELECT MIN(s3.rowid) FROM t_sorted s3"
                        "        WHERE s3.g=g.sid)"
                        " WHERE g.c>1",
                        NULL, NULL, NULL);
    }
    if (rc == SQLITE_OK) {
      rc = sqlite3_exec(g_db,
                        "INSERT INTO view(kind, book_id, series_id,"
                        " series_name, series_count)"
                        " SELECT kind, book_id, series_id, series_name,"
                        " series_count FROM t_out ORDER BY fk, kind",
                        NULL, NULL, NULL);
      inserted = sqlite3_changes(g_db);
    }
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_sorted", NULL, NULL, NULL);
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_grp", NULL, NULL, NULL);
    sqlite3_exec(g_db, "DROP TABLE IF EXISTS t_out", NULL, NULL, NULL);
  } else {
    /* Leaf: every chosen dimension is drilled; show the scope's books
     * flat. */
    snprintf(sql, sizeof sql,
             "INSERT INTO view(kind, book_id, series_id, series_name,"
             " series_count) SELECT 0, id, series_id, series, 0"
             " FROM books WHERE");
    view_where(sql, sizeof sql, bs_g_drill_level + 1);
    for (int L = 0; L < bs_g_drill_level; L++) {
      const char *e = dim_sql(dim_at(bs_g_group, L), 0);
      if (bs_g_drill_values[L][0])
        snprintf(sql + strlen(sql), sizeof sql - strlen(sql),
                 " AND (%s=?%d)", e, L + 1);
      else
        snprintf(sql + strlen(sql), sizeof sql - strlen(sql),
                 " AND (%s IS NULL OR %s='')", e, e);
    }
    snprintf(sql + strlen(sql), sizeof sql - strlen(sql), " ORDER BY %s",
             view_order());
    sqlite3_stmt *st = NULL;
    rc = sqlite3_prepare_v2(g_db, sql, -1, &st, NULL);
    if (rc == SQLITE_OK) {
      for (int L = 0; L < bs_g_drill_level; L++)
        if (bs_g_drill_values[L][0])
          bind_text_trunc(st, L + 1, bs_g_drill_values[L]);
      view_bind_query(st, bs_g_drill_level + 1);
      rc = sqlite3_step(st);
      inserted = sqlite3_changes(g_db);
      sqlite3_finalize(st);
    }
  }

  if (rc != SQLITE_DONE && rc != SQLITE_OK) {
    bs_LOG("[bookshelf] view_rebuild failed: %s\n", sqlite3_errmsg(g_db));
    sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, NULL);
    /* The rollback restored the previous view rows; the cached total
     * goes back to what it was before the rebuild started. */
    bs_g_view_total = prev_total;
    return; /* previous view and g_view_total stay intact */
  }
  if (sqlite3_exec(g_db, "COMMIT", NULL, NULL, NULL) != SQLITE_OK) {
    bs_LOG("[bookshelf] view_rebuild: COMMIT failed: %s\n",
        sqlite3_errmsg(g_db));
    sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, NULL);
    bs_g_view_total = prev_total;
    return; /* previous view and g_view_total stay intact */
  }

  /* The view table is only written here, so the insert count is the
   * authoritative total — no COUNT(*) per rebuild.  Record the source
   * this view was projected for: finish_sync compares against it to
   * catch source switches whose sync applies no rows. */
  bs_g_view_source = bs_g_state.source;
  bs_g_view_total = inserted;
  bs_LOG("[bookshelf] view_rebuild: view=%d sort=%d group=%d drill=%d\n",
      bs_g_view_total, (int)bs_g_state.sort, (int)bs_g_group,
      bs_g_drill_level);
}

int bs_view_total(void) {
  if (g_db == NULL)
    return 0;
  /* The view table is only ever written by view_rebuild, which keeps
   * g_view_total current; a COUNT(*) here would be pure overhead on
   * every page clamp and popup subline. */
  return bs_g_view_total;
}

/* Fill one TileRow from a joined view+books row.  BOOK_COLS_Q is 13
 * columns (0..12), then v.kind=13, v.book_id=14, v.series_id=15,
 * v.series_name=16, v.series_count=17. */
static void fill_row_from_stmt(sqlite3_stmt *st, BsTileRow *tr) {
  memset(tr, 0, sizeof *tr);
  fill_book_from_stmt(st, &tr->book); /* book cols first: 0..13 */
  tr->is_series = sqlite3_column_int(st, 14);
  snprintf(tr->series_id, sizeof tr->series_id, "%s",
           (const char *)sqlite3_column_text(st, 16));
  snprintf(tr->series_name, sizeof tr->series_name, "%s",
           (const char *)sqlite3_column_text(st, 17));
  tr->series_count = sqlite3_column_int(st, 18);
}

/* Read one page of the current view into rows[].  Returns the number of
 * rows filled. */
int bs_view_fetch_page(int page, BsTileRow *rows, int cap) {
  if (g_db == NULL)
    return 0;
  int ps = bs_view_pagesize();
  if (ps < 1)
    ps = BS_PAGESIZE;
  long long lo = (long long)page * ps; /* exclusive */
  long long hi = lo + ps;              /* inclusive */
  sqlite3_stmt *st = NULL;
  char sql[512];
  snprintf(sql, sizeof sql,
           "SELECT " BS_BOOK_COLS_Q
           ", v.kind, v.book_id, v.series_id, v.series_name,"
           " v.series_count FROM view v JOIN books b ON b.id=v.book_id"
           " WHERE v.pos>?1 AND v.pos<=?2 ORDER BY v.pos");
  if (sqlite3_prepare_v2(g_db, sql, -1, &st, NULL) != SQLITE_OK) {
    bs_LOG("[bookshelf] view_fetch_page PREPARE FAIL: %s\n", sqlite3_errmsg(g_db));
    return 0;
  }
  sqlite3_bind_int64(st, 1, lo);
  sqlite3_bind_int64(st, 2, hi);
  int n = 0;
  while (n < cap && sqlite3_step(st) == SQLITE_ROW)
    fill_row_from_stmt(st, &rows[n++]);
  sqlite3_finalize(st);
  /* A grouped stack card whose dimension value is empty gets the
   * "No <dim>" label instead of a blank caption. */
  if (grouped_active() && n > 0) {
    BsGroupDim d = dim_at(bs_g_group, bs_g_drill_level);
    for (int i = 0; i < n; i++)
      if (rows[i].is_series && rows[i].series_name[0] == '\0')
        group_label_for(d, "", rows[i].series_name, sizeof rows[i].series_name);
  }
  return n;
}

/* Fetch the single view row at global index idx (0-based). */
int bs_view_fetch_row(int idx, BsTileRow *out) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  char sql[512];
  snprintf(sql, sizeof sql,
           "SELECT " BS_BOOK_COLS_Q
           ", v.kind, v.book_id, v.series_id, v.series_name,"
           " v.series_count FROM view v JOIN books b ON b.id=v.book_id WHERE "
           "v.pos=?1");
  if (sqlite3_prepare_v2(g_db, sql, -1, &st, NULL) != SQLITE_OK)
    return 0;
  sqlite3_bind_int64(st, 1, idx + 1);
  int found = sqlite3_step(st) == SQLITE_ROW;
  if (found)
    fill_row_from_stmt(st, out);
  sqlite3_finalize(st);
  return found;
}
