/* eh_search.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_model.h"
#include "eh_store.h"
#include "eh_ui.h"

/* ── helpers extracted from eh_draw_search_tab (behavior-preserving) ── */

/* Magnifier circle + handle centered inside the search bar. */
static void
draw_search_magnifier(int bx, int by, int bh, int col)
{
    int gx = bx + 30, gy = by + bh / 2;
    eh_draw_circle_outline(gx, gy, 13, col);
    DrawLine(gx + 9, gy + 10, gx + 22, gy + 23, col);
    DrawLine(gx + 10, gy + 9, gx + 23, gy + 22, col);
}

/* Input-row text (query or placeholder) and the edit cursor. */
static void
draw_search_input_text(ifont *f, int bx, int by, int bh, int col)
{
    int tx = bx + 68;
    SetFont(f, col);
    if (eh_g_state.query[0] != '\0') {
        DrawString(tx, by + (bh - 28) / 2 - 2, eh_g_state.query);
    } else if (!eh_g_state.search_kb) {
        DrawString(tx, by + (bh - 28) / 2 - 2, eh_i18n("search.ph"));
    }
    /* cursor when the keyboard is editing the input */
    if (eh_g_state.search_kb) {
        int cursor_x = tx + StringWidth(eh_g_state.query) + 1;
        DrawLine(cursor_x, by + 6, cursor_x, by + bh - 6, WHITE);
    }
}

/* "No searches yet" message; closes the fonts and returns from the tab. */
static void
draw_search_empty(ifont *f, ifont *hf, int w, int top)
{
    if (f != NULL) {
        SetFont(f, DGRAY);
        const char *msg = eh_i18n("search.empty");
        DrawString((w - StringWidth(msg)) / 2, top + EH_SEARCH_ROW_H + 60, msg);
    }
    if (hf != NULL)
        CloseFont(hf);
    if (f != NULL)
        CloseFont(f);
}

/* Previously committed search terms, one page at a time. */
static void
draw_search_history(ifont *f, ifont *hf, int w, int top, int bot)
{
    int n = eh_store_search_count();
    if (n == 0) {
        draw_search_empty(f, hf, w, top);
        return;
    }
    int ps = eh_history_pagesize();
    if (ps < 1)
        ps = 1;
    int pages = (n + ps - 1) / ps;
    if (eh_g_state.page >= pages)
        eh_g_state.page = pages - 1;
    if (eh_g_state.page < 0)
        eh_g_state.page = 0;
    char terms[EH_SEARCH_HISTORY_MAX][EH_MAX_QUERY_LEN];
    int  got = eh_store_search_list(terms, EH_SEARCH_HISTORY_MAX, eh_g_state.page * ps);
    int  y = top + EH_SEARCH_ROW_H;
    for (int i = 0; i < got && y + EH_SEARCH_HISTORY_ROW_H <= bot; i++) {
        if (hf != NULL) {
            SetFont(hf, BLACK);
            char trunc[EH_MAX_QUERY_LEN];
            strncpy(trunc, terms[i], sizeof trunc - 1);
            trunc[sizeof trunc - 1] = '\0';
            eh_utf8_fit_width(trunc, sizeof trunc, w - 80);
            DrawString(24, y + (EH_SEARCH_HISTORY_ROW_H - 28) / 2 - 2, trunc);
        }
        DrawLine(20, y + EH_SEARCH_HISTORY_ROW_H - 1, w - 20, y + EH_SEARCH_HISTORY_ROW_H - 1, LGRAY);
        y += EH_SEARCH_HISTORY_ROW_H;
    }
    if (hf != NULL)
        CloseFont(hf);
    if (f != NULL)
        CloseFont(f);
}

/* Search sub-page body: the input row (magnifier + text box) at the
 * top, then the previously committed search terms below.  Tapping the
 * input opens the firmware keyboard; tapping a term re-runs that
 * search (see on_event). */
void
eh_draw_search_tab(void)
{
    /* Drawn on tab switch, keystroke, and suggestion taps — one line
     * per user event (the offline e2e test polls this marker). */
    eh_LOG("[bookshelf] draw_search_tab\n");
    int top, bot, cell_w, cell_h;
    (void)cell_w;
    (void)cell_h;
    eh_grid_geom(&top, &bot, &cell_w, &cell_h);
    int w = ScreenWidth();
    FillArea(0, top, w, bot - top, WHITE);

    /* ── input row: full-width search bar, magnifier inside ── */
    int bx = 16, bw = w - 32; /* bar spans the page width */
    int by = top + 10, bh = EH_SEARCH_ROW_H - 20;
    DrawRect(bx, by, bw, bh, BLACK);
    FillArea(bx + 1, by + 1, bw - 2, bh - 2, eh_g_state.search_kb ? BLACK : WHITE);
    int col = eh_g_state.search_kb ? WHITE : BLACK;
    draw_search_magnifier(bx, by, bh, col);

    ifont *f = OpenFont(DEFAULTFONT, 28, 0);
    ifont *hf = OpenFont(DEFAULTFONTB, 28, 0); /* history rows, hoisted */
    if (f != NULL)
        draw_search_input_text(f, bx, by, bh, col);

    /* ── previously searched terms ── */
    draw_search_history(f, hf, w, top, bot);
}

/* Screen rect of the live suggestion band: below the search input
 * row, above the on-screen keyboard.  While the keyboard is open the
 * band replaces the history list (draw_suggestions); when it is
 * empty the underlying page (history) stays visible underneath. */
void
eh_suggest_band(int *y_top, int *y_bot)
{
    int top, bot, cell_w, cell_h;
    (void)cell_w;
    (void)cell_h;
    eh_grid_geom(&top, &bot, &cell_w, &cell_h);
    if (y_top)
        *y_top = top + EH_SEARCH_ROW_H;
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
 * are drawn; the hit-test (eh_input.c) uses the same rule. */
void
eh_draw_suggestions(int y_top, int y_bot)
{
    int w = ScreenWidth();
    FillArea(0, y_top, w, y_bot - y_top, WHITE);
    int y = y_top;
    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    for (int i = 0; i < eh_g_nsuggest && y + EH_SEARCH_HISTORY_ROW_H <= y_bot; i++) {
        if (tf != NULL) {
            SetFont(tf, BLACK);
            char trunc[EH_SUGGEST_TERM_MAX];
            /* Bounded copy: the term source may be wider than the
             * buffer, so never feed it to a "%s" snprintf. */
            memcpy(trunc, eh_g_suggestions[i], sizeof trunc - 1);
            trunc[sizeof trunc - 1] = '\0';
            eh_utf8_fit_width(trunc, sizeof trunc, w - 80);
            DrawString(24, y + (EH_SEARCH_HISTORY_ROW_H - 28) / 2 - 2, trunc);
        }
        DrawLine(20, y + EH_SEARCH_HISTORY_ROW_H - 1, w - 20,
                 y + EH_SEARCH_HISTORY_ROW_H - 1, LGRAY);
        y += EH_SEARCH_HISTORY_ROW_H;
    }
    if (tf != NULL)
        CloseFont(tf);
}

/* History-term rows that fit below the input row on the Search page. */
int
eh_history_pagesize(void)
{
    int top, bot, cell_w, cell_h;
    eh_grid_geom(&top, &bot, &cell_w, &cell_h);
    int rows = (bot - top - EH_SEARCH_ROW_H) / EH_SEARCH_HISTORY_ROW_H;
    return rows < 1 ? 1 : rows;
}
