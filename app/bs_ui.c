/* bs_ui.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
#include "bs_browser.h"
#include "bs_config.h"
#include "bs_downloads.h"
#include "bs_extract.h"
#include "bs_launcher.h"
#include "bs_model.h"
#include "bs_progress.h"
#include "bs_store.h"
#include "bs_ui.h"
#include "bs_worker.h"

/* ── UTF-8 helpers ──────────────────────────────────────────────────── */

/* Cap `s` at `cap` bytes (NUL at cap-1), then walk back so the string
 * never ends mid-character: the continuation bytes (0x80..0xBF) of a
 * truncated multibyte char are removed together with its lead byte.
 * ASCII text and complete trailing characters are left untouched, so a
 * byte budget never leaves a dangling half character.  Every
 * title/author/suggestion display truncation in this file goes through
 * this. */
static void
utf8_cap(char *s, size_t cap)
{
    if (cap < 1)
        return;
    size_t n = cap - 1;
    s[n] = '\0';
    /* Walk back over continuation bytes to the last character's lead. */
    size_t i = n;
    while (i > 0 && ((unsigned char)s[i - 1] & 0xC0) == 0x80)
        i--;
    if (i == 0)
        return; /* no lead byte to anchor on; leave as-is */
    unsigned char lead = (unsigned char)s[i - 1];
    if ((lead & 0xC0) != 0xC0)
        return; /* ends with an ASCII char: already a boundary */
    /* A multibyte char is kept only when every continuation byte it
     * needs survived the cap; otherwise the whole char goes. */
    size_t expect = (lead & 0xE0) == 0xC0 ? 1 : (lead & 0xF0) == 0xE0 ? 2 : 3;
    if (n - i < expect)
        i--;
    s[i] = '\0';
}

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

/* ── top bar ─────────────────────────────────────────────────────────── */

/* Line-art globe (Kavita / online): circle, equator, meridian.  Drawn
 * in the common 52x52 icon box. */
static void
draw_globe_icon(int x, int y, int col)
{
    int cx = x + 26, cy = y + 26, r = 24;
    int px = 0, py = 0;
    for (int s = 0; s <= 16; s++) {
        double a = s * 2 * M_PI / 16.0;
        int    xx = cx + (int)(r * cos(a));
        int    yy = cy + (int)(r * sin(a));
        if (s > 0) {
            DrawLine(px, py, xx, yy, col);
            DrawLine(px, py + 1, xx, yy + 1, col);
        }
        px = xx;
        py = yy;
    }
    /* equator */
    px = py = 0;
    for (int s = 0; s <= 16; s++) {
        double a = s * 2 * M_PI / 16.0;
        int    xx = cx + (int)(r * cos(a));
        int    yy = cy + (int)(r * 0.42 * sin(a));
        if (s > 0) {
            DrawLine(px, py, xx, yy, col);
            DrawLine(px, py + 1, xx, yy + 1, col);
        }
        px = xx;
        py = yy;
    }
    /* meridian */
    px = py = 0;
    for (int s = 0; s <= 16; s++) {
        double a = s * 2 * M_PI / 16.0;
        int    xx = cx + (int)(r * 0.42 * cos(a));
        int    yy = cy + (int)(r * sin(a));
        if (s > 0) {
            DrawLine(px, py, xx, yy, col);
            DrawLine(px, py + 1, xx, yy + 1, col);
        }
        px = xx;
        py = yy;
    }
}

/* Line-art open book (Local): two pages over a spine, in the 52x52
 * icon box. */
static void
draw_book_icon(int x, int y, int col)
{
    int cx = x + 26, cy = y + 26;
    DrawLine(cx - 24, cy + 20, cx - 24, cy - 16, col);
    DrawLine(cx - 24, cy - 16, cx, cy - 6, col);
    DrawLine(cx + 24, cy + 20, cx + 24, cy - 16, col);
    DrawLine(cx + 24, cy - 16, cx, cy - 6, col);
    DrawLine(cx - 24, cy + 20, cx, cy + 24, col);
    DrawLine(cx + 24, cy + 20, cx, cy + 24, col);
}

/* Line-art folder (Folder source): tab + body, in the 52x52 icon
 * box. */
static void
draw_folder_icon(int x, int y, int col)
{
    DrawLine(x + 3, y + 10, x + 3, y + 50, col);
    DrawLine(x + 3, y + 50, x + 49, y + 50, col);
    DrawLine(x + 49, y + 50, x + 49, y + 10, col);
    DrawLine(x + 49, y + 10, x + 21, y + 10, col);
    DrawLine(x + 21, y + 10, x + 21, y + 4, col);
    DrawLine(x + 21, y + 4, x + 3, y + 4, col);
    DrawLine(x + 3, y + 4, x + 3, y + 10, col);
}

/* Short label of the active source (shown in the button). */
static const char *
source_short_label(void)
{
    switch (g_state.source) {
    case SOURCE_LOCAL:
        return i18n("source.local");
    case SOURCE_FOLDER:
        return i18n("source.folder");
    default:
        return i18n("source.kavita");
    }
}

/* The source button: the active library source's icon + label.  The
 * chooser opens on tap (hit_top_bar → 6). */
static void
draw_source_button(void)
{
    int col = BLACK;
    int x0 = SOURCE_BTN_X;
    FillArea(x0, 0, SOURCE_BTN_W, TOP_BAR_H, WHITE);
    int cy = TOP_BAR_H / 2;
    /* Icon in the common 52px box, bottom-aligned with the house icon
     * next to it; label at a larger font beside it. */
    int ic_x = x0 + 8, ic_y = cy - 24;
    switch (g_state.source) {
    case SOURCE_LOCAL:
        draw_book_icon(ic_x, ic_y, col);
        break;
    case SOURCE_FOLDER:
        draw_folder_icon(ic_x, ic_y, col);
        break;
    default:
        draw_globe_icon(ic_x, ic_y, col);
        break;
    }
    ifont *f = OpenFont(DEFAULTFONT, 30, 0);
    if (f != NULL) {
        SetFont(f, col);
        DrawString(ic_x + 60, cy - 15, source_short_label());
        CloseFont(f);
    }
}

void
draw_top_bar(void)
{
    int w = ScreenWidth();
    int y0 = 0; /* top bar sits at the very top; the system panel is at the bottom */
    int col = BLACK;

    FillArea(0, y0, w, TOP_BAR_H, WHITE);
    DrawLine(0, y0 + TOP_BAR_H, w, y0 + TOP_BAR_H, col);

    /* Left button: back-arrow when drilled into a series or on the
     * Search page, the stock house icon otherwise.  Both are drawn
     * inside the common 52x52 icon box centred in the button, matching
     * the other top-bar icons. */
    int home_w = 96;
    int home_x = 8;
    int home_y = y0 + (TOP_BAR_H - home_w) / 2;
    int hcx = home_x + home_w / 2;
    int hcy = home_y + home_w / 2;
    if (g_drilled_series[0] != '\0' || g_state.tab == TAB_SEARCH) {
        /* Left-pointing chevron arrow. */
        int ax = hcx - 8;
        int ay = hcy;
        DrawLine(ax, ay, ax + 26, ay - 26, col);
        DrawLine(ax, ay, ax + 26, ay + 26, col);
        DrawLine(ax + 4, ay, ax + 30, ay - 26, col);
        DrawLine(ax + 4, ay, ax + 30, ay + 26, col);
    } else {
        /* house outline (pentagon + floor break for door), scaled to
         * the icon box */
        DrawLine(hcx - 24, hcy + 8, hcx - 24, hcy + 26, col);
        DrawLine(hcx - 24, hcy + 8, hcx, hcy - 24, col);
        DrawLine(hcx, hcy - 24, hcx + 24, hcy + 8, col);
        DrawLine(hcx + 24, hcy + 8, hcx + 24, hcy + 26, col);
        DrawLine(hcx - 24, hcy + 26, hcx - 8, hcy + 26, col);
        DrawLine(hcx + 8, hcy + 26, hcx + 24, hcy + 26, col);
        /* door */
        DrawLine(hcx - 8, hcy + 26, hcx - 8, hcy + 12, col);
        DrawLine(hcx - 8, hcy + 12, hcx + 8, hcy + 12, col);
        DrawLine(hcx + 8, hcy + 12, hcx + 8, hcy + 26, col);
    }
    /* Source button right of the house: the active library source as a
     * small icon plus its label (globe = Kavita, book = Local,
     * folder = Folder).  Hidden on the Search page like the right-side
     * icons — the source chooser only makes sense on the shelf. */
    if (g_state.tab != TAB_SEARCH)
        draw_source_button();

    /* Centered title — series name when drilled, "Search" on the search
     * page, the active query on the filtered library shelf, nothing on
     * the plain shelf (the app name in the top bar was dropped per user
     * request). */
    ifont *tf = OpenFont(DEFAULTFONT, 44, 0);
    if (tf != NULL) {
        char title[MAX_QUERY_LEN + 16];
        if (g_state.tab == TAB_SEARCH) {
            snprintf(title, sizeof title, "%s", i18n("tab.search"));
        } else if (g_state.source == SOURCE_FOLDER && g_browse_open) {
            /* The file browser shows its current directory as the
             * title, like the shelf shows the active query — relative
             * to /mnt/ext1 (the mount point is hidden from the user). */
            char shown[96];
            user_path_display(g_browse_path, shown, sizeof shown);
            size_t plen = strlen(shown);
            if (plen > sizeof title - 1)
                plen = sizeof title - 1;
            memcpy(title, shown, plen);
            title[plen] = '\0';
        } else if (g_state.query[0] != '\0') {
            /* The active filter shown as the shelf title. */
            snprintf(title, sizeof title, "%s", g_state.query);
        } else {
            title[0] = '\0';
        }
        if (title[0] != '\0') {
            if (g_state.tab == TAB_SEARCH) {
                /* The Search bar carries only the back arrow: centre
                 * the title on the whole screen width, trimmed only so
                 * it cannot run under the back button (the button box
                 * plus its margin, mirrored around the screen centre).
                 * Centring itself is StringWidth-based, so translated
                 * titles of any length stay centred. */
                int guard = home_x + home_w + 8;
                int budget = w - 2 * guard;
                while (StringWidth(title) > budget && strlen(title) > 4)
                    utf8_cap(title, strlen(title)); /* never split a multibyte char */
                SetFont(tf, col);
                DrawString(
                    (w - StringWidth(title)) / 2, y0 + (TOP_BAR_H - 40) / 2, title);
            } else {
                /* Centre the title inside the free band between the
                 * flanking icon stacks (home + source left; search +
                 * downloads + menu right).  Centring on the whole
                 * screen width lets a long series name run under the
                 * right icons: the trim budget must be the band width,
                 * not w - 420, and the draw origin the band, not 0. */
                int left_w = 8 + 96 + 8 + SOURCE_BTN_W;
                int right_w = 8 + 3 * 96;
                int band_w = w - left_w - right_w;
                if (band_w < 64)
                    band_w = 64;
                while (StringWidth(title) > band_w && strlen(title) > 4)
                    utf8_cap(title, strlen(title)); /* never split a multibyte char */
                SetFont(tf, col);
                DrawString(left_w + (band_w - StringWidth(title)) / 2,
                           y0 + (TOP_BAR_H - 40) / 2,
                           title);
            }
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

    /* Right "menu" button — three black hamburger lines on the white
     * top bar, sized to the common icon box. */
    int menu_w = 96;
    int menu_x = w - menu_w - 8;
    int menu_y = y0 + (TOP_BAR_H - menu_w) / 2;
    int menu_cx = menu_x + menu_w / 2;
    int menu_cy = menu_y + menu_w / 2;
    int menu_r = menu_w / 2;
    FillArea(menu_cx - menu_r, menu_cy - menu_r, menu_r * 2, menu_r * 2, WHITE);
    int ml_w = 48;
    FillArea(menu_cx - ml_w / 2, menu_cy - 21, ml_w, 6, col);
    FillArea(menu_cx - ml_w / 2, menu_cy - 3, ml_w, 6, col);
    FillArea(menu_cx - ml_w / 2, menu_cy + 15, ml_w, 6, col);

    /* Vertical separators between the buttons, top to bottom border.
     * Drawn last so no button's white fill covers them. */
    DrawLine(8 + 96 + 4, y0, 8 + 96 + 4, y0 + TOP_BAR_H, col);
    DrawLine(SOURCE_BTN_X + SOURCE_BTN_W, y0, SOURCE_BTN_X + SOURCE_BTN_W, y0 + TOP_BAR_H, col);
    DrawLine(w - 296, y0, w - 296, y0 + TOP_BAR_H, col);
    DrawLine(w - 200, y0, w - 200, y0 + TOP_BAR_H, col);
    DrawLine(w - 104, y0, w - 104, y0 + TOP_BAR_H, col);
}

/* Sync button in the top bar, left of the menu button: two black arc
 * arrows (a "refresh" glyph) on the white top bar, rotating a few
 * degrees per second while a sync or download is in flight
 * (sync_set_active arms the rotation timer).  Tapping it runs a
 * library sync (see hit_top_bar). */
static int
sync_active(void)
{
    return g_state.sync_state == 1 || downloads_pending() > 0 || g_dl_batch_active;
}

/* 1 = any modal overlay or popup is up (input routing, long-press
 * arming, and background work like cover fetches should pause). */
int
modal_open(void)
{
    return g_state.overlay != OV_NONE || g_state.dl_popup || g_state.sync_popup;
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
    if (!modal_open() && g_state.tab != TAB_SEARCH) {
        draw_sync_icon();
        PartialUpdate(ScreenWidth() - 96 - 8 - 96, 0, 96, TOP_BAR_H);
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
    if (!modal_open() && g_state.tab != TAB_SEARCH) {
        draw_sync_icon();
        PartialUpdate(ScreenWidth() - 96 - 8 - 96, 0, 96, TOP_BAR_H);
    }
}

void
draw_sync_icon(void)
{
    int w = ScreenWidth();
    int y0 = 0;
    int ic_w = 96;
    int ic_x = w - ic_w - 8 - ic_w; /* left of the menu button */
    int ic_y = y0 + (TOP_BAR_H - ic_w) / 2;
    FillArea(ic_x, ic_y, ic_w, ic_w, WHITE);
    int cx = ic_x + ic_w / 2;
    int cy = ic_y + ic_w / 2;
    int r = 22; /* arcs fit the common 52px icon box */
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
                DrawLine(px, py, x, y, BLACK);
                DrawLine(px, py + 1, x, y + 1, BLACK);
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
            DrawLine(ex, ey, ex + (int)(11 * cos(ha)), ey + (int)(11 * sin(ha)), BLACK);
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
    int y0 = 0;
    int col = BLACK;
    int ic_w = 96;
    int menu_x = w - 96 - 8;
    int ic_x = menu_x - 2 * ic_w;
    int ic_y = y0 + (TOP_BAR_H - ic_w) / 2;
    int cx = ic_x + ic_w / 2 - 5; /* ring centre, offset for the handle */
    int cy = ic_y + ic_w / 2 - 5;
    int r = 20; /* ring + handle fit the common 52px icon box */

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
    LOG("[bookshelf] draw_search_tab page=%d\n", g_state.page);

    /* ── input row: full-width search bar, magnifier inside ── */
    int bx = 16, bw = w - 32; /* bar spans the page width */
    int by = top + 10, bh = SEARCH_ROW_H - 20;
    DrawRect(bx, by, bw, bh, BLACK);
    FillArea(bx + 1, by + 1, bw - 2, bh - 2, g_state.search_kb ? BLACK : WHITE);
    int col = g_state.search_kb ? WHITE : BLACK;
    int gx = bx + 30, gy = by + bh / 2;
    int px = 0, py = 0;
    for (int s = 0; s <= 16; s++) {
        double a = s * 2 * M_PI / 16.0;
        int    x = gx + (int)(13 * cos(a));
        int    yy = gy + (int)(13 * sin(a));
        if (s > 0) {
            DrawLine(px, py, x, yy, col);
            DrawLine(px, py + 1, x, yy + 1, col);
        }
        px = x;
        py = yy;
    }
    DrawLine(gx + 9, gy + 10, gx + 22, gy + 23, col);
    DrawLine(gx + 10, gy + 9, gx + 23, gy + 22, col);

    ifont *f = OpenFont(DEFAULTFONT, 28, 0);
    if (f != NULL) {
        int tx = bx + 68;
        SetFont(f, col);
        if (g_state.query[0] != '\0') {
            DrawString(tx, by + (bh - 28) / 2 - 2, g_state.query);
        } else if (!g_state.search_kb) {
            DrawString(tx, by + (bh - 28) / 2 - 2, i18n("search.ph"));
        }
        /* cursor when the keyboard is editing the input */
        if (g_state.search_kb) {
            int cursor_x = tx + StringWidth(g_state.query) + 1;
            DrawLine(cursor_x, by + 6, cursor_x, by + bh - 6, WHITE);
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
            utf8_cap(trunc, sizeof trunc);
            int maxw = w - 80;
            while (StringWidth(trunc) > maxw && strlen(trunc) > 4)
                utf8_cap(trunc, strlen(trunc)); /* never split a multibyte char */
            DrawString(24, y + (SEARCH_HISTORY_ROW_H - 28) / 2 - 2, trunc);
            CloseFont(tf);
        }
        DrawLine(20, y + SEARCH_HISTORY_ROW_H - 1, w - 20, y + SEARCH_HISTORY_ROW_H - 1, LGRAY);
        y += SEARCH_HISTORY_ROW_H;
    }
}

/* Screen rect of the live suggestion band: below the search input
 * row, above the on-screen keyboard.  While the keyboard is open the
 * band replaces the history list (draw_suggestions); when it is
 * empty the underlying page (history) stays visible underneath. */
void
suggest_band(int *y_top, int *y_bot)
{
    int top, bot, cell_w, cell_h;
    (void)cell_w;
    (void)cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    if (y_top)
        *y_top = top + SEARCH_ROW_H;
    int kb = ScreenHeight() / 2; /* fallback when the rect is unknown */
    if (GetKeyboardRect) {
        irect r;
        GetKeyboardRect(&r);
        if (r.y > 0)
            kb = r.y;
    }
    if (y_bot)
        *y_bot = kb;
}

/* Draw the suggestion rows into the band, exact history-row style
 * (see draw_search_tab).  Only rows that fully fit above the keyboard
 * are drawn; the hit-test (bs_input.c) uses the same rule. */
void
draw_suggestions(int y_top, int y_bot)
{
    int w = ScreenWidth();
    FillArea(0, y_top, w, y_bot - y_top, WHITE);
    int y = y_top;
    for (int i = 0; i < g_nsuggest && y + SEARCH_HISTORY_ROW_H <= y_bot; i++) {
        ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
        if (tf != NULL) {
            SetFont(tf, BLACK);
            char trunc[SUGGEST_TERM_MAX];
            snprintf(trunc, sizeof trunc, "%s", g_suggestions[i]);
            utf8_cap(trunc, sizeof trunc);
            int maxw = w - 80;
            while (StringWidth(trunc) > maxw && strlen(trunc) > 4)
                utf8_cap(trunc, strlen(trunc)); /* never split a multibyte char */
            DrawString(24, y + (SEARCH_HISTORY_ROW_H - 28) / 2 - 2, trunc);
            CloseFont(tf);
        }
        DrawLine(20, y + SEARCH_HISTORY_ROW_H - 1, w - 20,
                 y + SEARCH_HISTORY_ROW_H - 1, LGRAY);
        y += SEARCH_HISTORY_ROW_H;
    }
}

/* Number of downloads still pending (queued or in flight) — shown as a
 * badge on the downloads icon so the user can see work is in progress. */
int
downloads_pending(void)
{
    int n = 0;
    for (int i = 0; i < g_download_count; i++) {
        if (g_downloads[i].state == 0)
            n++;
        else if (g_downloads[i].state == 1 && !dl_fetch_idle())
            n++; /* in flight; counts until the fetch's worker fn is done */
    }
    return n;
}

/* 1 = the firmware's panel painter never activated (PanelHeight()==0 at
 * init); we draw the status strip ourselves — at the BOTTOM, where the
 * firmware's type-1 panel lives. */
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
    int y0 = ScreenHeight() - h;

    FillArea(0, y0, w, h, WHITE);
    DrawLine(0, y0, w, y0, BLACK);

    time_t    now = time(NULL);
    struct tm tmv;
    char      buf[32];
    localtime_r(&now, &tmv);
    strftime(buf, sizeof buf, "%a %H:%M", &tmv);
    ifont *tf = OpenFont(DEFAULTFONT, 40, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(24, y0 + (h - 40) / 2, buf);
        CloseFont(tf);
    }

    /* Frontlight bulb: circle with short rays. */
    int lx = w - 176;
    int ly = y0 + h / 2;
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
    int by = y0 + (h - bh) / 2;
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

/* Paint the bottom status strip: firmware-painted when the panel painter
 * is active (emulator), self-drawn when it never activates (device). */
void
stamp_panel(void)
{
    if (g_self_panel)
        draw_system_strip();
    else
        iv_update_panel(0);
}

/* Bottom edge of the app-owned content area: the firmware's type-1
 * system panel occupies [content_bottom(), ScreenHeight()), so every
 * app surface (top bar, grid, pager, overlays) lives above it.  The
 * stock desktop does the same (its MainFrame is created with height
 * ScreenHeight() - PanelHeight()). */
int
content_bottom(void)
{
    return ScreenHeight() - g_state.panel_h;
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
        draw_progress_bar(cx, cy, cww, chh, progress_percent(b->local_path));
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
            utf8_cap(truncated, sizeof truncated);
            while (StringWidth(truncated) > tw0 && strlen(truncated) > 4)
                utf8_cap(truncated, strlen(truncated)); /* never split a multibyte char */
            DrawString(tx0, y + pad + 8, truncated);
            CloseFont(f);
        }
        if (!tr->is_series && b->author[0] != '\0') {
            ifont *af = OpenFont(DEFAULTFONT, 24, 0);
            if (af != NULL) {
                SetFont(af, DGRAY);
                char truncated[80];
                snprintf(truncated, sizeof truncated, "%s", b->author);
                utf8_cap(truncated, sizeof truncated);
                while (StringWidth(truncated) > tw0 && strlen(truncated) > 4)
                    utf8_cap(truncated, strlen(truncated)); /* never split a multibyte char */
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

    /* Reading progress: a black bar at the cover's bottom edge. */
    draw_progress_bar(cx, cy, cw, ch, progress_percent(b->local_path));

    /* Caption: series name for cards, title for books. */
    int         cap_y = cy + ch + 6;
    const char *label = tr->is_series ? tr->series_name : b->title;
    ifont      *f = OpenFont(DEFAULTFONTB, 22, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        char truncated[MAX_TITLE_LEN];
        snprintf(truncated, sizeof truncated, "%s", label);
        utf8_cap(truncated, sizeof truncated);
        while (StringWidth(truncated) > w - 8 && strlen(truncated) > 4)
            utf8_cap(truncated, strlen(truncated)); /* never split a multibyte char */
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
            utf8_cap(truncated, sizeof truncated);
            while (StringWidth(truncated) > w - 8 && strlen(truncated) > 4)
                utf8_cap(truncated, strlen(truncated)); /* never split a multibyte char */
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

/* Cancel-button rect inside the download popup: directly right of the
 * batch progress bar (shared by the draw path and the tap hit-test).
 * The bar row runs at py + CTX_TITLE_H + 64 and spans the sheet width
 * minus the button column, so the button shares the bar's row instead
 * of sitting in the popup's title corner. */
void
dl_cancel_rect(int *x, int *y)
{
    int px, py, pw, ph;
    dl_popup_geom(&px, &py, &pw, &ph);
    int bar_y = py + CTX_TITLE_H + 64;
    *x = px + pw - CTX_PAD - DL_CANCEL_SIZE;
    *y = bar_y + (DL_BAR_H - DL_CANCEL_SIZE) / 2;
}

/* Repaint just the download-popup sheet (progress bar, current item,
 * status line).  The download job's completion calls this on every
 * queue change: the shelf around the popup is untouched during a
 * download, so a sheet-sized partial keeps the e-ink flicker local
 * instead of re-flashing the whole content area once per item (which
 * is what redraw_shelf() did — three times per finished download). */
void
refresh_dl_popup(void)
{
    int px, py, pw, ph;

    if (!g_state.dl_popup)
        return;
    draw_dl_popup();
    dl_popup_geom(&px, &py, &pw, &ph);
    PartialUpdate(px, py, pw, ph);
}

/* Download-popup sheet geometry: a centred 3/4-width sheet.  Shared by
 * the draw path, the cancel-button rect, and the popup-only refresh. */
void
dl_popup_geom(int *px, int *py, int *pw, int *ph)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    *pw = w * 3 / 4;
    *ph = 320;
    *px = (w - *pw) / 2;
    *py = (h - *ph) / 2;
}

/* Download-progress popup: a centred modal sheet over a dimmed shelf.
 * Title, the current item, the batch progress bar, and a status line.
 * A cancel (X) button right of the progress bar aborts the whole
 * queue — the in-flight fetch is told to cancel (it will not rename
 * its .part file into place), but QuickDownload still blocks to its
 * timeout, so it is left to finish while every queued item is dropped
 * (see cancel_downloads).  Shown whenever downloads run (book press,
 * context-menu Download, Download all).  While any download is active
 * the popup is non-dismissable — downloads never run in the
 * background; once the queue drains a tap or Back closes it.  When
 * the popup was opened by a single-book press (dl_popup_auto_open),
 * dl_job_done() launches the reader as soon as the queue drains. */
void
draw_dl_popup(void)
{
    int w = ScreenWidth();
    /* Dim the shelf body below the top bar and above the panel band,
     * so the top-bar icons (the spinning sync glyph among them) stay
     * fully visible while the download runs. */
    for (int yy = TOP_BAR_H; yy < content_bottom(); yy += 2)
        DrawLine(0, yy, w, yy, LGRAY);

    int pw, ph, px, py;
    dl_popup_geom(&px, &py, &pw, &ph);
    LOG("[bookshelf] draw_dl_popup open auto_open=%d count=%d\n",
        g_state.dl_popup_auto_open,
        g_download_count);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    /* Cancel (X) button right of the progress bar.  Always drawn:
     * while a download is active it aborts the queue; once the queue
     * has drained it just closes the popup, like a tap anywhere else. */
    int cx, cy;
    dl_cancel_rect(&cx, &cy);
    FillArea(cx, cy, DL_CANCEL_SIZE, DL_CANCEL_SIZE, WHITE);
    DrawRect(cx, cy, DL_CANCEL_SIZE, DL_CANCEL_SIZE, BLACK);
    DrawLine(cx + 16, cy + 16, cx + DL_CANCEL_SIZE - 16, cy + DL_CANCEL_SIZE - 16, BLACK);
    DrawLine(cx + DL_CANCEL_SIZE - 16, cy + 16, cx + 16, cy + DL_CANCEL_SIZE - 16, BLACK);

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
            utf8_cap(trunc, sizeof trunc);
            while (StringWidth(trunc) > pw - 2 * CTX_PAD && strlen(trunc) > 4)
                utf8_cap(trunc, strlen(trunc)); /* never split a multibyte char */
            DrawString(px + CTX_PAD, py + CTX_TITLE_H + 22, trunc);
            CloseFont(cf);
        }
    }

    draw_dl_progress(
        px + CTX_PAD, py + CTX_TITLE_H + 64, pw - 2 * CTX_PAD - DL_CANCEL_SIZE - DL_CANCEL_GAP);

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

/* ── sync progress popup ─────────────────────────────────────────────── */

void
sync_popup_geom(int *px, int *py, int *pw, int *ph)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    *pw = w * 3 / 4;
    *ph = 190;
    *px = (w - *pw) / 2;
    *py = (h - *ph) / 2;
}

/* Title / status line for the current sync stage.  The sub-line carries
 * the counter (batch number / scanned books / result count). */
static const char *
sync_popup_line(int *sub)
{
    *sub = 0;
    switch (g_state.sync_stage) {
    case SYNC_STAGE_META:
        *sub = 1;
        return i18n("sync.meta");
    case SYNC_STAGE_SCAN:
        *sub = 2;
        return i18n("sync.scan");
    case SYNC_STAGE_COVERS:
        return i18n("sync.covers");
    case SYNC_STAGE_FAIL:
        return i18n("status.fail");
    default:
        return i18n("sync.done");
    }
}

/* Sync-progress sheet: a centred modal card over the dimmed shelf,
 * telling the user what the in-flight sync is doing (metadata batch,
 * local scan, covers, done/failed).  Only manual syncs open it; boot
 * and timer syncs run silently behind the spinning top-bar icon. */
void
draw_sync_popup(void)
{
    int w = ScreenWidth();
    for (int yy = TOP_BAR_H; yy < content_bottom(); yy += 2)
        DrawLine(0, yy, w, yy, LGRAY);

    int px, py, pw, ph;
    sync_popup_geom(&px, &py, &pw, &ph);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 30, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + CTX_PAD, py + 18, i18n("action.sync"));
        CloseFont(tf);
    }
    DrawLine(px + CTX_PAD, py + CTX_TITLE_H - 1, px + pw - CTX_PAD, py + CTX_TITLE_H - 1, LGRAY);

    int         sub;
    const char *line = sync_popup_line(&sub);
    ifont      *lf = OpenFont(DEFAULTFONTB, 28, 0);
    if (lf != NULL) {
        SetFont(lf, BLACK);
        DrawString(px + CTX_PAD, py + CTX_TITLE_H + 24, line);
        CloseFont(lf);
    }
    ifont *sf = OpenFont(DEFAULTFONT, 24, 0);
    if (sf != NULL) {
        SetFont(sf, DGRAY);
        char subline[96];
        switch (sub) {
        case 1:
            snprintf(subline, sizeof subline, i18n("sync.batch"), g_state.sync_round);
            break;
        case 2:
            snprintf(subline, sizeof subline, i18n("sync.books"), g_state.sync_scan);
            break;
        default:
            snprintf(subline, sizeof subline, i18n("sync.books"), view_total());
            break;
        }
        DrawString(px + CTX_PAD, py + CTX_TITLE_H + 68, subline);
        CloseFont(sf);
    }
}

void
sync_popup_refresh(void)
{
    if (!g_state.sync_popup)
        return;
    int px, py, pw, ph;
    sync_popup_geom(&px, &py, &pw, &ph);
    draw_sync_popup();
    PartialUpdate(px, py, pw, ph);
}

void
sync_popup_open(void)
{
    if (g_state.sync_popup)
        return;
    g_state.sync_popup = 1;
    g_state.sync_stage = SYNC_STAGE_META;
    g_state.sync_round = 0;
    g_state.sync_scan = 0;
    sync_popup_refresh();
}

void
sync_popup_close(void)
{
    if (!g_state.sync_popup)
        return;
    g_state.sync_popup = 0;
    redraw_shelf();
}

/* Close the popup shortly after the sync finished (or failed).  While
 * covers are still loading the popup stays on the COVERS line and the
 * timer re-arms; the 15s cap guarantees it closes even on a slow link
 * (covers then finish in the background). */
static void
sync_popup_close_tick(void *ctx)
{
    (void)ctx;
    if (!g_state.sync_popup)
        return;
    if (g_state.sync_stage == SYNC_STAGE_COVERS && g_cover_armed) {
        SetWeakTimerEx("bsyncp", sync_popup_close_tick, NULL, 1000);
        return;
    }
    sync_popup_close();
}

static void
sync_popup_auto_close(int delay_ms)
{
    SetWeakTimerEx("bsyncp", sync_popup_close_tick, NULL, delay_ms);
}

void
sync_popup_finish(void)
{
    if (!g_state.sync_popup)
        return;
    g_state.sync_stage = SYNC_STAGE_COVERS;
    sync_popup_refresh();
    cover_schedule_next();
    if (g_cover_armed)
        sync_popup_auto_close(15000); /* safety cap; covers closing sooner is the norm */
    else
        sync_popup_auto_close(900);
}

void
sync_popup_fail(void)
{
    if (!g_state.sync_popup)
        return;
    g_state.sync_stage = SYNC_STAGE_FAIL;
    sync_popup_refresh();
    sync_popup_auto_close(1500);
}

/* Flush the app-owned content area [0, content_bottom()) as a partial
 * update.  Every app surface (top bar, grid, pager, overlays, settings)
 * is drawn strictly inside this region — the firmware's system panel
 * band [content_bottom(), ScreenHeight()) is never touched — so a
 * content-area partial is visually equivalent to FullUpdate() without
 * the full-screen flash, at a fraction of the cost.  FullUpdate() stays
 * reserved for the cases where it genuinely earns its price: first
 * paint (EVT_INIT), task foreground / external repaint (EVT_SHOW etc.,
 * where the framebuffer may hold another app's content), the
 * on-screen-keyboard commits (the keyboard wipes the panel band too),
 * and the launcher drag-end (ghost clear after an unflushed drag). */
void
flush_content(void)
{
    PartialUpdate(0, 0, ScreenWidth(), content_bottom());
}

/* Repaint the whole shelf (top bar, body, pager) in the current tab,
 * then the download popup on top when one is open.  Centralises the
 * sequence every state change needs. */
void
redraw_shelf(void)
{
    /* Clamp the page before the body draws: a view change that shrank
     * the page count (list mode on a deep page, a tightening filter)
     * would otherwise let draw_grid paint an empty page — draw_pager's
     * own clamp only runs after the grid. */
    int pages = current_pages();
    if (g_state.page >= pages)
        g_state.page = pages - 1;
    if (g_state.page < 0)
        g_state.page = 0;

    if (g_state.overlay == OV_LAUNCHER) {
        draw_overlay_launcher();
        FullUpdate();
        return;
    }
    FillArea(0, 0, ScreenWidth(), content_bottom(), WHITE);
    draw_top_bar();
    if (g_state.tab == TAB_SEARCH)
        draw_search_tab();
    else if (g_state.source == SOURCE_FOLDER && g_browse_open)
        draw_browse();
    else
        draw_grid();
    if (g_state.source != SOURCE_FOLDER)
        draw_pager();
    if (g_state.dl_popup)
        draw_dl_popup();
    if (g_state.sync_popup)
        draw_sync_popup();
    flush_content();
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
    FillArea(0, 0, w, content_bottom(), BLACK);
    int pw = w * 3 / 4;
    FillArea(0, 0, pw, content_bottom(), WHITE);
    DrawLine(pw, 0, pw, content_bottom(), BLACK);

    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(24, 32, i18n("action.menu"));
        CloseFont(f);
    }

    const char *labels[] = {
        "group.all",
        "group.author",
        "group.series",
        "group.recent",
    };
    int n = (int)(sizeof labels / sizeof labels[0]);
    int y0 = 96;
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
    FillArea(0, 0, w, content_bottom(), BLACK);
    int pw = w * 3 / 4;
    int px = w - pw;
    FillArea(px, 0, pw, content_bottom(), WHITE);
    DrawLine(px, 0, px, content_bottom(), BLACK);

    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(px + 24, 32, i18n("action.more"));
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
    int y0 = MORE_Y0;
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

/* ── source chooser ──────────────────────────────────────────────────── */

/* Sheet geometry of the source chooser (top-bar button right of home):
 * a centred 3/4-width sheet with the title row and three source rows. */
void
source_geom(int *px, int *py, int *pw, int *ph)
{
    int w = ScreenWidth();
    *pw = w * 3 / 4;
    *ph = 72 + 3 * 96 + 24;
    *px = (w - *pw) / 2;
    *py = (content_bottom() - *ph) / 2;
}

void
draw_overlay_source(void)
{
    int w = ScreenWidth();
    int pw, ph, px, py;
    source_geom(&px, &py, &pw, &ph);

    /* Dim the content area behind the sheet. */
    for (int yy = 0; yy < content_bottom(); yy += 2)
        DrawLine(0, yy, w, yy, LGRAY);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 32, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + CTX_PAD, py + 18, i18n("source.title"));
        CloseFont(tf);
    }
    DrawLine(px + CTX_PAD, py + 64, px + pw - CTX_PAD, py + 64, LGRAY);

    const char *labels[3] = {
        i18n("source.kavita"),
        i18n("source.local"),
        i18n("source.folder"),
    };
    int y0 = py + 80;
    for (int i = 0; i < 3; i++) {
        int sel = (g_state.source == i);
        FillArea(px + 12, y0 + i * 96, pw - 24, 96 - 12, sel ? BLACK : WHITE);
        DrawRect(px + 12, y0 + i * 96, pw - 24, 96 - 12, sel ? BLACK : WHITE);
        ifont *f = OpenFont(DEFAULTFONTB, 28, 0);
        if (f != NULL) {
            SetFont(f, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * 96 + (96 - 28) / 2 - 2, labels[i]);
            CloseFont(f);
        }
    }
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
         * endpoint builder always gets a scheme.  Cap the host portion
         * first so the prefixed URL always fits g_state.api_base: only
         * the host's tail can be cut, never the "http://" prefix, so
         * the committed value is never a truncated-mid-URL. */
        if (strncmp(val, "http://", 7) != 0 && strncmp(val, "https://", 8) != 0) {
            char tmp[sizeof g_state.api_base];
            snprintf(tmp, sizeof tmp, "%.*s", (int)(sizeof g_state.api_base - 8), val);
            utf8_cap(tmp, sizeof tmp);
            snprintf(g_state.api_base, sizeof g_state.api_base, "http://%s", tmp);
        } else {
            snprintf(g_state.api_base, sizeof g_state.api_base, "%s", val);
        }
    } else if (g_settings_edit == 2) {
        snprintf(g_state.api_token, sizeof g_state.api_token, "%s", val);
    }
    g_settings_edit = 0;
    draw_overlay_settings();
    /* The on-screen keyboard draws full-screen and wipes the bottom
     * status strip; re-stamp it before the flush so the panel survives
     * the commit redraw. */
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
    FillArea(0, 0, w, content_bottom(), WHITE);

    ifont *tf = OpenFont(DEFAULTFONTB, 40, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(32, 28, i18n("settings.title"));
        CloseFont(tf);
    }
    DrawLine(0, 92, w, 92, BLACK);

    /* Downloads folder: the pending picker choice, else the resolved
     * effective directory — shown relative to /mnt/ext1. */
    char        dl_shown[256];
    const char *dl = g_settings_dl_dir[0] ? g_settings_dl_dir : g_downloads_dir;
    user_path_display(dl, dl_shown, sizeof dl_shown);
    while (StringWidth(dl_shown) > w - 2 * 32 - 16 && strlen(dl_shown) > 4)
        utf8_cap(dl_shown, strlen(dl_shown)); /* never split a multibyte char */

    int y = 112;
    settings_draw_row(y, i18n("settings.api_host"), g_state.api_base, g_settings_edit == 1);
    y += SETTINGS_ROW_H;
    settings_draw_row(y, i18n("settings.api_key"), g_state.api_token, g_settings_edit == 2);
    y += SETTINGS_ROW_H;
    settings_draw_row(y, i18n("settings.reader"), settings_reader_label(), 0);
    y += SETTINGS_ROW_H;
    settings_draw_row(y, i18n("settings.dl_dir"), dl_shown, 0);
    y += SETTINGS_ROW_H + 24;
    settings_draw_button(y, i18n("settings.save"), 1);
    y += SETTINGS_BTN_H;
    settings_draw_button(y, i18n("settings.back"), 0);
    y += SETTINGS_BTN_H;
    settings_draw_button(y, i18n("settings.logs"), 0);
}

/* ── stock up/down scroll buttons ────────────────────────────────────── */

/* Draw the corner scroll buttons every scrollable surface uses (the
 * same pattern the stock firmware apps show, e.g. the coloring app):
 * up chevron bottom-left, down chevron bottom-right, overlaid on the
 * content.  A direction that cannot scroll renders in light grey.
 * Surfaces that cannot scroll at all draw nothing.  `y0` is the top
 * of the button band; surfaces with a fixed bottom bar (the folder
 * picker) raise it above that bar. */
void
draw_scroll_buttons_at(int up_ok, int down_ok, int y0)
{
    if (!up_ok && !down_ok)
        return;
    int w = ScreenWidth();

    FillArea(0, y0, SCROLL_BTN_W, SCROLL_BTN_H, WHITE);
    DrawRect(0, y0, SCROLL_BTN_W, SCROLL_BTN_H, up_ok ? BLACK : LGRAY);
    int col = up_ok ? BLACK : LGRAY;
    int cx = SCROLL_BTN_W / 2;
    int cy = y0 + SCROLL_BTN_H / 2;
    DrawLine(cx - 24, cy + 14, cx, cy - 14, col);
    DrawLine(cx + 24, cy + 14, cx, cy - 14, col);

    int x2 = w - SCROLL_BTN_W;
    FillArea(x2, y0, SCROLL_BTN_W, SCROLL_BTN_H, WHITE);
    DrawRect(x2, y0, SCROLL_BTN_W, SCROLL_BTN_H, down_ok ? BLACK : LGRAY);
    col = down_ok ? BLACK : LGRAY;
    cx = x2 + SCROLL_BTN_W / 2;
    DrawLine(cx - 24, cy - 14, cx, cy + 14, col);
    DrawLine(cx + 24, cy - 14, cx, cy + 14, col);
}

void
draw_scroll_buttons(int up_ok, int down_ok)
{
    draw_scroll_buttons_at(up_ok, down_ok, content_bottom() - SCROLL_BTN_H);
}

/* Hit test for the corner scroll buttons: -1 = up (bottom-left),
 * +1 = down (bottom-right), 0 = neither. */
int
hit_scroll_button_at(int x, int y, int y0)
{
    int w = ScreenWidth();
    if (y < y0 || y >= y0 + SCROLL_BTN_H)
        return 0;
    if (x >= 0 && x < SCROLL_BTN_W)
        return -1;
    if (x >= w - SCROLL_BTN_W && x < w)
        return +1;
    return 0;
}

int
hit_scroll_button(int x, int y)
{
    return hit_scroll_button_at(x, y, content_bottom() - SCROLL_BTN_H);
}

/* ── log viewer (Settings → Show logs) ──────────────────────────────── */

/* Read the log tail: at most `cap` bytes, aligned to a line boundary.
 * Returns a malloc'd NUL-terminated buffer, or NULL when the log does
 * not exist yet. */
static char *
log_tail_read(size_t cap)
{
    const char *path = log_path();
    FILE       *f = fopen(path, "r");
    if (f == NULL)
        return NULL;
    if (fseek(f, 0, SEEK_END) != 0) {
        fclose(f);
        return NULL;
    }
    long size = ftell(f);
    long start = 0;
    if (size > (long)cap) {
        start = size - (long)cap;
        if (fseek(f, start, SEEK_SET) != 0) {
            fclose(f);
            return NULL;
        }
        int c;
        while ((c = fgetc(f)) != EOF && c != '\n')
            ;
    } else {
        /* Back to the beginning: the SEEK_END above left the position
         * at EOF. */
        if (fseek(f, 0, SEEK_SET) != 0) {
            fclose(f);
            return NULL;
        }
    }
    size_t n = (size_t)(size - start);
    char  *buf = malloc(n + 1);
    if (buf == NULL) {
        fclose(f);
        return NULL;
    }
    size_t got = fread(buf, 1, n, f);
    fclose(f);
    buf[got] = '\0';
    return buf;
}

/* A wrapped display row: a span of the log buffer. */
typedef struct {
    const char *p;
    int         len;
} LogRow;

/* Width of a non-NUL-terminated span (the SDK only measures C
 * strings). */
static int
span_width(const char *p, int len)
{
    char tmp[1024];
    if (len > (int)sizeof tmp - 1)
        len = (int)sizeof tmp - 1;
    memcpy(tmp, p, (size_t)len);
    tmp[len] = '\0';
    return StringWidth(tmp);
}

/* Greedy word wrap of the log text into display rows no wider than
 * `maxw` px.  Rows point into `text` (never modified).  Returns the
 * row count. */
static int
log_wrap_rows(const char *text, int maxw, LogRow *rows, int cap)
{
    int         count = 0;
    const char *line = text;
    while (*line != '\0' && count < cap) {
        const char *nl = strchr(line, '\n');
        size_t      llen = nl ? (size_t)(nl - line) : strlen(line);
        const char *end = line + llen;
        const char *ws = line;
        while (ws < end) {
            const char *we = ws;
            while (we < end && *we != ' ')
                we++;
            if (we == ws) { /* collapse space runs */
                ws++;
                continue;
            }
            int wordw = span_width(ws, (int)(we - ws));
            int curw = rows[count].len > 0 ? span_width(rows[count].p, rows[count].len) : 0;
            if (rows[count].len > 0 && curw + wordw + 6 > maxw) {
                count++;
                if (count >= cap)
                    goto done;
            }
            if (rows[count].len == 0)
                rows[count].p = ws;
            rows[count].len += (int)(we - ws);
            if (we < end)
                rows[count].len++; /* the separating space */
            ws = we;
        }
        if (rows[count].len > 0) {
            count++;
            if (count >= cap)
                goto done;
        }
        if (nl == NULL)
            break;
        line = nl + 1;
    }
done:
    return count;
}

/* Full-screen log viewer: the app log tail, line-wrapped, page-scrolled
 * with the two bottom buttons; Back returns to the shelf. */
void
draw_log_view(void)
{
    int w = ScreenWidth();
    int h = content_bottom();
    FillArea(0, 0, w, h, WHITE);

    /* Header: back button + title + file path. */
    FillArea(LOG_BACK_X, LOG_BACK_Y, LOG_BACK_W, LOG_BACK_H, WHITE);
    DrawRect(LOG_BACK_X, LOG_BACK_Y, LOG_BACK_W, LOG_BACK_H, BLACK);
    ifont *bf = OpenFont(DEFAULTFONTB, 26, 0);
    if (bf != NULL) {
        SetFont(bf, BLACK);
        int tw = StringWidth(i18n("log.back"));
        DrawString(LOG_BACK_X + (LOG_BACK_W - tw) / 2,
                   LOG_BACK_Y + (LOG_BACK_H - 26) / 2,
                   i18n("log.back"));
        CloseFont(bf);
    }
    ifont *tf = OpenFont(DEFAULTFONTB, 34, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(LOG_BACK_X + LOG_BACK_W + 16, LOG_BACK_Y + 8, i18n("log.title"));
        CloseFont(tf);
    }
    ifont *pf = OpenFont(DEFAULTFONT, 20, 0);
    if (pf != NULL) {
        SetFont(pf, DGRAY);
        char shown[200];
        snprintf(shown, sizeof shown, "%s", log_path());
        while (StringWidth(shown) > w - LOG_BACK_X - LOG_BACK_W - 32 && strlen(shown) > 8)
            utf8_cap(shown, strlen(shown)); /* never split a multibyte char */
        DrawString(LOG_BACK_X + LOG_BACK_W + 16, LOG_BACK_Y + 46, shown);
        CloseFont(pf);
    }
    DrawLine(0, LOG_BACK_Y + LOG_BACK_H + 8, w, LOG_BACK_Y + LOG_BACK_H + 8, BLACK);

    int body_top = LOG_BACK_Y + LOG_BACK_H + 16;
    int btn_y = h - 8 - SCROLL_BTN_H;
    int body_h = btn_y - body_top - 8;
    if (body_h < LOG_ROW_H)
        body_h = LOG_ROW_H;
    int rows_vis = body_h / LOG_ROW_H;

    int   first = 0;
    int   max_first = 0;
    char *text = log_tail_read(160 * 1024);
    if (text == NULL) {
        ifont *ef = OpenFont(DEFAULTFONT, 26, 0);
        if (ef != NULL) {
            SetFont(ef, DGRAY);
            DrawString(32, body_top + 40, i18n("log.empty"));
            CloseFont(ef);
        }
    } else {
        LogRow *rows = calloc((size_t)rows_vis * 8, sizeof(LogRow));
        int     nrows = 0;
        if (rows != NULL) {
            nrows = log_wrap_rows(text, w - 48, rows, rows_vis * 8);
        }
        int maxf = nrows - rows_vis;
        if (maxf < 0)
            maxf = 0;
        max_first = maxf;
        first = g_state.log_scroll < 0 ? max_first : g_state.log_scroll;
        if (first > max_first)
            first = max_first;
        if (first < 0)
            first = 0;
        g_state.log_scroll = first;

        ifont *lf = OpenFont(DEFAULTFONT, LOG_FONT_PX, 0);
        if (lf != NULL) {
            SetFont(lf, BLACK);
            for (int i = 0; i < rows_vis && first + i < nrows; i++) {
                int         len = rows[first + i].len;
                const char *p = rows[first + i].p;
                if (len > 480)
                    len = 480;
                char tmp[512];
                memcpy(tmp, p, (size_t)len);
                tmp[len] = '\0';
                while (StringWidth(tmp) > w - 48 && strlen(tmp) > 4)
                    utf8_cap(tmp, strlen(tmp)); /* never split a multibyte char */
                DrawString(24, body_top + i * LOG_ROW_H, tmp);
            }
            CloseFont(lf);
        }
        free(rows);
    }
    free(text);

    /* Stock corner scroll buttons: older = up, newer = down. */
    draw_scroll_buttons(first > 0, first < max_first);
}
