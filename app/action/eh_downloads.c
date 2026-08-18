/* eh_downloads.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_downloads.h"
#include "eh_model.h"
#include "eh_store.h"
#include "eh_ui.h"
#include "eh_worker.h"

#include <dirent.h>

/* ── downloads, delete, context menu, long-press ───────────────────── */

/* Generation token for download-queue entries.  Bumped at every
 * enqueue and copied into the fetch job, so a job settles only the
 * entry whose id AND gen match — a canceled job that outlives its
 * queue entry can never mark the re-enqueued book failed (see
 * dl_job_done). */
static unsigned int g_dl_gen = 0;

/* Unlink stale "<file>.part" fragments left in the downloads dir by a
 * crash mid-fetch (dl_fetch writes the .part, then renames on success;
 * on any other exit the fragment would otherwise stay forever).
 * Bounded single pass over the directory; errors are ignored — the
 * worst case is a fragment surviving until the next startup. */
static void
sweep_stale_parts(void)
{
    DIR *d = opendir(eh_g_downloads_dir);
    if (d == NULL)
        return;
    struct dirent *e;
    int            seen = 0, removed = 0;
    while ((e = readdir(d)) != NULL && seen < 8192) {
        seen++;
        size_t len = strlen(e->d_name);
        if (len <= 5 || strcmp(e->d_name + len - 5, ".part") != 0)
            continue;
        char path[EH_MAX_PATH_LEN];
        snprintf(path, sizeof path, "%s/%s", eh_g_downloads_dir, e->d_name);
        if (unlink(path) == 0)
            removed++;
    }
    closedir(d);
    eh_LOG("[bookshelf] stale .part sweep removed=%d\n", removed);
}

/* Local path a book downloads to (matches the open-with launch path).
 * Prefers the provider's original filename (sanitized to a bare
 * basename) so the file is recognizable in the downloads folder;
 * falls back to <id>.<ext> when the server sent no filename. */
void
eh_book_local_path(const BsBook *b, char *out, size_t cap)
{
    if (b->filename[0] != '\0' && strcmp(b->filename, ".") != 0 && strcmp(b->filename, "..") != 0) {
        char   sanitized[EH_MAX_PATH_LEN];
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
            snprintf(out, cap, "%s/%s", eh_g_downloads_dir, sanitized);
            return;
        }
    }
    if (b->ext[0])
        snprintf(out, cap, "%s/%s.%s", eh_g_downloads_dir, b->id, b->ext);
    else
        snprintf(out, cap, "%s/%s", eh_g_downloads_dir, b->id);
}

/* Path of an existing download: the book's stored local_path when the
 * file is still on disk there, else the current downloads folder.
 * Needed because the downloads folder can move (the default changed
 * from /mnt/ext1/system/bin to /mnt/ext1/Downloads, or the user picked
 * another folder in Settings) — books fetched before the move live at
 * their stored location and must stay openable without re-downloading.
 * New downloads always land at the current folder (book_local_path). */
void
eh_book_existing_path(const BsBook *b, char *out, size_t cap)
{
    if (b->local_path[0] != '\0' && access(b->local_path, F_OK) == 0) {
        snprintf(out, cap, "%s", b->local_path);
        return;
    }
    eh_book_local_path(b, out, cap);
}

/* qsort/bsearch comparator for the downloads-dir listing. */
static int
dl_name_cmp(const void *a, const void *b)
{
    return strcmp(*(const char *const *)a, *(const char *const *)b);
}

/* Re-probe every book's on-device file and resync its downloaded flag.
 * Files can vanish or appear while the app is not running (tests clear
 * the downloads dir, the reader or the user deletes files), so the flag
 * must be reconciled at startup before anything counts "undownloaded"
 * books.
 *
 * The probe answers "does <downloads dir>/<sanitized filename> exist"
 * (book_local_path is a flat name in g_downloads_dir), so instead of
 * one access() per book — ~1ms of flash each, a 2-3 minute boot stall
 * at 100k books — the dir is listed ONCE and membership is answered
 * from the sorted listing.  The per-book stored-path fallback (a moved
 * downloads folder) still access()es, but only for books whose file is
 * not in the current dir.
 *
 * The scan is sliced: it pages the whole books b-tree
 * (store_next_dl_probes), which would otherwise stall the first frame
 * for tens of seconds at 100k books.  eh_main runs it in bounded
 * slices across event-loop frames via the "bootslice" weak timer
 * (refresh_downloaded_flags_boot_start / _boot_step); the synchronous
 * refresh_downloaded_flags() drives the same scan to completion for
 * callers that need it inline. */
#define EH_DL_FLAG_PAGES_PER_TICK 8 /* probe pages (64 books each) per slice */

typedef struct {
    char **names;      /* sorted downloads-dir listing, or NULL */
    int    n_names;
    long long rowid;   /* keyset cursor into books (0 = start) */
    int    changed;    /* flags flipped so far */
    /* The probe array lives in the heap-allocated scan: 64 x
     * DownloadProbe is ~32KB, and the device's task stack overflows
     * with it on the frame (boot crashed in an endless respawn loop
     * on hardware while the emulator's bigger stack stayed green). */
    BsDownloadProbe probes[64];
} BsDlFlagScan;

static BsDlFlagScan *g_dl_flag_scan = NULL;

/* Arm the resumable scan (idempotent): sweep stale .part fragments and
 * snapshot the downloads dir ONCE.  The probe answers membership from
 * this sorted listing, so the per-book test is a bsearch, not an
 * access() per book. */
static void
dl_flag_arm(void)
{
    if (g_dl_flag_scan != NULL)
        return;
    sweep_stale_parts();
    BsDlFlagScan *s = calloc(1, sizeof *s);
    if (s == NULL) {
        eh_LOG("[bookshelf] refresh_downloaded_flags: scan alloc failed\n");
        return; /* stale flags are better than a crash */
    }
    DIR *d = opendir(eh_g_downloads_dir);
    if (d != NULL) {
        struct dirent *e;
        int cap = 0;
        while ((e = readdir(d)) != NULL) {
            if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0)
                continue;
            if (s->n_names == cap) {
                int nc = cap ? cap * 2 : 256;
                char **nn = realloc(s->names, sizeof *nn * (size_t)nc);
                if (nn == NULL)
                    break; /* keep what we have; membership still exact */
                s->names = nn;
                cap = nc;
            }
            char *dup = strdup(e->d_name);
            if (dup == NULL)
                break;
            s->names[s->n_names++] = dup;
        }
        closedir(d);
        if (s->n_names > 1)
            qsort(s->names, (size_t)s->n_names, sizeof *s->names, dl_name_cmp);
    }
    g_dl_flag_scan = s;
}

/* Free the scan state and log the tally. */
static void
dl_flag_finish(void)
{
    BsDlFlagScan *s = g_dl_flag_scan;
    if (s == NULL)
        return;
    int changed = s->changed;
    for (int i = 0; i < s->n_names; i++)
        free(s->names[i]);
    free(s->names);
    free(s);
    g_dl_flag_scan = NULL;
    eh_LOG("[bookshelf] refresh_downloaded_flags: changed=%d\n", changed);
}

/* Arm the boot flag scan (called by eh_main's bootslice timer). */
void
eh_refresh_downloaded_flags_boot_start(void)
{
    dl_flag_arm();
}

/* Re-probe one bounded slice of books (DL_FLAG_PAGES_PER_TICK paged
 * queries, each in its own transaction) and publish any flag changes.
 * Returns 1 when the scan is finished (state freed and logged), 0 to
 * run again.  Per-slice transactions keep a long-lived transaction
 * from being held open across event-loop frames (eh_main's initsync
 * timer may write the store mid-scan). */
int
eh_refresh_downloaded_flags_boot_step(void)
{
    dl_flag_arm(); /* auto-arm on the first step */
    BsDlFlagScan *s = g_dl_flag_scan;
    if (s == NULL)
        return 1; /* couldn't arm: treat as done */
    for (int page = 0; page < EH_DL_FLAG_PAGES_PER_TICK; page++) {
        int got = eh_store_next_dl_probes(s->probes, 64, &s->rowid);
        if (got <= 0) {
            dl_flag_finish();
            return 1;
        }
        eh_store_begin();
        for (int i = 0; i < got; i++) {
            BsDownloadProbe *p = &s->probes[i];
            BsBook b;
            memset(&b, 0, sizeof b);
            snprintf(b.id, sizeof b.id, "%s", p->id);
            snprintf(b.filename, sizeof b.filename, "%s", p->filename);
            snprintf(b.ext, sizeof b.ext, "%s", p->ext);
            char path[EH_MAX_PATH_LEN];
            eh_book_local_path(&b, path, sizeof path);
            const char *base = strrchr(path, '/');
            base = base != NULL ? base + 1 : path;
            /* bsearch's key is a POINTER TO the key value — for a
             * char* array that is &base, not base (the comparator
             * dereferences it; passing base read the first four bytes
             * of the filename as a pointer and SIGSEGV'd on any
             * non-empty downloads dir). */
            int dl = s->names != NULL &&
                     bsearch(&base, s->names, (size_t)s->n_names,
                             sizeof *s->names, dl_name_cmp) != NULL;
            if (!dl && p->local_path[0] != '\0' &&
                access(p->local_path, F_OK) == 0) {
                /* File still at its stored location although the
                 * downloads folder has moved; keep it downloaded and
                 * keep the stored path (see book_existing_path). */
                dl = 1;
                snprintf(path, sizeof path, "%s", p->local_path);
            }
            if (dl != p->downloaded) {
                eh_store_set_downloaded(p->id, dl, dl ? path : "");
                s->changed++;
            }
        }
        if (eh_store_commit() != 0) {
            /* COMMIT failed and the store rolled the page back, so the
             * flag changes were not persisted; do not report the scan
             * finished — the next bootslice tick retries the probe. */
            return 0;
        }
        if (got < 64) {
            dl_flag_finish();
            return 1;
        }
    }
    return 0;
}

/* Re-probe every book's on-device file and resync its downloaded flag,
 * synchronously to completion.  The boot path uses the sliced
 * refresh_downloaded_flags_boot_start/_boot_step pair instead (see
 * eh_main's bootslice timer); this runs the same scan inline for
 * callers that need it done before returning. */
void
eh_refresh_downloaded_flags(void)
{
    dl_flag_arm();
    while (!eh_refresh_downloaded_flags_boot_step())
        ;
}

/* Find a download-queue entry by id (NULL if absent). */
BsDownloadItem *
eh_find_download(const char *id)
{
    for (int i = 0; i < eh_g_download_count; i++)
        if (strcmp(eh_g_downloads[i].id, id) == 0)
            return &eh_g_downloads[i];
    return NULL;
}

/* Drop every finished queue entry.  A manual (non-batch) download
 * starts a fresh tally, so stale finished rows from the last batch
 * must not inflate it or crowd the bounded queue out. */
static void
clear_finished_downloads(void)
{
    int w = 0;
    for (int i = 0; i < eh_g_download_count; i++) {
        if (eh_g_downloads[i].state == 2 || eh_g_downloads[i].state == 3)
            continue;
        if (w != i)
            eh_g_downloads[w] = eh_g_downloads[i];
        w++;
    }
    eh_g_download_count = w;
}

static void dl_start_next(void);
static void dl_job_done(BsJob *job);
static void dl_kick(void);

/* Add a book to the download queue (no-op if already queued, in
 * flight, or done; a failed entry is dropped and retried when no
 * batch is active) and start its fetch (or the first queued fetch)
 * right away. */
void
eh_enqueue_download(const BsBook *b)
{
    BsDownloadItem *d = eh_find_download(b->id);
    if (d != NULL && (eh_g_dl_batch_active || d->state != 3))
        return;
    if (!eh_g_dl_batch_active) {
        /* Manual download: the retained tally of the last batch must
         * not mask this one, and its finished rows must not inflate
         * the fresh queue tally (or crowd it out entirely).  A failed
         * entry (state 3) falls through here so re-tapping a failed
         * book retries it: clear_finished_downloads() drops the stale
         * row and the book is enqueued fresh below.  Batch mode keeps
         * its own semantics — failed ids stay tracked and are skipped
         * by the batch drain. */
        eh_g_dl_batch_total = 0;
        eh_g_dl_batch_done = 0;
        eh_g_dl_batch_failed = 0;
        clear_finished_downloads();
    }
    if (eh_g_download_count >= EH_MAX_DOWNLOADS)
        return;
    BsDownloadItem *n = &eh_g_downloads[eh_g_download_count++];
    snprintf(n->id, sizeof n->id, "%s", b->id);
    snprintf(n->title, sizeof n->title, "%s", b->title);
    n->state = 0;
    n->gen = ++g_dl_gen; /* new generation: stale in-flight jobs for
                            this id must not settle this entry */
    eh_sync_set_active(1);
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
    for (int i = 0; i < eh_g_download_count; i++) {
        if (eh_g_downloads[i].state == 2 || eh_g_downloads[i].state == 3) {
            best = i;
            break;
        }
    }
    if (best < 0)
        return 0;
    for (int i = best; i + 1 < eh_g_download_count; i++)
        eh_g_downloads[i] = eh_g_downloads[i + 1];
    eh_g_download_count--;
    return 1;
}

/* ── async download worker ────────────────────────────────────────────
 * Each file fetch runs as a one-shot job on the shared background
 * worker (eh_worker.c) so the event loop stays responsive while a book
 * downloads — QuickDownload blocks for the whole transfer (up to the
 * 60 s timeout), which used to freeze the UI for the duration.  One
 * job is in flight at a time, matching the old single-worker drain;
 * the job fn only fetches the file and writes it to disk, and its
 * done_cb settles each queue item on the main thread and applies
 * store_set_downloaded().  The worker touches no UI and no store
 * state. */

typedef struct {
    char id[EH_MAX_ID_LEN];
    char url[EH_MAX_URL_LEN + 128];
    char path[EH_MAX_PATH_LEN];
    unsigned int gen; /* generation token of the queue entry this job serves */
} BsDlJob;

static BsJob *g_dl_inflight; /* the one in-flight download job, main thread */

/* Worker: fetch one book's file to disk (blocking).  Writes to
 * "<path>.part", verifies the write, then renames into place, so a
 * crash, canceled job, or failed download never leaves a truncated
 * file at the final path (the .part is unlinked).  No UI, no store
 * access — the caller settles the store. */
static void
dl_fetch(BsJob *job)
{
    BsDlJob *a = job->arg;
    int    rsize = 0;
    char  *data = QuickDownload(a->url, &rsize, 60);
    int    ok = 0;
    if (data != NULL && rsize > 0) {
        if (atomic_load_explicit(&job->cancel, memory_order_acquire)) {
            eh_LOG("[bookshelf] download_book_file CANCELED id=%s\n", a->id);
        } else {
            char tmp[EH_MAX_PATH_LEN + 8]; /* room for ".part" suffix */
            snprintf(tmp, sizeof tmp, "%s.part", a->path);
            FILE *f = fopen(tmp, "wb");
            if (f != NULL) {
                size_t w = fwrite(data, 1, (size_t)rsize, f);
                int    werr = (w != (size_t)rsize);
                if (fclose(f) != 0)
                    werr = 1;
                if (!werr && rename(tmp, a->path) == 0) {
                    ok = 1;
                    eh_LOG("[bookshelf] download_book_file OK id=%s path=%s bytes=%d\n",
                        a->id, a->path, rsize);
                } else {
                    eh_LOG("[bookshelf] download_book_file write/rename FAILED "
                        "id=%s path=%s errno=%d\n",
                        a->id, a->path, errno);
                    unlink(tmp); /* never leave the .part behind */
                }
            } else {
                eh_LOG("[bookshelf] download_book_file fopen FAILED id=%s path=%s errno=%d\n",
                    a->id, a->path, errno);
            }
        }
        free(data);
    } else {
        if (data != NULL)
            free(data);
        eh_LOG("[bookshelf] download_book_file FAILED id=%s url=%s rsize=%d errno=%d\n",
            a->id, a->url, rsize, errno);
    }
    job->rc = ok ? 0 : -1;
    atomic_store_explicit(&job->done, 1, memory_order_release);
}

/* Launch the configured reader on an already-downloaded book.
 *
 * The launch mechanics are platform policy (how a book-open reaches the
 * reader, which task flags, whether a third-party reader is exec'd):
 * the PocketBook backend picks OpenBook() vs NewTaskEx() behind the
 * eh_plat_launch_reader seam; a future platform does its own.  The
 * neutral code only resolves the chosen reader path and owns the
 * hourglass-during-launch UX. */
void
eh_launch_reader(BsBook *b)
{
    char path[EH_MAX_PATH_LEN];
    /* Open the file where it actually lives (stored location when the
     * downloads folder moved since the fetch), not just the current
     * folder's path. */
    eh_book_existing_path(b, path, sizeof path);

    /* Same hourglass the launcher shows: the reader draws over it once
     * it becomes the foreground task, so a slow reader start reads as
     * work-in-progress instead of a dead tap.  On launch failure no
     * reader will ever draw over it, so hide it and repaint the shelf. */
    eh_show_hourglass();

    const char *reader_path = NULL;
    if (eh_g_state.reader_pref > 0 && eh_g_state.reader_pref <= eh_g_reader_count)
        reader_path = eh_g_readers[eh_g_state.reader_pref - 1].path;

    if (eh_plat_launch_reader(path, reader_path, b->title) != 0) {
        HideHourglass();
        eh_redraw_shelf();
    }
}

/* Press a book (single tap or context-menu Open): if the file is not
 * on device, show the download-progress popup, queue the download, and
 * auto-open the reader when the queue drains (see dl_job_done).
 * Already-downloaded books open immediately.  Persists the downloaded
 * flag so the next launch sees the file. */
void
eh_book_press_action(BsBook *b)
{
    char path[EH_MAX_PATH_LEN];
    eh_book_local_path(b, path, sizeof path);
    int dl = (access(path, F_OK) == 0);
    if (!dl && b->local_path[0] != '\0' && access(b->local_path, F_OK) == 0) {
        /* The file lives at its stored location (downloads folder
         * moved since the fetch); it is downloaded and opens from
         * there — see book_existing_path. */
        dl = 1;
        snprintf(path, sizeof path, "%s", b->local_path);
    }
    if (dl != b->downloaded)
        eh_store_set_downloaded(b->id, dl, dl ? path : "");
    b->downloaded = dl;
    if (!b->downloaded) {
        eh_g_state.dl_popup = 1;
        eh_g_state.dl_popup_auto_open = 1;
        snprintf(eh_g_state.dl_popup_book_id, sizeof eh_g_state.dl_popup_book_id, "%s", b->id);
        eh_enqueue_download(b);
        eh_redraw_shelf(); /* draws the popup on top */
        return;
    }
    eh_launch_reader(b);
}
/* Failed-id set for the download-all batch: every id the batch already
 * attempted and failed, so the next slice never re-enqueues it (its
 * downloaded flag stays 0, so without this guard the batch would loop
 * over the failing books forever).  Heap-allocated and grown by
 * doubling: the old fixed 256-entry array silently dropped ids once
 * full, and the batch then re-enqueued the failing books forever.
 * Freed at the start of the next batch (download_all_start). */
static char *g_dl_batch_failed_ids; /* NULL until the first failure */
static int   g_dl_batch_failed_count = 0;
static int   g_dl_batch_failed_cap = 0;

/* Comparator for the sorted failed-id set: elements are fixed
 * MAX_ID_LEN byte strings stored contiguously, so a pointer to an
 * element is just a char*. */
static int
batch_failed_cmp(const void *a, const void *b)
{
    return strcmp((const char *)a, (const char *)b);
}

/* True when the current batch already attempted *id* and it failed.
 * Failed books keep their downloaded flag at 0, so without this guard
 * the next slice would re-enqueue them and the batch would loop over
 * the failing books forever.  The set is kept sorted ascending (see
 * batch_note_failed), so the probe is a binary search: O(log n) rather
 * than the O(n) linear scan that dominated a flaky mass download. */
static int
batch_failed_id(const char *id)
{
    return bsearch(id, g_dl_batch_failed_ids,
                   (size_t)g_dl_batch_failed_count, EH_MAX_ID_LEN,
                   batch_failed_cmp) != NULL;
}

static void
batch_note_failed(const char *id)
{
    if (g_dl_batch_failed_count >= g_dl_batch_failed_cap) {
        int    newcap = g_dl_batch_failed_cap == 0 ? 64 : g_dl_batch_failed_cap * 2;
        char  *nids = realloc(g_dl_batch_failed_ids, (size_t)newcap * EH_MAX_ID_LEN);
        if (nids == NULL)
            return; /* keep the old set; this id just is not remembered */
        g_dl_batch_failed_ids = nids;
        g_dl_batch_failed_cap = newcap;
    }
    /* Insert at the sorted position so the set stays ascending and
     * batch_failed_id can stay a binary search.  Each id is noted at
     * most once per batch (batch_failed_id skips already-failed books
     * before they are ever re-enqueued), so no dedup is needed here.
     * Ids are fixed MAX_ID_LEN byte strings, so shifting the tail is a
     * cheap memmove. */
    int lo = 0, hi = g_dl_batch_failed_count; /* first slot >= id */
    while (lo < hi) {
        int mid = (lo + hi) / 2;
        if (strcmp(g_dl_batch_failed_ids + (size_t)mid * EH_MAX_ID_LEN, id) < 0)
            lo = mid + 1;
        else
            hi = mid;
    }
    char *dst = g_dl_batch_failed_ids + (size_t)lo * EH_MAX_ID_LEN;
    size_t tail = (size_t)(g_dl_batch_failed_count - lo) * EH_MAX_ID_LEN;
    if (tail > 0)
        memmove(dst + EH_MAX_ID_LEN, dst, tail);
    snprintf(dst, EH_MAX_ID_LEN, "%s", id);
    g_dl_batch_failed_count++;
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
    char ids[64][EH_MAX_ID_LEN];
    *got = eh_store_next_undownloaded(ids, 64);
    int enq = 0;
    for (int i = 0; i < *got; i++) {
        if (eh_find_download(ids[i]) != NULL)
            continue;
        if (batch_failed_id(ids[i]))
            continue;
        BsBook b;
        if (!eh_store_get_book(ids[i], &b))
            continue;
        if (eh_g_download_count >= EH_MAX_DOWNLOADS) {
            prune_finished_download();
            if (eh_g_download_count >= EH_MAX_DOWNLOADS)
                break;
        }
        eh_enqueue_download(&b);
        enq++;
    }
    return enq;
}

/* Start (or restart) the download-all batch.  The first bounded slice
 * is queued synchronously so the popup shows the whole batch right
 * away; each completed download job tops the queue up as items
 * finish.  The popup opens here (no auto-open — a batch never
 * launches a reader).  With nothing undownloaded the popup is not
 * opened at all: batch_active=1 with no job in flight would make every
 * dismiss tap a no-op (eh_main requires downloads_pending()==0 &&
 * !g_dl_batch_active) and wedge the popup shut. */
void
eh_download_all_start(void)
{
    int total = eh_store_count_undownloaded();
    if (total == 0) {
        eh_LOG("[bookshelf] download-all nothing to download\n");
        return;
    }
    /* New batch: drop the previous batch's failed-id set (grown back
     * as this batch's failures accrue) and reset the tally. */
    free(g_dl_batch_failed_ids);
    g_dl_batch_failed_ids = NULL;
    g_dl_batch_failed_cap = 0;
    g_dl_batch_failed_count = 0;
    eh_g_dl_batch_active = 1;
    eh_g_dl_batch_total = total;
    eh_g_dl_batch_done = 0;
    eh_g_dl_batch_failed = 0;
    int got = 0;
    int enq = batch_enqueue_slice(&got); /* starts the first fetch via enqueue */
    if (enq == 0) {
        /* The store reported undownloaded books but none could be
         * queued (every id failed store_get_book): nothing is in
         * flight, so kick the drain — dl_advance's empty-queue path
         * finalizes the batch instead of wedging with batch_active=1,
         * an empty queue and no job to settle. */
        dl_kick();
    }
    eh_g_state.dl_popup = 1;
    eh_g_state.dl_popup_auto_open = 0;
    eh_redraw_shelf();
    eh_LOG("[bookshelf] download-all queued=%d\n", eh_g_dl_batch_total);
}

static void dl_advance(void);

/* Start the job for the next queued item (main thread).  One download
 * is in flight at a time; each job's done_cb advances the queue, so
 * starting the first item here kicks the drain.  A start failure
 * (store miss, alloc or submit failure) marks the entry failed and
 * falls through to the next queued entry, so one bad book cannot stall
 * the drain with no job in flight; when every entry fails, the drain
 * is re-entered so the empty-queue path finalizes (batch tally /
 * popup) instead of wedging with nothing ever calling dl_advance. */
static void
dl_start_next(void)
{
    if (g_dl_inflight != NULL)
        return;
    int attempted = 0;
    for (int i = 0; i < eh_g_download_count; i++) {
        BsDownloadItem *target = &eh_g_downloads[i];
        if (target->state != 0)
            continue;
        attempted = 1;
        target->state = 1;

        BsBook b;
        if (!eh_store_get_book(target->id, &b)) {
            target->state = 3;
            continue;
        }
        BsDlJob *a = calloc(1, sizeof *a);
        if (a == NULL) {
            target->state = 3;
            continue;
        }
        char path[EH_MAX_PATH_LEN];
        eh_book_local_path(&b, path, sizeof path);
        snprintf(a->id, sizeof a->id, "%s", b.id);
        snprintf(a->url,
                 sizeof a->url,
                 "%s/api/v1/books/%s/file?access_token=%s",
                 eh_g_state.api_base,
                 b.id,
                 eh_g_state.api_token);
        snprintf(a->path, sizeof a->path, "%s", path);
        a->gen = target->gen; /* the settle must match this exact generation */
        BsJob *j = eh_worker_submit(dl_fetch, dl_job_done, a);
        if (j == NULL) {
            target->state = 3;
            free(a);
            continue;
        }
        g_dl_inflight = j;
        return;
    }
    /* Every queued entry failed to start: no job is in flight and
     * nothing else would call dl_advance again, so re-enter the drain
     * to reach the empty-queue finalize.  The entries stay marked
     * state=3 so the popup shows them as failed. */
    if (attempted)
        dl_advance();
}

/* No-op job that re-enters the drain on the main thread.  Reproduces
 * the old drain-timer poll for the batch slice-stall case, where every
 * id the store returned already owns a queue entry or already failed
 * (see dl_advance). */
static void
dl_kick_fn(BsJob *job)
{
    (void)job;
    atomic_store_explicit(&job->done, 1, memory_order_release);
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
    eh_worker_submit(dl_kick_fn, dl_kick_done, NULL);
}

/* 1 = no download fetch is actively running right now: either no job
 * is in flight, or the in-flight job's worker fn already finished (its
 * done flag is set) and only the main-thread settle (next worker tick,
 * <=100 ms later) is pending.  The file is already on disk in that
 * case, so the queue item counts as finished for dismiss checks — a
 * tap that lands in the settle window must close the popup, not be
 * swallowed. */
int
eh_dl_fetch_idle(void)
{
    return g_dl_inflight == NULL ||
           atomic_load_explicit(&g_dl_inflight->done, memory_order_acquire);
}

/* 1 = a download job is in flight whose settle has not run yet (the
 * queue item is still marked in-flight even though the worker fn may
 * be done).  Used to keep the single-book auto-open flow intact: a
 * dismiss tap in the settle window is swallowed so dl_advance() still
 * launches the reader. */
int
eh_dl_job_pending(void)
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
    BsDlJob *a = job->arg;
    int    ok = job->rc == 0;

    /* Settle the finished queue item.  The entry must match BOTH the
     * id and the generation token: after cancel_downloads the queue
     * is gone, so find_download() misses and the completion is
     * absorbed harmlessly — no store update, no batch tally, no popup
     * redraw, and the canceled job's .part was never renamed.  And a
     * book canceled and re-enqueued while its old job was still in
     * flight must not have the new entry settled by the stale job. */
    BsDownloadItem *d = eh_find_download(a->id);
    if (d != NULL && d->gen == a->gen) {
        d->state = ok ? 2 : 3;
        if (ok)
            eh_store_set_downloaded(d->id, 1, a->path);
        if (eh_g_dl_batch_active) {
            /* Successes and failures both settle a batch slot; the
             * bar counts failures separately so it reaches full
             * width even if some books fail.  A failure is recorded
             * so the batch never re-enqueues the book. */
            if (ok)
                eh_g_dl_batch_done++;
            else {
                eh_g_dl_batch_failed++;
                batch_note_failed(d->id);
            }
        }
        /* The popup refresh is deferred to the spawn path in
         * dl_advance, which repaints the sheet once with the settled
         * tally AND the next item's title.  Without a popup, just
         * refresh the top-bar badge. */
        if (!eh_g_state.dl_popup)
            eh_draw_top_bar();
        eh_sync_set_active(eh_downloads_pending() > 0 || eh_g_dl_batch_active);
    } else if (d != NULL) {
        /* Stale job: the queue holds a newer generation of this id
         * (canceled and re-enqueued).  Leave the fresh entry alone —
         * settling it would mis-mark the new download failed. */
        eh_LOG("[bookshelf] stale download job settle dropped id=%s gen=%u entry_gen=%u\n",
            a->id,
            a->gen,
            d->gen);
    }
    if (g_dl_inflight == job)
        g_dl_inflight = NULL;
    free(a);

    dl_advance();

    /* One tally per completed download, AFTER dl_advance so the batch
     * finalize (active=0) is already reflected — the tests poll this
     * line for the finished tally; per-draw logging was removed to
     * keep the log viewer quiet. */
    int dtotal, ddone, dfailed, dactive;
    eh_dl_progress_metrics(&dtotal, &ddone, &dfailed, &dactive);
    eh_LOG("[bookshelf] dl_progress done=%d failed=%d total=%d active=%d\n",
        ddone, dfailed, dtotal, dactive);
}

/* Advance the queue (main thread): start the next queued item, or top
 * the batch up / finalise when the queue is drained.  Started by
 * enqueue_download and by every completed download job; a no-op while
 * a job is in flight. */
/* Batch-mode only: try to enqueue the next slice of undownloaded ids.
 * Returns 1 when a slice was started (the caller's work is done), 0
 * otherwise (no action taken — the batch ended or was never active). */
static int
dl_advance_batch(void)
{
    if (!eh_g_dl_batch_active)
        return 0;
    int got = 0, enq = batch_enqueue_slice(&got);
    int settled = eh_g_dl_batch_done + eh_g_dl_batch_failed;
    if (enq > 0 || (got == 64 && settled < eh_g_dl_batch_total)) {
        if (enq == 0) {
            /* Full slice, nothing enqueued: every id already
             * owns a queue entry or already failed.  Prune one
             * finished entry so the queue makes room and the
             * next pass can enqueue, instead of looping on the
             * same slice forever. */
            if (prune_finished_download()) {
                dl_kick();
                return 1;
            }
            /* Nothing finished left to prune: the whole slice
             * is made of ids that are already failed (or
             * unreadable), so no retry can ever make progress
             * — finalize the batch instead of kicking on the
             * same slice forever. */
            eh_LOG("[bookshelf] download-all batch stalled, finalizing\n");
        } else {
            dl_start_next();
            if (eh_g_state.dl_popup)
                eh_refresh_dl_popup();
            else
                eh_draw_top_bar();
            return 1;
        }
    }
    /* Every batch book has settled (done + failed == total),
     * or the slice is exhausted with nothing left to enqueue:
     * end the batch.  Keep the final tally on screen — zeroing
     * the counters here made the bar fall back to queue-derived
     * counts, and the pruned queue only holds the last slice
     * (<=64).  download_all_start() resets the counters for the
     * next batch; a manual enqueue_download() clears them. */
    eh_g_dl_batch_active = 0;
    eh_LOG("[bookshelf] download-all batch complete\n");
    return 0;
}

/* Queue fully drained with a popup up.  A single-book press auto-opens
 * the reader once its file landed; any other popup stays up showing the
 * finished tally until the user taps it closed.  Returns 1 when the
 * reader was launched, 0 otherwise. */
static int
dl_advance_drain_popup(void)
{
    if (eh_g_state.dl_popup) {
        if (eh_g_state.dl_popup_auto_open) {
            BsBook b;
            if (eh_store_get_book(eh_g_state.dl_popup_book_id, &b) && b.downloaded) {
                eh_g_state.dl_popup = 0;
                eh_g_state.dl_popup_auto_open = 0;
                eh_redraw_shelf();
                eh_LOG("[bookshelf] popup drain complete, launching reader id=%s\n", b.id);
                eh_launch_reader(&b);
                return 1;
            }
        }
        eh_redraw_shelf(); /* popup shows the finished/failed state */
    }
    return 0;
}

static void
dl_advance(void)
{
    if (g_dl_inflight != NULL)
        return;

    BsDownloadItem *target = NULL;
    for (int i = 0; i < eh_g_download_count; i++) {
        if (eh_g_downloads[i].state == 0) {
            target = &eh_g_downloads[i];
            break;
        }
    }
    if (target == NULL) {
        if (dl_advance_batch())
            return;
        eh_sync_set_active(0);
        dl_advance_drain_popup();
        return;
    }

    /* Start the next queued item. */
    dl_start_next();
    /* One popup refresh per item: the sheet now shows the settled
     * tally and the new current-item title.  The dimmed shelf behind
     * it never changed, so a sheet-sized partial suffices — a
     * content-area refresh per finished download is what made
     * download-all flicker. */
    if (eh_g_state.dl_popup)
        eh_refresh_dl_popup();
    else
        eh_draw_top_bar(); /* refresh the pending-count badge in top bar */
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
eh_cancel_downloads(void)
{
    eh_LOG("[bookshelf] cancel_downloads batch=%d in_flight=%p\n",
        eh_g_dl_batch_active, (void *)g_dl_inflight);
    eh_g_dl_batch_active = 0;
    eh_g_dl_batch_total = 0;
    eh_g_dl_batch_done = 0;
    eh_g_dl_batch_failed = 0;
    /* Drop the failed-id set for symmetry with download_all_start so a
     * canceled batch's failures don't survive into and grow across the
     * next batch. */
    free(g_dl_batch_failed_ids);
    g_dl_batch_failed_ids = NULL;
    g_dl_batch_failed_cap = 0;
    g_dl_batch_failed_count = 0;
    eh_g_download_count = 0;
    if (g_dl_inflight != NULL)
        eh_worker_cancel(g_dl_inflight);
    eh_g_state.dl_popup = 0;
    eh_g_state.dl_popup_auto_open = 0;
    eh_sync_set_active(0);
    eh_redraw_shelf();
}

/* Shared bounded-slice walk over a series' member ids: pages through
 * eh_store_series_ids() in chunks of 64 and invokes cb once per id, in
 * order.  cb returns non-zero to stop the walk early (queue full); user
 * is passed through untouched.  download_series / delete_series differ
 * only in the per-id action, so this is their one pagination loop. */
static void
series_walk_ids(const char *series_id, int (*cb)(const char *, void *),
                void *user)
{
    char ids[64][EH_MAX_ID_LEN];
    int  off = 0, got;
    while ((got = eh_store_series_ids(series_id, ids, 64, off)) > 0) {
        for (int i = 0; i < got; i++) {
            if (cb(ids[i], user) != 0)
                return;
        }
        off += got;
        if (got < 64)
            break;
    }
}

/* Enqueue one series member, pruning a finished entry when the bounded
 * queue is full; returns non-zero to stop paging (queue still full). */
static int
series_cb_enqueue(const char *id, void *user)
{
    BsBook b;
    if (!eh_store_get_book(id, &b))
        return 0;
    if (eh_g_download_count >= EH_MAX_DOWNLOADS) {
        prune_finished_download();
        if (eh_g_download_count >= EH_MAX_DOWNLOADS)
            return 1;
    }
    eh_enqueue_download(&b);
    (*(int *)user)++;
    return 0;
}

/* Delete one series member's local file. */
static int
series_cb_delete(const char *id, void *user)
{
    eh_store_delete_book_file(id);
    (*(int *)user)++;
    return 0;
}

/* Queue every member of a series (by series_id), in bounded slices, and
 * open the download-progress popup so the drain is visible.  When the
 * queue is full, a finished entry is pruned first (same pattern as
 * batch_enqueue_slice), so finished rows never crowd the series out
 * and a series larger than MAX_DOWNLOADS keeps flowing as room is
 * freed; if no finished entry is left to make room, the rest of the
 * series is not queued in this pass. */
void
eh_download_series(const char *series_id)
{
    int n = 0;
    series_walk_ids(series_id, series_cb_enqueue, &n);
    eh_g_state.dl_popup = 1;
    eh_g_state.dl_popup_auto_open = 0;
    eh_LOG("[bookshelf] download_series %s queued=%d\n", series_id, n);
}

/* Delete the local files of every member of a series. */
void
eh_delete_series(const char *series_id)
{
    int n = 0;
    series_walk_ids(series_id, series_cb_delete, &n);
    eh_LOG("[bookshelf] delete_series %s removed=%d\n", series_id, n);
}

/* Context menu geometry: a centred modal sheet.  Returns the sheet rect
 * and the y of the first item row. */
void
eh_context_geom(int *px, int *py, int *pw, int *ph, int n_items)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    *pw = w * 3 / 4;
    *ph = EH_CTX_TITLE_H + n_items * EH_CTX_ITEM_H + EH_CTX_PAD;
    *px = (w - *pw) / 2;
    *py = (h - *ph) / 2;
}

int
eh_context_item_count(void)
{
    /* A book offers Open + Download + Delete; a series card offers
     * Download all + Delete series. */
    return eh_g_state.ctx_is_series ? 2 : 3;
}

/* Draw the long-press context menu over a dimmed shelf. */
void
eh_draw_context_menu(void)
{
    int w = ScreenWidth();
    /* Dim mask over the whole app content area (panel band stays). */
    for (int yy = 0; yy < eh_content_bottom(); yy += 2)
        DrawLine(0, yy, w, yy, LGRAY);

    int n = eh_context_item_count();
    int px, py, pw, ph;
    eh_context_geom(&px, &py, &pw, &ph, n);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    /* Title: series name or book title, resolved from the store. */
    static char title_buf[EH_MAX_TITLE_LEN];
    const char *title;
    if (eh_g_state.ctx_is_series) {
        eh_store_series_name(eh_g_state.ctx_series_id, title_buf, sizeof title_buf);
        title = title_buf[0] != '\0' ? title_buf : "Series";
    } else {
        BsBook tmp;
        title_buf[0] = '\0';
        if (eh_store_get_book(eh_g_state.ctx_book_id, &tmp))
            snprintf(title_buf, sizeof title_buf, "%s", tmp.title);
        title = title_buf;
    }
    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        char trunc[EH_MAX_TITLE_LEN];
        snprintf(trunc, sizeof trunc, "%s", title);
        eh_utf8_fit_width(trunc, sizeof trunc, pw - 2 * EH_CTX_PAD);
        DrawString(px + EH_CTX_PAD, py + (EH_CTX_TITLE_H - 28) / 2 - 2, trunc);
        CloseFont(tf);
    }
    DrawLine(px + EH_CTX_PAD, py + EH_CTX_TITLE_H - 1, px + pw - EH_CTX_PAD, py + EH_CTX_TITLE_H - 1, LGRAY);

    const char *labels[3] = {0};
    if (eh_g_state.ctx_is_series) {
        labels[0] = eh_i18n("ctx.download_all");
        labels[1] = eh_i18n("ctx.delete_series");
    } else {
        labels[0] = eh_i18n("ctx.open");
        labels[1] = eh_i18n("ctx.download");
        labels[2] = eh_i18n("ctx.delete");
    }
    ifont *f = OpenFont(DEFAULTFONTB, 30, 0);
    if (f == NULL)
        return;
    SetFont(f, BLACK);
    for (int i = 0; i < n; i++) {
        int iy = py + EH_CTX_TITLE_H + i * EH_CTX_ITEM_H;
        DrawString(px + EH_CTX_PAD, iy + (EH_CTX_ITEM_H - 30) / 2 - 2, labels[i]);
        if (i + 1 < n)
            DrawLine(
                px + EH_CTX_PAD, iy + EH_CTX_ITEM_H - 1, px + pw - EH_CTX_PAD, iy + EH_CTX_ITEM_H - 1, LGRAY);
    }
    CloseFont(f);
}

void
eh_close_context(void)
{
    eh_g_state.overlay = EH_OV_NONE;
    eh_redraw_shelf();
}

/* Open the context menu for a view tile (series card or book). */
void
eh_open_context_for_tile(int vi)
{
    BsTileRow tr;
    if (!eh_view_fetch_row(vi, &tr))
        return;
    eh_g_state.overlay = EH_OV_CTX;
    eh_g_state.ctx_is_series = tr.is_series;
    if (tr.is_series) {
        snprintf(eh_g_state.ctx_series_id, sizeof eh_g_state.ctx_series_id, "%s", tr.series_id);
        eh_g_state.ctx_book_id[0] = '\0';
    } else {
        snprintf(eh_g_state.ctx_book_id, sizeof eh_g_state.ctx_book_id, "%s", tr.book.id);
        eh_g_state.ctx_series_id[0] = '\0';
    }
    eh_draw_context_menu();
    eh_flush_content();
    eh_LOG("[bookshelf] context menu open series=%d vi=%d\n", tr.is_series, vi);
}

/* Long-press timer fired with the finger still down: open the menu. */
void
eh_longpress_tick(void *ctx)
{
    (void)ctx;
    if (!eh_g_lp_armed || eh_g_lp_vi < 0)
        return;
    eh_g_lp_armed = 0;
    int vi = eh_g_lp_vi;
    eh_g_lp_vi = -1;
    eh_g_ctx_suppress_up = 1;
    eh_open_context_for_tile(vi);
}

/* Handle a tap while the context menu is open. */
void
eh_on_tap_context(int x, int y)
{
    int n = eh_context_item_count();
    int px, py, pw, ph;
    eh_context_geom(&px, &py, &pw, &ph, n);
    if (x < px || x >= px + pw || y < py + EH_CTX_TITLE_H || y >= py + ph) {
        eh_close_context();
        return;
    }
    int  item = (y - (py + EH_CTX_TITLE_H)) / EH_CTX_ITEM_H;
    int  is_series = eh_g_state.ctx_is_series;
    char series_id[EH_MAX_ID_LEN];
    snprintf(series_id, sizeof series_id, "%s", eh_g_state.ctx_series_id);
    eh_g_state.overlay = EH_OV_NONE;

    if (is_series) {
        if (item == 0)
            eh_download_series(series_id);
        else if (item == 1)
            eh_delete_series(series_id);
    } else {
        BsBook b;
        if (eh_store_get_book(eh_g_state.ctx_book_id, &b)) {
            if (item == 0) {
                /* Open works exactly like a single tap: download if
                 * needed (with the progress popup), then launch. */
                eh_book_press_action(&b);
            } else if (item == 1) {
                eh_g_state.dl_popup = 1;
                eh_g_state.dl_popup_auto_open = 0;
                eh_enqueue_download(&b);
            } else if (item == 2) {
                eh_store_delete_book_file(b.id);
            }
        }
    }
    eh_redraw_shelf();
}
