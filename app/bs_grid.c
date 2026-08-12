/* bs_grid.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
#include "bs_browser.h"
#include "bs_extract.h"
#include "bs_model.h"
#include "bs_progress.h"
#include "bs_store.h"
#include "bs_worker.h"
#include "bs_ui.h"

/* -- cover helpers ------------------------------------------------------ */

static long cover_lru = 0;

CoverSlot *
cover_slot(const char *id, int create)
{
    CoverSlot *empty = NULL;
    for (int i = 0; i < NCOVER_SLOTS; i++) {
        if (g_covers[i].id[0] && strcmp(g_covers[i].id, id) == 0) {
            g_covers[i].last_use = ++cover_lru;
            return &g_covers[i];
        }
        if (empty == NULL && g_covers[i].id[0] == '\0')
            empty = &g_covers[i];
    }
    if (!create)
        return NULL;
    if (empty == NULL) {
        /* Table full: evict the least-recently-used slot. */
        for (int i = 0; i < NCOVER_SLOTS; i++) {
            if (empty == NULL || g_covers[i].last_use < empty->last_use)
                empty = &g_covers[i];
        }
    }
    if (empty->cover_bmp) {
        free(empty->cover_bmp);
        empty->cover_bmp = NULL;
    }
    memset(empty, 0, sizeof *empty);
    snprintf(empty->id, sizeof empty->id, "%s", id);
    empty->last_use = ++cover_lru;
    return empty;
}

/* 1 = the display is colour-capable (device_display_colormask() != 0);
 * covers decode as RGB24 then.  Resolved once at EVT_INIT. */
int g_display_color = 0;

/* Mode-aware layout accessors.  Grid mode keeps the fixed 3×2 cover
 * layout; list mode is a single column of short full-width rows, so it
 * fits many more books per page.  Every draw/hit/paging path reads the
 * grid through these so the two modes stay consistent. */
int
view_cols(void)
{
    return g_state.view_mode == VIEW_LIST ? 1 : COLS;
}

int
view_rows(void)
{
    if (g_state.view_mode != VIEW_LIST)
        return ROWS;
    int t = TOP_BAR_H + TOP_BAR_PAD;
    int b = content_bottom() - PAGER_H;
    if (g_state.overlay == OV_MENU || g_state.overlay == OV_MORE)
        b = content_bottom();
    int rows = (b - t - 8) / LIST_ROW_H;
    if (rows < 1)
        rows = 1;
    return rows;
}

int
view_pagesize(void)
{
    return view_cols() * view_rows();
}

/* Shared grid geometry so the draw loop and the per-tile fetch blit
 * agree on every coordinate. */
void
grid_geom(int *top, int *bot, int *cell_w, int *cell_h)
{
    int w = ScreenWidth();
    int t = TOP_BAR_H + TOP_BAR_PAD;
    int b = content_bottom() - PAGER_H;
    if (g_state.overlay == OV_MENU || g_state.overlay == OV_MORE)
        b = content_bottom();
    int avail_h = b - t - 8;
    int avail_w = w - 16;
    int cw, ch;
    if (g_state.view_mode == VIEW_LIST) {
        /* List rows are full-width bands of fixed height; the grid
         * min/max clamps would distort them, so they are skipped. */
        cw = avail_w;
        ch = LIST_ROW_H;
    } else {
        cw = avail_w / COLS;
        ch = avail_h / ROWS;
        if (ch > CELL_MAX_H)
            ch = CELL_MAX_H;
        if (cw > CELL_MAX_W)
            cw = CELL_MAX_W;
        if (ch < CELL_MIN_H)
            ch = CELL_MIN_H;
        if (cw < CELL_MIN_W)
            cw = CELL_MIN_W;
    }
    *top = t;
    *bot = b;
    *cell_w = cw;
    *cell_h = ch;
}

/* Screen rect of tile `idx`, or 0 when it isn't on the current page. */
int
tile_rect_for_index(int idx, int *x, int *y, int *w, int *h)
{
    int top, bot, cell_w, cell_h;
    (void)bot;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int cols = view_cols();
    int ps = view_pagesize();
    int page_start = g_state.page * ps;
    int rel = idx - page_start;
    if (rel < 0 || rel >= ps || idx >= g_view_total)
        return 0;
    int row = rel / cols;
    int col = rel % cols;
    *x = 8 + col * cell_w;
    *y = top + 4 + row * cell_h;
    *w = cell_w - 8;
    *h = cell_h - 6;
    return 1;
}

/* Centered 2:3 portrait card inside the tile, leaving room below for the
 * title and author lines. */
void
cover_rect(int tx, int ty, int tw, int th, int *cx, int *cy, int *cw, int *ch)
{
    int inner_w = tw - 2 * THUMB_BORDER;
    int inner_h = th - 2 * THUMB_BORDER;
    int ch0 = inner_h - TEXT_AREA;
    int cw0 = ch0 * 2 / 3;
    if (cw0 > inner_w) {
        cw0 = inner_w;
        ch0 = cw0 * 3 / 2;
    }
    if (ch0 > inner_h)
        ch0 = inner_h;
    if (ch0 < 8)
        ch0 = 8;
    *cw = cw0;
    *ch = ch0;
    *cx = tx + THUMB_BORDER + (inner_w - cw0) / 2;
    *cy = ty + THUMB_BORDER;
}

/* Id of the i-th row of the current page (NULL past the end).  The page
 * rows live in g_rows[], filled by draw_grid / view_fetch_page. */
static const char *
page_row_id(int i)
{
    if (i < 0 || i >= g_row_count)
        return NULL;
    return g_rows[i].book.id;
}

void
cover_schedule_next(void)
{
    if (g_cover_armed)
        return;
    int top, bot, cell_w, cell_h;
    (void)top;
    (void)cell_w;
    (void)cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int ps = view_pagesize();
    int page_start = g_state.page * ps;
    int lim = page_start + ps;
    if (lim > g_view_total)
        lim = g_view_total;
    for (int i = page_start; i < lim; i++) {
        const char *id = page_row_id(i - page_start);
        if (id == NULL)
            break;
        CoverSlot *s = cover_slot(id, 1);
        if (s != NULL && s->state == 0) {
            g_cover_armed = 1;
            SetWeakTimerEx("bcov", cover_tick, NULL, COVER_FETCH_MS);
            return;
        }
    }
}

/* Blit an RGB24 cover directly into the libinkview canvas, bypassing
 * the 8-bit draw pipeline (iv_area flattens 24-bit sources to grey).
 * The QPA bridge that eink-reader uses does exactly this, and it is the
 * only way an app gets colour on the Kaleido panel.  Nearest-neighbour
 * scale to the tile rect; the canvas must be 24bpp, else fall back. */
static void
blit_cover_color24(int cx, int cy, int cw, int ch, const ibitmap *src)
{
    icanvas *cv = GetCanvas();
    if (cv == NULL || cv->depth != 24 || cv->addr == 0)
        return;
    uint8_t *base = (uint8_t *)(uintptr_t)cv->addr;
    lockCanvasDrawing();
    for (int y = 0; y < ch; y++) {
        int sy = (y * src->height) / ch;
        if (sy >= src->height)
            sy = src->height - 1;
        uint8_t       *dst = base + (size_t)(cy + y) * (size_t)cv->scanline + (size_t)cx * 3u;
        const uint8_t *row = src->data + (size_t)sy * (size_t)src->scanline;
        for (int x = 0; x < cw; x++) {
            int sx = (x * src->width) / cw;
            if (sx >= src->width)
                sx = src->width - 1;
            /* The 24-bit bitmap from LoadPNGToFormat is already in the
             * fb's byte order (RGB); writing it verbatim keeps the
             * colours correct on the device and in the viewer. */
            dst[x * 3u + 0] = row[sx * 3u + 0];
            dst[x * 3u + 1] = row[sx * 3u + 1];
            dst[x * 3u + 2] = row[sx * 3u + 2];
        }
    }
    unlockCanvasDrawing();
}

void
blit_cover(int cx, int cy, int cw, int ch, const Book *b)
{
    CoverSlot *s = cover_slot(b->id, 1);
    if (s != NULL && s->cover_bmp != NULL) {
        if (s->cover_bmp->depth == 24) {
            blit_cover_color24(cx, cy, cw, ch, s->cover_bmp);
            return;
        }
        StretchBitmap(cx, cy, cw, ch, s->cover_bmp, 0);
        return;
    }
    DrawRect(cx, cy, cw, ch, BLACK);
}

/* Series card decoration: draw the cover as the front book of a stack.
 * Two "page" sheets peek out along the top and left edges (offset up and
 * left), so the pile reads as a stack with the single book sitting at the
 * bottom-right.  A count badge sits in the cover's top-right corner. */
void
draw_series_stack_back(int cx, int cy, int cw, int ch)
{
    int step = 5;
    /* Back page sheet (furthest up-left). */
    FillArea(cx - 2 * step, cy - 2 * step, cw, ch, WHITE);
    DrawRect(cx - 2 * step, cy - 2 * step, cw, ch, BLACK);
    /* Front page sheet. */
    FillArea(cx - step, cy - step, cw, ch, WHITE);
    DrawRect(cx - step, cy - step, cw, ch, BLACK);
}

/* Fonts a thumbnail pass needs, hoisted once per redraw instead of
 * opened+closed per tile (each tile used to do 4 OpenFont/CloseFont
 * pairs; a shelf redraw is ~15 tiles).  Sizes/faces match what the
 * per-tile code opened before. */
typedef struct {
    ifont *grid_title;  /* DEFAULTFONTB 22 — grid caption */
    ifont *grid_author; /* DEFAULTFONT 18 — grid author line */
    ifont *list_title;  /* DEFAULTFONTB 30 — list row title */
    ifont *list_author; /* DEFAULTFONT 24 — list row author */
    ifont *badge;       /* DEFAULTFONTB 20 — series count badge */
} GridFonts;

static void
grid_fonts_open(GridFonts *gf)
{
    gf->grid_title  = OpenFont(DEFAULTFONTB, 22, 0);
    gf->grid_author = OpenFont(DEFAULTFONT, 18, 0);
    gf->list_title  = OpenFont(DEFAULTFONTB, 30, 0);
    gf->list_author = OpenFont(DEFAULTFONT, 24, 0);
    gf->badge       = OpenFont(DEFAULTFONTB, 20, 0);
}

static void
grid_fonts_close(const GridFonts *gf)
{
    if (gf->badge)       CloseFont(gf->badge);
    if (gf->list_author) CloseFont(gf->list_author);
    if (gf->list_title)  CloseFont(gf->list_title);
    if (gf->grid_author) CloseFont(gf->grid_author);
    if (gf->grid_title)  CloseFont(gf->grid_title);
}

void
draw_series_stack_badge(int cx, int cy, int cw, int ch, int count, ifont *bf)
{
    /* Outline the cover rect so it reads as the top book of the stack. */
    DrawRect(cx, cy, cw, ch, BLACK);

    char badge[8];
    snprintf(badge, sizeof badge, "%d", count);
    if (bf != NULL) {
        SetFont(bf, WHITE);
        int bw = StringWidth(badge) + 12;
        int bh = 26;
        int bx = cx + cw - bw - 2;
        int by = cy + 2;
        FillArea(bx, by, bw, bh, BLACK);
        DrawString(bx + 6, by + 2, badge);
    }
}

/* Reading-progress bar inside the bottom of a cover: a thin black
 * track with a black fill proportional to the percent read (0..100).
 * Progress comes from the firmware's books_settings table, which both
 * the integrated reader and the KOReader pocketbooksync plugin write. */
static void
draw_progress_bar(int cx, int cy, int cw, int ch, int pct)
{
    int bar_h = cw >= 150 ? 10 : 6;
    if (pct < 0)
        pct = 0;
    if (pct > 100)
        pct = 100;
    int by = cy + ch - bar_h;
    FillArea(cx, by, cw, bar_h, WHITE);
    DrawRect(cx, by, cw, bar_h, BLACK);
    int fill = cw * pct / 100;
    if (fill >= 2)
        FillArea(cx + 1, by + 1, fill - 2, bar_h - 2, BLACK);
}

static void
draw_thumbnail_fonts(int x, int y, int w, int h, const TileRow *tr, int vi,
                     const GridFonts *gf)
{
    (void)vi;
    const Book *b = &tr->book;

    FillArea(x, y, w, h, WHITE);
    /* List mode: one full-width row — small 2:3 cover on the left, title
     * and author stacked to its right.  Returns early so the grid card
     * layout below never runs for list rows. */
    if (g_state.view_mode == VIEW_LIST) {
        int pad = 8;
        int chh = h - 2 * pad;
        if (chh < 40)
            chh = 40;
        int cww = chh * 2 / 3;
        int cx = x + pad, cy = y + pad;
        FillArea(cx, cy, cww, chh, WHITE);
        if (tr->is_series)
            draw_series_stack_back(cx, cy, cww, chh);
        blit_cover(cx, cy, cww, chh, b);
        if (tr->is_series)
            draw_series_stack_badge(cx, cy, cww, chh, tr->series_count, gf->badge);
        draw_progress_bar(cx, cy, cww, chh, progress_percent(b->local_path));
        int tx0 = cx + cww + 16;
        int tw0 = (x + w - pad) - tx0;
        if (tw0 < 64)
            tw0 = 64;
        const char *label = tr->is_series ? tr->series_name : b->title;
        ifont      *f = gf->list_title;
        if (f != NULL) {
            SetFont(f, BLACK);
            char truncated[MAX_TITLE_LEN];
            snprintf(truncated, sizeof truncated, "%s", label);
            utf8_fit_width(truncated, sizeof truncated, tw0);
            DrawString(tx0, y + pad + 8, truncated);
        }
        if (!tr->is_series && b->author[0] != '\0') {
            ifont *af = gf->list_author;
            if (af != NULL) {
                SetFont(af, DGRAY);
                char truncated[80];
                snprintf(truncated, sizeof truncated, "%s", b->author);
                utf8_fit_width(truncated, sizeof truncated, tw0);
                DrawString(tx0, y + pad + 8 + 40, truncated);
            }
        }
        return;
    }

    int cx, cy, cw, ch;
    cover_rect(x, y, w, h, &cx, &cy, &cw, &ch);

    if (tr->is_series)
        draw_series_stack_back(cx, cy, cw, ch);

    blit_cover(cx, cy, cw, ch, b);

    /* Series cards: badge + outline on top of the cover. */
    if (tr->is_series)
        draw_series_stack_badge(cx, cy, cw, ch, tr->series_count, gf->badge);

    /* Reading progress: a black bar at the cover's bottom edge. */
    draw_progress_bar(cx, cy, cw, ch, progress_percent(b->local_path));

    /* Caption: series name for cards, title for books. */
    int         cap_y = cy + ch + 6;
    const char *label = tr->is_series ? tr->series_name : b->title;
    ifont      *f = gf->grid_title;
    if (f != NULL) {
        SetFont(f, BLACK);
        char truncated[MAX_TITLE_LEN];
        snprintf(truncated, sizeof truncated, "%s", label);
        utf8_fit_width(truncated, sizeof truncated, w - 8);
        DrawString(x + 4, cap_y, truncated);
    }

    /* Second line: author for books, omitted for series cards. */
    if (!tr->is_series && b->author[0] != '\0') {
        ifont *af = gf->grid_author;
        if (af != NULL) {
            SetFont(af, DGRAY);
            char truncated[80];
            snprintf(truncated, sizeof truncated, "%s", b->author);
            utf8_fit_width(truncated, sizeof truncated, w - 8);
            DrawString(x + 4, cap_y + 24, truncated);
        }
    }
}

void
draw_thumbnail(int x, int y, int w, int h, const TileRow *tr, int vi)
{
    GridFonts gf;
    grid_fonts_open(&gf);
    draw_thumbnail_fonts(x, y, w, h, tr, vi, &gf);
    grid_fonts_close(&gf);
}

void
draw_grid(void)
{
    /* Layout: [top bar] [grid] [pager] [system panel].  The firmware's
     * type-1 panel owns the bottom band [content_bottom(),
     * ScreenHeight()); the pager sits directly above it. */
    int top, bot, cell_w, cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    /* Clear the grid area first so cells from a previous page don't
     * bleed through.  We do this every redraw, not just on page change,
     * so partial updates stay simple.
     */
    FillArea(0, top, ScreenWidth(), bot - top, WHITE);
    LOG("[bookshelf] draw_grid view=%d page=%d cell=%dx%d top=%d bot=%d\n",
        g_view_total,
        g_state.page,
        cell_w,
        cell_h,
        top,
        bot);

    int ps = view_pagesize();
    g_row_count = view_fetch_page(g_state.page, g_rows, MAX_ROWS * COLS);
    int cols = view_cols();
    int rows = view_rows();
    int drawn = 0;
    /* Open the tile fonts once for the whole page pass instead of once
     * per tile (each draw_thumbnail used to open/close 4 fonts). */
    GridFonts gf;
    grid_fonts_open(&gf);
    for (int row = 0; row < rows; row++) {
        for (int col = 0; col < cols; col++) {
            if (drawn >= g_row_count)
                goto done;
            int tx = 8 + col * cell_w;
            int ty = top + 4 + row * cell_h;
            int tw = cell_w - 8;
            int th = cell_h - 6;
            draw_thumbnail_fonts(tx, ty, tw, th, &g_rows[drawn], g_state.page * ps + drawn, &gf);
            drawn++;
        }
    }
done:
    grid_fonts_close(&gf);
    cover_schedule_next();
}

/* The one remote cover fetch in flight, main thread only. */
static BsJob *g_cover_job;

/* Remote cover fetch job: download the raw PNG and persist it.  Pure
 * file I/O on the worker — the SDK decode stays on the main thread
 * (libinkview is not thread-safe). */
typedef struct {
    char url[MAX_URL_LEN + 128];
    char id[MAX_ID_LEN];
    char cache_path[MAX_PATH_LEN];
} CoverJobArg;

static void
cover_fetch_job(BsJob *job)
{
    CoverJobArg *a = job->arg;
    int          rsize = 0;
    char        *data = QuickDownload(a->url, &rsize, HTTP_TIMEOUT);
    int          ok = 0;
    if (data != NULL && rsize > 8 &&
        !__atomic_load_n(&job->cancel, __ATOMIC_ACQUIRE)) {
        /* Stage the decode source in COVER_TMP (always writable) and
         * best-effort persist the raw PNG so the next launch can skip
         * the network entirely. */
        FILE *f = fopen(COVER_TMP, "wb");
        if (f != NULL) {
            size_t w = fwrite(data, 1, (size_t)rsize, f);
            if (w == (size_t)rsize && fclose(f) == 0) {
                ok = 1;
                cover_cache_save(a->id, data, rsize);
            }
        }
    }
    free(data);
    job->rc = ok ? 0 : -1;
    __atomic_store_n(&job->done, 1, __ATOMIC_RELEASE);
}

/* 1 = the cover grid is the live on-screen view, so a per-tile cover
 * blit is safe.  Matches what redraw_shelf() actually draws as the
 * body: only the library tab with no modal overlay up and the folder
 * browser closed shows the grid — on the search page, the launcher,
 * the settings/source/menu overlays, or while the folder browser is
 * open, a blit would paint shelf tiles over the wrong page.  The
 * decoded bitmap is cached on the slot either way; the next full
 * redraw (redraw_shelf) shows it. */
static int
shelf_active_view(void)
{
    return !modal_open() && g_state.tab == TAB_LIBRARY &&
           !(g_state.source == SOURCE_FOLDER && g_browse_open);
}

/* Cover fetch finished (main thread): decode on the main thread and
 * blit the tile if it is still on the current page, then schedule the
 * next cover.  A failed or canceled job still schedules the next. */
static void
cover_job_done(BsJob *job)
{
    CoverJobArg *a = job->arg;
    g_cover_job = NULL;

    CoverSlot *s = cover_slot(a->id, 1);
    ibitmap   *bmp = NULL;
    if (job->rc == 0) {
        LOG("[bookshelf] cover_job_done load_cover_scaled begin id=%s\n", a->id);
        bmp = load_cover_scaled(COVER_TMP);
        LOG("[bookshelf] cover_job_done load_cover_scaled done bmp=%p\n", (void *)bmp);
    }
    if (bmp != NULL) {
        if (s->cover_bmp) {
            LOG("[bookshelf] cover_job_done free(old cover_bmp) begin\n");
            free(s->cover_bmp);
            LOG("[bookshelf] cover_job_done free(old cover_bmp) done\n");
        }
        s->cover_bmp = bmp;
        s->state = 2;
    } else {
        s->state = 3;
    }
    /* The cached bitmap is stored on the slot regardless; only the
     * on-screen blit is skipped while a modal owns the framebuffer or
     * the shelf is not the live view, so a single-tile PartialUpdate
     * can't punch a hole through an overlay's dim mask or paint over
     * the wrong page (the full redraw then shows the now-cached
     * cover). */
    int modal = modal_open();
    LOG("[bookshelf] cover_job_done blit begin modal=%d\n", modal);

    /* The fetch is async now, so the user may have flipped pages (or
     * left the shelf) while it ran: blit only when the grid is on
     * screen and the tile is still on the current page. */
    int tx, ty, tw, th;
    int target = -1;
    if (shelf_active_view()) {
        int top, bot, cell_w, cell_h;
        (void)top;
        (void)bot;
        (void)cell_w;
        (void)cell_h;
        grid_geom(&top, &bot, &cell_w, &cell_h);
        int ps = view_pagesize();
        int page_start = g_state.page * ps;
        int lim = page_start + ps;
        if (lim > g_view_total)
            lim = g_view_total;
        for (int i = page_start; i < lim; i++) {
            const char *id = page_row_id(i - page_start);
            if (id != NULL && strcmp(id, a->id) == 0) {
                target = i;
                break;
            }
        }
        if (target >= 0 && tile_rect_for_index(target, &tx, &ty, &tw, &th)) {
            FillArea(tx, ty, tw, th, WHITE);
            draw_thumbnail(tx, ty, tw, th, &g_rows[target - page_start], target);
            PartialUpdate(tx, ty, tw, th);
        }
    }
    LOG("[bookshelf] cover_job_done blit done, scheduling next\n");
    free(a);
    cover_schedule_next();
}

/* Fetch one not-yet-loaded visible cover per tick.  Local (EPUB/PDF)
 * covers are extracted and decoded here on the main thread as before;
 * a remote cover that misses the on-disk cache is fetched by a
 * one-shot job on the shared background worker (bs_worker.c) — the
 * old code called QuickDownload() directly on the event loop, freezing
 * the UI for up to the 8 s HTTP timeout.  The job fn only downloads
 * and writes the PNG files; its done_cb decodes on the main thread
 * (libinkview is not thread-safe) and blits just that tile. */
void
cover_tick(void *ctx)
{
    (void)ctx;
    /* Run any worker-flagged cover-cache sweep here, on the main
     * thread, so the worker's unlink never races this tick's .raw
     * extraction. */
    cover_cache_sweep_if_pending();
    LOG("[bookshelf] cover_tick ENTER page=%d view=%d armed->0\n", g_state.page, g_view_total);
    g_cover_armed = 0;

    /* One remote fetch at a time: the in-flight job's done_cb
     * schedules the next cover when it lands. */
    if (g_cover_job != NULL)
        return;

    int top, bot, cell_w, cell_h;
    (void)top;
    (void)bot;
    (void)cell_w;
    (void)cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int ps = view_pagesize();
    int page_start = g_state.page * ps;
    int lim = page_start + ps;
    if (lim > g_view_total)
        lim = g_view_total;

    int target = -1;
    for (int i = page_start; i < lim; i++) {
        const char *id = page_row_id(i - page_start);
        if (id == NULL)
            break;
        CoverSlot *s = cover_slot(id, 1);
        if (s != NULL && s->state == 0) {
            target = i;
            break;
        }
    }
    if (target < 0) {
        /* Nothing pending on this page.  A manual sync that opened the
         * progress popup ends here: the covers have drained, so move
         * the popup to its "done" state (it auto-closes shortly). */
        if (g_state.sync_popup && g_state.sync_stage == SYNC_STAGE_COVERS) {
            g_state.sync_stage = SYNC_STAGE_DONE;
            sync_popup_refresh();
            sync_popup_auto_close(900);
        }
        return; /* nothing pending on this page */
    }

    const char *bid = page_row_id(target - page_start);
    if (bid == NULL)
        return;
    CoverSlot *s = cover_slot(bid, 1);
    LOG("[bookshelf] cover_tick target=%d id=%s slot=%p\n", target, bid, (void *)s);

    /* Local (filesystem) books have no remote cover: extract the
     * embedded cover image (EPUB) when the format has one, otherwise
     * the tile keeps the placeholder. */
    Book cbook;
    int  local_book = !store_get_book(bid, &cbook) || strcmp(cbook.source, "kavita") != 0;
    s->state = local_book ? 3 : 1;

    ibitmap *bmp = NULL;
    if (local_book) {
        /* The raw extracted cover is cached on disk next to the PNG
         * cache; only unknown books hit the zip parser. */
        char cover_path[MAX_PATH_LEN];
        cover_raw_path(bid, cover_path, sizeof cover_path);
        if (access(cover_path, R_OK) != 0 && cbook.local_path[0] != '\0') {
            if (extract_book_cover(cbook.local_path, cbook.ext, cover_path, sizeof cover_path) != 0)
                cover_path[0] = '\0'; /* extraction failed; no cover */
        }
        if (cover_path[0] != '\0' && access(cover_path, R_OK) == 0) {
            bmp = load_image_scaled(cover_path);
            LOG("[bookshelf] cover_tick cover id=%s bmp=%p\n", bid, (void *)bmp);
        }
    } else if (cover_cache_load(bid, &bmp) == 0) {
        LOG("[bookshelf] cover_tick cache hit id=%s\n", bid);
    } else if (!(QueryNetwork() & 0xf00)) {
        /* No active connection: skip the fetch silently and let the
         * slot land in the failed state below so the next sync — the
         * only place the app may ask for WiFi — retries it.  An
         * unguarded QuickDownload() here would pop the firmware's
         * "Turn on WiFi" dialog whenever an offline launch shows
         * books whose covers are not in the on-disk cache. */
        LOG("[bookshelf] cover_tick offline, skipping cover fetch id=%s\n", bid);
    } else {
        /* Remote cover, not cached, online: hand the fetch to the
         * shared worker; the done_cb decodes and blits. */
        char url[MAX_URL_LEN + 128];
        snprintf(url,
                 sizeof url,
                 "%s/api/v1/books/%s/cover?access_token=%s",
                 g_state.api_base,
                 bid,
                 g_state.api_token);
        LOG("[bookshelf] cover_tick submitting fetch url=%s\n", url);
        CoverJobArg *a = calloc(1, sizeof *a);
        if (a != NULL) {
            snprintf(a->url, sizeof a->url, "%s", url);
            snprintf(a->id, sizeof a->id, "%s", bid);
            cover_cache_path(bid, a->cache_path, sizeof a->cache_path);
            BsJob *j = bs_worker_submit(cover_fetch_job, cover_job_done, a);
            if (j != NULL) {
                g_cover_job = j;
                return; /* the done_cb blits and schedules the next */
            }
            free(a);
        }
        /* Cannot submit: fall through to the failed state. */
    }

    if (bmp != NULL) {
        if (s->cover_bmp) {
            LOG("[bookshelf] cover_tick free(old cover_bmp) begin\n");
            free(s->cover_bmp);
            LOG("[bookshelf] cover_tick free(old cover_bmp) done\n");
        }
        s->cover_bmp = bmp;
        s->state = 2;
    } else {
        s->state = 3;
    }
    /* The cached bitmap is stored on the slot regardless; only the
     * on-screen blit is skipped while a modal owns the framebuffer or
     * the shelf is not the live view, so a single-tile PartialUpdate
     * can't punch a hole through an overlay's dim mask or paint over
     * the wrong page (the full redraw then shows the now-cached
     * cover). */
    int modal = modal_open();
    LOG("[bookshelf] cover_tick blit begin modal=%d\n", modal);

    int tx, ty, tw, th;
    if (shelf_active_view() && tile_rect_for_index(target, &tx, &ty, &tw, &th)) {
        FillArea(tx, ty, tw, th, WHITE);
        draw_thumbnail(tx, ty, tw, th, &g_rows[target - page_start], target);
        PartialUpdate(tx, ty, tw, th);
    }
    LOG("[bookshelf] cover_tick blit done, scheduling next\n");
    cover_schedule_next();
    LOG("[bookshelf] cover_tick EXIT\n");
}

void
draw_pager(void)
{
    int w = ScreenWidth();
    /* Pager sits directly above the bottom system panel band. */
    int y = content_bottom() - PAGER_H;
    FillArea(0, y, w, PAGER_H, WHITE);
    DrawLine(0, y, w, y, BLACK);

    int pages = current_pages();
    if (g_state.page >= pages)
        g_state.page = pages - 1;
    if (g_state.page < 0)
        g_state.page = 0;

    ifont *f = OpenFont(DEFAULTFONTB, 28, 0);
    if (f == NULL)
        return;

    char info[32];
    snprintf(info, sizeof info, i18n("pager.info"), g_state.page + 1, pages);
    SetFont(f, BLACK);
    draw_text_centered(f, w / 2, y + (PAGER_H - 28) / 2 - 2, info, BLACK);

    /* Four 96x64 buttons: < prev, << first, >> last, > next.  Disabled
     * buttons render as faint grey text on white (draw_button's selected
     * fill is skipped and label_color forces grey).  The buttons reuse
     * the pass-level font `f` above instead of each opening its own. */
    int by = y + (PAGER_H - 64) / 2;
    int gray = LGRAY;
    /* < prev */
    draw_button_font(12, by, 96, 64, 0, i18n("pager.prev"), 28, f, g_state.page > 0 ? 0 : gray);
    /* << first page */
    draw_button_font(116, by, 96, 64, 0, i18n("pager.first"), 28, f, g_state.page > 0 ? 0 : gray);
    /* >> last page */
    draw_button_font(
        w - 212, by, 96, 64, 0, i18n("pager.last"), 28, f, g_state.page + 1 < pages ? 0 : gray);
    /* > next */
    draw_button_font(
        w - 108, by, 96, 64, 0, i18n("pager.next"), 28, f, g_state.page + 1 < pages ? 0 : gray);
    CloseFont(f);
}
