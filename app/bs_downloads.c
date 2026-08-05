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

/* Download one book's file to disk (blocking).  Returns 1 on success. */
int
download_book_file(Book *b)
{
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);

    char url[MAX_URL_LEN + 128];
    snprintf(url,
             sizeof url,
             "%s/api/v1/books/%s/file?access_token=%s",
             g_state.api_base,
             b->id,
             g_state.api_token);

    int   rsize = 0;
    char *data = QuickDownload(url, &rsize, 60);
    if (data == NULL || rsize <= 0) {
        if (data)
            free(data);
        LOG("[bookshelf] download_book_file FAILED id=%s\n", b->id);
        return 0;
    }
    FILE *f = fopen(path, "wb");
    if (f == NULL) {
        free(data);
        LOG("[bookshelf] download_book_file fopen FAILED path=%s\n", path);
        return 0;
    }
    fwrite(data, 1, (size_t)rsize, f);
    fclose(f);
    free(data);
    store_set_downloaded(b->id, 1, path);
    b->downloaded = 1;
    LOG("[bookshelf] download_book_file OK id=%s path=%s bytes=%d\n", b->id, path, rsize);
    return 1;
}

/* Launch the configured reader on an already-downloaded book. */
void
launch_reader(Book *b)
{
    char app[80];
    char full_path[160];
    if (g_state.reader_pref > 0 && g_state.reader_pref <= g_reader_count) {
        const char *rpath = g_readers[g_state.reader_pref - 1].path;
        const char *rbase = strrchr(rpath, '/');
        rbase = rbase ? rbase + 1 : rpath;
        snprintf(app, sizeof app, "%s", rbase);
        snprintf(full_path, sizeof full_path, "%s", rpath);
    } else {
        /* Auto: ask the server which app handles this extension. */
        char body[160];
        snprintf(body, sizeof body, "{\"id\":\"%s\",\"ext\":\"%s\"}", b->id, b->ext);
        char *resp = NULL;
        int   rl = 0;
        char  resolved[64] = "eink-reader";
        if (http_post(g_state.url_openwith, body, &resp, &rl) == 0 && resp) {
            char tmp[64];
            if (json_find_key(resp, "app", tmp, sizeof tmp))
                snprintf(resolved, sizeof resolved, "%s", tmp);
            free(resp);
        }
        size_t alen = strlen(resolved);
        if (alen < 4 || strcmp(resolved + alen - 4, ".app") != 0)
            snprintf(app, sizeof app, "%s.app", resolved);
        else
            snprintf(app, sizeof app, "%s", resolved);
        /* Build full path from basename. */
        if (strchr(app, '/') == NULL)
            snprintf(full_path, sizeof full_path, "/ebrmain/bin/%s", app);
        else
            snprintf(full_path, sizeof full_path, "%s", app);
    }
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    char path_copy[MAX_PATH_LEN];
    snprintf(path_copy, sizeof path_copy, "%s", path);
    /* argv[0] must be the program path: the firmware's task launcher
     * passes the args array through as-is, so with only the book path
     * in the array the reader received it as argv[0] and never saw a
     * book argument (the integrated reader then opened to its home
     * screen instead of the book). */
    char *args[3] = {full_path, path_copy, NULL};
    LOG("[bookshelf] launching reader app=%s path=%s reader_pref=%d\n",
        app,
        path_copy,
        g_state.reader_pref);
    NewTaskEx(full_path, args, app, b->title, NULL, 1u << 30, 0);
}

/* Press a book: download it if needed, then open it in the reader.
 * Persists the downloaded flag so the next launch sees the file. */
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
        snprintf(g_state.status, sizeof g_state.status, "%s", i18n("dl.in_progress"));
        if (!download_book_file(b)) {
            snprintf(g_state.status, sizeof g_state.status, "%s", i18n("dl.failed"));
            return;
        }
    }
    launch_reader(b);
}

/* Enqueue the next bounded slice of undownloaded ids for the
 * download-all batch, skipping ids that already own a queue entry
 * (in flight, done, or failed).  The query is offset-free: ids whose
 * file landed earlier shrink the "downloaded=0" result set, so any
 * OFFSET cursor would skip books on later slices.  *got reports how
 * many ids the store slice held so the caller can tell "drained" from
 * "full slice, more to come".  Returns the number actually enqueued. */
static int
batch_enqueue_slice(int *got)
{
    char ids[64][MAX_ID_LEN];
    *got = store_next_undownloaded(ids, 64);
    int enq = 0;
    for (int i = 0; i < *got; i++) {
        if (find_download(ids[i]) != NULL)
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
 * is queued synchronously so the Downloads tab shows the whole batch
 * right away; the drain timer tops the queue up as items finish. */
void
download_all_start(void)
{
    g_dl_batch_active = 1;
    g_dl_batch_total = store_count_undownloaded();
    g_dl_batch_done = 0;
    int got = 0;
    batch_enqueue_slice(&got);
    if (!g_download_armed) {
        g_download_armed = 1;
        SetWeakTimerEx("bdl", download_tick, NULL, 300);
    }
    LOG("[bookshelf] download-all queued=%d\n", g_dl_batch_total);
}

/* Drain the download queue one item per tick so a "Download all" shows
 * live progress on the Downloads tab instead of blocking the UI for the
 * whole batch. */
void
download_tick(void *ctx)
{
    (void)ctx;
    g_download_armed = 0;
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
            if (enq > 0 || got == 64) {
                if (enq == 0) {
                    /* Full slice, nothing enqueued: every id already owns
                     * a queue entry.  Prune one finished entry so the
                     * queue makes room and the next slice can enqueue,
                     * instead of re-arming forever on the same slice. */
                    prune_finished_download();
                }
                /* Keep draining until every batch book settles.  The
                 * last item of a slice finishing must not stop the
                 * timer — the batch-enqueue branch runs only from this
                 * tick. */
                SetWeakTimerEx("bdl", download_tick, NULL, enq > 0 ? 120 : 300);
                g_download_armed = 1;
                return;
            }
            /* Keep the final tally on screen: zeroing the counters
             * here made the bar fall back to queue-derived counts,
             * and the pruned queue only holds the last slice (<=64) —
             * "93 downloaded" snapped back to "64 downloaded".
             * download_all_start() resets the counters for the next
             * batch; a manual enqueue_download() clears them. */
            g_dl_batch_active = 0;
            LOG("[bookshelf] download-all batch complete\n");
        }
        sync_set_active(0);
        if (g_state.tab == TAB_DOWNLOADS)
            redraw_shelf();
        return;
    }
    target->state = 1;
    if (g_state.tab == TAB_DOWNLOADS)
        redraw_shelf();

    Book b;
    int  ok = 0;
    if (store_get_book(target->id, &b))
        ok = download_book_file(&b);
    target->state = ok ? 2 : 3;
    if (g_dl_batch_active) {
        /* Successes and failures both settle a batch slot; the bar
         * counts failures separately so it reaches full width even if
         * some books fail. */
        if (ok)
            g_dl_batch_done++;
        else
            g_dl_batch_failed++;
    }

    if (g_state.tab == TAB_DOWNLOADS)
        redraw_shelf();
    else
        draw_top_bar(); /* refresh the pending-count badge in top bar */
    sync_set_active(downloads_pending() > 0);

    /* More work queued?  Also re-arm when the batch is still topping
     * up: the last item of a slice finishing must not stop the drain
     * timer — the batch-enqueue branch runs only from this tick. */
    if (g_dl_batch_active || downloads_pending() > 0) {
        g_download_armed = 1;
        SetWeakTimerEx("bdl", download_tick, NULL, 120);
    }
}

/* Queue every member of a series (by series_id), in bounded slices. */
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
    /* Both the book and series menus offer exactly two actions. */
    return 2;
}

/* Draw the long-press context menu over a dimmed shelf. */
void
draw_context_menu(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    /* Dim mask. */
    for (int yy = g_state.panel_h; yy < h; yy += 2)
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

    const char *labels[2];
    if (g_state.ctx_is_series) {
        labels[0] = i18n("ctx.download_all");
        labels[1] = i18n("ctx.delete_series");
    } else {
        labels[0] = i18n("ctx.download");
        labels[1] = i18n("ctx.delete");
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
            if (item == 0)
                enqueue_download(&b);
            else if (item == 1)
                store_delete_book_file(b.id);
        }
    }
    redraw_shelf();
}
