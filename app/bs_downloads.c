/* bs_downloads.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── downloads, delete, context menu, long-press ───────────────────── */

/* Local path a book downloads to (matches the open-with launch path). */
void
book_local_path(const Book *b, char *out, size_t cap)
{
    if (b->ext[0])
        snprintf(out, cap, "%s/%s.%s", g_downloads_dir, b->id, b->ext);
    else
        snprintf(out, cap, "%s/%s", g_downloads_dir, b->id);
}

/* Sync a book's downloaded flag by probing its on-device file, in the
 * store and in the caller's copy. */
void
refresh_downloaded(Book *b)
{
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
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

/* Add a book to the download queue (no-op if already queued/done) and
 * arm the drain timer. */
void
enqueue_download(const Book *b)
{
    DownloadItem *d = find_download(b->id);
    if (d != NULL)
        return;
    if (!g_dl_batch_active) {
        /* Manual download: the retained tally of the last batch must
         * not mask this one, and its finished rows must not inflate
         * the fresh queue tally (or crowd it out entirely). */
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
    if (!g_download_armed) {
        g_download_armed = 1;
        SetWeakTimerEx("bdl", download_tick, NULL, 120);
    }
    sync_set_active(1);
}

/* Drop the oldest finished queue entry to make room (batch mode keeps
 * the queue bounded regardless of library size). */
static void
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
        return;
    for (int i = best; i + 1 < g_download_count; i++)
        g_downloads[i] = g_downloads[i + 1];
    g_download_count--;
}

/* ── async download worker ────────────────────────────────────────────
 * The file fetch runs on a worker thread so the event loop stays
 * responsive while a book downloads — QuickDownload blocks for the
 * whole transfer (up to the 60 s timeout), which used to freeze the UI
 * for the duration.  download_tick() polls the worker and settles each
 * queue item when it finishes.  The worker touches no UI and no store
 * state: it only fetches the file and writes it to disk; the main
 * thread applies store_set_downloaded() afterwards. */

typedef struct {
    char id[MAX_ID_LEN];
    char url[MAX_URL_LEN + 128];
    char path[MAX_PATH_LEN];
} DlJob;

static pthread_t   g_dl_thread;
static _Atomic int g_dl_thread_running; /* 1 while a worker is active */
static _Atomic int g_dl_thread_ok;      /* 0 pending, 1 ok, -1 failed */
static char        g_dl_thread_id[MAX_ID_LEN];
static char        g_dl_thread_path[MAX_PATH_LEN];

/* Worker: fetch one book's file to disk (blocking).  Returns 1 on
 * success.  No UI, no store access — the caller settles the store. */
static void *
dl_thread_main(void *arg)
{
    DlJob *job = arg;
    int    rsize = 0;
    char  *data = QuickDownload(job->url, &rsize, 60);
    int    ok = 0;
    if (data != NULL && rsize > 0) {
        FILE *f = fopen(job->path, "wb");
        if (f != NULL) {
            fwrite(data, 1, (size_t)rsize, f);
            fclose(f);
            ok = 1;
            LOG("[bookshelf] download_book_file OK id=%s path=%s bytes=%d\n",
                job->id,
                job->path,
                rsize);
        } else {
            LOG("[bookshelf] download_book_file fopen FAILED id=%s path=%s errno=%d\n",
                job->id,
                job->path,
                errno);
        }
        free(data);
    } else {
        if (data != NULL)
            free(data);
        LOG("[bookshelf] download_book_file FAILED id=%s url=%s rsize=%d errno=%d\n",
            job->id,
            job->url,
            rsize,
            errno);
    }
    __atomic_store_n(&g_dl_thread_ok, ok ? 1 : -1, __ATOMIC_RELEASE);
    __atomic_store_n(&g_dl_thread_running, 0, __ATOMIC_RELEASE);
    free(job);
    return NULL;
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
    book_local_path(b, path, sizeof path);

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
 * auto-open the reader when the queue drains (see download_tick).
 * Already-downloaded books open immediately.  Persists the downloaded
 * flag so the next launch sees the file. */
void
book_press_action(Book *b)
{
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    int dl = (access(path, F_OK) == 0);
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
 * away; the drain timer tops the queue up as items finish.  The popup
 * opens here (no auto-open — a batch never launches a reader). */
void
download_all_start(void)
{
    g_dl_batch_active = 1;
    g_dl_batch_total = store_count_undownloaded();
    g_dl_batch_done = 0;
    g_dl_batch_failed_count = 0;
    int got = 0;
    batch_enqueue_slice(&got);
    if (!g_download_armed) {
        g_download_armed = 1;
        SetWeakTimerEx("bdl", download_tick, NULL, 300);
    }
    g_state.dl_popup = 1;
    g_state.dl_popup_auto_open = 0;
    redraw_shelf();
    LOG("[bookshelf] download-all queued=%d\n", g_dl_batch_total);
}

/* Drain the download queue one item per tick.  Each item's file fetch
 * runs on the worker thread; the tick polls it, settles the finished
 * item, and starts the next, so the event loop never blocks on the
 * network. */
void
download_tick(void *ctx)
{
    (void)ctx;
    g_download_armed = 0;

    /* Worker still fetching: keep the popup/live UI ticking and return
     * without touching the queue — the settle happens on the next tick
     * after the worker finishes. */
    if (__atomic_load_n(&g_dl_thread_running, __ATOMIC_ACQUIRE)) {
        if (g_dl_batch_active || downloads_pending() > 0 || g_state.dl_popup) {
            g_download_armed = 1;
            SetWeakTimerEx("bdl", download_tick, NULL, 120);
        }
        return;
    }

    /* A worker just finished: settle its queue item. */
    if (g_dl_thread_id[0] != '\0') {
        int           ok = __atomic_load_n(&g_dl_thread_ok, __ATOMIC_ACQUIRE);
        DownloadItem *d = find_download(g_dl_thread_id);
        if (d != NULL) {
            d->state = (ok == 1) ? 2 : 3;
            if (ok == 1)
                store_set_downloaded(d->id, 1, g_dl_thread_path);
            if (g_dl_batch_active) {
                /* Successes and failures both settle a batch slot; the
                 * bar counts failures separately so it reaches full
                 * width even if some books fail.  A failure is recorded
                 * so the batch never re-enqueues the book. */
                if (ok == 1)
                    g_dl_batch_done++;
                else {
                    g_dl_batch_failed++;
                    batch_note_failed(d->id);
                }
            }
        }
        g_dl_thread_id[0] = '\0';
        if (g_state.dl_popup)
            redraw_shelf();
        else
            draw_top_bar(); /* refresh the pending-count badge */
        sync_set_active(downloads_pending() > 0 || g_dl_batch_active);
    }

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
                    /* Full slice, nothing enqueued: every id already owns
                     * a queue entry or already failed.  Prune one
                     * finished entry so the queue makes room and the
                     * next slice can enqueue, instead of re-arming
                     * forever on the same slice. */
                    prune_finished_download();
                }
                /* Keep draining until every batch book settles. */
                SetWeakTimerEx("bdl", download_tick, NULL, enq > 0 ? 120 : 300);
                g_download_armed = 1;
                return;
            }
            /* Every batch book has settled (done + failed == total):
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
            return;
        }
        return;
    }
    target->state = 1;
    if (g_state.dl_popup)
        redraw_shelf();

    /* Spawn the worker for this item. */
    Book b;
    if (store_get_book(target->id, &b)) {
        DlJob *job = calloc(1, sizeof *job);
        if (job != NULL) {
            char path[MAX_PATH_LEN];
            book_local_path(&b, path, sizeof path);
            snprintf(job->id, sizeof job->id, "%s", b.id);
            snprintf(job->url,
                     sizeof job->url,
                     "%s/api/v1/books/%s/file?access_token=%s",
                     g_state.api_base,
                     b.id,
                     g_state.api_token);
            snprintf(job->path, sizeof job->path, "%s", path);
            snprintf(g_dl_thread_id, sizeof g_dl_thread_id, "%s", b.id);
            snprintf(g_dl_thread_path, sizeof g_dl_thread_path, "%s", path);
            __atomic_store_n(&g_dl_thread_ok, 0, __ATOMIC_RELEASE);
            __atomic_store_n(&g_dl_thread_running, 1, __ATOMIC_RELEASE);
            if (pthread_create(&g_dl_thread, NULL, dl_thread_main, job) == 0) {
                pthread_detach(g_dl_thread);
            } else {
                __atomic_store_n(&g_dl_thread_running, 0, __ATOMIC_RELEASE);
                g_dl_thread_id[0] = '\0';
                target->state = 3;
                free(job);
            }
        } else {
            target->state = 3;
        }
    } else {
        target->state = 3;
    }

    if (g_state.dl_popup)
        redraw_shelf();
    else
        draw_top_bar(); /* refresh the pending-count badge in top bar */

    /* More work queued?  Also re-arm when the batch is still topping
     * up, and always once more while the popup is open so the
     * queue-drained branch can finalise (auto-open the reader or show
     * the finished tally) after the last item settles. */
    if (g_dl_batch_active || downloads_pending() > 0 || g_state.dl_popup) {
        g_download_armed = 1;
        SetWeakTimerEx("bdl", download_tick, NULL, 120);
    }
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
            trunc[strlen(trunc) - 1] = '\0';
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
    g_state.ctx_open = 0;
    redraw_shelf();
}

/* Open the context menu for a view tile (series card or book). */
void
open_context_for_tile(int vi)
{
    TileRow tr;
    if (!view_fetch_row(vi, &tr))
        return;
    g_state.ctx_open = 1;
    g_state.ctx_is_series = tr.is_series;
    if (tr.is_series) {
        snprintf(g_state.ctx_series_id, sizeof g_state.ctx_series_id, "%s", tr.series_id);
        g_state.ctx_book_id[0] = '\0';
    } else {
        snprintf(g_state.ctx_book_id, sizeof g_state.ctx_book_id, "%s", tr.book.id);
        g_state.ctx_series_id[0] = '\0';
    }
    draw_context_menu();
    FullUpdate();
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
    g_state.ctx_open = 0;

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
