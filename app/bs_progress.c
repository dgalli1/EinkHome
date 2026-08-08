/* bs_progress.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
#include "sqlite3.h"

/* ── reading progress ──────────────────────────────────────────────────
 * Both readers report into the firmware's explorer-3.db
 * `books_settings` table: the integrated reader writes its current
 * page / total pages while reading, and the KOReader plugin
 * "pocketbooksync" (github.com/ckilb/pocketbooksync.koplugin) writes
 * KOReader's progress into the very same table.  So one read of that
 * table covers both sources.  The shelf renders percent-read as a
 * black bar at the bottom of every cover. */

#define PROGRESS_DB "/mnt/ext1/system/explorer-3/explorer-3.db"

typedef struct {
    char path[MAX_PATH_LEN]; /* folder + "/" + filename */
    int  percent;            /* 0..100 */
} ProgressEntry;

static ProgressEntry g_progress[4096];
static int           g_progress_count = 0;

/* Open the explorer DB read-only.  A live -wal/-shm set may block a
 * read-only open for a non-writable guest (emulator); fall back to a
 * /tmp snapshot copy of db+wal+shm. */
static sqlite3 *
progress_db_open(void)
{
    sqlite3 *db = NULL;
    if (sqlite3_open_v2(PROGRESS_DB, &db, SQLITE_OPEN_READONLY, NULL) == SQLITE_OK) {
        sqlite3_stmt *st = NULL;
        int ok = sqlite3_prepare_v2(db, "SELECT COUNT(*) FROM books_settings", -1, &st, NULL) ==
                     SQLITE_OK &&
                 sqlite3_step(st) == SQLITE_ROW;
        if (st != NULL)
            sqlite3_finalize(st);
        if (ok)
            return db;
        sqlite3_close(db);
        db = NULL;
    }

    const char *suffixes[] = {"", "-wal", "-shm"};
    for (size_t i = 0; i < sizeof suffixes / sizeof suffixes[0]; i++) {
        char src[300], dst[300];
        snprintf(src, sizeof src, "%s%s", PROGRESS_DB, suffixes[i]);
        snprintf(dst, sizeof dst, "/tmp/progress_import.db%s", suffixes[i]);
        FILE *in = fopen(src, "rb");
        if (in == NULL) {
            remove(dst);
            continue;
        }
        FILE *out = fopen(dst, "wb");
        if (out == NULL) {
            fclose(in);
            continue;
        }
        char   buf[16384];
        size_t n;
        while ((n = fread(buf, 1, sizeof buf, in)) > 0)
            fwrite(buf, 1, n, out);
        fclose(in);
        fclose(out);
    }
    if (sqlite3_open_v2("/tmp/progress_import.db", &db, SQLITE_OPEN_READWRITE, NULL) == SQLITE_OK)
        return db;
    LOG("[bookshelf] progress: cannot open %s\n", PROGRESS_DB);
    return NULL;
}

/* Reload the progress map from books_settings.  Cheap (one indexed
 * query over the handful of rows that have page data), so it is safe
 * to call at startup, on source switches and whenever the shelf is
 * shown again after reading. */
void
progress_reload(void)
{
    sqlite3 *db = progress_db_open();
    g_progress_count = 0;
    if (db == NULL)
        return;
    sqlite3_stmt *st = NULL;
    int           rc = sqlite3_prepare_v2(db,
                                "SELECT fol.name, f.filename, bs.cpage, bs.npage"
                                          " FROM books_settings bs"
                                          " JOIN files f ON f.book_id = bs.bookid"
                                          " JOIN folders fol ON fol.id = f.folder_id"
                                          " WHERE bs.npage IS NOT NULL AND bs.npage > 0",
                                -1,
                                &st,
                                NULL);
    if (rc == SQLITE_OK) {
        while (sqlite3_step(st) == SQLITE_ROW && g_progress_count < 4096) {
            const char *folder = (const char *)sqlite3_column_text(st, 0);
            const char *file = (const char *)sqlite3_column_text(st, 1);
            long long   cpage = sqlite3_column_int64(st, 2);
            long long   npage = sqlite3_column_int64(st, 3);
            if (folder == NULL || file == NULL || npage <= 0)
                continue;
            ProgressEntry *e = &g_progress[g_progress_count++];
            snprintf(e->path, sizeof e->path, "%s/%s", folder, file);
            int pct = (int)(cpage * 100 / npage);
            e->percent = pct < 1 ? 0 : (pct > 100 ? 100 : pct);
        }
        sqlite3_finalize(st);
    } else {
        LOG("[bookshelf] progress: query failed: %s\n", sqlite3_errmsg(db));
    }
    sqlite3_close(db);
    LOG("[bookshelf] progress: %d entries\n", g_progress_count);
}

/* Percent read (0..100) for a book file, 0 when unknown. */
int
progress_percent(const char *path)
{
    if (path == NULL || path[0] == '\0')
        return 0;
    for (int i = 0; i < g_progress_count; i++)
        if (strcmp(g_progress[i].path, path) == 0)
            return g_progress[i].percent;
    return 0;
}
