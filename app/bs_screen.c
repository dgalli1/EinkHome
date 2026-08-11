/* bs_screen.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
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

/* Drop whole characters from the end of `s` until its on-screen width
 * (StringWidth) fits `maxw` pixels, or it is down to 4 bytes — a lone
 * over-wide glyph then overflows instead of looping forever.  `cap`
 * bounds the byte budget exactly like utf8_cap (NUL at cap-1).  Never
 * splits a multibyte UTF-8 sequence.  Moved from bs_browser.c and
 * shared by every title/term truncation in the app. */
void
utf8_fit_width(char *s, size_t cap, int maxw)
{
    if (cap < 1)
        return;
    size_t len = strlen(s);
    if (len > cap - 1) {
        s[cap - 1] = '\0';
        len = cap - 1;
    }
    while (StringWidth(s) > maxw && len > 4) {
        /* Back up to the last character's lead byte, then cut there —
         * a multibyte char is either kept intact or removed entirely. */
        size_t i = len - 1;
        while (i > 0 && ((unsigned char)s[i] & 0xC0) == 0x80)
            i--;
        s[i] = '\0';
        len = i;
    }
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
show_hourglass(void)
{
    ibitmap *hg = GetResource("hourglass", NULL);
    if (hg == NULL)
        return;
    int x = (ScreenWidth() - hg->width) / 2;
    int y = (content_bottom() - hg->height) / 2;
    /* White backing so the glyph reads over the frozen screen. */
    FillArea(x - 12, y - 12, hg->width + 24, hg->height + 24, WHITE);
    DrawRect(x - 12, y - 12, hg->width + 24, hg->height + 24, BLACK);
    DrawBitmap(x, y, hg);
    PartialUpdate(x - 12, y - 12, hg->width + 24, hg->height + 24);
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
        /* Only a finished launcher drag leaves unflushed ghost pixels
         * in the framebuffer (the drag draws without flushing; the
         * lift flushes).  A plain state change has nothing stale, so
         * flush_content() avoids the full-screen flash. */
        if (g_state.launcher_moved)
            FullUpdate();
        else
            flush_content();
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

/* Page-flip repaint: turning a page only changes the grid/list body
 * and the pager text — the top bar (title, icons) is untouched.  So a
 * flip repaints just those two bands and flushes them as partial
 * updates, keeping the e-ink flicker local to the changed rows instead
 * of redraw_shelf()'s whole-content flush once per keypress/tap.
 * Clamps the page like redraw_shelf.  Page flips only ever run on the
 * library shelf or the Search history list — the folder browser pages
 * via browse_page and the modal overlays route keys to Back. */
void
flip_page(void)
{
    int pages = current_pages();
    if (g_state.page >= pages)
        g_state.page = pages - 1;
    if (g_state.page < 0)
        g_state.page = 0;
    int top, bot, cell_w, cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    if (g_state.tab == TAB_SEARCH)
        draw_search_tab();
    else
        draw_grid();
    PartialUpdate(0, top, ScreenWidth(), bot - top);
    draw_pager();
    PartialUpdate(0, content_bottom() - PAGER_H, ScreenWidth(), PAGER_H);
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
