/* bs_progress.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_progress.h"
#include "bs_worker.h"

#include "sqlite3.h"

/* ── reading progress ──────────────────────────────────────────────────
 * Both readers report into the firmware's explorer-3.db
 * `books_settings` table: the integrated reader writes its current
 * page / total pages while reading, and the KOReader plugin
 * "pocketbooksync" (github.com/ckilb/pocketbooksync.koplugin) writes
 * KOReader's progress into the very same table.  So one read of that
 * table covers both sources.  The shelf renders percent-read as a
 * black bar at the bottom of every cover. */

#define BS_PROGRESS_DB "/mnt/ext1/system/explorer-3/explorer-3.db"
/* /tmp snapshot the worker refreshes (db + -wal + -shm). */
#define BS_PROGRESS_SNAP "/tmp/progress_import.db"
/* Upper bound for a fallback copy: the explorer db is a handful of MB
 * at most; refusing to copy anything pathological keeps a huge source
 * from stalling the worker. */
#define BS_PROGRESS_COPY_MAX (64 * 1024 * 1024)

typedef struct {
    char path[BS_MAX_PATH_LEN]; /* folder + "/" + filename */
    int  percent;            /* 0..100 */
} BsProgressEntry;

static BsProgressEntry g_progress[4096];
static int           g_progress_count = 0;

/* Worker thread: refresh the /tmp snapshot of the explorer db by
 * copying db + -wal + -shm when the source is newer than the snapshot
 * (a fresh-enough copy is not re-done) and bounded by size.  Runs off
 * the main thread so the copy never stalls the event loop.  Errors are
 * ignored — progress just stays empty on failure. */
static void
progress_snapshot(void)
{
    const char *suffixes[] = {"", "-wal", "-shm"};
    for (size_t i = 0; i < sizeof suffixes / sizeof suffixes[0]; i++) {
        char src[300], dst[300];
        snprintf(src, sizeof src, "%s%s", BS_PROGRESS_DB, suffixes[i]);
        snprintf(dst, sizeof dst, "%s%s", BS_PROGRESS_SNAP, suffixes[i]);
        struct stat st, sd;
        if (iv_stat(src, &st) != 0) {
            remove(dst);
            continue;
        }
        /* Bound: refuse to copy a pathological source. */
        if ((unsigned long long)st.st_size > BS_PROGRESS_COPY_MAX)
            continue;
        /* Skip when the snapshot is already at least as new as the
         * source (a fresh-enough copy from a prior reload). */
        if (iv_stat(dst, &sd) == 0 && sd.st_mtime >= st.st_mtime)
            continue;
        FILE *in = fopen(src, "rb");
        if (in == NULL)
            continue;
        FILE *out = fopen(dst, "wb");
        if (out == NULL) {
            fclose(in);
            continue;
        }
        char   buf[16384];
        size_t n;
        int    write_err = 0;
        while ((n = fread(buf, 1, sizeof buf, in)) > 0) {
            if (fwrite(buf, 1, n, out) != n) {
                write_err = 1;
                break;
            }
        }
        fclose(in);
        if (fclose(out) != 0)
            write_err = 1;
        if (write_err) {
            /* A truncated snapshot must never be read back as valid:
             * drop it so the next reload copies afresh. */
            remove(dst);
        }
    }
}

/* Worker fn: refresh the snapshot, then hand off to the main thread. */
static void
progress_copy_job(BsJob *job)
{
    progress_snapshot();
    job->rc = 0;
    atomic_store_explicit(&job->done, 1, memory_order_release);
}

/* Open the progress DB for reading.  The worker has already refreshed
 * the /tmp snapshot, so this opens the snapshot directly — a read-only
 * open of the live explorer-3.db can block on a live -wal/-shm set for
 * a non-writable guest, and doing that on the main thread would stall
 * the event loop. */
static sqlite3 *
progress_db_open(void)
{
    sqlite3 *db = NULL;
    if (sqlite3_open_v2(BS_PROGRESS_SNAP, &db, SQLITE_OPEN_READWRITE, NULL) == SQLITE_OK)
        return db;
    bs_LOG("[bookshelf] progress: cannot open %s\n", BS_PROGRESS_SNAP);
    return NULL;
}

/* Re-read the progress map from books_settings into the shared array,
 * publishing it wholesale at the end (g_progress_count is set last) so
 * progress_percent never observes a half-populated map — it reads either
 * the previous complete map or the new one.  Runs on the main thread;
 * the worker only copied the snapshot. */
static void
progress_reload_db(void)
{
    sqlite3 *db = progress_db_open();
    if (db == NULL)
        return;
    int n = 0;
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
        while (sqlite3_step(st) == SQLITE_ROW && n < 4096) {
            const char *folder = (const char *)sqlite3_column_text(st, 0);
            const char *file = (const char *)sqlite3_column_text(st, 1);
            long long   cpage = sqlite3_column_int64(st, 2);
            long long   npage = sqlite3_column_int64(st, 3);
            if (folder == NULL || file == NULL || npage <= 0)
                continue;
            BsProgressEntry *e = &g_progress[n];
            snprintf(e->path, sizeof e->path, "%s/%s", folder, file);
            int pct = (int)(cpage * 100 / npage);
            e->percent = pct < 1 ? 0 : (pct > 100 ? 100 : pct);
            n++;
        }
        sqlite3_finalize(st);
    } else {
        bs_LOG("[bookshelf] progress: query failed: %s\n", sqlite3_errmsg(db));
        sqlite3_close(db);
        return;
    }
    sqlite3_close(db);
    /* Publish on complete: the count is the commit point. */
    g_progress_count = n;
    bs_LOG("[bookshelf] progress: %d entries\n", n);
}

/* Main-thread done_cb: the snapshot copy finished on the worker, so
 * parse and publish the map here. */
static void
progress_reload_done(BsJob *job)
{
    (void)job;
    progress_reload_db();
}

/* Reload the progress map from books_settings.  Cheap (one indexed
 * query over the handful of rows that have page data), so it is safe
 * to call at startup, on source switches and whenever the shelf is
 * shown again after reading.  The fallback snapshot copy of the whole
 * explorer db (+ -wal + -shm) runs on the worker thread; the main
 * thread only parses and publishes the result. */
void
bs_progress_reload(void)
{
    if (bs_worker_submit(progress_copy_job, progress_reload_done, NULL) == NULL) {
        /* Worker unavailable: fall back to a synchronous reload. */
        progress_snapshot();
        progress_reload_db();
    }
}

/* Percent read (0..100) for a book file, 0 when unknown. */
int
bs_progress_percent(const char *path)
{
    if (path == NULL || path[0] == '\0')
        return 0;
    for (int i = 0; i < g_progress_count; i++)
        if (strcmp(g_progress[i].path, path) == 0)
            return g_progress[i].percent;
    return 0;
}