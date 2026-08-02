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

/* Look a book up in the master library by id (NULL if unknown). */
Book *
find_lib_book(const char *id)
{
    for (int i = 0; i < g_lib_count; i++)
        if (strcmp(g_lib[i].id, id) == 0)
            return &g_lib[i];
    return NULL;
}

/* Sync a book's downloaded flag by probing its on-device file. */
void
refresh_downloaded(Book *b)
{
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    b->downloaded = (access(path, F_OK) == 0);
    if (b->downloaded)
        snprintf(b->local_path, sizeof b->local_path, "%s", path);
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

/* Add a book to the download queue (no-op if already queued/done) and
 * arm the drain timer. */
void
enqueue_download(const Book *b)
{
    DownloadItem *d = find_download(b->id);
    if (d != NULL)
        return;
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
    b->downloaded = 1;
    snprintf(b->local_path, sizeof b->local_path, "%s", path);
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
    char *args[2] = {path_copy, NULL};
    LOG("[bookshelf] launching reader app=%s path=%s reader_pref=%d\n",
        app,
        path_copy,
        g_state.reader_pref);
    NewTaskEx(full_path, args, app, b->title, NULL, 1u << 30, 0);
}

/* Press a book: download it if needed, then open it in the reader. */
void
book_press_action(Book *b)
{
    refresh_downloaded(b);
    if (!b->downloaded) {
        snprintf(g_state.status, sizeof g_state.status, "%s", i18n("dl.in_progress"));
        if (!download_book_file(b)) {
            snprintf(g_state.status, sizeof g_state.status, "%s", i18n("dl.failed"));
            return;
        }
    }
    launch_reader(b);
}

/* Delete a book's local file (server metadata is untouched — there is no
 * delete endpoint).  Marks the book not-downloaded so it can be fetched
 * again on the next press. */
void
delete_book_file(Book *b)
{
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    if (unlink(path) == 0)
        LOG("[bookshelf] delete_book_file removed %s\n", path);
    else
        LOG("[bookshelf] delete_book_file unlink failed %s\n", path);
    b->downloaded = 0;
    b->local_path[0] = '\0';
    DownloadItem *d = find_download(b->id);
    if (d != NULL)
        d->state = 3;
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
        if (g_state.tab == TAB_DOWNLOADS)
            redraw_shelf();
        return;
    }
    target->state = 1;
    if (g_state.tab == TAB_DOWNLOADS)
        redraw_shelf();

    Book *b = find_lib_book(target->id);
    int   ok = 0;
    if (b != NULL)
        ok = download_book_file(b);
    target->state = ok ? 2 : 3;

    if (g_state.tab == TAB_DOWNLOADS)
        redraw_shelf();
    else
        draw_tab_row(); /* refresh the pending-count badge */

    /* More queued? keep draining. */
    for (int i = 0; i < g_download_count; i++) {
        if (g_downloads[i].state == 0) {
            g_download_armed = 1;
            SetWeakTimerEx("bdl", download_tick, NULL, 120);
            break;
        }
    }
}

/* Queue every member of a series (by series_id). */
void
download_series(const char *series_id)
{
    int n = 0;
    for (int i = 0; i < g_lib_count; i++) {
        if (strcmp(g_lib[i].series_id, series_id) == 0) {
            enqueue_download(&g_lib[i]);
            n++;
        }
    }
    LOG("[bookshelf] download_series %s queued=%d\n", series_id, n);
}

/* Delete the local files of every member of a series. */
void
delete_series(const char *series_id)
{
    int n = 0;
    for (int i = 0; i < g_lib_count; i++) {
        if (strcmp(g_lib[i].series_id, series_id) == 0) {
            delete_book_file(&g_lib[i]);
            n++;
        }
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

    /* Title: series name or book title. */
    const char *title;
    if (g_state.ctx_is_series) {
        /* ctx_series_id holds a series id; recover the name from any member. */
        title = "Series";
        for (int i = 0; i < g_lib_count; i++) {
            if (strcmp(g_lib[i].series_id, g_state.ctx_series_id) == 0) {
                title = g_lib[i].series;
                break;
            }
        }
    } else {
        title = g_state.books[g_state.ctx_book_idx].title;
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
    if (vi < 0 || vi >= g_view_count)
        return;
    const ViewTile *vt = &g_view[vi];
    g_state.ctx_open = 1;
    g_state.ctx_is_series = vt->is_series;
    if (vt->is_series) {
        snprintf(g_state.ctx_series_id, sizeof g_state.ctx_series_id, "%s", vt->series_id);
        g_state.ctx_book_idx = -1;
    } else {
        g_state.ctx_book_idx = vt->book_idx;
        g_state.ctx_series_id[0] = '\0';
    }
    draw_context_menu();
    FullUpdate();
    LOG("[bookshelf] context menu open series=%d vi=%d\n", vt->is_series, vi);
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
    int  book_idx = g_state.ctx_book_idx;
    char series_id[MAX_ID_LEN];
    snprintf(series_id, sizeof series_id, "%s", g_state.ctx_series_id);
    g_state.ctx_open = 0;

    if (is_series) {
        if (item == 0)
            download_series(series_id);
        else if (item == 1)
            delete_series(series_id);
    } else {
        Book *b = (book_idx >= 0 && book_idx < g_state.count) ? &g_state.books[book_idx] : NULL;
        if (b != NULL) {
            Book *lib = find_lib_book(b->id);
            Book *target = lib ? lib : b;
            if (item == 0)
                enqueue_download(target);
            else if (item == 1)
                delete_book_file(target);
        }
    }
    redraw_shelf();
}

