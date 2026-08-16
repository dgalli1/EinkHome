/* bs_topbar.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_browser.h"
#include "bs_downloads.h"
#include "bs_model.h"
#include "bs_ui.h"

/* ── top bar ─────────────────────────────────────────────────────────── */

/* Left-pointing chevron back arrow, centred at (cx, cy).  Used by the
 * top bar (Search page / drilled series), the settings header and the
 * launcher header, so every back affordance shares one icon. */
void
bs_draw_back_icon(int cx, int cy, int col)
{
    int ax = cx - 8;
    int ay = cy;
    DrawLine(ax, ay, ax + 26, ay - 26, col);
    DrawLine(ax, ay, ax + 26, ay + 26, col);
    DrawLine(ax + 4, ay, ax + 30, ay - 26, col);
    DrawLine(ax + 4, ay, ax + 30, ay + 26, col);
}

/* Draw one polyline segment of a globe arc from the previous point to
 * (xx, yy); s == 0 is the arc's start point, drawn as nothing.  The
 * second, y-offset line thickens the 1px stroke. */
static void
globe_arc_piece(int px, int py, int xx, int yy, int col, int s)
{
    if (s > 0) {
        DrawLine(px, py, xx, yy, col);
        DrawLine(px, py + 1, xx, yy + 1, col);
    }
}

/* Line-art globe (Kavita / online): circle, equator, meridian.  Drawn
 * in the common TOP_ICON_SIZE x TOP_ICON_SIZE icon box. */
static void
draw_globe_icon(int x, int y, int col)
{
    int cx = x + BS_TOP_ICON_HALF, cy = y + BS_TOP_ICON_HALF, r = 24;
    bs_draw_circle_outline(cx, cy, r, col);
    /* equator */
    int px = 0, py = 0;
    for (int s = 0; s <= 16; s++) {
        double a = s * 2 * M_PI / 16.0;
        int    xx = cx + (int)(r * cos(a));
        int    yy = cy + (int)(r * 0.42 * sin(a));
        globe_arc_piece(px, py, xx, yy, col, s);
        px = xx;
        py = yy;
    }
    /* meridian */
    px = py = 0;
    for (int s = 0; s <= 16; s++) {
        double a = s * 2 * M_PI / 16.0;
        int    xx = cx + (int)(r * 0.42 * cos(a));
        int    yy = cy + (int)(r * sin(a));
        globe_arc_piece(px, py, xx, yy, col, s);
        px = xx;
        py = yy;
    }
}

/* Line-art open book (Local): two pages over a spine, in the
 * TOP_ICON_SIZE x TOP_ICON_SIZE icon box. */
static void
draw_book_icon(int x, int y, int col)
{
    int cx = x + BS_TOP_ICON_HALF, cy = y + BS_TOP_ICON_HALF;
    DrawLine(cx - 24, cy + 20, cx - 24, cy - 16, col);
    DrawLine(cx - 24, cy - 16, cx, cy - 6, col);
    DrawLine(cx + 24, cy + 20, cx + 24, cy - 16, col);
    DrawLine(cx + 24, cy - 16, cx, cy - 6, col);
    DrawLine(cx - 24, cy + 20, cx, cy + 24, col);
    DrawLine(cx + 24, cy + 20, cx, cy + 24, col);
}

/* Line-art folder (Folder source): tab + body, in the TOP_ICON_SIZE
 * icon box. */
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
    switch (bs_g_state.source) {
    case BS_SOURCE_LOCAL:
        return bs_i18n("source.local");
    case BS_SOURCE_FOLDER:
        return bs_i18n("source.folder");
    default:
        return bs_i18n("source.kavita");
    }
}

/* Width of the source button.  On the 6-inch (758px) panels the
 * button grows to fill the whole band up to the right icon stack (the
 * empty space would otherwise dwarf the label); everywhere else it
 * keeps the fixed width, with the divider staying at its edge. */
int
bs_source_btn_w(void)
{
    if (ScreenWidth() <= 758)
        return (ScreenWidth() - (BS_TOP_BTN_PAD + 4 * BS_TOP_BTN_SIZE)) - BS_SOURCE_BTN_X;
    return BS_SOURCE_BTN_W;
}

/* The source button: the active library source's icon + label.  The
 * chooser opens on tap (hit_top_bar → 6). */
static void
draw_source_button(void)
{
    int col = BLACK;
    int x0 = BS_SOURCE_BTN_X;
    int w_btn = bs_source_btn_w();
    FillArea(x0, 0, w_btn, BS_TOP_BAR_H, WHITE);
    int cy = BS_TOP_BAR_H / 2;
    /* Icon in the common TOP_ICON_SIZE box, bottom-aligned with the
     * house icon next to it; label at a larger font beside it. */
    int ic_x = x0 + 8, ic_y = cy - 24;
    switch (bs_g_state.source) {
    case BS_SOURCE_LOCAL:
        draw_book_icon(ic_x, ic_y, col);
        break;
    case BS_SOURCE_FOLDER:
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

/* The top-bar title: series name when drilled into a series, "Search"
 * on the search page, the active query on the filtered library shelf,
 * the folder browser's current directory while it is open, and nothing
 * on the plain shelf. */
static void
top_bar_title(char *title, size_t cap)
{
    if (bs_g_state.tab == BS_TAB_SEARCH) {
        snprintf(title, cap, "%s", bs_i18n("tab.search"));
    } else if (bs_g_state.source == BS_SOURCE_FOLDER && bs_g_browse_open) {
        /* The file browser shows its current directory as the title,
         * relative to /mnt/ext1 (the mount point is hidden from the
         * user). */
        char shown[96];
        bs_user_path_display(bs_g_browse_path, shown, sizeof shown);
        size_t plen = strlen(shown);
        if (plen > cap - 1)
            plen = cap - 1;
        memcpy(title, shown, plen);
        title[plen] = '\0';
    } else if (bs_g_state.query[0] != '\0') {
        snprintf(title, cap, "%s", bs_g_state.query);
    } else {
        title[0] = '\0';
    }
}

/* Draw the centred top-bar title, trimmed to either half the screen
 * width (search page) or the free band between the flanking icon
 * stacks. */
static void
draw_top_bar_title(int w, int y0, int home_x, int home_w, int col, ifont *tf,
                   char *title, size_t tsiz)
{
    if (bs_g_state.tab == BS_TAB_SEARCH) {
        /* The Search bar carries only the back arrow: centre the
         * title on the whole screen width, trimmed only so it cannot
         * run under the back button.  Centring itself is
         * StringWidth-based, so translated titles of any length stay
         * centred. */
        int guard = home_x + home_w + BS_TOP_BTN_PAD;
        int budget = w - 2 * guard;
        bs_utf8_fit_width(title, tsiz, budget);
        SetFont(tf, col);
        DrawString((w - StringWidth(title)) / 2, y0 + (BS_TOP_BAR_H - 40) / 2, title);
    } else {
        /* Centre inside the free band between the flanking icon
         * stacks (home + source left; search + downloads + menu
         * right).  Centring on the whole screen width would let a
         * long series name run under the right icons: the trim budget
         * is the band width and the draw origin the band, not 0. */
        int left_w = BS_TOP_BTN_PAD + BS_TOP_BTN_SIZE + BS_TOP_BTN_PAD + bs_source_btn_w();
        int right_w = BS_TOP_BTN_PAD + 4 * BS_TOP_BTN_SIZE;
        int band_w = w - left_w - right_w;
        if (band_w < 64)
            band_w = 64;
        bs_utf8_fit_width(title, tsiz, band_w);
        SetFont(tf, col);
        DrawString(left_w + (band_w - StringWidth(title)) / 2,
                   y0 + (BS_TOP_BAR_H - 40) / 2,
                   title);
    }
}

void
bs_draw_top_bar(void)
{
    int w = ScreenWidth();
    int y0 = 0; /* top bar sits at the very top; the system panel is at the bottom */
    int col = BLACK;

    FillArea(0, y0, w, BS_TOP_BAR_H, WHITE);
    DrawLine(0, y0 + BS_TOP_BAR_H, w, y0 + BS_TOP_BAR_H, col);

    /* Left button: back-arrow when drilled into a dimension group, or
     * on the Search page; the stock house icon otherwise (i.e. the
     * plain shelf / grouped top level, where the tap is a no-op).
     * Both are drawn inside the common TOP_ICON_SIZE icon box centred
     * in the button, matching the other top-bar icons. */
    int home_w = BS_TOP_BTN_SIZE;
    int home_x = BS_TOP_BTN_PAD;
    int home_y = y0 + (BS_TOP_BAR_H - home_w) / 2;
    int hcx = home_x + home_w / 2;
    int hcy = home_y + home_w / 2;
    if (bs_g_drill_level > 0 || bs_g_state.tab == BS_TAB_SEARCH) {
        bs_draw_back_icon(hcx, hcy, col);
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
    if (bs_g_state.tab != BS_TAB_SEARCH)
        draw_source_button();

    /* Centered title — series name when drilled, "Search" on the search
     * page, the active query on the filtered library shelf, nothing on
     * the plain shelf (the app name in the top bar was dropped per user
     * request). */
    ifont *tf = OpenFont(DEFAULTFONT, 44, 0);
    if (tf != NULL) {
        char title[BS_MAX_QUERY_LEN + 16];
        top_bar_title(title, sizeof title);
        if (title[0] != '\0')
            draw_top_bar_title(w, y0, home_x, home_w, col, tf, title, sizeof title);
        CloseFont(tf);
    }
    if (bs_g_state.tab == BS_TAB_SEARCH) {
        /* Search page: the input row owns search here, so no right
         * icons — the corner stays empty (taps there fall through). */
        return;
    }
    bs_draw_search_icon();
    bs_draw_layout_icon();
    bs_draw_sync_icon();

    /* Right "menu" button — three black hamburger lines on the white
     * top bar, sized to the common icon box. */
    int menu_w = BS_TOP_BTN_SIZE;
    int menu_x = w - menu_w - BS_TOP_BTN_PAD;
    int menu_y = y0 + (BS_TOP_BAR_H - menu_w) / 2;
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
    DrawLine(BS_TOP_BTN_PAD + BS_TOP_BTN_SIZE + 4, y0, BS_TOP_BTN_PAD + BS_TOP_BTN_SIZE + 4, y0 + BS_TOP_BAR_H, col);
    DrawLine(BS_SOURCE_BTN_X + bs_source_btn_w(), y0, BS_SOURCE_BTN_X + bs_source_btn_w(), y0 + BS_TOP_BAR_H, col);
    DrawLine(w - (BS_TOP_BTN_PAD + 4 * BS_TOP_BTN_SIZE), y0, w - (BS_TOP_BTN_PAD + 4 * BS_TOP_BTN_SIZE), y0 + BS_TOP_BAR_H, col);
    DrawLine(w - (BS_TOP_BTN_PAD + 3 * BS_TOP_BTN_SIZE), y0, w - (BS_TOP_BTN_PAD + 3 * BS_TOP_BTN_SIZE), y0 + BS_TOP_BAR_H, col);
    DrawLine(w - (BS_TOP_BTN_PAD + 2 * BS_TOP_BTN_SIZE), y0, w - (BS_TOP_BTN_PAD + 2 * BS_TOP_BTN_SIZE), y0 + BS_TOP_BAR_H, col);
    DrawLine(w - (BS_TOP_BTN_PAD + BS_TOP_BTN_SIZE), y0, w - (BS_TOP_BTN_PAD + BS_TOP_BTN_SIZE), y0 + BS_TOP_BAR_H, col);
}

/* Sync button in the top bar, left of the menu button: two black arc
 * arrows (a "refresh" glyph) on the white top bar, rotating a few
 * degrees per second while a sync or download is in flight
 * (sync_set_active arms the rotation timer).  Tapping it runs a
 * library sync (see hit_top_bar). */
static int
sync_active(void)
{
    return bs_g_state.sync_state == 1 || bs_downloads_pending() > 0 || bs_g_dl_batch_active;
}

/* 1 = any modal overlay or popup is up (input routing, long-press
 * arming, and background work like cover fetches should pause). */
int
bs_modal_open(void)
{
    return bs_g_state.overlay != BS_OV_NONE || bs_g_state.dl_popup || bs_g_state.sync_popup;
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
    bs_g_state.sync_angle = (bs_g_state.sync_angle + 15) % 360;
    /* The glyph only exists on the Library tab; elsewhere the top bar
     * is redrawn whole when the state that feeds it changes. */
    if (!bs_modal_open() && bs_g_state.tab != BS_TAB_SEARCH) {
        bs_draw_sync_icon();
        PartialUpdate(ScreenWidth() - BS_TOP_BTN_PAD - 2 * BS_TOP_BTN_SIZE, 0, BS_TOP_BTN_SIZE, BS_TOP_BAR_H);
    }
    SetWeakTimerEx("bspin", sync_spin_tick, NULL, 1000);
}

void
bs_sync_set_active(int on)
{
    /* Arm the 1s rotation timer exactly once per active stretch; repeated
     * calls (every download tick) must not reset it or it never fires. */
    if (on && sync_active() && !spin_armed) {
        spin_armed = 1;
        SetWeakTimerEx("bspin", sync_spin_tick, NULL, 1000);
    }
    if (!bs_modal_open() && bs_g_state.tab != BS_TAB_SEARCH) {
        bs_draw_sync_icon();
        PartialUpdate(ScreenWidth() - BS_TOP_BTN_PAD - 2 * BS_TOP_BTN_SIZE, 0, BS_TOP_BTN_SIZE, BS_TOP_BAR_H);
    }
}

void
bs_draw_sync_icon(void)
{
    int w = ScreenWidth();
    int y0 = 0;
    int ic_w = BS_TOP_BTN_SIZE;
    int ic_x = w - ic_w - BS_TOP_BTN_PAD - ic_w; /* left of the menu button */
    int ic_y = y0 + (BS_TOP_BAR_H - ic_w) / 2;
    FillArea(ic_x, ic_y, ic_w, ic_w, WHITE);
    int cx = ic_x + ic_w / 2;
    int cy = ic_y + ic_w / 2;
    int r = 22; /* arcs fit the common TOP_ICON_SIZE icon box */
    /* Two 120-degree arc arrows, rotated by g_state.sync_angle. */
    for (int half = 0; half < 2; half++) {
        int a0 = bs_g_state.sync_angle + half * 180;
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

/* Layout-switch button, between the search and sync icons: toggles
 * the cover grid / list view.  The glyph reflects the CURRENT layout —
 * a 2x2 grid of cells in grid mode, three rows with leading squares
 * in list mode — so it visibly changes on every toggle. */
void
bs_draw_layout_icon(void)
{
    int w = ScreenWidth();
    int y0 = 0;
    int col = BLACK;
    int ic_w = BS_TOP_BTN_SIZE;
    int ic_x = w - ic_w - BS_TOP_BTN_PAD - 2 * ic_w; /* left of the sync button */
    int ic_y = y0 + (BS_TOP_BAR_H - ic_w) / 2;
    FillArea(ic_x, ic_y, ic_w, ic_w, WHITE);
    int cx = ic_x + ic_w / 2;
    int cy = ic_y + ic_w / 2;
    if (bs_g_state.view_mode == BS_VIEW_LIST) {
        /* List glyph: three rows, each a leading square + a line. */
        for (int i = 0; i < 3; i++) {
            int ry = cy - 26 + i * 26;
            DrawRect(cx - 30, ry - 8, 18, 16, col);
            DrawLine(cx - 4, ry, cx + 30, ry, col);
        }
    } else {
        /* Grid glyph: 2x2 cells. */
        for (int r = 0; r < 2; r++) {
            for (int c = 0; c < 2; c++) {
                DrawRect(cx - 36 + c * 36, cy - 36 + r * 36, 28, 28, col);
            }
        }
    }
}

/* Magnifying-glass icon in the top bar.  Replaces the old separate
 * search row: tapping it opens the Search sub-page (see on_event).
 * Line-art style matching home/sync.  Position: left of the sync
 * button. */
void
bs_draw_search_icon(void)
{
    int w = ScreenWidth();
    int y0 = 0;
    int col = BLACK;
    int ic_w = BS_TOP_BTN_SIZE;
    int menu_x = w - BS_TOP_BTN_SIZE - BS_TOP_BTN_PAD;
    int ic_x = menu_x - 3 * ic_w; /* left of the layout + sync buttons */
    int ic_y = y0 + (BS_TOP_BAR_H - ic_w) / 2;
    int cx = ic_x + ic_w / 2 - 5; /* ring centre, offset for the handle */
    int cy = ic_y + ic_w / 2 - 5;
    int r = 20; /* ring + handle fit the common TOP_ICON_SIZE icon box */

    /* Outlined ring (polyline; DrawCircle fills). */
    bs_draw_circle_outline(cx, cy, r, col);
    /* Handle: double-width diagonal from the ring edge out to the
     * corner of the icon box. */
    DrawLine(cx + r - 4, cy + r - 4, cx + r + 10, cy + r + 10, col);
    DrawLine(cx + r - 3, cy + r - 5, cx + r + 11, cy + r + 9, col);
}

/* Number of downloads still pending (queued or in flight) — shown as a
 * badge on the downloads icon so the user can see work is in progress. */
int
bs_downloads_pending(void)
{
    int n = 0;
    for (int i = 0; i < bs_g_download_count; i++) {
        if (bs_g_downloads[i].state == 0)
            n++;
        else if (bs_g_downloads[i].state == 1 && !bs_dl_fetch_idle())
            n++; /* in flight; counts until the fetch's worker fn is done */
    }
    return n;
}
