/* eh_progress.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_progress.h"
#include "eh_worker.h"

#include "sqlite3.h"

/* ── reading progress ──────────────────────────────────────────────────
 * Percent-read per book is sourced from the platform's progress store
 * (on PocketBook the firmware's explorer-3.db: the integrated reader
 * writes page/total while reading, and the KOReader plugin
 * "pocketbooksync" writes into the very same table).  The schema query
 * is platform-owned behind eh_plat_progress_read; this module only
 * orchestrates the copy, caches the map, and answers lookups.  The
 * shelf renders percent-read as a black bar at the bottom of every
 * cover. */

/* The progress source DB and its writable snapshot (db + -wal + -shm)
 * live at platform-owned paths: eh_plat_progress_db() is the firmware
 * explorer db, eh_plat_progress_snap() the snapshot the worker copies
 * into so a read-only open never blocks a non-writable guest. */
/* Upper bound for a fallback copy: the explorer db is a handful of MB
 * at most; refusing to copy anything pathological keeps a huge source
 * from stalling the worker. */
#define EH_PROGRESS_COPY_MAX (64 * 1024 * 1024)

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
        snprintf(src, sizeof src, "%s%s", eh_plat_progress_db(), suffixes[i]);
        snprintf(dst, sizeof dst, "%s%s", eh_plat_progress_snap(), suffixes[i]);
        struct stat st, sd;
        if (iv_stat(src, &st) != 0) {
            remove(dst);
            continue;
        }
        /* Bound: refuse to copy a pathological source. */
        if ((unsigned long long)st.st_size > EH_PROGRESS_COPY_MAX)
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
    const char *snap = eh_plat_progress_snap();
    if (sqlite3_open_v2(snap, &db, SQLITE_OPEN_READWRITE, NULL) == SQLITE_OK)
        return db;
    eh_LOG("[bookshelf] progress: cannot open %s\n", snap);
    return NULL;
}

/* Re-read the progress map from books_settings into the shared array,
 * publishing it wholesale at the end (g_progress_count is set last) so
 * progress_percent never observes a half-populated map — it reads either
 * the previous complete map or the new one.  Runs on the main thread;
 * the worker only copied the snapshot.  The schema query itself is
 * platform-owned (eh_plat_progress_read). */
static void
progress_reload_db(void)
{
    sqlite3 *db = progress_db_open();
    if (db == NULL)
        return;
    int n = eh_plat_progress_read(db, g_progress,
                                  (int)(sizeof g_progress / sizeof g_progress[0]));
    sqlite3_close(db);
    /* Publish on complete: the count is the commit point. */
    g_progress_count = n;
    eh_LOG("[bookshelf] progress: %d entries\n", n);
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
eh_progress_reload(void)
{
    if (eh_worker_submit(progress_copy_job, progress_reload_done, NULL) == NULL) {
        /* Worker unavailable: fall back to a synchronous reload. */
        progress_snapshot();
        progress_reload_db();
    }
}

/* Percent read (0..100) for a book file, 0 when unknown. */
int
eh_progress_percent(const char *path)
{
    if (path == NULL || path[0] == '\0')
        return 0;
    for (int i = 0; i < g_progress_count; i++)
        if (strcmp(g_progress[i].path, path) == 0)
            return g_progress[i].percent;
    return 0;
}