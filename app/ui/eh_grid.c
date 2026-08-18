/* eh_grid.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_browser.h"
#include "eh_extract.h"
#include "eh_model.h"
#include "eh_progress.h"
#include "eh_store.h"
#include "eh_worker.h"
#include "eh_ui.h"

/* -- cover helpers ------------------------------------------------------ */

static long cover_lru = 0;

/* ── post-sync cover warm-up ───────────────────────────────────────────
 * After a remote sync the app walks the whole library in the
 * background, fetching every book's cover into the on-disk cache so the
 * shelf still shows real covers when the network is gone — not just the
 * pages the user happened to view.  Reuses the one-at-a-time
 * single-flight fetch (g_cover_job), so it never competes with the
 * user's on-page cover loading; the fetch chain self-drives via
 * cover_job_done → eh_cover_schedule_next until the library is
 * exhausted.  Providers that serve only 1x1 placeholders (the mock) are
 * detected by probing the first warm covers and the pass aborts early —
 * a mirror of the server's own placeholder skip. */
static int       g_cover_warm_enabled = 0;    /* a sync queued a warm pass */
static long long g_cover_warm_rowid = 0;      /* next remote-rowid to probe */
static char      g_cover_warm_id[EH_MAX_ID_LEN] = ""; /* next uncached candidate */
static int       g_cover_warm_probe_ph = 0;   /* consecutive placeholder warm fetch */
static int       g_cover_warm_seen_real = 0;  /* any real cover warmed */
static int       g_cover_warm_total = 0;      /* remote books this run (progress bar) */
static int       g_cover_warm_done = 0;       /* examined (fetched or skipped) so far */

/* Cap of already-cached books the warm scan hops over in one event-loop
 * slice, so a huge all-cached library never stalls a frame while seeking
 * the next real gap; the keyset cursor persists and the next arm
 * resumes. */
#define EH_COVER_WARM_SKIP 256

/* 1 = the network is up, so a warm fetch would not pop the firmware's
 * "Turn on WiFi" dialog.  Same guard the on-page fetcher uses. */
static int
cover_warm_online(void)
{
    return (QueryNetwork() & 0xf00) ? 1 : 0;
}

/* A fetched cover is a useless 1x1 placeholder when the raw PNG is
 * exactly 1px x 1px (placeholder-serving providers and books with no
 * cover).  A real processed cover from the server is a 240x360 PNG, so
 * 1x1 never occurs for a genuine book.  Used by the warm pass to detect
 * a placeholder-only provider and stop early. */
static int
cover_is_1x1_png(const unsigned char *p, int len)
{
    if (p == NULL || len < 24)
        return 0;
    if (p[0] != 0x89 || p[1] != 'P' || p[2] != 'N' || p[3] != 'G')
        return 0; /* not PNG */
    /* PNG signature (8) + chunk length (4) + "IHDR" (4): the dimensions
     * sit at offset 16 (width) and 20 (height), big-endian. */
    unsigned long w = ((unsigned long)p[16] << 24) | ((unsigned long)p[17] << 16) |
                      ((unsigned long)p[18] << 8) | (unsigned long)p[19];
    unsigned long h = ((unsigned long)p[20] << 24) | ((unsigned long)p[21] << 16) |
                      ((unsigned long)p[22] << 8) | (unsigned long)p[23];
    return w == 1 && h == 1;
}

/* Fill g_cover_warm_id with the next remote book whose cover is not yet
 * on disk, keying forward over the store past already-cached books.
 * Bounded per call (EH_COVER_WARM_SKIP hops) so a large library is
 * scanned across ticks without a frame stall; a paused (offline) pass
 * leaves the cursor put and resumes on the next sync.  Disables the pass
 * once the library is exhausted. */
static void
cover_warm_fill(void)
{
    if (!g_cover_warm_enabled || g_cover_warm_id[0] != '\0' || !cover_warm_online())
        return;
    int   hops = 0;
    char  id[EH_MAX_ID_LEN];
    char  safe[EH_MAX_PATH_LEN];
    while (eh_store_next_warm_book(id, (int)sizeof id, &g_cover_warm_rowid)) {
        g_cover_warm_done++; /* a book passed on the shelf (fetch or skip) */
        eh_cover_cache_path(id, safe, sizeof safe);
        if (access(safe, R_OK) != 0) {
            snprintf(g_cover_warm_id, sizeof g_cover_warm_id, "%s", id);
            return;
        }
        if (++hops >= EH_COVER_WARM_SKIP)
            return; /* resume from g_cover_warm_rowid on the next arm */
    }
    /* Library exhausted: nothing left to warm. */
    g_cover_warm_enabled = 0;
    g_cover_warm_id[0] = '\0';
}

/* 1 = the warm pass still has a cover to fetch (fills the candidate
 * first).  Called on the main thread when deciding whether to arm the
 * cover tick. */
static int
cover_warm_pending(void)
{
    if (!g_cover_warm_enabled || !cover_warm_online())
        return 0;
    cover_warm_fill();
    return g_cover_warm_id[0] != '\0';
}

/* Public: a remote sync finished, so start warming the library's covers
 * into the on-disk cache in the background.  Idempotent and self-
 * terminating: each start re-scans from the first book, skipping covers
 * already on disk, and the pass disables itself when it finds nothing
 * left (or a placeholder-only provider). */
void
eh_cover_warm_start(void)
{
    g_cover_warm_enabled = 1;
    g_cover_warm_rowid = 0;
    g_cover_warm_id[0] = '\0';
    g_cover_warm_probe_ph = 0;
    g_cover_warm_seen_real = 0;
    /* Denominator for the sync-popup progress bar: the whole remote
     * library this pass walks (covers are all remote books on this
     * source). */
    g_cover_warm_total = eh_store_count();
    g_cover_warm_done = 0;
}

/* 1 = the warm pass is running right now (used to keep the sync popup
 * open and to draw its progress bar).  Offline the pass pauses and is
 * reported inactive so the popup is not stuck on a frozen bar. */
int
eh_cover_warm_active(void)
{
    return g_cover_warm_enabled && cover_warm_online();
}

/* Progress of the active warm pass: fills *done (books examined) and
 * *total (remote books this run), clamped so done never exceeds total;
 * returns 1 when the pass is active. */
int
eh_cover_warm_progress(int *done_out, int *total_out)
{
    int done = g_cover_warm_done;
    if (done > g_cover_warm_total)
        done = g_cover_warm_total;
    if (done_out)
        *done_out = done;
    if (total_out)
        *total_out = g_cover_warm_total;
    return eh_cover_warm_active();
}

BsCoverSlot *
eh_cover_slot(const char *id, int create)
{
    BsCoverSlot *empty = NULL;
    for (int i = 0; i < EH_NCOVER_SLOTS; i++) {
        if (eh_g_covers[i].id[0] && strcmp(eh_g_covers[i].id, id) == 0) {
            eh_g_covers[i].last_use = ++cover_lru;
            return &eh_g_covers[i];
        }
        if (empty == NULL && eh_g_covers[i].id[0] == '\0')
            empty = &eh_g_covers[i];
    }
    if (!create)
        return NULL;
    if (empty == NULL) {
        /* Table full: evict the least-recently-used slot. */
        for (int i = 0; i < EH_NCOVER_SLOTS; i++) {
            if (empty == NULL || eh_g_covers[i].last_use < empty->last_use)
                empty = &eh_g_covers[i];
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

/* 1 = the display is colour-capable (eh_plat_display_color(), resolved
 * once at EVT_INIT from the platform backend); covers decode as RGB24
 * then. */
int eh_g_display_color = 0;

/* Mode-aware layout accessors.  Grid mode keeps the fixed 3×2 cover
 * layout; list mode is a single column of short full-width rows, so it
 * fits many more books per page.  Every draw/hit/paging path reads the
 * grid through these so the two modes stay consistent. */
int
eh_view_cols(void)
{
    if (eh_g_state.view_mode == EH_VIEW_LIST)
        return 1;
    /* Column count adapts to the panel: 4 on the 1404px class (cells
     * stay ~347px, matching the cover aspect), 3 on standard panels,
     * 2 on narrow ones that cannot fit three EH_CELL_MIN_W covers
     * (758/825-wide screens). */
    int avail_w = ScreenWidth() - 16;
    if (avail_w >= 4 * EH_CELL_MIN_W + 240)
        return 4;
    return avail_w >= 3 * EH_CELL_MIN_W ? EH_COLS : 2;
}

int
eh_view_rows(void)
{
    if (eh_g_state.view_mode == EH_VIEW_LIST) {
        int t = EH_TOP_BAR_H + EH_TOP_BAR_PAD;
        int b = eh_content_bottom() - EH_PAGER_H;
        if (eh_g_state.overlay == EH_OV_MORE)
            b = eh_content_bottom();
        int rows = (b - t - 8) / EH_LIST_ROW_H;
        if (rows < 1)
            rows = 1;
        return rows;
    }
    /* Three rows on the very tall (1872px) class: the two-row layout
     * leaves ~500px-tall cells with most of the screen unused. */
    int t = EH_TOP_BAR_H + EH_TOP_BAR_PAD;
    int avail_h = (eh_content_bottom() - EH_PAGER_H) - t - 8;
    if (avail_h >= 3 * EH_CELL_MIN_H + 560)
        return 3;
    return EH_ROWS;
}

int
eh_view_pagesize(void)
{
    return eh_view_cols() * eh_view_rows();
}

/* Shared grid geometry so the draw loop and the per-tile fetch blit
 * agree on every coordinate. */
void
eh_grid_geom(int *top, int *bot, int *cell_w, int *cell_h)
{
    int w = ScreenWidth();
    int t = EH_TOP_BAR_H + EH_TOP_BAR_PAD;
    int b = eh_content_bottom() - EH_PAGER_H;
    if (eh_g_state.overlay == EH_OV_MORE)
        b = eh_content_bottom();
    int avail_h = b - t - 8;
    int avail_w = w - 16;
    int cw, ch;
    if (eh_g_state.view_mode == EH_VIEW_LIST) {
        /* List rows are full-width bands of fixed height; the grid
         * min/max clamps would distort them, so they are skipped. */
        cw = avail_w;
        ch = EH_LIST_ROW_H;
    } else {
        cw = avail_w / eh_view_cols();
        ch = avail_h / eh_view_rows();
        if (ch > EH_CELL_MAX_H)
            ch = EH_CELL_MAX_H;
        if (cw > EH_CELL_MAX_W)
            cw = EH_CELL_MAX_W;
        if (ch < EH_CELL_MIN_H)
            ch = EH_CELL_MIN_H;
        if (cw < EH_CELL_MIN_W)
            cw = EH_CELL_MIN_W;
    }
    *top = t;
    *bot = b;
    *cell_w = cw;
    *cell_h = ch;
}

/* Left origin of the cover grid: 8px margin, plus half the leftover
 * width when the cells clamp (EH_CELL_MAX_W) so the grid stays centred
 * on wide panels instead of hugging the left edge. */
int
eh_grid_x0(void)
{
    if (eh_g_state.view_mode == EH_VIEW_LIST)
        return 8;
    int cols = eh_view_cols();
    int cw;
    int top, bot, cell_h;
    eh_grid_geom(&top, &bot, &cw, &cell_h);
    int avail_w = ScreenWidth() - 16;
    return 8 + (avail_w - cols * cw) / 2;
}

/* Screen rect of tile `idx`, or 0 when it isn't on the current page. */
int
eh_tile_rect_for_index(int idx, int *x, int *y, int *w, int *h)
{
    int top, bot, cell_w, cell_h;
    (void)bot;
    eh_grid_geom(&top, &bot, &cell_w, &cell_h);
    int cols = eh_view_cols();
    int ps = eh_view_pagesize();
    int page_start = eh_g_state.page * ps;
    int rel = idx - page_start;
    if (rel < 0 || rel >= ps || idx >= eh_g_view_total)
        return 0;
    int row = rel / cols;
    int col = rel % cols;
    *x = eh_grid_x0() + col * cell_w;
    *y = top + 4 + row * cell_h;
    *w = cell_w - 8;
    *h = cell_h - 6;
    return 1;
}

/* Centered 2:3 portrait card inside the tile, leaving room below for the
 * title and author lines. */
void
eh_cover_rect(int tx, int ty, int tw, int th, int *cx, int *cy, int *cw, int *ch)
{
    int inner_w = tw - 2 * EH_THUMB_BORDER;
    int inner_h = th - 2 * EH_THUMB_BORDER;
    int ch0 = inner_h - EH_TEXT_AREA;
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
    *cx = tx + EH_THUMB_BORDER + (inner_w - cw0) / 2;
    *cy = ty + EH_THUMB_BORDER;
}

/* Id of the i-th row of the current page (NULL past the end).  The page
 * rows live in g_rows[], filled by draw_grid / view_fetch_page. */
static const char *
page_row_id(int i)
{
    if (i < 0 || i >= eh_g_row_count)
        return NULL;
    return eh_g_rows[i].book.id;
}

void
eh_cover_schedule_next(void)
{
    if (eh_g_cover_armed)
        return;
    /* Schedule from the fetched page rows directly (grouped pages have
     * a lo offset that isn't a multiple of the pagesize). */
    for (int i = 0; i < eh_g_row_count; i++) {
        const char *id = page_row_id(i);
        if (id == NULL)
            break;
        BsCoverSlot *s = eh_cover_slot(id, 1);
        if (s != NULL && s->state == 0) {
            eh_g_cover_armed = 1;
            SetWeakTimerEx("bcov", eh_cover_tick, NULL, EH_COVER_FETCH_MS);
            return;
        }
    }
    /* Nothing pending on the visible page — the post-sync warm pass may
     * still have off-page covers to fetch.  Arm for it so the
     * one-at-a-time chain keeps filling the on-disk cache (offline the
     * shelf then shows those covers too). */
    if (cover_warm_pending()) {
        eh_g_cover_armed = 1;
        SetWeakTimerEx("bcov", eh_cover_tick, NULL, EH_COVER_FETCH_MS);
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
eh_blit_cover(int cx, int cy, int cw, int ch, const BsBook *b)
{
    BsCoverSlot *s = eh_cover_slot(b->id, 1);
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
eh_draw_series_stack_back(int cx, int cy, int cw, int ch)
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
} BsGridFonts;

static void
grid_fonts_open(BsGridFonts *gf)
{
    gf->grid_title  = OpenFont(DEFAULTFONTB, 22, 0);
    gf->grid_author = OpenFont(DEFAULTFONT, 18, 0);
    gf->list_title  = OpenFont(DEFAULTFONTB, 30, 0);
    gf->list_author = OpenFont(DEFAULTFONT, 24, 0);
    gf->badge       = OpenFont(DEFAULTFONTB, 20, 0);
}

static void
grid_fonts_close(const BsGridFonts *gf)
{
    if (gf->badge)       CloseFont(gf->badge);
    if (gf->list_author) CloseFont(gf->list_author);
    if (gf->list_title)  CloseFont(gf->list_title);
    if (gf->grid_author) CloseFont(gf->grid_author);
    if (gf->grid_title)  CloseFont(gf->grid_title);
}

void
eh_draw_series_stack_badge(int cx, int cy, int cw, int ch, int count, ifont *bf)
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
draw_thumbnail_text(int tx0, int title_y, int author_y, int fitw,
                    const BsTileRow *tr, ifont *tf, ifont *af)
{
    const BsBook *b = &tr->book;
    const char *label = tr->is_series ? tr->series_name : b->title;
    if (tf != NULL) {
        SetFont(tf, BLACK);
        char truncated[EH_MAX_TITLE_LEN];
        snprintf(truncated, sizeof truncated, "%s", label);
        eh_utf8_fit_width(truncated, sizeof truncated, fitw);
        DrawString(tx0, title_y, truncated);
    }
    if (!tr->is_series && b->author[0] != '\0') {
        if (af != NULL) {
            SetFont(af, DGRAY);
            char truncated[80];
            snprintf(truncated, sizeof truncated, "%s", b->author);
            eh_utf8_fit_width(truncated, sizeof truncated, fitw);
            DrawString(tx0, author_y, truncated);
        }
    }
}

static void
draw_thumbnail_fonts(int x, int y, int w, int h, const BsTileRow *tr, int vi,
                     const BsGridFonts *gf)
{
    (void)vi;
    const BsBook *b = &tr->book;

    FillArea(x, y, w, h, WHITE);
    /* List mode: one full-width row — small 2:3 cover on the left, title
     * and author stacked to its right.  Returns early so the grid card
     * layout below never runs for list rows. */
    if (eh_g_state.view_mode == EH_VIEW_LIST) {
        int pad = 8;
        int chh = h - 2 * pad;
        if (chh < 40)
            chh = 40;
        int cww = chh * 2 / 3;
        int cx = x + pad, cy = y + pad;
        FillArea(cx, cy, cww, chh, WHITE);
        if (tr->is_series)
            eh_draw_series_stack_back(cx, cy, cww, chh);
        eh_blit_cover(cx, cy, cww, chh, b);
        if (tr->is_series)
            eh_draw_series_stack_badge(cx, cy, cww, chh, tr->series_count, gf->badge);
        draw_progress_bar(cx, cy, cww, chh, eh_progress_percent(b->local_path));
        int tx0 = cx + cww + 16;
        int tw0 = (x + w - pad) - tx0;
        if (tw0 < 64)
            tw0 = 64;
        draw_thumbnail_text(tx0, y + pad + 8, y + pad + 8 + 40, tw0, tr,
                            gf->list_title, gf->list_author);
        return;
    }

    int cx, cy, cw, ch;
    eh_cover_rect(x, y, w, h, &cx, &cy, &cw, &ch);

    if (tr->is_series)
        eh_draw_series_stack_back(cx, cy, cw, ch);

    eh_blit_cover(cx, cy, cw, ch, b);

    /* Series cards: badge + outline on top of the cover. */
    if (tr->is_series)
        eh_draw_series_stack_badge(cx, cy, cw, ch, tr->series_count, gf->badge);

    /* Reading progress: a black bar at the cover's bottom edge. */
    draw_progress_bar(cx, cy, cw, ch, eh_progress_percent(b->local_path));

    /* Caption: series name for cards, title for books, with the author
     * skipped for series cards. */
    int cap_y = cy + ch + 6;
    draw_thumbnail_text(x + 4, cap_y, cap_y + 24, w - 8, tr,
                        gf->grid_title, gf->grid_author);
}

void
eh_draw_thumbnail(int x, int y, int w, int h, const BsTileRow *tr, int vi)
{
    BsGridFonts gf;
    grid_fonts_open(&gf);
    draw_thumbnail_fonts(x, y, w, h, tr, vi, &gf);
    grid_fonts_close(&gf);
}


/* ── dimension-group drill actions ──────────────────────────────────── */

/* Tap a group card: record the group's value at the next drill level, so
 * the shelf regroups within that group (or shows flat books at the
 * preset's last level). */
void
eh_group_drill(const char *value)
{
    if (eh_g_drill_level < 0 || eh_g_drill_level >= EH_GROUP_MAX_LEVELS)
        return;
    /* Remember the page of the level we're leaving, so drill-back lands
     * the user back where they were instead of page 0. */
    eh_g_saved_pages[eh_g_drill_level] = eh_g_state.page;
    snprintf(eh_g_drill_values[eh_g_drill_level],
             sizeof eh_g_drill_values[0], "%s", value ? value : "");
    eh_g_drill_level++;
    eh_g_state.page = 0;
    eh_view_rebuild();
    eh_redraw_shelf();
}

/* Pop the group drill (top-bar back button / back key). */
void
eh_group_drill_back(void)
{
    if (eh_g_drill_level > 0) {
        eh_g_drill_level--;
        eh_g_drill_values[eh_g_drill_level][0] = '\0';
    }
    /* Restore the page of the level we return into (saved when its
     * group card was tapped), so back from a deep drill continues
     * where the user left off. */
    eh_g_state.page = eh_g_saved_pages[eh_g_drill_level];
    eh_view_rebuild();
    eh_redraw_shelf();
}

void
eh_draw_grid(void)
{
    /* Layout: [top bar] [grid] [pager] [system panel].  The firmware's
     * type-1 panel owns the bottom band [content_bottom(),
     * ScreenHeight()); the pager sits directly above it. */
    int top, bot, cell_w, cell_h;
    eh_grid_geom(&top, &bot, &cell_w, &cell_h);
    /* Clear the grid area first so cells from a previous page don't
     * bleed through.  We do this every redraw, not just on page change,
     * so partial updates stay simple.
     */
    FillArea(0, top, ScreenWidth(), bot - top, WHITE);
    eh_LOG("[bookshelf] draw_grid view=%d page=%d cell=%dx%d top=%d bot=%d\n",
        eh_g_view_total,
        eh_g_state.page,
        cell_w,
        cell_h,
        top,
        bot);

    eh_g_row_count = eh_view_fetch_page(eh_g_state.page, eh_g_rows, EH_MAX_ROWS * EH_COLS);
    int cols = eh_view_cols();
    int rows = eh_view_rows();
    int ps = eh_view_pagesize();
    int drawn = 0;
    /* Open the tile fonts once for the whole page pass instead of once
     * per tile (each draw_thumbnail used to open/close 4 fonts). */
    BsGridFonts gf;
    grid_fonts_open(&gf);
    int lo = eh_g_state.page * ps;
    for (int row = 0; row < rows; row++) {
        for (int col = 0; col < cols; col++) {
            if (drawn >= eh_g_row_count)
                goto done;
            int tx = eh_grid_x0() + col * cell_w;
            int ty = top + 4 + row * cell_h;
            int tw = cell_w - 8;
            int th = cell_h - 6;
            draw_thumbnail_fonts(tx, ty, tw, th, &eh_g_rows[drawn], lo + drawn, &gf);
            drawn++;
        }
    }
done:
    grid_fonts_close(&gf);
    eh_cover_schedule_next();
}

/* The one remote cover fetch in flight, main thread only. */
static BsJob *g_cover_job;

/* Remote cover fetch job: download the raw PNG and persist it.  Pure
 * file I/O on the worker — the SDK decode stays on the main thread
 * (libinkview is not thread-safe). */
typedef struct {
    char url[EH_MAX_URL_LEN + 128];
    char id[EH_MAX_ID_LEN];
    char cache_path[EH_MAX_PATH_LEN];
    int  warm;           /* submitted by the post-sync warm pass */
    int  is_placeholder; /* fetched bytes were a 1x1 PNG (warm only) */
} BsCoverJobArg;

static void
cover_fetch_job(BsJob *job)
{
    BsCoverJobArg *a = job->arg;
    int          rsize = 0;
    char        *data = QuickDownload(a->url, &rsize, EH_HTTP_TIMEOUT);
    int          ok = 0;
    a->is_placeholder = 0;
    if (data != NULL && rsize > 8 &&
        !atomic_load_explicit(&job->cancel, memory_order_acquire)) {
        /* A warm fetch that lands on a 1x1 placeholder flags the result
         * so cover_job_done can abort the pass early — but the bytes are
         * still persisted below (a placeholder is a cover's absence, not
         * a transient error), so the warm keyset scans past this book
         * instead of re-fetching it. */
        if (a->warm)
            a->is_placeholder = cover_is_1x1_png((const unsigned char *)data, rsize);
        /* Stage the decode source in COVER_TMP (always writable) and
         * best-effort persist the raw PNG so the next launch can skip
         * the network entirely. */
        FILE *f = fopen(eh_plat_cover_tmp(), "wb");
        if (f != NULL) {
            size_t w = fwrite(data, 1, (size_t)rsize, f);
            int    fc = fclose(f);
            if (w == (size_t)rsize && fc == 0) {
                ok = 1;
                eh_cover_cache_save(a->id, data, rsize);
            }
        }
    }
    free(data);
    job->rc = ok ? 0 : -1;
    atomic_store_explicit(&job->done, 1, memory_order_release);
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
    return !eh_modal_open() && eh_g_state.tab == EH_TAB_LIBRARY &&
           !(eh_g_state.source == EH_SOURCE_FOLDER && eh_g_browse_open);
}

/* Cover fetch finished (main thread): decode on the main thread and
 * blit the tile if it is still on the current page, then schedule the
 * next cover.  A failed or canceled job still schedules the next. */
/* Warm-pass bookkeeping: a placeholder-only provider gets disabled once
 * the probe threshold is reached, while a single real cover rules the
 * placeholder-only provider out permanently. */
static void
cover_job_handle_warm(BsCoverJobArg *a)
{
    if (a->warm) {
        if (a->is_placeholder) {
            if (!g_cover_warm_seen_real && ++g_cover_warm_probe_ph >= 5)
                g_cover_warm_enabled = 0;
        } else {
            g_cover_warm_seen_real = 1;
        }
    }
}

/* Find the current page row index whose book id matches `id`, or -1. */
static int
cover_job_page_target(const char *id, int page_start)
{
    int target = -1;
    for (int k = 0; k < eh_g_row_count; k++) {
        const char *pid = page_row_id(k);
        if (pid != NULL && strcmp(pid, id) == 0) {
            target = page_start + k;
            break;
        }
    }
    return target;
}

static void
cover_job_done(BsJob *job)
{
    BsCoverJobArg *a = job->arg;
    g_cover_job = NULL;

    cover_job_handle_warm(a);

    BsCoverSlot *s = eh_cover_slot(a->id, 1);
    ibitmap   *bmp = NULL;
    if (job->rc == 0) {
        eh_LOG("[bookshelf] cover_job_done load_cover_scaled begin id=%s\n", a->id);
        bmp = eh_load_cover_scaled(eh_plat_cover_tmp());
        eh_LOG("[bookshelf] cover_job_done load_cover_scaled done bmp=%p\n", (void *)bmp);
    }
    if (bmp != NULL) {
        if (s->cover_bmp) {
            eh_LOG("[bookshelf] cover_job_done free(old cover_bmp) begin\n");
            free(s->cover_bmp);
            eh_LOG("[bookshelf] cover_job_done free(old cover_bmp) done\n");
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
    int modal = eh_modal_open();
    eh_LOG("[bookshelf] cover_job_done blit begin modal=%d\n", modal);

    /* The fetch is async now, so the user may have flipped pages (or
     * left the shelf) while it ran: blit only when the grid is on
     * screen and the tile is still on the current page. */
    int tx, ty, tw, th;
    if (shelf_active_view()) {
        int page_start = eh_g_state.page * eh_view_pagesize();
        int target = cover_job_page_target(a->id, page_start);
        if (target >= 0 && eh_tile_rect_for_index(target, &tx, &ty, &tw, &th)) {
            FillArea(tx, ty, tw, th, WHITE);
            eh_draw_thumbnail(tx, ty, tw, th, &eh_g_rows[target - page_start], target);
            PartialUpdate(tx, ty, tw, th);
        }
    }
    eh_LOG("[bookshelf] cover_job_done blit done, scheduling next\n");
    free(a);
    eh_cover_schedule_next();
}

/* Fetch the not-yet-loaded visible covers.  All covers that need no
 * network round-trip (a PNG already in the on-disk cache, an embedded
 * EPUB/PDF cover, or an offline miss that just lands in the failed
 * state) are decoded in ONE tick and presented with a single combined
 * partial update — the old code decoded and blitted one cover per
 * 60 ms weak-timer tick, so a cached page popped in one tile at a time
 * (on SDL each tile partial update repaints the whole window).  Only a
 * cover that must be downloaded from the server is deferred to a
 * one-shot job on the shared background worker (eh_worker.c), one in
 * flight at a time; its done_cb decodes on the main thread (libinkview
 * is not thread-safe) and blits just that tile, then reschedules.  The
 * batched decodes stay on the main thread too; they are short on a
 * cached page and bounded by one event-loop slice rather than a busy
 * loop. */
/* Load the cover for one pending slot from the local filesystem, the
 * on-disk PNG cache, or a remote fetch — mirroring the original
 * else-if chain.  Returns 1 if the slot was already handled (a fetch
 * was submitted, or a second remote cover stays pending) and the loop
 * must skip storage/blit; 0 if the caller should store *bmp (NULL =
 * failed state) and blit it. */
static int
cover_slot_fetch(BsCoverSlot *s, const char *bid, int local_book,
                 const BsBook *cbook, int *submitted, ibitmap **bmp)
{
    *bmp = NULL;
    if (local_book) {
        /* Local (filesystem) books have no remote cover: extract the
         * embedded cover image (EPUB) when the format has one,
         * otherwise the tile keeps the placeholder.  The raw extracted
         * cover is cached on disk next to the PNG cache; only unknown
         * books hit the zip parser. */
        char cover_path[EH_MAX_PATH_LEN];
        eh_cover_raw_path(bid, cover_path, sizeof cover_path);
        if (access(cover_path, R_OK) != 0 && cbook->local_path[0] != '\0') {
            eh_cover_ensure_bucket(bid); /* sharded dir must exist to write the .raw */
            if (eh_extract_book_cover(cbook->local_path, cbook->ext, cover_path, sizeof cover_path) != 0)
                cover_path[0] = '\0'; /* extraction failed; no cover */
        }
        if (cover_path[0] != '\0' && access(cover_path, R_OK) == 0) {
            *bmp = eh_load_image_scaled(cover_path);
            eh_LOG("[bookshelf] cover_tick cover id=%s bmp=%p\n", bid, (void *)*bmp);
        }
        return 0;
    }
    if (eh_cover_cache_load(bid, bmp) == 0) {
        eh_LOG("[bookshelf] cover_tick cache hit id=%s\n", bid);
        return 0;
    }
    if (!(QueryNetwork() & 0xf00)) {
        /* No active connection: skip the fetch silently and let the
         * slot land in the failed state below so the next sync — the
         * only place the app may ask for WiFi — retries it. */
        eh_LOG("[bookshelf] cover_tick offline, skipping cover fetch id=%s\n", bid);
        return 0;
    }
    if (*submitted)
        return 1; /* a second remote cover stays pending (state 0) */
    /* Remote cover, not cached, online: hand the fetch to the shared
     * worker; the done_cb decodes and blits and then reschedules the
     * tick.  At most one remote job per tick. */
    {
        char url[EH_MAX_URL_LEN + 128];
        snprintf(url,
                 sizeof url,
                 "%s/api/v1/books/%s/cover?access_token=%s",
                 eh_g_state.api_base,
                 bid,
                 eh_g_state.api_token);
        eh_LOG("[bookshelf] cover_tick submitting fetch url=%s\n", url);
        BsCoverJobArg *a = calloc(1, sizeof *a);
        if (a != NULL) {
            snprintf(a->url, sizeof a->url, "%s", url);
            snprintf(a->id, sizeof a->id, "%s", bid);
            eh_cover_cache_path(bid, a->cache_path, sizeof a->cache_path);
            BsJob *j = eh_worker_submit(cover_fetch_job, cover_job_done, a);
            if (j != NULL) {
                s->state = 1; /* in flight until the done_cb lands */
                g_cover_job = j;
                *submitted = 1;
                return 1;
            }
            free(a);
        }
        /* Cannot submit: fall through to the failed state. */
    }
    return 0;
}

/* Accumulate the on-screen blit for one tile into the shared min/max
 * bounds, mirroring the original bounding-box update. */
static void
cover_tick_blit_region(int idx, int k, const BsGridFonts *gf,
                       int *min_x, int *min_y, int *max_x, int *max_y, int *nblit)
{
    int tx, ty, tw, th;
    if (shelf_active_view() && eh_tile_rect_for_index(idx, &tx, &ty, &tw, &th)) {
        FillArea(tx, ty, tw, th, WHITE);
        draw_thumbnail_fonts(tx, ty, tw, th, &eh_g_rows[k], idx, gf);
        if (!*nblit) {
            *min_x = tx;
            *min_y = ty;
            *max_x = tx + tw;
            *max_y = ty + th;
        } else {
            if (tx < *min_x)
                *min_x = tx;
            if (ty < *min_y)
                *min_y = ty;
            if (tx + tw > *max_x)
                *max_x = tx + tw;
            if (ty + th > *max_y)
                *max_y = ty + th;
        }
        (*nblit)++;
    }
}

/* Walk the visible page once, loading/caching covers and accumulating
 * the dirty region.  Mirrors the original loop body. */
static void
cover_tick_drain_page(BsGridFonts *gf, int *processed, int *submitted,
                      int *min_x, int *min_y, int *max_x, int *max_y, int *nblit)
{
    int page_start = eh_g_state.page * eh_view_pagesize();
    for (int k = 0; k < eh_g_row_count; k++) {
        const char *bid = page_row_id(k);
        if (bid == NULL)
            break;
        int         idx = page_start + k;
        BsCoverSlot *s = eh_cover_slot(bid, 1);
        if (s == NULL || s->state != 0)
            continue; /* already loaded / in flight / failed */
        *processed = 1;
        BsBook   cbook;
        int      local_book = !eh_store_get_book(bid, &cbook) || strcmp(cbook.source, "kavita") != 0;
        ibitmap *bmp = NULL;
        if (cover_slot_fetch(s, bid, local_book, &cbook, submitted, &bmp))
            continue; /* fetch in flight or a second remote cover pending */
        if (bmp != NULL) {
            if (s->cover_bmp) {
                eh_LOG("[bookshelf] cover_tick free(old cover_bmp) begin\n");
                free(s->cover_bmp);
                eh_LOG("[bookshelf] cover_tick free(old cover_bmp) done\n");
            }
            s->cover_bmp = bmp;
            s->state = 2;
        } else {
            s->state = 3;
        }
        cover_tick_blit_region(idx, k, gf, min_x, min_y, max_x, max_y, nblit);
    }
}

/* Hand the pending off-page warm cover to the worker; returns 1 if a job
 * was submitted (so the caller returns).  On failure the candidate is
 * dropped; the next arm picks up from the cursor. */
static int
cover_tick_warm_fetch(void)
{
    const char *bid = g_cover_warm_id;
    char        url[EH_MAX_URL_LEN + 128];
    snprintf(url,
             sizeof url,
             "%s/api/v1/books/%s/cover?access_token=%s",
             eh_g_state.api_base,
             bid,
             eh_g_state.api_token);
    BsCoverJobArg *a = calloc(1, sizeof *a);
    if (a != NULL) {
        snprintf(a->url, sizeof a->url, "%s", url);
        snprintf(a->id, sizeof a->id, "%s", bid);
        eh_cover_cache_path(bid, a->cache_path, sizeof a->cache_path);
        a->warm = 1;
        BsCoverSlot *s = eh_cover_slot(bid, 1);
        if (s != NULL)
            s->state = 1; /* in flight until the done_cb lands */
        BsJob *j = eh_worker_submit(cover_fetch_job, cover_job_done, a);
        if (j != NULL) {
            g_cover_job = j;
            g_cover_warm_id[0] = '\0'; /* consumed; fill finds the next */
            eh_LOG("[bookshelf] cover_tick warm fetch id=%s\n", bid);
            return 1; /* the done_cb decodes, persists and reschedules */
        }
        free(a);
        if (s != NULL)
            s->state = 0;
    }
    /* Could not submit: drop the candidate; the next arm picks up
     * from the cursor. */
    g_cover_warm_id[0] = '\0';
    return 0;
}

void
eh_cover_tick(void *ctx)
{
    (void)ctx;
    /* Run any worker-flagged cover-cache sweep here, on the main
     * thread, so the worker's unlink never races this tick's .raw
     * extraction. */
    eh_cover_cache_sweep_if_pending();
    eh_LOG("[bookshelf] cover_tick ENTER page=%d view=%d armed->0\n", eh_g_state.page, eh_g_view_total);
    eh_g_cover_armed = 0;

    /* One remote fetch at a time: the in-flight job's done_cb
     * schedules the next cover when it lands. */
    if (g_cover_job != NULL)
        return;

    int        processed = 0; /* a pending cover was classified this tick */
    int        submitted = 0; /* a remote fetch was handed to the worker */
    BsGridFonts gf;
    grid_fonts_open(&gf);
    int min_x = 0, min_y = 0, max_x = 0, max_y = 0;
    int nblit = 0;
    cover_tick_drain_page(&gf, &processed, &submitted,
                          &min_x, &min_y, &max_x, &max_y, &nblit);
    grid_fonts_close(&gf);

    int modal = eh_modal_open();
    if (nblit) {
        eh_LOG("[bookshelf] cover_tick blit %d tiles modal=%d\n", nblit, modal);
        PartialUpdate(min_x, min_y, max_x - min_x, max_y - min_y);
    }

    if (submitted)
        return; /* the in-flight job's done_cb blits and reschedules */

    /* Visible page drained with nothing in flight.  If the post-sync
     * warm pass still has an off-page cover queued, hand it to the
     * worker here — same single-flight g_cover_job; its done_cb re-arms
     * and the chain continues until the whole library is warm.  Off-page
     * so there is no blit: the PNG is persisted to the on-disk cache,
     * which is what offline rendering needs. */
    if (cover_warm_pending() && cover_tick_warm_fetch())
        return;

    if (!processed) {
        /* Nothing pending on this page.  A manual sync that opened the
         * progress popup ends here: the covers have drained, so move
         * the popup to its "done" state (it auto-closes shortly). */
        if (eh_g_state.sync_popup && eh_g_state.sync_stage == EH_SYNC_STAGE_COVERS) {
            eh_g_state.sync_stage = EH_SYNC_STAGE_DONE;
            eh_sync_popup_refresh();
            eh_sync_popup_auto_close(900);
        }
        return; /* nothing pending on this page */
    }

    eh_LOG("[bookshelf] cover_tick EXIT\n");
}

void
eh_draw_pager(void)
{
    int w = ScreenWidth();
    /* Pager sits directly above the bottom system panel band. */
    int y = eh_content_bottom() - EH_PAGER_H;
    FillArea(0, y, w, EH_PAGER_H, WHITE);
    DrawLine(0, y, w, y, BLACK);

    int pages = eh_current_pages();
    if (eh_g_state.page >= pages)
        eh_g_state.page = pages - 1;
    if (eh_g_state.page < 0)
        eh_g_state.page = 0;

    ifont *f = OpenFont(DEFAULTFONTB, 28, 0);
    if (f == NULL)
        return;

    char info[32];
    snprintf(info, sizeof info, eh_i18n("pager.info"), eh_g_state.page + 1, pages);
    SetFont(f, BLACK);
    eh_draw_text_centered(f, w / 2, y + (EH_PAGER_H - 28) / 2 - 2, info, BLACK);

    /* Four 96x64 buttons: < prev, << first, >> last, > next.  Disabled
     * buttons render as faint grey text on white (draw_button's selected
     * fill is skipped and label_color forces grey).  The buttons reuse
     * the pass-level font `f` above instead of each opening its own. */
    int by = y + (EH_PAGER_H - 64) / 2;
    int gray = LGRAY;
    /* < prev */
    eh_draw_button_font(12, by, 96, 64, 0, eh_i18n("pager.prev"), 28, f, eh_g_state.page > 0 ? 0 : gray);
    /* << first page */
    eh_draw_button_font(116, by, 96, 64, 0, eh_i18n("pager.first"), 28, f, eh_g_state.page > 0 ? 0 : gray);
    /* >> last page */
    eh_draw_button_font(
        w - 212, by, 96, 64, 0, eh_i18n("pager.last"), 28, f, eh_g_state.page + 1 < pages ? 0 : gray);
    /* > next */
    eh_draw_button_font(
        w - 108, by, 96, 64, 0, eh_i18n("pager.next"), 28, f, eh_g_state.page + 1 < pages ? 0 : gray);
    CloseFont(f);
}
