/* bs_search.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
#include "bs_model.h"
#include "bs_store.h"
#include "bs_ui.h"

/* Search sub-page body: the input row (magnifier + text box) at the
 * top, then the previously committed search terms below.  Tapping the
 * input opens the firmware keyboard; tapping a term re-runs that
 * search (see on_event). */
void
draw_search_tab(void)
{
    /* Drawn on tab switch, keystroke, and suggestion taps — one line
     * per user event (the offline e2e test polls this marker). */
    LOG("[bookshelf] draw_search_tab\n");
    int top, bot, cell_w, cell_h;
    (void)cell_w;
    (void)cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int w = ScreenWidth();
    FillArea(0, top, w, bot - top, WHITE);

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
    ifont *hf = OpenFont(DEFAULTFONTB, 28, 0); /* history rows, hoisted */
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
    }

    /* ── previously searched terms ── */
    int n = store_search_count();
    if (n == 0) {
        if (f != NULL) {
            SetFont(f, DGRAY);
            const char *msg = i18n("search.empty");
            DrawString((w - StringWidth(msg)) / 2, top + SEARCH_ROW_H + 60, msg);
        }
        if (hf != NULL)
            CloseFont(hf);
        if (f != NULL)
            CloseFont(f);
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
        if (hf != NULL) {
            SetFont(hf, BLACK);
            char trunc[MAX_QUERY_LEN];
            strncpy(trunc, terms[i], sizeof trunc - 1);
            trunc[sizeof trunc - 1] = '\0';
            utf8_fit_width(trunc, sizeof trunc, w - 80);
            DrawString(24, y + (SEARCH_HISTORY_ROW_H - 28) / 2 - 2, trunc);
        }
        DrawLine(20, y + SEARCH_HISTORY_ROW_H - 1, w - 20, y + SEARCH_HISTORY_ROW_H - 1, LGRAY);
        y += SEARCH_HISTORY_ROW_H;
    }
    if (hf != NULL)
        CloseFont(hf);
    if (f != NULL)
        CloseFont(f);
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
    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    for (int i = 0; i < g_nsuggest && y + SEARCH_HISTORY_ROW_H <= y_bot; i++) {
        if (tf != NULL) {
            SetFont(tf, BLACK);
            char trunc[SUGGEST_TERM_MAX];
            /* Bounded copy: the term source may be wider than the
             * buffer, so never feed it to a "%s" snprintf. */
            memcpy(trunc, g_suggestions[i], sizeof trunc - 1);
            trunc[sizeof trunc - 1] = '\0';
            utf8_fit_width(trunc, sizeof trunc, w - 80);
            DrawString(24, y + (SEARCH_HISTORY_ROW_H - 28) / 2 - 2, trunc);
        }
        DrawLine(20, y + SEARCH_HISTORY_ROW_H - 1, w - 20,
                 y + SEARCH_HISTORY_ROW_H - 1, LGRAY);
        y += SEARCH_HISTORY_ROW_H;
    }
    if (tf != NULL)
        CloseFont(tf);
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
