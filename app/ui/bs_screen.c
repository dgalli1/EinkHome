/* bs_screen.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_browser.h"
#include "bs_launcher.h"
#include "bs_model.h"
#include "bs_store.h"
#include "bs_ui.h"

/* ── UTF-8 helpers ──────────────────────────────────────────────────── */

/* Cap `s` at `cap` bytes (NUL at cap-1), then walk back so the string
 * never ends mid-character: the continuation bytes (0x80..0xBF) of a
 * truncated multibyte char are removed together with its lead byte.
 * ASCII text and complete trailing characters are left untouched, so a
 * byte budget never leaves a dangling half character.  Title/author/
 * suggestion display truncation goes through this (or through
 * utf8_fit_width, which caps by pixel width on top of the byte
 * budget). */
void
bs_utf8_cap(char *s, size_t cap)
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

/* Drop whole characters from the end of `s` until its on-screen width
 * (StringWidth) fits `maxw` pixels, or it is down to 4 bytes — a lone
 * over-wide glyph then overflows instead of looping forever.  `cap`
 * bounds the byte budget exactly like utf8_cap (NUL at cap-1).  Never
 * splits a multibyte UTF-8 sequence.  Moved from bs_browser.c and
 * shared by every title/term truncation in the app. */
static int
prefix_width(char *s, size_t b)
{
    /* Width of the prefix s[0..b), measured at the character boundary b
     * by temporarily terminating there; the byte is restored after. */
    char saved = s[b];
    s[b] = '\0';
    int w = StringWidth(s);
    s[b] = saved;
    return w;
}

void
bs_utf8_fit_width(char *s, size_t cap, int maxw)
{
    if (cap < 1)
        return;
    size_t len = strlen(s);
    if (len > cap - 1) {
        s[cap - 1] = '\0';
        len = cap - 1;
    }
    /* Trivial cases: already within the 4-char floor, or already fits. */
    if (len <= 4 || StringWidth(s) <= maxw)
        return;

    /* A fixed-font StringWidth grows monotonically with the prefix, so
     * the fit predicate is monotone and binary search finds the longest
     * fitting character-aligned prefix in O(log n) width measurements
     * instead of the old O(n²) chop-and-re-measure loop.  `lo` is a
     * character boundary that fits, `hi` one that is too wide. */
    size_t lo = 4;
    while (lo < len && ((unsigned char)s[lo] & 0xC0) == 0x80)
        lo++;                 /* lo = end of the 4th character */
    size_t hi = len;          /* full string is too wide here */

    if (prefix_width(s, lo) <= maxw) {
        while (hi - lo > 1) {
            size_t mid = lo + (hi - lo) / 2;
            /* Snap mid to a character boundary: back up continuation
             * bytes to the char's lead byte.  If that lands on `lo`
             * (mid sits inside the char that starts at lo), step
             * forward to that char's end instead.  Either way the byte
             * cut lands exactly on a boundary, so a multibyte char is
             * kept intact or dropped whole. */
            size_t b = mid;
            while (b > lo && ((unsigned char)s[b] & 0xC0) == 0x80)
                b--;
            if (b == lo) {
                b = mid;
                while (b < hi && ((unsigned char)s[b] & 0xC0) == 0x80)
                    b++;
                if (b >= hi)
                    break;   /* no boundary strictly between lo and hi */
            }
            if (prefix_width(s, b) <= maxw)
                lo = b;
            else
                hi = b;
        }
    }
    /* Else the 4-char floor itself is too wide: keep it, matching the
     * old loop stopping at len == 4.  `lo` is the longest fitting
     * boundary; truncate there. */
    s[lo] = '\0';
}

/* ── drawing primitives ─────────────────────────────────────────────── */

void
bs_draw_text_centered(ifont *f, int cx, int cy, const char *text, int color)
{
    if (f == NULL)
        return;
    SetFont(f, color);
    DrawString(cx - StringWidth(text) / 2, cy, text);
}

void
bs_draw_button_font(
    int x, int y, int w, int h, int selected, const char *label, int label_size,
    ifont *f, int label_color)
{
    DrawRect(x, y, w, h, BLACK);
    FillArea(x + 1, y + 1, w - 2, h - 2, selected ? BLACK : WHITE);
    if (label == NULL || label[0] == '\0')
        return;
    if (f != NULL) {
        SetFont(f, label_color != 0 ? label_color : (selected ? WHITE : BLACK));
        DrawString(x + (w - StringWidth(label)) / 2, y + (h - label_size) / 2 - 2, label);
    }
}

void
bs_draw_button(
    int x, int y, int w, int h, int selected, const char *label, int label_size, int label_color)
{
    ifont *f = OpenFont(DEFAULTFONTB, label_size, 0);
    bs_draw_button_font(x, y, w, h, selected, label, label_size, f, label_color);
    if (f != NULL)
        CloseFont(f);
}

/* 1 = the firmware's panel painter never activated (PanelHeight()==0 at
 * init); we draw the status strip ourselves — at the BOTTOM, where the
 * firmware's type-1 panel lives. */
int bs_g_self_panel = 0;

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
bs_draw_system_strip(void)
{
    int w = ScreenWidth();
    int h = bs_g_state.panel_h;
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
bs_stamp_panel(void)
{
    if (bs_g_self_panel)
        bs_draw_system_strip();
    else
        iv_update_panel(0);
}

/* Bottom edge of the app-owned content area: the firmware's type-1
 * system panel occupies [content_bottom(), ScreenHeight()), so every
 * app surface (top bar, grid, pager, overlays) lives above it.  The
 * stock desktop does the same (its MainFrame is created with height
 * ScreenHeight() - PanelHeight()). */
int
bs_content_bottom(void)
{
    return ScreenHeight() - bs_g_state.panel_h;
}

/* Page count for the active tab: the library pages the cover grid, the
 * search page pages the history terms.  Always >= 1. */
int
bs_current_pages(void)
{
    int n, ps;
    if (bs_g_state.tab == BS_TAB_SEARCH) {
        n = bs_store_search_count();
        ps = bs_history_pagesize();
    } else {
        n = bs_g_view_total;
        ps = bs_view_pagesize();
    }
    if (ps < 1)
        ps = 1;
    int pages = (n + ps - 1) / ps;
    return pages < 1 ? 1 : pages;
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
 * and the launcher drag-end (ghost clear after an unflushed drag).
 * The pager band [content_bottom()-PAGER_H, content_bottom()) is a
 * separate update region on the firmware (flip_page commits it
 * explicitly), so surfaces that draw into it (launcher scroll buttons)
 * must commit it with its own partial update — a plain content partial
 * leaves it stale. */
void
bs_flush_content(void)
{
    PartialUpdate(0, 0, ScreenWidth(), bs_content_bottom());
    PartialUpdate(0, bs_content_bottom() - BS_PAGER_H, ScreenWidth(), BS_PAGER_H);
}

/* Show the theme's hourglass centered on the screen and leave it up
 * until the launched task draws over it (or the caller hides it and
 * redraws on launch failure).
 *
 * The firmware's own ShowHourglassForceAt() is not used: its animation
 * is driven by monitor.app via REQ_HOURGLASS and never lands in the
 * app framebuffer (verified: nothing appears).  Drawing the theme's
 * hourglass bitmap directly with DrawBitmap() is guaranteed to show on
 * any build. */
void
bs_show_hourglass(void)
{
    ibitmap *hg = GetResource("hourglass", NULL);
    if (hg == NULL)
        return;
    int x = (ScreenWidth() - hg->width) / 2;
    int y = (bs_content_bottom() - hg->height) / 2;
    /* White backing so the glyph reads over the frozen screen. */
    FillArea(x - 12, y - 12, hg->width + 24, hg->height + 24, WHITE);
    DrawRect(x - 12, y - 12, hg->width + 24, hg->height + 24, BLACK);
    DrawBitmap(x, y, hg);
    PartialUpdate(x - 12, y - 12, hg->width + 24, hg->height + 24);
}

/* Draw the shelf content (top bar, body, pager, popups) WITHOUT
 * flushing, so a caller can follow with a single refresh of its
 * choosing.  redraw_shelf() draws here then flushes the content area
 * as a partial; the keyboard-commit path draws here and follows with
 * one full-screen FullUpdate so the panel band the keyboard wiped is
 * repainted in the same refresh instead of a second full cycle. */
void
bs_draw_shelf_nofb(void)
{
    /* Clamp the page before the body draws: a view change that shrank
     * the page count (list mode on a deep page, a tightening filter)
     * would otherwise let draw_grid paint an empty page — draw_pager's
     * own clamp only runs after the grid. */
    int pages = bs_current_pages();
    if (bs_g_state.page >= pages)
        bs_g_state.page = pages - 1;
    if (bs_g_state.page < 0)
        bs_g_state.page = 0;

    if (bs_g_state.overlay == BS_OV_LAUNCHER) {
        bs_draw_overlay_launcher();
        return;
    }
    FillArea(0, 0, ScreenWidth(), bs_content_bottom(), WHITE);
    bs_draw_top_bar();
    if (bs_g_state.tab == BS_TAB_SEARCH)
        bs_draw_search_tab();
    else if (bs_g_state.source == BS_SOURCE_FOLDER && bs_g_browse_open)
        bs_draw_browse();
    else
        bs_draw_grid();
    if (bs_g_state.source != BS_SOURCE_FOLDER)
        bs_draw_pager();
    if (bs_g_state.dl_popup)
        bs_draw_dl_popup();
    if (bs_g_state.sync_popup)
        bs_draw_sync_popup();
}

/* Repaint the whole shelf (top bar, body, pager) in the current tab,
 * then the download popup on top when one is open.  Centralises the
 * sequence every state change needs. */
void
bs_redraw_shelf(void)
{
    bs_draw_shelf_nofb();
    if (bs_g_state.overlay == BS_OV_LAUNCHER) {
        /* Only a finished launcher drag leaves unflushed ghost pixels
         * in the framebuffer (the drag draws without flushing; the
         * lift flushes).  A plain state change has nothing stale, so
         * flush_content() avoids the full-screen flash. */
        if (bs_g_state.launcher_moved)
            FullUpdate();
        else
            bs_flush_content();
        return;
    }
    bs_flush_content();
}

/* Page-flip repaint: turning a page only changes the grid/list body
 * and the pager text — the top bar (title, icons) is untouched.  So a
 * flip repaints just those two bands and flushes them as partial
 * updates, keeping the e-ink flicker local to the changed rows instead
 * of redraw_shelf()'s whole-content flush once per keypress/tap.
 * Clamps the page like redraw_shelf.  Page flips only ever run on the
 * library shelf or the Search history list — the folder browser pages
 * via browse_page and the modal overlays route keys to Back. */
void
bs_flip_page(void)
{
    int pages = bs_current_pages();
    if (bs_g_state.page >= pages)
        bs_g_state.page = pages - 1;
    if (bs_g_state.page < 0)
        bs_g_state.page = 0;
    int top, bot, cell_w, cell_h;
    bs_grid_geom(&top, &bot, &cell_w, &cell_h);
    if (bs_g_state.tab == BS_TAB_SEARCH)
        bs_draw_search_tab();
    else
        bs_draw_grid();
    PartialUpdate(0, top, ScreenWidth(), bot - top);
    bs_draw_pager();
    PartialUpdate(0, bs_content_bottom() - BS_PAGER_H, ScreenWidth(), BS_PAGER_H);
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
bs_draw_scroll_buttons_at(int up_ok, int down_ok, int y0)
{
    if (!up_ok && !down_ok)
        return;
    int w = ScreenWidth();

    FillArea(0, y0, BS_SCROLL_BTN_W, BS_SCROLL_BTN_H, WHITE);
    DrawRect(0, y0, BS_SCROLL_BTN_W, BS_SCROLL_BTN_H, up_ok ? BLACK : LGRAY);
    int col = up_ok ? BLACK : LGRAY;
    int cx = BS_SCROLL_BTN_W / 2;
    int cy = y0 + BS_SCROLL_BTN_H / 2;
    DrawLine(cx - 24, cy + 14, cx, cy - 14, col);
    DrawLine(cx + 24, cy + 14, cx, cy - 14, col);

    int x2 = w - BS_SCROLL_BTN_W;
    FillArea(x2, y0, BS_SCROLL_BTN_W, BS_SCROLL_BTN_H, WHITE);
    DrawRect(x2, y0, BS_SCROLL_BTN_W, BS_SCROLL_BTN_H, down_ok ? BLACK : LGRAY);
    col = down_ok ? BLACK : LGRAY;
    cx = x2 + BS_SCROLL_BTN_W / 2;
    DrawLine(cx - 24, cy - 14, cx, cy + 14, col);
    DrawLine(cx + 24, cy - 14, cx, cy + 14, col);
}

void
bs_draw_scroll_buttons(int up_ok, int down_ok)
{
    bs_draw_scroll_buttons_at(up_ok, down_ok, bs_content_bottom() - BS_SCROLL_BTN_H);
}

/* Hit test for the corner scroll buttons: -1 = up (bottom-left),
 * +1 = down (bottom-right), 0 = neither. */
int
bs_hit_scroll_button_at(int x, int y, int y0)
{
    int w = ScreenWidth();
    if (y < y0 || y >= y0 + BS_SCROLL_BTN_H)
        return 0;
    if (x >= 0 && x < BS_SCROLL_BTN_W)
        return -1;
    if (x >= w - BS_SCROLL_BTN_W && x < w)
        return +1;
    return 0;
}

int
bs_hit_scroll_button(int x, int y)
{
    return bs_hit_scroll_button_at(x, y, bs_content_bottom() - BS_SCROLL_BTN_H);
}

/* ── full-screen overlay header ─────────────────────────────────────── */

/* Back-button touch box in the shared overlay header: used by the draw
 * path (bs_draw_overlay_header) and every overlay's tap hit-test so
 * the tappable region always matches the painted chevron. */
void
bs_overlay_back_rect(int *bx, int *by, int *bw, int *bh)
{
    *bx = BS_OVERLAY_BACK_X;
    *by = BS_OVERLAY_BACK_Y;
    *bw = BS_OVERLAY_BACK_W;
    *bh = BS_OVERLAY_BACK_H;
}

/* The one header every full-screen overlay (launcher, settings, log
 * viewer) draws: a fixed white bar with the Back chevron in the shared
 * touch box — same offset and size as the search page's top-bar back
 * button — and the title centred on the bar.  Sharing this (plus
 * BS_OVERLAY_* in bs_core.h) keeps the three headers pixel-identical;
 * before the share, the settings and log headers had drifted to
 * different heights and offsets. */
void
bs_draw_overlay_header(const char *title)
{
    int w = ScreenWidth();
    FillArea(0, 0, w, BS_OVERLAY_HEADER_H, WHITE);
    DrawLine(0, BS_OVERLAY_HEADER_H - 1, w, BS_OVERLAY_HEADER_H - 1, BLACK);

    int bx, by, bw, bh;
    bs_overlay_back_rect(&bx, &by, &bw, &bh);
    bs_draw_back_icon(bx + bw / 2, by + bh / 2, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 36, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        int tw = StringWidth(title);
        DrawString((w - tw) / 2, (BS_OVERLAY_HEADER_H - 36) / 2, title);
        CloseFont(tf);
    }
}
