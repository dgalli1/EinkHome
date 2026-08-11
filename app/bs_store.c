/* bs_store.c - part of the bookshelf app (see bookshelf.h)
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

#include "bookshelf.h"
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
static sqlite3_stmt *g_st_next_ids;      /* rowid-keyset id scan */

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
    " filename TEXT, source TEXT, search_text TEXT);"
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
    " ON books(series_id, series_idx, id);"
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
    "CREATE INDEX IF NOT EXISTS idx_suggest_book ON suggest(book_id);";

/* Build the absolute store path next to the config file. */
static void store_path(char *out, size_t cap) {
  char dir[MAX_PATH_LEN];
  dirname_of(g_config_path, dir, sizeof dir);
  snprintf(out, cap, "%s/%s", dir, LIB_DB_FILENAME);
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
      {"search_text", "TEXT"},
  };
  int changed = 0;
  for (size_t i = 0; i < sizeof mig / sizeof mig[0]; i++) {
    int has = store_has_column("books", mig[i].col);
    LOG("[bookshelf] store: dbg col=%s has=%d err=%s\n", mig[i].col, has,
        g_db ? sqlite3_errmsg(g_db) : "?");
    if (has)
      continue;
    char sql[128];
    snprintf(sql, sizeof sql, "ALTER TABLE books ADD COLUMN %s %s", mig[i].col,
             mig[i].type);
    if (sqlite3_exec(g_db, sql, NULL, NULL, NULL) != SQLITE_OK)
      LOG("[bookshelf] store: migrate %s failed: %s\n", mig[i].col,
          sqlite3_errmsg(g_db));
    else
      changed = 1;
  }
  if (changed) {
    /* Rows written before the migration carry no data in the new
     * columns; a full re-sync repopulates them.  The marker makes
     * the reset one-shot: otherwise every boot would reset the
     * cursor and re-sync the whole library. */
    store_set_cursor(0);
    store_set_meta("schema_version", "2");
    LOG("[bookshelf] store: schema migrated; sync cursor reset\n");
  }
}

/* Persist one meta key/value pair (used for one-shot migration markers). */
static int bind_text_trunc(sqlite3_stmt *st, int i, const char *s);

void store_set_meta(const char *key, const char *value) {
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

int store_meta_value(const char *key, char *out, size_t cap) {
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
  char *txt = read_text_file(legacy_path);
  if (txt == NULL)
    return 0;
  cJSON *root = cJSON_Parse(txt);
  free(txt);
  if (root == NULL) {
    LOG("[bookshelf] store: legacy import: JSON parse failed\n");
    return -1;
  }
  int count = 0;
  int failed = 0;
  Book tmp;
  if (sqlite3_exec(g_db, "BEGIN", NULL, NULL, NULL) != SQLITE_OK) {
    LOG("[bookshelf] store: legacy import BEGIN failed: %s\n",
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
      if (parse_book_obj(it, &tmp, 1) == 0 && store_upsert_book(&tmp) == 0)
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
void store_open(void) {
  char path[MAX_PATH_LEN * 2];
  store_path(path, sizeof path);

  if (sqlite3_open(path, &g_db) != SQLITE_OK) {
    LOG("[bookshelf] store: open failed %s: %s\n", path,
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
    if (store_meta_value("schema_version", ver, sizeof ver) != 1 ||
        strcmp(ver, "2") != 0) {
      if (store_has_column("books", "id"))
        store_migrate_columns();
      store_set_meta("schema_version", "2");
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
      LOG("[bookshelf] store: schema failed: %s\n", sqlite3_errmsg(g_db));
      sqlite3_close(g_db);
      g_db = NULL;
      return;
    }
  }

  /* One-time legacy JSON import. */
  char legacy[MAX_PATH_LEN * 2];
  char dir[MAX_PATH_LEN];
  dirname_of(g_config_path, dir, sizeof dir);
  snprintf(legacy, sizeof legacy, "%s/%s", dir, LIB_LEGACY_FILENAME);
  FILE *f = fopen(legacy, "r");
  if (f != NULL) {
    fclose(f);
    int n = store_import_legacy(legacy);
    if (n < 0) {
      /* Import failed midway (or could not commit): keep the JSON in
       * place so the import re-runs on the next boot. */
      LOG("[bookshelf] store: legacy import incomplete, keeping %s\n",
          legacy);
    } else {
      char migrated[MAX_PATH_LEN * 2 + 16];
      snprintf(migrated, sizeof migrated, "%s.migrated", legacy);
      rename(legacy, migrated);
      LOG("[bookshelf] store: migrated legacy JSON (%d books)\n", n);
    }
  }
}

void store_close(void) {
  sqlite3_finalize(g_st_upsert_lookup);
  sqlite3_finalize(g_st_upsert);
  sqlite3_finalize(g_st_suggest_del);
  sqlite3_finalize(g_st_suggest_ins);
  sqlite3_finalize(g_st_get_book);
  sqlite3_finalize(g_st_set_downloaded);
  sqlite3_finalize(g_st_next_ids);
  g_st_upsert_lookup = NULL;
  g_st_upsert = NULL;
  g_st_suggest_del = NULL;
  g_st_suggest_ins = NULL;
  g_st_get_book = NULL;
  g_st_set_downloaded = NULL;
  g_st_next_ids = NULL;
  if (g_db != NULL) {
    sqlite3_close(g_db);
    g_db = NULL;
  }
}

/* ── row CRUD ------------------------------------------------------------- */

static int bind_text_trunc(sqlite3_stmt *st, int i, const char *s) {
  return sqlite3_bind_text(st, i, s ? s : "", -1, SQLITE_TRANSIENT);
}

/* Insert or update one book row.  An existing row keeps its
 * downloaded/local_path state (file removal goes through
 * store_set_downloaded); a fresh row inherits whatever the caller
 * probed.  Returns 0 on success. */
int store_upsert_book(const Book *b) {
  if (g_db == NULL)
    return -1;

  int downloaded = b->downloaded;
  /* Copy the existing row's local_path into a local buffer BEFORE the
   * lookup statement is reused: sqlite3_column_text() pointers die
   * with the statement's next step/reset, and the value is re-bound
   * below.  Default to the caller's path so fresh rows (and rows
   * whose downloaded flag is 0) keep the exact pre-fix semantics. */
  char lp[MAX_PATH_LEN];
  snprintf(lp, sizeof lp, "%s", b->local_path);
  sqlite3_stmt *q = st_prep_once(
      &g_st_upsert_lookup,
      "SELECT downloaded, local_path FROM books WHERE id=?1");
  if (q != NULL) {
    sqlite3_reset(q);
    sqlite3_clear_bindings(q);
    bind_text_trunc(q, 1, b->id);
    if (sqlite3_step(q) == SQLITE_ROW) {
      if (sqlite3_column_int(q, 0) == 1) {
        downloaded = 1;
        const char *t = (const char *)sqlite3_column_text(q, 1);
        snprintf(lp, sizeof lp, "%s", t ? t : "");
      }
    }
  }

  sqlite3_stmt *st = st_prep_once(
      &g_st_upsert,
      "INSERT OR REPLACE INTO books("
      "id,title,author,series,series_id,series_idx,"
      "ext,size,downloaded,local_path,added_at,"
      "filename,source,search_text)"
      " VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)");
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
  int rc = sqlite3_step(st);
  if (rc != SQLITE_DONE)
    LOG("[bookshelf] upsert FAILED id=%s rc=%d: %s\n", b->id, rc,
        sqlite3_errmsg(g_db));
  return rc == SQLITE_DONE ? 0 : -1;
}

void store_delete_book(const char *id) {
  if (g_db == NULL)
    return;
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
void store_delete_source(const char *source) {
  if (g_db == NULL)
    return;
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
int store_local_meta_get(const char *id, char *title, size_t title_cap,
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

void store_local_meta_put(const char *id, const char *title,
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

void store_set_downloaded(const char *id, int downloaded,
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
  sqlite3_step(st);
}

static void fill_book_from_stmt(sqlite3_stmt *st, Book *b) {
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
}

#define BOOK_COLS                                                              \
  "id,title,author,series,series_id,series_idx,ext,size,downloaded,local_"     \
  "path,added_at,"                                                             \
  "filename,source"
/* books columns qualified for the view JOIN (bare BOOK_COLS would leave
 * every column after the first unqualified and ambiguous). */
#define BOOK_COLS_Q                                                            \
  "b.id,b.title,b.author,b.series,b.series_id,b.series_idx,b.ext,b.size,b."    \
  "downloaded,"                                                                \
  "b.local_path,b.added_at,b.filename,b.source"

/* Fetch one book row by id.  Returns 1 when found, 0 otherwise. */
int store_get_book(const char *id, Book *out) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = st_prep_once(
      &g_st_get_book, "SELECT " BOOK_COLS " FROM books WHERE id=?1");
  if (st == NULL)
    return 0;
  sqlite3_reset(st);
  sqlite3_clear_bindings(st);
  bind_text_trunc(st, 1, id);
  int found = sqlite3_step(st) == SQLITE_ROW;
  if (found)
    fill_book_from_stmt(st, out);
  return found;
}

void store_series_name(const char *series_id, char *out, size_t cap) {
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

int store_count(void) {
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

int store_count_undownloaded(void) {
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
int store_next_undownloaded(char ids[][MAX_ID_LEN], int cap) {
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
    snprintf(ids[n], MAX_ID_LEN, "%s", id ? id : "");
    n++;
  }
  sqlite3_finalize(st);
  return n;
}

/* Slice of every book id in rowid order (downloaded or not), for the
 * startup flag refresh.  Rowid keyset pagination: the caller keeps the
 * last rowid seen (*after_rowid, 0 to start), so each page is one
 * b-tree scan of the rowid index with no OFFSET re-walk.  Results are
 * copied into ids[] before the statement is reused.  Returns the number
 * of ids written (< cap = done); *after_rowid advances to the last
 * rowid read and stays unchanged when no rows are returned. */
int store_next_ids(char ids[][MAX_ID_LEN], int cap, long long *after_rowid) {
  if (g_db == NULL || after_rowid == NULL)
    return 0;
  sqlite3_stmt *st = st_prep_once(
      &g_st_next_ids,
      "SELECT rowid, id FROM books WHERE rowid > ?1 ORDER BY rowid LIMIT ?2");
  if (st == NULL)
    return 0;
  sqlite3_reset(st);
  sqlite3_clear_bindings(st);
  sqlite3_bind_int64(st, 1, *after_rowid);
  sqlite3_bind_int(st, 2, cap);
  int n = 0;
  while (n < cap && sqlite3_step(st) == SQLITE_ROW) {
    const char *id = (const char *)sqlite3_column_text(st, 1);
    snprintf(ids[n], MAX_ID_LEN, "%s", id ? id : "");
    *after_rowid = sqlite3_column_int64(st, 0);
    n++;
  }
  return n;
}

/* Delete a book's local file and mark it not downloaded.  The metadata
 * row stays — the server remains the source of truth for the library. */
void store_delete_book_file(const char *id) {
  Book b;
  if (!store_get_book(id, &b))
    return;
  char path[MAX_PATH_LEN];
  /* Remove the file where it actually lives (the stored location may
   * predate a downloads-folder change). */
  book_existing_path(&b, path, sizeof path);
  if (unlink(path) == 0)
    LOG("[bookshelf] delete_book_file removed %s\n", path);
  else
    LOG("[bookshelf] delete_book_file unlink failed %s\n", path);
  store_set_downloaded(id, 0, "");
  DownloadItem *d = find_download(id);
  if (d != NULL)
    d->state = 3;
}

/* Slice of one series' member ids in volume order. */
int store_series_ids(const char *series_id, char ids[][MAX_ID_LEN], int cap,
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
    snprintf(ids[n], MAX_ID_LEN, "%s", id ? id : "");
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
void store_search_add(const char *term) {
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
    sqlite3_bind_int(trim, 1, SEARCH_HISTORY_MAX);
    sqlite3_step(trim);
    sqlite3_finalize(trim);
  }
}

int store_search_count(void) {
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
int store_search_list(char terms[][MAX_QUERY_LEN], int cap, int offset) {
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
    snprintf(terms[n], MAX_QUERY_LEN, "%s", t ? t : "");
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
void store_suggest_set(const char *book_id,
                       const char terms[][SUGGEST_TERM_MAX], int n) {
  if (g_db == NULL || book_id == NULL)
    return;
  sqlite3_stmt *del = st_prep_once(&g_st_suggest_del,
                                   "DELETE FROM suggest WHERE book_id=?1");
  if (del == NULL)
    return;
  sqlite3_reset(del);
  sqlite3_clear_bindings(del);
  bind_text_trunc(del, 1, book_id);
  sqlite3_step(del);
  if (n <= 0 || terms == NULL)
    return;
  /* One multi-row statement instead of up to 96 single-row inserts: a
   * sync round runs this per book, so statement count matters on a
   * 300MHz core.  96 pairs fit comfortably in the stack buffer. */
  char   sql[2048];
  int    nterms = 0;
  size_t len = snprintf(sql, sizeof sql,
                        "INSERT OR IGNORE INTO suggest(term, book_id) VALUES");
  for (int i = 0; i < n; i++) {
    if (terms[i][0] == '\0')
      continue;
    len += snprintf(sql + len, sizeof sql - len, "%s(?,?)",
                    nterms == 0 ? " " : ",");
    nterms++;
  }
  if (nterms == 0)
    return; /* all terms empty: the DELETE above was the whole job */
  sqlite3_stmt *ins = NULL;
  if (sqlite3_prepare_v2(g_db, sql, -1, &ins, NULL) != SQLITE_OK)
    return;
  int p = 1;
  for (int i = 0; i < n; i++) {
    if (terms[i][0] == '\0')
      continue;
    bind_text_trunc(ins, p++, terms[i]);
    bind_text_trunc(ins, p++, book_id);
  }
  sqlite3_step(ins);
  sqlite3_finalize(ins);
}

/* Prefix lookup over the term index, most-popular first (a term's
 * count = number of books containing it; ties by term).  Only
 * ASCII-lowercasing happens here — the server folded terms, so a
 * folded-ASCII term matches typed ASCII input; non-ASCII typed input
 * may miss (accepted).  Empty and single-char prefixes return 0: the
 * range is useless at 100k books and the UI keeps showing history.
 * Bounded output: LIMIT keeps RAM O(cap), never the whole index. */
int store_suggest_list(const char *prefix, char out[][SUGGEST_TERM_MAX],
                       int cap) {
  if (g_db == NULL || prefix == NULL || cap <= 0)
    return 0;
  size_t len = strlen(prefix);
  if (len < 2 || len >= SUGGEST_TERM_MAX)
    return 0;
  char norm[SUGGEST_TERM_MAX];
  for (size_t i = 0; i <= len; i++)
    norm[i] = (prefix[i] >= 'A' && prefix[i] <= 'Z') ? (char)(prefix[i] + 32)
                                                     : prefix[i];
  char bound[SUGGEST_TERM_MAX + 4];
  int has_bound = suggest_upper_bound(norm, len, bound, sizeof bound);

  const char *sql =
      has_bound
          ? "SELECT term FROM suggest WHERE term >= ?1 AND term < ?2"
            " GROUP BY term ORDER BY COUNT(*) DESC, term ASC LIMIT ?3"
          : "SELECT term FROM suggest WHERE term >= ?1"
            " GROUP BY term ORDER BY COUNT(*) DESC, term ASC LIMIT ?3";
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
    snprintf(out[n], SUGGEST_TERM_MAX, "%s", t ? t : "");
    n++;
  }
  sqlite3_finalize(st);
  return n;
}

/* ── sync cursor + batch transaction -------------------------------------- */

long long store_get_cursor(void) {
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

void store_set_cursor(long long cursor) {
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

void store_begin(void) {
  if (g_db != NULL) {
    char *msg = NULL;
    if (sqlite3_exec(g_db, "BEGIN", NULL, NULL, &msg) != SQLITE_OK) {
      LOG("[bookshelf] store_begin FAILED: %s\n",
          msg ? msg : sqlite3_errmsg(g_db));
      if (msg)
        sqlite3_free(msg);
    }
  }
}

void store_commit(void) {
  if (g_db != NULL) {
    char *msg = NULL;
    if (sqlite3_exec(g_db, "COMMIT", NULL, NULL, &msg) != SQLITE_OK) {
      LOG("[bookshelf] store_commit FAILED: %s\n",
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
void store_rollback(void) {
  if (g_db != NULL) {
    char *msg = NULL;
    if (sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, &msg) != SQLITE_OK) {
      LOG("[bookshelf] store_rollback FAILED: %s\n",
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

/* Append the active filter/query WHERE clause (AND-joined) to sql.
 * qbind is the parameter index the query pattern will be bound at
 * (0 = no query parameter). */
static void view_where(char *sql, size_t cap, int qbind) {
  /* The downloaded/remote filter was removed; every caller builds
   * "WHERE" + this clause, so a constant true keeps the pattern. */
  snprintf(sql + strlen(sql), cap - strlen(sql), " 1=1");
  if (qbind > 0 && g_state.query[0] != '\0') {
    /* One bound pattern serves all LIKEs (same ?qbind index).  The
     * folded search_text column joins the raw fields so folded
     * suggestions (e.g. "songgong" from "sŏnggong") and diacritic
     * queries actually find books; it is NULL for local imports,
     * where the raw fields still match.  Series joined so series-word
     * suggestions produce results. */
    snprintf(sql + strlen(sql), cap - strlen(sql),
             " AND (title LIKE ?%d ESCAPE '\\' OR author LIKE ?%d ESCAPE '\\'"
             " OR series LIKE ?%d ESCAPE '\\'"
             " OR search_text LIKE ?%d ESCAPE '\\')",
             qbind, qbind, qbind, qbind);
  }
  /* Only the active source's books are visible; rows written before
   * the source column existed are kavita books.  The value comes from
   * a fixed enum, so no quoting concerns. */
  const char *src = g_state.source == SOURCE_LOCAL    ? "local"
                    : g_state.source == SOURCE_FOLDER ? "folder"
                                                      : "kavita";
  snprintf(sql + strlen(sql), cap - strlen(sql),
           " AND COALESCE(source,'kavita')='%s'", src);
}

static const char *view_order(void) {
  switch (g_state.sort) {
  case SORT_AUTHOR:
    return "author COLLATE NOCASE, title COLLATE NOCASE, id";
  case SORT_SERIES:
    return "series COLLATE NOCASE, series_idx, id";
  case SORT_RECENT:
    return "added_at DESC, title COLLATE NOCASE, id";
  default:
    return "title COLLATE NOCASE, id";
  }
}

/* Bind the query pattern at qbind (no-op when the query is empty or
 * qbind is 0). */
static void view_bind_query(sqlite3_stmt *st, int qbind) {
  if (qbind <= 0 || g_state.query[0] == '\0')
    return;
  char pat[MAX_QUERY_LEN * 2 + 4];
  char esc[MAX_QUERY_LEN * 2];
  like_escape(g_state.query, esc, sizeof esc);
  snprintf(pat, sizeof pat, "%%%s%%", esc);
  bind_text_trunc(st, qbind, pat);
}

/* Rebuild the materialised view table for the current filter/sort/
 * group/drill.  Collapse modes (GROUP_ALL / GROUP_BY_SERIES at the top
 * level) emit standalone books as flat tiles and multi-book series as
 * single cards, interleaved in first-seen order of the active sort;
 * single-member series stay flat.  Everything else (drill, author /
 * recent grouping) is a flat projection.  All ordering and grouping
 * happens in SQL so RAM never holds the whole library. */
void view_rebuild(void) {
  if (g_db == NULL)
    return;

  /* A rebuilt view renumbers every tile: disarm any in-flight
   * long-press so it cannot fire on a row that no longer maps to the
   * same book (or is out of range). */
  g_lp_vi = -1;
  g_lp_armed = 0;

  /* Keep the count the previous view had: the rollback paths below
   * restore the old view rows, so the cached total must go back to
   * that value instead of being recounted. */
  int prev_total = g_view_total;

  if (sqlite3_exec(g_db, "BEGIN", NULL, NULL, NULL) != SQLITE_OK) {
    LOG("[bookshelf] view_rebuild: BEGIN failed: %s\n",
        sqlite3_errmsg(g_db));
    return; /* previous view and g_view_total stay intact */
  }
  if (sqlite3_exec(g_db, "DELETE FROM view", NULL, NULL, NULL) != SQLITE_OK) {
    LOG("[bookshelf] view_rebuild: DELETE failed: %s\n",
        sqlite3_errmsg(g_db));
    sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, NULL);
    return; /* previous view and g_view_total stay intact */
  }
  g_view_total = 0;

  char sql[2048];
  int rc = SQLITE_OK;
  /* Rows written to view by the INSERT below; becomes g_view_total
   * after COMMIT (sqlite3_changes must be read before any DROP TABLE
   * or the COMMIT — both reset the counter). */
  int inserted = 0;

  if (g_drilled_series[0] != '\0') {
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
      bind_text_trunc(st, 1, g_drilled_series);
      view_bind_query(st, 2);
      rc = sqlite3_step(st);
      inserted = sqlite3_changes(g_db);
      sqlite3_finalize(st);
    }
  } else if (g_state.group == GROUP_BY_SERIES || g_state.group == GROUP_ALL) {
    /* Collapse mode: order the filtered set once into a temp table
     * (rowid = sort position), then emit flats and series cards
     * keyed by first-seen position. */
    rc = sqlite3_exec(g_db,
                      "CREATE TEMP TABLE t_sorted(id TEXT, series_id TEXT,"
                      " series_idx REAL, series TEXT)",
                      NULL, NULL, NULL);
    if (rc != SQLITE_OK)
      LOG("[bookshelf] view_rebuild: t_sorted create rc=%d: %s\n", rc,
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
  } else {
    /* Flat projection (author / recent grouping). */
    snprintf(
        sql, sizeof sql,
        "INSERT INTO view(kind, book_id, series_id, series_name, series_count)"
        " SELECT 0, id, series_id, series, 0 FROM books WHERE");
    view_where(sql, sizeof sql, 1);
    snprintf(sql + strlen(sql), sizeof sql - strlen(sql), " ORDER BY %s",
             view_order());
    sqlite3_stmt *st = NULL;
    rc = sqlite3_prepare_v2(g_db, sql, -1, &st, NULL);
    if (rc == SQLITE_OK) {
      view_bind_query(st, 1);
      rc = sqlite3_step(st);
      inserted = sqlite3_changes(g_db);
      sqlite3_finalize(st);
    }
  }

  if (rc != SQLITE_DONE && rc != SQLITE_OK) {
    LOG("[bookshelf] view_rebuild failed: %s\n", sqlite3_errmsg(g_db));
    sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, NULL);
    /* The rollback restored the previous view rows; the cached total
     * goes back to what it was before the rebuild started. */
    g_view_total = prev_total;
    return; /* previous view and g_view_total stay intact */
  }
  if (sqlite3_exec(g_db, "COMMIT", NULL, NULL, NULL) != SQLITE_OK) {
    LOG("[bookshelf] view_rebuild: COMMIT failed: %s\n",
        sqlite3_errmsg(g_db));
    sqlite3_exec(g_db, "ROLLBACK", NULL, NULL, NULL);
    g_view_total = prev_total;
    return; /* previous view and g_view_total stay intact */
  }

  /* The view table is only written here, so the insert count is the
   * authoritative total — no COUNT(*) per rebuild.  Record the source
   * this view was projected for: finish_sync compares against it to
   * catch source switches whose sync applies no rows. */
  g_view_source = g_state.source;
  g_view_total = inserted;
  LOG("[bookshelf] view_rebuild: view=%d sort=%d group=%d drill=%d\n",
      g_view_total, (int)g_state.sort, (int)g_state.group,
      g_drilled_series[0] != '\0');
}

int view_total(void) {
  if (g_db == NULL)
    return 0;
  /* The view table is only ever written by view_rebuild, which keeps
   * g_view_total current; a COUNT(*) here would be pure overhead on
   * every page clamp and popup subline. */
  return g_view_total;
}

/* Fill one TileRow from a joined view+books row.  BOOK_COLS_Q is 13
 * columns (0..12), then v.kind=13, v.book_id=14, v.series_id=15,
 * v.series_name=16, v.series_count=17. */
static void fill_row_from_stmt(sqlite3_stmt *st, TileRow *tr) {
  memset(tr, 0, sizeof *tr);
  fill_book_from_stmt(st, &tr->book); /* book cols first: 0..12 */
  tr->is_series = sqlite3_column_int(st, 13);
  snprintf(tr->series_id, sizeof tr->series_id, "%s",
           (const char *)sqlite3_column_text(st, 15));
  snprintf(tr->series_name, sizeof tr->series_name, "%s",
           (const char *)sqlite3_column_text(st, 16));
  tr->series_count = sqlite3_column_int(st, 17);
}

/* Read one page of the current view into rows[].  Returns the number of
 * rows filled. */
int view_fetch_page(int page, TileRow *rows, int cap) {
  if (g_db == NULL)
    return 0;
  int ps = view_pagesize();
  if (ps < 1)
    ps = PAGESIZE;
  long long lo = (long long)page * ps; /* exclusive */
  long long hi = lo + ps;              /* inclusive */
  sqlite3_stmt *st = NULL;
  char sql[512];
  snprintf(sql, sizeof sql,
           "SELECT " BOOK_COLS_Q
           ", v.kind, v.book_id, v.series_id, v.series_name,"
           " v.series_count FROM view v JOIN books b ON b.id=v.book_id"
           " WHERE v.pos>?1 AND v.pos<=?2 ORDER BY v.pos");
  if (sqlite3_prepare_v2(g_db, sql, -1, &st, NULL) != SQLITE_OK) {
    LOG("[bookshelf] view_fetch_page PREPARE FAIL: %s\n", sqlite3_errmsg(g_db));
    return 0;
  }
  sqlite3_bind_int64(st, 1, lo);
  sqlite3_bind_int64(st, 2, hi);
  int n = 0;
  while (n < cap && sqlite3_step(st) == SQLITE_ROW)
    fill_row_from_stmt(st, &rows[n++]);
  sqlite3_finalize(st);
  return n;
}

/* Fetch the single view row at global index idx (0-based). */
int view_fetch_row(int idx, TileRow *out) {
  if (g_db == NULL)
    return 0;
  sqlite3_stmt *st = NULL;
  char sql[512];
  snprintf(sql, sizeof sql,
           "SELECT " BOOK_COLS_Q
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
