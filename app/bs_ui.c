/* bs_ui.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── drawing primitives ─────────────────────────────────────────────── */

void
draw_text_centered(ifont *f, int cx, int cy, const char *text, int color)
{
    if (f == NULL)
        return;
    SetFont(f, color);
    DrawString(cx - StringWidth(text) / 2, cy, text);
}

void
draw_button(
    int x, int y, int w, int h, int selected, const char *label, int label_size, int label_color)
{
    DrawRect(x, y, w, h, BLACK);
    FillArea(x + 1, y + 1, w - 2, h - 2, selected ? BLACK : WHITE);
    if (label == NULL || label[0] == '\0')
        return;
    ifont *f = OpenFont(DEFAULTFONTB, label_size, 0);
    if (f != NULL) {
        SetFont(f, label_color != 0 ? label_color : (selected ? WHITE : BLACK));
        DrawString(x + (w - StringWidth(label)) / 2, y + (h - label_size) / 2 - 2, label);
        CloseFont(f);
    }
}

void
draw_top_bar(void)
{
    int w = ScreenWidth();
    int y0 = g_state.panel_h;
    int col = BLACK;

    FillArea(0, y0, w, TOP_BAR_H, WHITE);
    DrawLine(0, y0 + TOP_BAR_H, w, y0 + TOP_BAR_H, col);

    /* Left button: back-arrow when drilled, house icon otherwise. */
    int home_w = 96;
    int home_x = 8;
    int home_y = y0 + (TOP_BAR_H - home_w) / 2;
    if (g_drilled_series[0] != '\0' || g_state.tab == TAB_SEARCH) {
        /* Left-pointing chevron arrow. */
        int ax = home_x + 20;
        int ay = home_y + home_w / 2;
        DrawLine(ax, ay, ax + 30, ay - 30, col);
        DrawLine(ax, ay, ax + 30, ay + 30, col);
        DrawLine(ax + 4, ay, ax + 34, ay - 30, col);
        DrawLine(ax + 4, ay, ax + 34, ay + 30, col);
    } else {
        /* house outline (pentagon + floor break for door) */
        DrawLine(home_x + 5, home_y + 29, home_x + 5, home_y + 85, col);
        DrawLine(home_x + 5, home_y + 29, home_x + 48, home_y - 8, col);
        DrawLine(home_x + 48, home_y - 8, home_x + 91, home_y + 29, col);
        DrawLine(home_x + 91, home_y + 29, home_x + 91, home_y + 85, col);
        DrawLine(home_x + 5, home_y + 85, home_x + 37, home_y + 85, col);
        DrawLine(home_x + 53, home_y + 85, home_x + 91, home_y + 85, col);
        /* door */
        DrawLine(home_x + 37, home_y + 85, home_x + 37, home_y + 61, col);
        DrawLine(home_x + 37, home_y + 61, home_x + 53, home_y + 61, col);
        DrawLine(home_x + 53, home_y + 61, home_x + 53, home_y + 85, col);
    }
    /* Centered title — series name when drilled, "Search" on the search
     * page, the active query on the filtered library shelf, nothing on
     * the plain shelf (the app name in the top bar was dropped per user
     * request). */
    ifont *tf = OpenFont(DEFAULTFONT, 44, 0);
    if (tf != NULL) {
        char title[MAX_QUERY_LEN + 16];
        if (g_drilled_series[0] != '\0') {
            /* Series name is resolved once at drill time. */
            snprintf(title, sizeof title, "%s", g_drilled_series_name);
            if (title[0] == '\0')
                snprintf(title, sizeof title, "Series");
        } else if (g_state.tab == TAB_SEARCH) {
            snprintf(title, sizeof title, "%s", i18n("tab.search"));
        } else if (g_state.query[0] != '\0') {
            /* The active filter shown as the shelf title. */
            snprintf(title, sizeof title, "%s", g_state.query);
        } else {
            title[0] = '\0';
        }
        if (title[0] != '\0') {
            /* Centre the title inside the free band between the flanking
             * icon stacks (home/back left; search + downloads + menu
             * right).  Centring on the whole screen width lets a long
             * series name run under the right icons: the trim budget
             * must be the band width, not w - 420, and the draw origin
             * the band, not 0. */
            int left_w = 8 + 96;
            int right_w = 8 + 3 * 96;
            int band_w = w - left_w - right_w;
            if (band_w < 64)
                band_w = 64;
            while (StringWidth(title) > band_w && strlen(title) > 4)
                title[strlen(title) - 1] = '\0';
            SetFont(tf, col);
            DrawString(
                left_w + (band_w - StringWidth(title)) / 2, y0 + (TOP_BAR_H - 40) / 2, title);
        }
        CloseFont(tf);
    }
    if (g_state.tab == TAB_SEARCH) {
        /* Search page: the input row owns search here, so no right
         * icons — the corner stays empty (taps there fall through). */
        return;
    }
    draw_search_icon();
    draw_sync_icon();

    /* Right "menu" button — 96×96 solid black square with three
     * white hamburger lines. */
    int menu_w = 96;
    int menu_x = w - menu_w - 8;
    int menu_y = y0 + (TOP_BAR_H - menu_w) / 2;
    int menu_cx = menu_x + menu_w / 2;
    int menu_cy = menu_y + menu_w / 2;
    int menu_r = menu_w / 2;
    FillArea(menu_cx - menu_r, menu_cy - menu_r, menu_r * 2, menu_r * 2, col);
    int ml_w = 44;
    FillArea(menu_cx - ml_w / 2, menu_cy - 19, ml_w, 6, WHITE);
    FillArea(menu_cx - ml_w / 2, menu_cy - 3, ml_w, 6, WHITE);
    FillArea(menu_cx - ml_w / 2, menu_cy + 13, ml_w, 6, WHITE);
}

/* Sync button in the top bar, left of the menu button: a solid black
 * square with two white arc arrows (a "refresh" glyph) that rotate a
 * few degrees per second while a sync or download is in flight
 * (sync_set_active arms the rotation timer).  Tapping it runs a
 * library sync (see hit_top_bar). */
static int
sync_active(void)
{
    return g_state.sync_state == 1 || downloads_pending() > 0 || g_dl_batch_active;
}

static int
sync_modal_open(void)
{
    return g_state.ctx_open || g_state.menu_open || g_state.more_open || g_state.settings_open ||
           g_state.launcher_open;
}

static int spin_armed = 0;

static void
sync_spin_tick(void *ctx)
{
    (void)ctx;
    if (!sync_active()) {
        spin_armed = 0; /* nothing in flight — the glyph rests */
        return;
    }
    g_state.sync_angle = (g_state.sync_angle + 15) % 360;
    /* The glyph only exists on the Library tab; elsewhere the top bar
     * is redrawn whole when the state that feeds it changes. */
    if (!sync_modal_open() && g_state.tab != TAB_SEARCH) {
        draw_sync_icon();
        PartialUpdate(ScreenWidth() - 96 - 8 - 96, g_state.panel_h, 96, TOP_BAR_H);
    }
    SetWeakTimerEx("bspin", sync_spin_tick, NULL, 1000);
}

void
sync_set_active(int on)
{
    /* Arm the 1s rotation timer exactly once per active stretch; repeated
     * calls (every download tick) must not reset it or it never fires. */
    if (on && sync_active() && !spin_armed) {
        spin_armed = 1;
        SetWeakTimerEx("bspin", sync_spin_tick, NULL, 1000);
    }
    if (!sync_modal_open() && g_state.tab != TAB_SEARCH) {
        draw_sync_icon();
        PartialUpdate(ScreenWidth() - 96 - 8 - 96, g_state.panel_h, 96, TOP_BAR_H);
    }
}

void
draw_sync_icon(void)
{
    int w = ScreenWidth();
    int y0 = g_state.panel_h;
    int ic_w = 96;
    int ic_x = w - ic_w - 8 - ic_w; /* left of the menu button */
    int ic_y = y0 + (TOP_BAR_H - ic_w) / 2;
    FillArea(ic_x, ic_y, ic_w, ic_w, BLACK);
    int cx = ic_x + ic_w / 2;
    int cy = ic_y + ic_w / 2;
    int r = 28;
    /* Two 120-degree arc arrows, rotated by g_state.sync_angle. */
    for (int half = 0; half < 2; half++) {
        int a0 = g_state.sync_angle + half * 180;
        int px = 0, py = 0;
        int ex = 0, ey = 0;
        for (int s = 0; s <= 8; s++) {
            double a = (a0 + s * 15) * M_PI / 180.0;
            int    x = cx + (int)(r * cos(a));
            int    y = cy + (int)(r * sin(a));
            if (s > 0) {
                DrawLine(px, py, x, y, WHITE);
                DrawLine(px, py + 1, x, y + 1, WHITE);
            }
            px = x;
            py = y;
            if (s == 8) {
                ex = x;
                ey = y;
            }
        }
        /* Arrowhead: two ticks trailing the tangent at the arc end. */
        double ta = (a0 + 120) * M_PI / 180.0 + M_PI / 2.0;
        for (int t = 0; t < 2; t++) {
            double ha = ta + M_PI + (t ? 0.6 : -0.6);
            DrawLine(ex, ey, ex + (int)(11 * cos(ha)), ey + (int)(11 * sin(ha)), WHITE);
        }
    }
}

/* Magnifying-glass icon in the top bar.  Replaces the old separate
 * search row: tapping it opens the Search sub-page (see on_event).
 * Line-art style matching home/sync.  Position: left of the sync
 * button. */
void
draw_search_icon(void)
{
    int w = ScreenWidth();
    int y0 = g_state.panel_h;
    int col = BLACK;
    int ic_w = 96;
    int menu_x = w - 96 - 8;
    int ic_x = menu_x - 2 * ic_w;
    int ic_y = y0 + (TOP_BAR_H - ic_w) / 2;
    int cx = ic_x + ic_w / 2 - 5; /* ring centre, offset for the handle */
    int cy = ic_y + ic_w / 2 - 5;
    int r = 18;

    /* Outlined ring (polyline; DrawCircle fills). */
    int px = 0, py = 0;
    for (int s = 0; s <= 16; s++) {
        double a = s * 2 * M_PI / 16.0;
        int    x = cx + (int)(r * cos(a));
        int    yy = cy + (int)(r * sin(a));
        if (s > 0) {
            DrawLine(px, py, x, yy, col);
            DrawLine(px, py + 1, x, yy + 1, col);
        }
        px = x;
        py = yy;
    }
    /* Handle: double-width diagonal from the ring edge out to the
     * corner of the icon box. */
    DrawLine(cx + r - 4, cy + r - 4, cx + r + 10, cy + r + 10, col);
    DrawLine(cx + r - 3, cy + r - 5, cx + r + 11, cy + r + 9, col);
}

/* Search sub-page body: the input row (magnifier + text box) at the
 * top, then the previously committed search terms below.  Tapping the
 * input opens the firmware keyboard; tapping a term re-runs that
 * search (see on_event). */
void
draw_search_tab(void)
{
    int top, bot, cell_w, cell_h;
    (void)cell_w;
    (void)cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int w = ScreenWidth();
    FillArea(0, top, w, bot - top, WHITE);
    DrawLine(0, top, w, top, BLACK);
    LOG("[bookshelf] draw_search_tab page=%d\n", g_state.page);

    /* ── input row: magnifier icon + text box ── */
    int gx = 30, gy = top + SEARCH_ROW_H / 2 - 8;
    int px = 0, py = 0;
    for (int s = 0; s <= 16; s++) {
        double a = s * 2 * M_PI / 16.0;
        int    x = gx + (int)(13 * cos(a));
        int    yy = gy + (int)(13 * sin(a));
        if (s > 0) {
            DrawLine(px, py, x, yy, BLACK);
            DrawLine(px, py + 1, x, yy + 1, BLACK);
        }
        px = x;
        py = yy;
    }
    DrawLine(gx + 9, gy + 10, gx + 22, gy + 23, BLACK);
    DrawLine(gx + 10, gy + 9, gx + 23, gy + 22, BLACK);

    ifont *f = OpenFont(DEFAULTFONT, 28, 0);
    if (f != NULL) {
        int tx = 64;
        int tw = w - 128;
        int ty = top + 10;
        int th = SEARCH_ROW_H - 20;
        DrawRect(tx, ty, tw, th, BLACK);
        FillArea(tx + 1, ty + 1, tw - 2, th - 2, g_state.search_kb ? BLACK : WHITE);
        if (g_state.query[0] != '\0') {
            SetFont(f, g_state.search_kb ? WHITE : BLACK);
            DrawString(tx + 10, ty + (th - 28) / 2 - 2, g_state.query);
        } else if (!g_state.search_kb) {
            SetFont(f, BLACK);
            DrawString(tx + 10, ty + (th - 28) / 2 - 2, i18n("search.ph"));
        }
        /* cursor when the keyboard is editing the input */
        if (g_state.search_kb) {
            int cursor_x = tx + 10 + StringWidth(g_state.query) + 1;
            DrawLine(cursor_x, ty + 6, cursor_x, ty + th - 6, WHITE);
        }
        CloseFont(f);
    }

    /* ── previously searched terms ── */
    int n = store_search_count();
    if (n == 0) {
        ifont *ef = OpenFont(DEFAULTFONT, 28, 0);
        if (ef != NULL) {
            SetFont(ef, DGRAY);
            const char *msg = i18n("search.empty");
            DrawString((w - StringWidth(msg)) / 2, top + SEARCH_ROW_H + 60, msg);
            CloseFont(ef);
        }
        return;
    }
    int ps = history_pagesize();
    if (ps < 1)
        ps = 1;
    int pages = (n + ps - 1) / ps;
    if (g_state.page >= pages)
        g_state.page = pages - 1;
    if (g_state.page < 0)
        g_state.page = 0;
    char terms[SEARCH_HISTORY_MAX][MAX_QUERY_LEN];
    int  got = store_search_list(terms, SEARCH_HISTORY_MAX, g_state.page * ps);
    int  y = top + SEARCH_ROW_H;
    for (int i = 0; i < got && y + SEARCH_HISTORY_ROW_H <= bot; i++) {
        ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
        if (tf != NULL) {
            SetFont(tf, BLACK);
            char trunc[MAX_QUERY_LEN];
            strncpy(trunc, terms[i], sizeof trunc - 1);
            trunc[sizeof trunc - 1] = '\0';
            int maxw = w - 80;
            while (StringWidth(trunc) > maxw && strlen(trunc) > 4)
                trunc[strlen(trunc) - 1] = '\0';
            DrawString(24, y + (SEARCH_HISTORY_ROW_H - 28) / 2 - 2, trunc);
            CloseFont(tf);
        }
        DrawLine(20, y + SEARCH_HISTORY_ROW_H - 1, w - 20, y + SEARCH_HISTORY_ROW_H - 1, LGRAY);
        y += SEARCH_HISTORY_ROW_H;
    }
}

/* Number of downloads still pending (queued or in flight) — shown as a
 * badge on the downloads icon so the user can see work is in progress. */
int
downloads_pending(void)
{
    int n = 0;
    for (int i = 0; i < g_download_count; i++)
        if (g_downloads[i].state == 0 || g_downloads[i].state == 1)
            n++;
    return n;
}

/* 1 = the firmware's panel painter never activated (PanelHeight()==0 at
 * init, the live-device case); we draw the status strip ourselves. */
int g_self_panel = 0;

static void
draw_circle_outline(int cx, int cy, int r)
{
    int px = cx + r, py = cy;
    for (int s = 1; s <= 20; s++) {
        double a = s * 2 * M_PI / 20.0;
        int    x = cx + (int)(r * cos(a));
        int    y = cy + (int)(r * sin(a));
        DrawLine(px, py, x, y, BLACK);
        px = x;
        py = y;
    }
}

/* Self-drawn replacement for the firmware status strip.  On the live
 * device the panel painter never activates for this task, so without a
 * fallback the screen would show no clock/battery bar and our home row
 * would sit flush against the top edge.  Mirrors the stock collapsed
 * bar: day + 24h time on the left, frontlight bulb + battery on the
 * right, separator line at the bottom. */
void
draw_system_strip(void)
{
    int w = ScreenWidth();
    int h = g_state.panel_h;

    FillArea(0, 0, w, h, WHITE);
    DrawLine(0, h - 1, w, h - 1, BLACK);

    time_t    now = time(NULL);
    struct tm tmv;
    char      buf[32];
    localtime_r(&now, &tmv);
    strftime(buf, sizeof buf, "%a %H:%M", &tmv);
    ifont *tf = OpenFont(DEFAULTFONT, 40, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(24, (h - 40) / 2, buf);
        CloseFont(tf);
    }

    /* Frontlight bulb: circle with short rays. */
    int lx = w - 176;
    int ly = h / 2;
    draw_circle_outline(lx, ly, 12);
    for (int a = 0; a < 8; a++) {
        double ang = a * M_PI / 4.0 + M_PI / 8.0;
        DrawLine(lx + (int)(16 * cos(ang)),
                 ly + (int)(16 * sin(ang)),
                 lx + (int)(22 * cos(ang)),
                 ly + (int)(22 * sin(ang)),
                 BLACK);
    }

    /* Battery: outline + nub + fill proportional to charge. */
    int bw = 84, bh = 40;
    int bx = w - 116;
    int by = (h - bh) / 2;
    DrawRect(bx, by, bw, bh, BLACK);
    FillArea(bx + bw + 1, by + bh / 2 - 7, 6, 14, BLACK);
    int lvl = GetBatteryPower();
    if (lvl < 0)
        lvl = 0;
    if (lvl > 100)
        lvl = 100;
    int fw = (bw - 8) * lvl / 100;
    if (fw > 0)
        FillArea(bx + 4, by + 4, fw, bh - 8, BLACK);
}

/* Paint the top status strip: firmware-painted when the panel painter
 * is active (emulator), self-drawn when it never activates (device). */
void
stamp_panel(void)
{
    if (g_self_panel)
        draw_system_strip();
    else
        iv_update_panel(0);
}

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
    int t = g_state.panel_h + TOP_BAR_H;
    int b = ScreenHeight() - PAGER_H;
    if (g_state.menu_open || g_state.more_open)
        b = ScreenHeight();
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
    int t = g_state.panel_h + TOP_BAR_H;
    int b = ScreenHeight() - PAGER_H;
    if (g_state.menu_open || g_state.more_open)
        b = ScreenHeight();
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

void
draw_series_stack_badge(int cx, int cy, int cw, int ch, int count)
{
    /* Outline the cover rect so it reads as the top book of the stack. */
    DrawRect(cx, cy, cw, ch, BLACK);

    char badge[8];
    snprintf(badge, sizeof badge, "%d", count);
    ifont *bf = OpenFont(DEFAULTFONTB, 20, 0);
    if (bf != NULL) {
        SetFont(bf, WHITE);
        int bw = StringWidth(badge) + 12;
        int bh = 26;
        int bx = cx + cw - bw - 2;
        int by = cy + 2;
        FillArea(bx, by, bw, bh, BLACK);
        DrawString(bx + 6, by + 2, badge);
        CloseFont(bf);
    }
}

void
draw_thumbnail(int x, int y, int w, int h, const TileRow *tr, int vi)
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
            draw_series_stack_badge(cx, cy, cww, chh, tr->series_count);
        int tx0 = cx + cww + 16;
        int tw0 = (x + w - pad) - tx0;
        if (tw0 < 64)
            tw0 = 64;
        const char *label = tr->is_series ? tr->series_name : b->title;
        ifont      *f = OpenFont(DEFAULTFONTB, 30, 0);
        if (f != NULL) {
            SetFont(f, BLACK);
            char truncated[MAX_TITLE_LEN];
            snprintf(truncated, sizeof truncated, "%s", label);
            while (StringWidth(truncated) > tw0 && strlen(truncated) > 4)
                truncated[strlen(truncated) - 1] = '\0';
            DrawString(tx0, y + pad + 8, truncated);
            CloseFont(f);
        }
        if (!tr->is_series && b->author[0] != '\0') {
            ifont *af = OpenFont(DEFAULTFONT, 24, 0);
            if (af != NULL) {
                SetFont(af, DGRAY);
                char truncated[80];
                snprintf(truncated, sizeof truncated, "%s", b->author);
                while (StringWidth(truncated) > tw0 && strlen(truncated) > 4)
                    truncated[strlen(truncated) - 1] = '\0';
                DrawString(tx0, y + pad + 8 + 40, truncated);
                CloseFont(af);
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
        draw_series_stack_badge(cx, cy, cw, ch, tr->series_count);

    /* Caption: series name for cards, title for books. */
    int         cap_y = cy + ch + 6;
    const char *label = tr->is_series ? tr->series_name : b->title;
    ifont      *f = OpenFont(DEFAULTFONTB, 22, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        char truncated[MAX_TITLE_LEN];
        snprintf(truncated, sizeof truncated, "%s", label);
        while (StringWidth(truncated) > w - 8 && strlen(truncated) > 4)
            truncated[strlen(truncated) - 1] = '\0';
        DrawString(x + 4, cap_y, truncated);
        CloseFont(f);
    }

    /* Second line: author for books, omitted for series cards. */
    if (!tr->is_series && b->author[0] != '\0') {
        ifont *af = OpenFont(DEFAULTFONT, 18, 0);
        if (af != NULL) {
            SetFont(af, DGRAY);
            char truncated[80];
            snprintf(truncated, sizeof truncated, "%s", b->author);
            while (StringWidth(truncated) > w - 8 && strlen(truncated) > 4)
                truncated[strlen(truncated) - 1] = '\0';
            DrawString(x + 4, cap_y + 24, truncated);
            CloseFont(af);
        }
    }
}

/* History-term rows that fit below the input row on the Search page. */

int
history_pagesize(void)
{
    int top, bot, cell_w, cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int rows = (bot - top - SEARCH_ROW_H) / SEARCH_HISTORY_ROW_H;
    return rows < 1 ? 1 : rows;
}
/* Page count for the active tab: the library pages the cover grid, the
 * search page pages the history terms.  Always >= 1. */
int
current_pages(void)
{
    int n, ps;
    if (g_state.tab == TAB_SEARCH) {
        n = store_search_count();
        ps = history_pagesize();
    } else {
        n = g_view_total;
        ps = view_pagesize();
    }
    if (ps < 1)
        ps = 1;
    int pages = (n + ps - 1) / ps;
    return pages < 1 ? 1 : pages;
}

/* Tally the open download queue (falling back to the whole-batch tally
 * when a download-all batch is active, since the queue only holds the
 * current slice).  Shared by the popup bar and its status line. */
void
dl_progress_metrics(int *total_out, int *done_out, int *failed_out, int *active_out)
{
    int total = 0, done = 0, failed = 0, active = 0;
    for (int i = 0; i < g_download_count; i++) {
        total++;
        if (g_downloads[i].state == 2)
            done++;
        else if (g_downloads[i].state == 3)
            failed++;
        if (g_downloads[i].state == 0 || g_downloads[i].state == 1)
            active++;
    }
    if (g_dl_batch_total > 0) {
        total = g_dl_batch_total;
        done = g_dl_batch_done;
        failed = g_dl_batch_failed;
    }
    /* Retries can settle the same slot twice; keep the fill bounded. */
    if (done > total)
        done = total;
    if (done + failed > total)
        failed = total - done;
    if (g_dl_batch_active)
        active++;
    LOG("[bookshelf] dl_progress done=%d failed=%d total=%d active=%d\n",
        done,
        failed,
        total,
        active);
    if (total_out)
        *total_out = total;
    if (done_out)
        *done_out = done;
    if (failed_out)
        *failed_out = failed;
    if (active_out)
        *active_out = active;
}

/* Single batch progress bar for the download popup: one bar for the
 * whole open batch, filled by done/total, with a striped overlay on the
 * unfilled portion while anything is still in flight.  The bar spans
 * [x, x+w); the label sits above it. */
void
draw_dl_progress(int x, int y, int w)
{
    int total = 0, done = 0, failed = 0, active = 0;
    dl_progress_metrics(&total, &done, &failed, &active);
    if (total <= 0)
        return;

    ifont *f = OpenFont(DEFAULTFONT, 22, 0);
    int    label_h = 26;
    char   label[48];
    if (active > 0)
        snprintf(label, sizeof label, i18n("dl.progress"), done, total);
    else if (failed > 0 && done == 0)
        snprintf(label, sizeof label, i18n("dl.failed_count"), failed);
    else
        snprintf(label, sizeof label, i18n("dl.complete"), done);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(x + 4, y + 2, label);
        CloseFont(f);
    }

    int bar_y = y + label_h;
    int bar_h = DL_BAR_H - label_h - 6;
    if (bar_h < 8)
        bar_h = 8;
    if (w < 16)
        w = 16;
    DrawRect(x, bar_y, w, bar_h, BLACK);
    int settled = done + failed;
    int fill = (settled * w) / total;
    if (fill > 2)
        FillArea(x + 1, bar_y + 1, fill - 2, bar_h - 2, BLACK);
    /* Striped "in progress" overlay across the unfinished portion. */
    if (active > 0) {
        for (int sx = x + 1 + fill; sx < x + w - 1; sx += 6)
            DrawLine(sx, bar_y + 1, sx + 2, bar_y + bar_h - 2, DGRAY);
    }
}

/* Download-progress popup: a centred modal sheet over a dimmed shelf.
 * Title, the current item, the batch progress bar, and a status line.
 * Shown whenever downloads run (book press, context-menu Download,
 * Download all).  While any download is active the popup is
 * non-dismissable — downloads never run in the background; once the
 * queue drains a tap or Back closes it.  When the popup was opened by
 * a single-book press (dl_popup_auto_open), download_tick() launches
 * the reader as soon as the queue drains. */
void
draw_dl_popup(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    /* Dim the shelf body below the top bar, so the top-bar icons (the
     * spinning sync glyph among them) stay fully visible while the
     * download runs. */
    for (int yy = g_state.panel_h + TOP_BAR_H; yy < h; yy += 2)
        DrawLine(0, yy, w, yy, LGRAY);

    int pw = w * 3 / 4;
    int ph = 320;
    int px = (w - pw) / 2;
    LOG("[bookshelf] draw_dl_popup open auto_open=%d count=%d\n",
        g_state.dl_popup_auto_open,
        g_download_count);
    int py = (h - ph) / 2;
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 30, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + CTX_PAD, py + 18, i18n("dl.title"));
        CloseFont(tf);
    }
    DrawLine(px + CTX_PAD, py + CTX_TITLE_H - 1, px + pw - CTX_PAD, py + CTX_TITLE_H - 1, LGRAY);

    /* Current item: the first queued/in-flight entry, else the last one. */
    const DownloadItem *cur = NULL;
    for (int i = 0; i < g_download_count; i++) {
        if (g_downloads[i].state == 0 || g_downloads[i].state == 1) {
            cur = &g_downloads[i];
            break;
        }
    }
    if (cur == NULL && g_download_count > 0)
        cur = &g_downloads[g_download_count - 1];
    if (cur != NULL) {
        ifont *cf = OpenFont(DEFAULTFONTB, 26, 0);
        if (cf != NULL) {
            SetFont(cf, BLACK);
            char trunc[MAX_TITLE_LEN];
            snprintf(trunc, sizeof trunc, "%s", cur->title);
            while (StringWidth(trunc) > pw - 2 * CTX_PAD && strlen(trunc) > 4)
                trunc[strlen(trunc) - 1] = '\0';
            DrawString(px + CTX_PAD, py + CTX_TITLE_H + 22, trunc);
            CloseFont(cf);
        }
    }

    draw_dl_progress(px + CTX_PAD, py + CTX_TITLE_H + 64, pw - 2 * CTX_PAD);

    int total = 0, done = 0, failed = 0, active = 0;
    dl_progress_metrics(&total, &done, &failed, &active);
    ifont *sf = OpenFont(DEFAULTFONT, 22, 0);
    if (sf != NULL) {
        SetFont(sf, DGRAY);
        const char *hint;
        if (active > 0)
            hint = i18n("dl.in_progress");
        else if (failed > 0 && done + failed >= total)
            hint = i18n("dl.failed");
        else
            hint = i18n("dl.tap_close");
        DrawString(px + CTX_PAD, py + CTX_TITLE_H + 64 + DL_BAR_H + 12, hint);
        CloseFont(sf);
    }
}

/* Repaint the whole shelf (top bar, body, pager) in the current tab,
 * then the download popup on top when one is open.  Centralises the
 * sequence every state change needs. */
void
redraw_shelf(void)
{
    if (g_state.launcher_open) {
        draw_overlay_launcher();
        FullUpdate();
        return;
    }
    FillArea(0, g_state.panel_h, ScreenWidth(), ScreenHeight() - g_state.panel_h, WHITE);
    draw_top_bar();
    if (g_state.tab == TAB_SEARCH)
        draw_search_tab();
    else
        draw_grid();
    draw_pager();
    if (g_state.dl_popup)
        draw_dl_popup();
    FullUpdate();
}

void
draw_grid(void)
{
    /* Layout: [system panel] [our top bar] [grid] [pager].
     * The system panel renders at the TOP of the screen (PANEL_NO_FB_OFFSET
     * flag), occupying rows [0, panel_h).  Everything we draw is offset
     * below it; the pager sits at the very bottom with no reservation.
     */
    int top, bot, cell_w, cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    /* Clear the grid area first so cells from a previous page don't
     * bleed through.  We do this every redraw, not just on page change,
     * so partial updates stay simple.
     */
    FillArea(0, top, ScreenWidth(), bot - top, WHITE);
    DrawLine(0, top, ScreenWidth(), top, BLACK);
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
    for (int row = 0; row < rows; row++) {
        for (int col = 0; col < cols; col++) {
            if (drawn >= g_row_count)
                goto done;
            int tx = 8 + col * cell_w;
            int ty = top + 4 + row * cell_h;
            int tw = cell_w - 8;
            int th = cell_h - 6;
            draw_thumbnail(tx, ty, tw, th, &g_rows[drawn], g_state.page * ps + drawn);
            drawn++;
        }
    }
done:
    cover_schedule_next();
}

/* Fetch one not-yet-loaded visible cover per tick, then blit just that
 * tile.  Running on the event loop keeps the SDK single-threaded; the
 * blocking download is short (cached PNGs over the loopback link). */
void
cover_tick(void *ctx)
{
    (void)ctx;
    LOG("[bookshelf] cover_tick ENTER page=%d view=%d armed->0\n", g_state.page, g_view_total);
    g_cover_armed = 0;

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
    if (target < 0)
        return; /* nothing pending on this page */

    const char *bid = page_row_id(target - page_start);
    if (bid == NULL)
        return;
    CoverSlot *s = cover_slot(bid, 1);
    LOG("[bookshelf] cover_tick target=%d id=%s slot=%p\n", target, bid, (void *)s);
    s->state = 1;

    ibitmap *bmp = NULL;

    /* Try the on-disk cover cache first — avoids a network round-trip
     * when the cover was fetched in a previous session. */
    if (cover_cache_load(bid, &bmp) == 0) {
        LOG("[bookshelf] cover_tick cache hit id=%s\n", bid);
    } else {
        char url[MAX_URL_LEN + 128];
        snprintf(url,
                 sizeof url,
                 "%s/api/v1/books/%s/cover?access_token=%s",
                 g_state.api_base,
                 bid,
                 g_state.api_token);

        int rsize = 0;
        LOG("[bookshelf] cover_tick downloading url=%s\n", url);
        char *data = QuickDownload(url, &rsize, HTTP_TIMEOUT);
        LOG("[bookshelf] cover_tick downloaded data=%p rsize=%d\n", (void *)data, rsize);
        if (data != NULL && rsize > 8) {
            /* Persist the raw PNG so the next launch can skip the
     * network entirely. */
            cover_cache_save(bid, data, rsize);
            FILE *f = fopen(COVER_TMP, "wb");
            if (f != NULL) {
                fwrite(data, 1, (size_t)rsize, f);
                fclose(f);
                LOG("[bookshelf] cover_tick load_cover_scaled begin\n");
                bmp = load_cover_scaled(COVER_TMP);
                LOG("[bookshelf] cover_tick load_cover_scaled done bmp=%p\n", (void *)bmp);
            }
        }
        if (data != NULL) {
            LOG("[bookshelf] cover_tick free(data) begin\n");
            free(data);
            LOG("[bookshelf] cover_tick free(data) done\n");
        }
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
     * on-screen blit is skipped while a modal owns the framebuffer, so a
     * single-tile PartialUpdate can't punch a hole through the overlay's
     * dim mask (the full redraw on close then shows the now-cached cover). */
    int modal = g_state.ctx_open || g_state.menu_open || g_state.more_open ||
                g_state.settings_open || g_state.dl_popup;
    LOG("[bookshelf] cover_tick blit begin modal=%d\n", modal);

    int tx, ty, tw, th;
    if (!modal && tile_rect_for_index(target, &tx, &ty, &tw, &th)) {
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
    int h = ScreenHeight();
    /* Pager sits at the very bottom; the system panel is at the top. */
    int y = h - PAGER_H;
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
     * fill is skipped and label_color forces grey). */
    int by = y + (PAGER_H - 64) / 2;
    int gray = 0xAAAAAA;
    /* < prev */
    draw_button(12, by, 96, 64, 0, i18n("pager.prev"), 28, g_state.page > 0 ? 0 : gray);
    /* << first page */
    draw_button(116, by, 96, 64, 0, i18n("pager.first"), 28, g_state.page > 0 ? 0 : gray);
    /* >> last page */
    draw_button(
        w - 212, by, 96, 64, 0, i18n("pager.last"), 28, g_state.page + 1 < pages ? 0 : gray);
    /* > next */
    draw_button(
        w - 108, by, 96, 64, 0, i18n("pager.next"), 28, g_state.page + 1 < pages ? 0 : gray);
    CloseFont(f);
}

void
draw_overlay_menu(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    FillArea(0, g_state.panel_h, w, h - g_state.panel_h, BLACK);
    int pw = w * 3 / 4;
    FillArea(0, g_state.panel_h, pw, h - g_state.panel_h, WHITE);
    DrawLine(pw, g_state.panel_h, pw, h, BLACK);

    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(24, g_state.panel_h + 32, i18n("action.menu"));
        CloseFont(f);
    }

    const char *labels[] = {
        "group.all",
        "group.author",
        "group.series",
        "group.recent",
    };
    int n = (int)(sizeof labels / sizeof labels[0]);
    int y0 = g_state.panel_h + 96;
    int item_h = 88;
    for (int i = 0; i < n; i++) {
        int sel = (i == (int)g_state.group);
        FillArea(12, y0 + i * item_h, pw - 24, item_h - 12, sel ? BLACK : WHITE);
        ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
        if (tf != NULL) {
            SetFont(tf, sel ? WHITE : BLACK);
            DrawString(32, y0 + i * item_h + (item_h - 28) / 2 - 2, i18n(labels[i]));
            CloseFont(tf);
        }
    }
}

void
draw_overlay_more(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    FillArea(0, g_state.panel_h, w, h - g_state.panel_h, BLACK);
    int pw = w * 3 / 4;
    int px = w - pw;
    FillArea(px, g_state.panel_h, pw, h - g_state.panel_h, WHITE);
    DrawLine(px, g_state.panel_h, px, h, BLACK);

    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(px + 24, g_state.panel_h + 32, i18n("action.more"));
        CloseFont(f);
    }
    const char *labels[] = {
        "action.sync",
        "sort.title_az",
        "sort.author",
        "sort.series",
        "sort.recent",
        "view.grid",
        "view.list",
        "action.download_all",
        "action.settings",
        "action.apps",
    };
    int n = (int)(sizeof labels / sizeof labels[0]);
    int y0 = g_state.panel_h + MORE_Y0;
    for (int i = 0; i < n; i++) {
        int sel = 0;
        if (i == 0 && g_state.sync_state == 1)
            sel = 1;
        if (i >= 1 && i <= 4 && (i - 1) == (int)g_state.sort)
            sel = 1;
        if (i == MORE_GRID_IDX && g_state.view_mode == VIEW_GRID)
            sel = 1;
        if (i == MORE_LIST_IDX && g_state.view_mode == VIEW_LIST)
            sel = 1;
        FillArea(px + 12, y0 + i * MORE_ITEM_H, pw - 24, MORE_ITEM_H - 12, sel ? BLACK : WHITE);
        ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
        if (tf != NULL) {
            SetFont(tf, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * MORE_ITEM_H + (MORE_ITEM_H - 28) / 2 - 2, i18n(labels[i]));
            CloseFont(tf);
        }
    }
}

void
draw_status_line(void)
{
    /* Currently unused — status is shown via sync-button feedback and
     * the top-bar title (active query).  Kept as an extension point.
     */
}

/* ── settings overlay ────────────────────────────────────────────────── */

/* Which settings row currently owns the on-screen keyboard:
 * 0 = none, 1 = API host, 2 = API key. */
int g_settings_edit = 0;

/* Scratch buffer the keyboard edits; committed on close. */
char g_settings_kb_buf[260];

void
settings_keyboard_handler(char *buffer)
{
    const char *val = buffer ? buffer : "";
    if (g_settings_edit == 1) {
        /* Normalise a bare host[:port] into a full http:// URL so the
 * endpoint builder always gets a scheme. */
        if (strncmp(val, "http://", 7) != 0 && strncmp(val, "https://", 8) != 0) {
            char tmp[260];
            snprintf(tmp, sizeof tmp, "http://%s", val);
            snprintf(g_state.api_base, sizeof g_state.api_base, "%s", tmp);
        } else {
            snprintf(g_state.api_base, sizeof g_state.api_base, "%s", val);
        }
    } else if (g_settings_edit == 2) {
        snprintf(g_state.api_token, sizeof g_state.api_token, "%s", val);
    }
    g_settings_edit = 0;
    draw_overlay_settings();
    /* The on-screen keyboard draws full-screen and wipes the top status
     * strip; re-stamp it before the flush so the panel survives the commit
     * redraw (draw_overlay_settings clears only from panel_h). */
    stamp_panel();
    FullUpdate();
}

/* Full-screen settings page.  Three editable rows (API host, API key,
 * reader app) plus Save and Back buttons.  The API host / key rows open
 * the on-screen keyboard; the reader row cycles through Auto plus every
 * detected reader.  Generous row heights keep the targets comfortable on
 * the 300 DPI e-ink panel. */

const char *
settings_reader_label(void)
{
    if (g_state.reader_pref > 0 && g_state.reader_pref <= g_reader_count)
        return g_readers[g_state.reader_pref - 1].label;
    return i18n("settings.reader_auto");
}

void
settings_draw_row(int y, const char *label, const char *value, int editing)
{
    int w = ScreenWidth();
    int mx = 32; /* left/right margin */
    FillArea(mx, y, w - 2 * mx, SETTINGS_ROW_H - 12, WHITE);
    DrawRect(mx, y, w - 2 * mx, SETTINGS_ROW_H - 12, BLACK);
    if (editing)
        FillArea(mx + 2, y + 2, w - 2 * mx - 4, SETTINGS_ROW_H - 16, BLACK);

    ifont *lf = OpenFont(DEFAULTFONTB, 26, 0);
    if (lf != NULL) {
        SetFont(lf, editing ? WHITE : BLACK);
        DrawString(mx + 16, y + 12, label);
        CloseFont(lf);
    }
    ifont *vf = OpenFont(DEFAULTFONT, 30, 0);
    if (vf != NULL) {
        SetFont(vf, editing ? WHITE : BLACK);
        DrawString(mx + 16, y + 52, value);
        CloseFont(vf);
    }
}

void
settings_draw_button(int y, const char *label, int filled)
{
    int w = ScreenWidth();
    int mx = 32;
    FillArea(mx, y, w - 2 * mx, SETTINGS_BTN_H - 12, filled ? BLACK : WHITE);
    DrawRect(mx, y, w - 2 * mx, SETTINGS_BTN_H - 12, BLACK);
    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, filled ? WHITE : BLACK);
        int tw = StringWidth(label);
        DrawString((w - tw) / 2, y + (SETTINGS_BTN_H - 12 - 32) / 2, label);
        CloseFont(f);
    }
}

void
draw_overlay_settings(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    FillArea(0, g_state.panel_h, w, h - g_state.panel_h, WHITE);

    ifont *tf = OpenFont(DEFAULTFONTB, 40, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(32, g_state.panel_h + 28, i18n("settings.title"));
        CloseFont(tf);
    }
    DrawLine(0, g_state.panel_h + 92, w, g_state.panel_h + 92, BLACK);

    int y = g_state.panel_h + 112;
    settings_draw_row(y, i18n("settings.api_host"), g_state.api_base, g_settings_edit == 1);
    y += SETTINGS_ROW_H;
    settings_draw_row(y, i18n("settings.api_key"), g_state.api_token, g_settings_edit == 2);
    y += SETTINGS_ROW_H;
    settings_draw_row(y, i18n("settings.reader"), settings_reader_label(), 0);
    y += SETTINGS_ROW_H + 24;
    settings_draw_button(y, i18n("settings.save"), 1);
    y += SETTINGS_BTN_H;
    settings_draw_button(y, i18n("settings.back"), 0);
}
