/* bs_downloads.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
#include "bs_downloads.h"
#include "bs_model.h"
#include "bs_store.h"
#include "bs_ui.h"
#include "bs_worker.h"

#include <dirent.h>

/* ── downloads, delete, context menu, long-press ───────────────────── */

/* Generation token for download-queue entries.  Bumped at every
 * enqueue and copied into the fetch job, so a job settles only the
 * entry whose id AND gen match — a canceled job that outlives its
 * queue entry can never mark the re-enqueued book failed (see
 * dl_job_done). */
static unsigned int g_dl_gen = 0;

/* Drop the last character of a UTF-8 string at a character boundary:
 * walk back over continuation bytes (10xxxxxx) to the sequence's lead
 * byte, so a truncated title never ends in a half multibyte char. */
static void
utf8_drop_last_char(char *s)
{
    size_t len = strlen(s);
    if (len == 0)
        return;
    size_t i = len - 1;
    while (i > 0 && ((unsigned char)s[i] & 0xC0) == 0x80)
        i--;
    s[i] = '\0';
}

/* Unlink stale "<file>.part" fragments left in the downloads dir by a
 * crash mid-fetch (dl_fetch writes the .part, then renames on success;
 * on any other exit the fragment would otherwise stay forever).
 * Bounded single pass over the directory; errors are ignored — the
 * worst case is a fragment surviving until the next startup. */
static void
sweep_stale_parts(void)
{
    DIR *d = opendir(g_downloads_dir);
    if (d == NULL)
        return;
    struct dirent *e;
    int            seen = 0, removed = 0;
    while ((e = readdir(d)) != NULL && seen < 8192) {
        seen++;
        size_t len = strlen(e->d_name);
        if (len <= 5 || strcmp(e->d_name + len - 5, ".part") != 0)
            continue;
        char path[MAX_PATH_LEN];
        snprintf(path, sizeof path, "%s/%s", g_downloads_dir, e->d_name);
        if (unlink(path) == 0)
            removed++;
    }
    closedir(d);
    LOG("[bookshelf] stale .part sweep removed=%d\n", removed);
}

/* Local path a book downloads to (matches the open-with launch path).
 * Prefers the provider's original filename (sanitized to a bare
 * basename) so the file is recognizable in the downloads folder;
 * falls back to <id>.<ext> when the server sent no filename. */
void
book_local_path(const Book *b, char *out, size_t cap)
{
    if (b->filename[0] != '\0' && strcmp(b->filename, ".") != 0 && strcmp(b->filename, "..") != 0) {
        char   sanitized[MAX_PATH_LEN];
        size_t n = 0;
        for (const char *p = b->filename; *p != '\0' && n + 1 < sizeof sanitized; p++) {
            char c = *p;
            if (c == '/')
                c = '_';
            if (c < 0x20 || c == 0x7f)
                continue;
            sanitized[n++] = c;
        }
        sanitized[n] = '\0';
        if (n > 0) {
            snprintf(out, cap, "%s/%s", g_downloads_dir, sanitized);
            return;
        }
    }
    if (b->ext[0])
        snprintf(out, cap, "%s/%s.%s", g_downloads_dir, b->id, b->ext);
    else
        snprintf(out, cap, "%s/%s", g_downloads_dir, b->id);
}

/* Path of an existing download: the book's stored local_path when the
 * file is still on disk there, else the current downloads folder.
 * Needed because the downloads folder can move (the default changed
 * from /mnt/ext1/system/bin to /mnt/ext1/Downloads, or the user picked
 * another folder in Settings) — books fetched before the move live at
 * their stored location and must stay openable without re-downloading.
 * New downloads always land at the current folder (book_local_path). */
void
book_existing_path(const Book *b, char *out, size_t cap)
{
    if (b->local_path[0] != '\0' && access(b->local_path, F_OK) == 0) {
        snprintf(out, cap, "%s", b->local_path);
        return;
    }
    book_local_path(b, out, cap);
}

/* Sync a book's downloaded flag by probing its on-device file, in the
 * store and in the caller's copy.  The stored location counts as
 * downloaded too — see book_existing_path. */
void
refresh_downloaded(Book *b)
{
    char path[MAX_PATH_LEN];
    book_existing_path(b, path, sizeof path);
    int dl = (access(path, F_OK) == 0);
    store_set_downloaded(b->id, dl, dl ? path : "");
    b->downloaded = dl;
    if (dl)
        snprintf(b->local_path, sizeof b->local_path, "%s", path);
}

/* Re-probe every book's on-device file and resync its downloaded flag
 * (bounded slices, one transaction).  Files can vanish or appear while
 * the app is not running (tests clear the downloads dir, the reader or
 * the user deletes files), so the flag must be reconciled at startup
 * before anything counts "undownloaded" books. */
void
refresh_downloaded_flags(void)
{
    /* A crashed fetch can leave a "<path>.part" fragment behind
     * (dl_fetch renames only on success); sweep them at startup. */
    sweep_stale_parts();

    char ids[64][MAX_ID_LEN];
    int  off = 0, got, changed = 0;
    store_begin();
    while ((got = store_next_ids(ids, 64, off)) > 0) {
        for (int i = 0; i < got; i++) {
            Book b;
            if (!store_get_book(ids[i], &b))
                continue;
            char path[MAX_PATH_LEN];
            book_local_path(&b, path, sizeof path);
            int dl = (access(path, F_OK) == 0);
            if (!dl && b.local_path[0] != '\0' && access(b.local_path, F_OK) == 0) {
                /* File still at its stored location although the
                 * downloads folder has moved; keep it downloaded and
                 * keep the stored path (see book_existing_path). */
                dl = 1;
                snprintf(path, sizeof path, "%s", b.local_path);
            }
            if (dl != b.downloaded) {
                store_set_downloaded(ids[i], dl, dl ? path : "");
                changed++;
            }
        }
        off += got;
        if (got < 64)
            break;
    }
    store_commit();
    LOG("[bookshelf] refresh_downloaded_flags: changed=%d\n", changed);
}

/* Find a download-queue entry by id (NULL if absent). */
DownloadItem *
find_download(const char *id)
{
    for (int i = 0; i < g_download_count; i++)
        if (strcmp(g_downloads[i].id, id) == 0)
            return &g_downloads[i];
    return NULL;
}

/* Drop every finished queue entry.  A manual (non-batch) download
 * starts a fresh tally, so stale finished rows from the last batch
 * must not inflate it or crowd the bounded queue out. */
static void
clear_finished_downloads(void)
{
    int w = 0;
    for (int i = 0; i < g_download_count; i++) {
        if (g_downloads[i].state == 2 || g_downloads[i].state == 3)
            continue;
        if (w != i)
            g_downloads[w] = g_downloads[i];
        w++;
    }
    g_download_count = w;
}

static void dl_start_next(void);
static void dl_job_done(BsJob *job);

/* Add a book to the download queue (no-op if already queued, in
 * flight, or done; a failed entry is dropped and retried when no
 * batch is active) and start its fetch (or the first queued fetch)
 * right away. */
void
enqueue_download(const Book *b)
{
    DownloadItem *d = find_download(b->id);
    if (d != NULL && (g_dl_batch_active || d->state != 3))
        return;
    if (!g_dl_batch_active) {
        /* Manual download: the retained tally of the last batch must
         * not mask this one, and its finished rows must not inflate
         * the fresh queue tally (or crowd it out entirely).  A failed
         * entry (state 3) falls through here so re-tapping a failed
         * book retries it: clear_finished_downloads() drops the stale
         * row and the book is enqueued fresh below.  Batch mode keeps
         * its own semantics — failed ids stay tracked and are skipped
         * by the batch drain. */
        g_dl_batch_total = 0;
        g_dl_batch_done = 0;
        g_dl_batch_failed = 0;
        clear_finished_downloads();
    }
    if (g_download_count >= MAX_DOWNLOADS)
        return;
    DownloadItem *n = &g_downloads[g_download_count++];
    snprintf(n->id, sizeof n->id, "%s", b->id);
    snprintf(n->title, sizeof n->title, "%s", b->title);
    n->state = 0;
    n->gen = ++g_dl_gen; /* new generation: stale in-flight jobs for
                            this id must not settle this entry */
    sync_set_active(1);
    /* Start the fetch right away (no-op when one is already in
     * flight; the in-flight job's done_cb advances the queue). */
    dl_start_next();
}

/* Drop the oldest finished queue entry to make room (batch mode keeps
 * the queue bounded regardless of library size).  Returns 1 when an
 * entry was dropped, 0 when the queue held nothing finished. */
static int
prune_finished_download(void)
{
    int best = -1;
    for (int i = 0; i < g_download_count; i++) {
        if (g_downloads[i].state == 2 || g_downloads[i].state == 3) {
            best = i;
            break;
        }
    }
    if (best < 0)
        return 0;
    for (int i = best; i + 1 < g_download_count; i++)
        g_downloads[i] = g_downloads[i + 1];
    g_download_count--;
    return 1;
}

/* ── async download worker ────────────────────────────────────────────
 * Each file fetch runs as a one-shot job on the shared background
 * worker (bs_worker.c) so the event loop stays responsive while a book
 * downloads — QuickDownload blocks for the whole transfer (up to the
 * 60 s timeout), which used to freeze the UI for the duration.  One
 * job is in flight at a time, matching the old single-worker drain;
 * the job fn only fetches the file and writes it to disk, and its
 * done_cb settles each queue item on the main thread and applies
 * store_set_downloaded().  The worker touches no UI and no store
 * state. */

typedef struct {
    char id[MAX_ID_LEN];
    char url[MAX_URL_LEN + 128];
    char path[MAX_PATH_LEN];
    unsigned int gen; /* generation token of the queue entry this job serves */
} DlJob;

static BsJob *g_dl_inflight; /* the one in-flight download job, main thread */

/* Worker: fetch one book's file to disk (blocking).  Writes to
 * "<path>.part", verifies the write, then renames into place, so a
 * crash, canceled job, or failed download never leaves a truncated
 * file at the final path (the .part is unlinked).  No UI, no store
 * access — the caller settles the store. */
static void
dl_fetch(BsJob *job)
{
    DlJob *a = job->arg;
    int    rsize = 0;
    char  *data = QuickDownload(a->url, &rsize, 60);
    int    ok = 0;
    if (data != NULL && rsize > 0) {
        if (__atomic_load_n(&job->cancel, __ATOMIC_ACQUIRE)) {
            LOG("[bookshelf] download_book_file CANCELED id=%s\n", a->id);
        } else {
            char tmp[MAX_PATH_LEN + 8]; /* room for ".part" suffix */
            snprintf(tmp, sizeof tmp, "%s.part", a->path);
            FILE *f = fopen(tmp, "wb");
            if (f != NULL) {
                size_t w = fwrite(data, 1, (size_t)rsize, f);
                int    werr = (w != (size_t)rsize);
                if (fclose(f) != 0)
                    werr = 1;
                if (!werr && rename(tmp, a->path) == 0) {
                    ok = 1;
                    LOG("[bookshelf] download_book_file OK id=%s path=%s bytes=%d\n",
                        a->id, a->path, rsize);
                } else {
                    LOG("[bookshelf] download_book_file write/rename FAILED "
                        "id=%s path=%s errno=%d\n",
                        a->id, a->path, errno);
                    unlink(tmp); /* never leave the .part behind */
                }
            } else {
                LOG("[bookshelf] download_book_file fopen FAILED id=%s path=%s errno=%d\n",
                    a->id, a->path, errno);
            }
        }
        free(data);
    } else {
        if (data != NULL)
            free(data);
        LOG("[bookshelf] download_book_file FAILED id=%s url=%s rsize=%d errno=%d\n",
            a->id, a->url, rsize, errno);
    }
    job->rc = ok ? 0 : -1;
    __atomic_store_n(&job->done, 1, __ATOMIC_RELEASE);
}

/* Launch the configured reader on an already-downloaded book.
 *
 * The standard reader (and the auto default) goes through OpenBook() —
 * the firmware's canonical book-open path.  OpenBook() routes the book
 * to monitor.app / reader_controller, which picks the reader for the
 * file type, registers the book with the task, and brings the reader to
 * the foreground.  NewTaskEx() on the reader binary does none of that:
 * it execs the app without a book-open request (the reader came up on
 * its home screen), it never makes the task visible, and it fails
 * silently when the resolved app does not exist on this firmware (the
 * server's open-with table names pdfviewer, which the Era image does
 * not ship — access() inside NewTaskEx then returns -1 and nothing
 * happens).
 *
 * Only an explicitly selected third-party reader (KOReader) is still
 * launched via NewTaskEx() — it is a standalone app that takes the book
 * path as its argument and has no OpenBook integration.  argv[0] must
 * be the program path: the task launcher passes the args array through
 * as-is, so with only the book path in the array the reader would
 * receive it as argv[0] and never see a book argument.  Flags 0x25
 * (TASK_HIDDEN|TASK_NOUPDATEONFOCUS|TASK_SINGLEINSTANCE|TASK_OUTOFSTACK)
 * match what reader_controller.app and the stock bookshelf pass to
 * NewTaskEx() for app launches. */
void
launch_reader(Book *b)
{
    char path[MAX_PATH_LEN];
    /* Open the file where it actually lives (stored location when the
     * downloads folder moved since the fetch), not just the current
     * folder's path. */
    book_existing_path(b, path, sizeof path);

    const char *reader_path = NULL;
    if (g_state.reader_pref > 0 && g_state.reader_pref <= g_reader_count)
        reader_path = g_readers[g_state.reader_pref - 1].path;
    if (reader_path != NULL && access(reader_path, X_OK) == 0 &&
        strcmp(reader_path, READER_STD_PATH) != 0) {
        const char *rbase = strrchr(reader_path, '/');
        rbase = rbase ? rbase + 1 : reader_path;
        char *args[3] = {(char *)reader_path, path, NULL};
        LOG("[bookshelf] launching reader app=%s path=%s reader_pref=%d\n",
            rbase,
            path,
            g_state.reader_pref);
        NewTaskEx(reader_path, args, rbase, b->title, NULL, 0x25, 0);
        return;
    }

    LOG("[bookshelf] launching reader via OpenBook path=%s reader_pref=%d\n",
        path,
        g_state.reader_pref);
    OpenBook(path, NULL, 1);
}

/* Press a book (single tap or context-menu Open): if the file is not
 * on device, show the download-progress popup, queue the download, and
 * auto-open the reader when the queue drains (see dl_job_done).
 * Already-downloaded books open immediately.  Persists the downloaded
 * flag so the next launch sees the file. */
void
book_press_action(Book *b)
{
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    int dl = (access(path, F_OK) == 0);
    if (!dl && b->local_path[0] != '\0' && access(b->local_path, F_OK) == 0) {
        /* The file lives at its stored location (downloads folder
         * moved since the fetch); it is downloaded and opens from
         * there — see book_existing_path. */
        dl = 1;
        snprintf(path, sizeof path, "%s", b->local_path);
    }
    if (dl != b->downloaded)
        store_set_downloaded(b->id, dl, dl ? path : "");
    b->downloaded = dl;
    if (!b->downloaded) {
        g_state.dl_popup = 1;
        g_state.dl_popup_auto_open = 1;
        snprintf(g_state.dl_popup_book_id, sizeof g_state.dl_popup_book_id, "%s", b->id);
        enqueue_download(b);
        redraw_shelf(); /* draws the popup on top */
        return;
    }
    launch_reader(b);
}
/* True when the current batch already attempted *id* and it failed.
 * Failed books keep their downloaded flag at 0, so without this guard
 * the next slice would re-enqueue them and the batch would loop over
 * the failing books forever. */
static int
batch_failed_id(const char *id)
{
    for (int i = 0; i < g_dl_batch_failed_count; i++)
        if (strcmp(g_dl_batch_failed_ids[i], id) == 0)
            return 1;
    return 0;
}

static void
batch_note_failed(const char *id)
{
    if (g_dl_batch_failed_count >=
        (int)(sizeof g_dl_batch_failed_ids / sizeof g_dl_batch_failed_ids[0]))
        return; /* set full: the drain treats the slice as exhausted */
    snprintf(g_dl_batch_failed_ids[g_dl_batch_failed_count++],
             sizeof g_dl_batch_failed_ids[0],
             "%s",
             id);
}

/* Enqueue the next bounded slice of undownloaded ids for the
 * download-all batch, skipping ids that already own a queue entry
 * (in flight, done, or failed) or that the batch already failed.  The
 * query is offset-free: ids whose file landed earlier shrink the
 * "downloaded=0" result set, so any OFFSET cursor would skip books on
 * later slices.  *got reports how many ids the store slice held so the
 * caller can tell "drained" from "full slice, more to come".  Returns
 * the number actually enqueued. */
static int
batch_enqueue_slice(int *got)
{
    char ids[64][MAX_ID_LEN];
    *got = store_next_undownloaded(ids, 64);
    int enq = 0;
    for (int i = 0; i < *got; i++) {
        if (find_download(ids[i]) != NULL)
            continue;
        if (batch_failed_id(ids[i]))
            continue;
        Book b;
        if (!store_get_book(ids[i], &b))
            continue;
        if (g_download_count >= MAX_DOWNLOADS) {
            prune_finished_download();
            if (g_download_count >= MAX_DOWNLOADS)
                break;
        }
        enqueue_download(&b);
        enq++;
    }
    return enq;
}

/* Start (or restart) the download-all batch.  The first bounded slice
 * is queued synchronously so the popup shows the whole batch right
 * away; each completed download job tops the queue up as items
 * finish.  The popup opens here (no auto-open — a batch never
 * launches a reader). */
void
download_all_start(void)
{
    g_dl_batch_active = 1;
    g_dl_batch_total = store_count_undownloaded();
    g_dl_batch_done = 0;
    g_dl_batch_failed_count = 0;
    int got = 0;
    batch_enqueue_slice(&got); /* starts the first fetch via enqueue */
    g_state.dl_popup = 1;
    g_state.dl_popup_auto_open = 0;
    redraw_shelf();
    LOG("[bookshelf] download-all queued=%d\n", g_dl_batch_total);
}

static void dl_advance(void);

/* Start the job for the first queued item (main thread).  One download
 * is in flight at a time; each job's done_cb advances the queue, so
 * starting the first item here kicks the drain. */
static void
dl_start_next(void)
{
    if (g_dl_inflight != NULL)
        return;
    DownloadItem *target = NULL;
    for (int i = 0; i < g_download_count; i++) {
        if (g_downloads[i].state == 0) {
            target = &g_downloads[i];
            break;
        }
    }
    if (target == NULL)
        return;
    target->state = 1;

    Book b;
    if (!store_get_book(target->id, &b)) {
        target->state = 3;
        return;
    }
    DlJob *a = calloc(1, sizeof *a);
    if (a == NULL) {
        target->state = 3;
        return;
    }
    char path[MAX_PATH_LEN];
    book_local_path(&b, path, sizeof path);
    snprintf(a->id, sizeof a->id, "%s", b.id);
    snprintf(a->url,
             sizeof a->url,
             "%s/api/v1/books/%s/file?access_token=%s",
             g_state.api_base,
             b.id,
             g_state.api_token);
    snprintf(a->path, sizeof a->path, "%s", path);
    a->gen = target->gen; /* the settle must match this exact generation */
    BsJob *j = bs_worker_submit(dl_fetch, dl_job_done, a);
    if (j == NULL) {
        target->state = 3;
        free(a);
        return;
    }
    g_dl_inflight = j;
}

/* No-op job that re-enters the drain on the main thread.  Reproduces
 * the old drain-timer poll for the batch slice-stall case, where every
 * id the store returned already owns a queue entry or already failed
 * (see dl_advance). */
static void
dl_kick_fn(BsJob *job)
{
    (void)job;
    __atomic_store_n(&job->done, 1, __ATOMIC_RELEASE);
}

static void
dl_kick_done(BsJob *job)
{
    (void)job;
    dl_advance();
}

/* Submit a kick job (main thread). */
static void
dl_kick(void)
{
    bs_worker_submit(dl_kick_fn, dl_kick_done, NULL);
}

/* 1 = no download fetch is actively running right now: either no job
 * is in flight, or the in-flight job's worker fn already finished (its
 * done flag is set) and only the main-thread settle (next worker tick,
 * <=100 ms later) is pending.  The file is already on disk in that
 * case, so the queue item counts as finished for dismiss checks — a
 * tap that lands in the settle window must close the popup, not be
 * swallowed. */
int
dl_fetch_idle(void)
{
    return g_dl_inflight == NULL ||
           __atomic_load_n(&g_dl_inflight->done, __ATOMIC_ACQUIRE);
}

/* 1 = a download job is in flight whose settle has not run yet (the
 * queue item is still marked in-flight even though the worker fn may
 * be done).  Used to keep the single-book auto-open flow intact: a
 * dismiss tap in the settle window is swallowed so dl_advance() still
 * launches the reader. */
int
dl_job_pending(void)
{
    return g_dl_inflight != NULL;
}

/* Settle the finished download and advance the queue (main thread).
 * Each job's file fetch runs on the worker thread; the done_cb marks
 * the item, applies the store update and batch tally, then starts the
 * next queued item or tops the batch up from the store; when the
 * queue is fully drained it finalises (auto-open the reader or show
 * the finished tally in the popup).  The event loop never blocks on
 * the network. */
static void
dl_job_done(BsJob *job)
{
    DlJob *a = job->arg;
    int    ok = job->rc == 0;

    /* Settle the finished queue item.  The entry must match BOTH the
     * id and the generation token: after cancel_downloads the queue
     * is gone, so find_download() misses and the completion is
     * absorbed harmlessly — no store update, no batch tally, no popup
     * redraw, and the canceled job's .part was never renamed.  And a
     * book canceled and re-enqueued while its old job was still in
     * flight must not have the new entry settled by the stale job. */
    DownloadItem *d = find_download(a->id);
    if (d != NULL && d->gen == a->gen) {
        d->state = ok ? 2 : 3;
        if (ok)
            store_set_downloaded(d->id, 1, a->path);
        if (g_dl_batch_active) {
            /* Successes and failures both settle a batch slot; the
             * bar counts failures separately so it reaches full
             * width even if some books fail.  A failure is recorded
             * so the batch never re-enqueues the book. */
            if (ok)
                g_dl_batch_done++;
            else {
                g_dl_batch_failed++;
                batch_note_failed(d->id);
            }
        }
        /* The popup refresh is deferred to the spawn path in
         * dl_advance, which repaints the sheet once with the settled
         * tally AND the next item's title.  Without a popup, just
         * refresh the top-bar badge. */
        if (!g_state.dl_popup)
            draw_top_bar();
        sync_set_active(downloads_pending() > 0 || g_dl_batch_active);
    } else if (d != NULL) {
        /* Stale job: the queue holds a newer generation of this id
         * (canceled and re-enqueued).  Leave the fresh entry alone —
         * settling it would mis-mark the new download failed. */
        LOG("[bookshelf] stale download job settle dropped id=%s gen=%u entry_gen=%u\n",
            a->id,
            a->gen,
            d->gen);
    }
    if (g_dl_inflight == job)
        g_dl_inflight = NULL;
    free(a);

    dl_advance();
}

/* Advance the queue (main thread): start the next queued item, or top
 * the batch up / finalise when the queue is drained.  Started by
 * enqueue_download and by every completed download job; a no-op while
 * a job is in flight. */
static void
dl_advance(void)
{
    if (g_dl_inflight != NULL)
        return;

    DownloadItem *target = NULL;
    for (int i = 0; i < g_download_count; i++) {
        if (g_downloads[i].state == 0) {
            target = &g_downloads[i];
            break;
        }
    }
    if (target == NULL) {
        if (g_dl_batch_active) {
            /* Batch mode: enqueue the next slice of undownloaded ids. */
            int got = 0, enq = batch_enqueue_slice(&got);
            int settled = g_dl_batch_done + g_dl_batch_failed;
            if (enq > 0 || (got == 64 && settled < g_dl_batch_total)) {
                if (enq == 0) {
                    /* Full slice, nothing enqueued: every id already
                     * owns a queue entry or already failed.  Prune one
                     * finished entry so the queue makes room and the
                     * next pass can enqueue, instead of looping on the
                     * same slice forever. */
                    if (prune_finished_download()) {
                        dl_kick();
                        return;
                    }
                    /* Nothing finished left to prune: the whole slice
                     * is made of ids that are already failed (or
                     * unreadable), so no retry can ever make progress
                     * — finalize the batch instead of kicking on the
                     * same slice forever. */
                    LOG("[bookshelf] download-all batch stalled, finalizing\n");
                } else {
                    dl_start_next();
                    if (g_state.dl_popup)
                        refresh_dl_popup();
                    else
                        draw_top_bar();
                    return;
                }
            }
            /* Every batch book has settled (done + failed == total),
             * or the slice is exhausted with nothing left to enqueue:
             * end the batch.  Keep the final tally on screen — zeroing
             * the counters here made the bar fall back to queue-derived
             * counts, and the pruned queue only holds the last slice
             * (<=64).  download_all_start() resets the counters for the
             * next batch; a manual enqueue_download() clears them. */
            g_dl_batch_active = 0;
            LOG("[bookshelf] download-all batch complete\n");
        }
        sync_set_active(0);
        /* Queue drained.  A single-book press auto-opens the reader
         * once its file landed; any other popup stays up showing the
         * finished tally until the user taps it closed. */
        if (g_state.dl_popup) {
            if (g_state.dl_popup_auto_open) {
                Book b;
                if (store_get_book(g_state.dl_popup_book_id, &b) && b.downloaded) {
                    g_state.dl_popup = 0;
                    g_state.dl_popup_auto_open = 0;
                    redraw_shelf();
                    LOG("[bookshelf] popup drain complete, launching reader id=%s\n", b.id);
                    launch_reader(&b);
                    return;
                }
            }
            redraw_shelf(); /* popup shows the finished/failed state */
        }
        return;
    }

    /* Start the next queued item. */
    dl_start_next();
    /* One popup refresh per item: the sheet now shows the settled
     * tally and the new current-item title.  The dimmed shelf behind
     * it never changed, so a sheet-sized partial suffices — a
     * content-area refresh per finished download is what made
     * download-all flicker. */
    if (g_state.dl_popup)
        refresh_dl_popup();
    else
        draw_top_bar(); /* refresh the pending-count badge in top bar */
}

/* Abort every open download: drop the whole queue and end the batch
 * (download-all, series, or single-book).  The one in-flight file fetch
 * cannot be interrupted — QuickDownload blocks until the transfer or
 * its timeout ends — so its job is told to cancel (it will not rename
 * its .part file into place) and is left to finish in the background.
 * Its completion is absorbed harmlessly: dl_job_done() only settles
 * items still present in the queue, and the queue is gone here, so the
 * settle path skips it entirely (no store update, no batch tally, no
 * popup redraw).  Any file that landed before the cancel stays on disk
 * and is reconciled by refresh_downloaded_flags() on the next launch. */
void
cancel_downloads(void)
{
    LOG("[bookshelf] cancel_downloads batch=%d in_flight=%p\n",
        g_dl_batch_active, (void *)g_dl_inflight);
    g_dl_batch_active = 0;
    g_dl_batch_total = 0;
    g_dl_batch_done = 0;
    g_dl_batch_failed = 0;
    g_dl_batch_failed_count = 0;
    g_download_count = 0;
    if (g_dl_inflight != NULL)
        bs_worker_cancel(g_dl_inflight);
    g_state.dl_popup = 0;
    g_state.dl_popup_auto_open = 0;
    sync_set_active(0);
    redraw_shelf();
}

/* Queue every member of a series (by series_id), in bounded slices, and
 * open the download-progress popup so the drain is visible. */
void
download_series(const char *series_id)
{
    char ids[64][MAX_ID_LEN];
    int  n = 0, off = 0, got;
    while ((got = store_series_ids(series_id, ids, 64, off)) > 0) {
        for (int i = 0; i < got; i++) {
            Book b;
            if (store_get_book(ids[i], &b)) {
                enqueue_download(&b);
                n++;
            }
        }
        off += got;
        if (got < 64)
            break;
    }
    g_state.dl_popup = 1;
    g_state.dl_popup_auto_open = 0;
    LOG("[bookshelf] download_series %s queued=%d\n", series_id, n);
}

/* Delete the local files of every member of a series. */
void
delete_series(const char *series_id)
{
    char ids[64][MAX_ID_LEN];
    int  n = 0, off = 0, got;
    while ((got = store_series_ids(series_id, ids, 64, off)) > 0) {
        for (int i = 0; i < got; i++) {
            store_delete_book_file(ids[i]);
            n++;
        }
        off += got;
        if (got < 64)
            break;
    }
    LOG("[bookshelf] delete_series %s removed=%d\n", series_id, n);
}

/* Context menu geometry: a centred modal sheet.  Returns the sheet rect
 * and the y of the first item row. */
void
context_geom(int *px, int *py, int *pw, int *ph, int n_items)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    *pw = w * 3 / 4;
    *ph = CTX_TITLE_H + n_items * CTX_ITEM_H + CTX_PAD;
    *px = (w - *pw) / 2;
    *py = (h - *ph) / 2;
}

int
context_item_count(void)
{
    /* A book offers Open + Download + Delete; a series card offers
     * Download all + Delete series. */
    return g_state.ctx_is_series ? 2 : 3;
}

/* Draw the long-press context menu over a dimmed shelf. */
void
draw_context_menu(void)
{
    int w = ScreenWidth();
    /* Dim mask over the whole app content area (panel band stays). */
    for (int yy = 0; yy < content_bottom(); yy += 2)
        DrawLine(0, yy, w, yy, LGRAY);

    int n = context_item_count();
    int px, py, pw, ph;
    context_geom(&px, &py, &pw, &ph, n);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    /* Title: series name or book title, resolved from the store. */
    static char title_buf[MAX_TITLE_LEN];
    const char *title;
    if (g_state.ctx_is_series) {
        store_series_name(g_state.ctx_series_id, title_buf, sizeof title_buf);
        title = title_buf[0] != '\0' ? title_buf : "Series";
    } else {
        Book tmp;
        title_buf[0] = '\0';
        if (store_get_book(g_state.ctx_book_id, &tmp))
            snprintf(title_buf, sizeof title_buf, "%s", tmp.title);
        title = title_buf;
    }
    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        char trunc[MAX_TITLE_LEN];
        snprintf(trunc, sizeof trunc, "%s", title);
        while (StringWidth(trunc) > pw - 2 * CTX_PAD && strlen(trunc) > 4)
            utf8_drop_last_char(trunc); /* never split a multibyte char */
        DrawString(px + CTX_PAD, py + (CTX_TITLE_H - 28) / 2 - 2, trunc);
        CloseFont(tf);
    }
    DrawLine(px + CTX_PAD, py + CTX_TITLE_H - 1, px + pw - CTX_PAD, py + CTX_TITLE_H - 1, LGRAY);

    const char *labels[3];
    if (g_state.ctx_is_series) {
        labels[0] = i18n("ctx.download_all");
        labels[1] = i18n("ctx.delete_series");
    } else {
        labels[0] = i18n("ctx.open");
        labels[1] = i18n("ctx.download");
        labels[2] = i18n("ctx.delete");
    }
    ifont *f = OpenFont(DEFAULTFONTB, 30, 0);
    if (f == NULL)
        return;
    SetFont(f, BLACK);
    for (int i = 0; i < n; i++) {
        int iy = py + CTX_TITLE_H + i * CTX_ITEM_H;
        DrawString(px + CTX_PAD, iy + (CTX_ITEM_H - 30) / 2 - 2, labels[i]);
        if (i + 1 < n)
            DrawLine(
                px + CTX_PAD, iy + CTX_ITEM_H - 1, px + pw - CTX_PAD, iy + CTX_ITEM_H - 1, LGRAY);
    }
    CloseFont(f);
}

void
close_context(void)
{
    g_state.overlay = OV_NONE;
    redraw_shelf();
}

/* Open the context menu for a view tile (series card or book). */
void
open_context_for_tile(int vi)
{
    TileRow tr;
    if (!view_fetch_row(vi, &tr))
        return;
    g_state.overlay = OV_CTX;
    g_state.ctx_is_series = tr.is_series;
    if (tr.is_series) {
        snprintf(g_state.ctx_series_id, sizeof g_state.ctx_series_id, "%s", tr.series_id);
        g_state.ctx_book_id[0] = '\0';
    } else {
        snprintf(g_state.ctx_book_id, sizeof g_state.ctx_book_id, "%s", tr.book.id);
        g_state.ctx_series_id[0] = '\0';
    }
    draw_context_menu();
    flush_content();
    LOG("[bookshelf] context menu open series=%d vi=%d\n", tr.is_series, vi);
}

/* Long-press timer fired with the finger still down: open the menu. */
void
longpress_tick(void *ctx)
{
    (void)ctx;
    if (!g_lp_armed || g_lp_vi < 0)
        return;
    g_lp_armed = 0;
    int vi = g_lp_vi;
    g_lp_vi = -1;
    g_ctx_suppress_up = 1;
    open_context_for_tile(vi);
}

/* Handle a tap while the context menu is open. */
void
on_tap_context(int x, int y)
{
    int n = context_item_count();
    int px, py, pw, ph;
    context_geom(&px, &py, &pw, &ph, n);
    if (x < px || x >= px + pw || y < py + CTX_TITLE_H || y >= py + ph) {
        close_context();
        return;
    }
    int  item = (y - (py + CTX_TITLE_H)) / CTX_ITEM_H;
    int  is_series = g_state.ctx_is_series;
    char series_id[MAX_ID_LEN];
    snprintf(series_id, sizeof series_id, "%s", g_state.ctx_series_id);
    g_state.overlay = OV_NONE;

    if (is_series) {
        if (item == 0)
            download_series(series_id);
        else if (item == 1)
            delete_series(series_id);
    } else {
        Book b;
        if (store_get_book(g_state.ctx_book_id, &b)) {
            if (item == 0) {
                /* Open works exactly like a single tap: download if
                 * needed (with the progress popup), then launch. */
                book_press_action(&b);
            } else if (item == 1) {
                g_state.dl_popup = 1;
                g_state.dl_popup_auto_open = 0;
                enqueue_download(&b);
            } else if (item == 2) {
                store_delete_book_file(b.id);
            }
        }
    }
    redraw_shelf();
}
