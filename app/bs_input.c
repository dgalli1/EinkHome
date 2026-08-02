/* bs_input.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── hit-testing ─────────────────────────────────────────────────────── */

int
hit_top_bar(int x, int y)
{
    int bar_top = g_state.panel_h;
    int bar_bot = bar_top + TOP_BAR_H;
    if (y < bar_top || y >= bar_bot)
        return -1;
    int w = ScreenWidth();
    /* Left "home" button — 96×96 region, padded 8 px on the left. */
    if (x >= 8 && x < 8 + 96)
        return 1;
    /* Right "menu" button — 96×96 region, padded 8 px on the right. */
    if (x >= w - 96 - 8 && x < w - 8)
        return 3;
    return -1;
}

int
hit_search(int x, int y)
{
    int row_top = g_state.panel_h + TOP_BAR_H;
    int row_bot = row_top + SEARCH_ROW_H;
    if (y < row_top || y >= row_bot)
        return -1;
    int w = ScreenWidth();
    int tx = 64, tw = w - 128;
    int ty = row_top + 10;
    int th = SEARCH_ROW_H - 20;
    if (x < tx || x >= tx + tw)
        return -1;
    if (y < ty || y >= ty + th)
        return -1;
    return 1;
}

/* Returns 0 for the Library tab, 1 for the Downloads tab, -1 elsewhere. */
int
hit_tab_row(int x, int y)
{
    int row_top = g_state.panel_h + TOP_BAR_H + SEARCH_ROW_H;
    int row_bot = row_top + TAB_ROW_H;
    if (y < row_top || y >= row_bot)
        return -1;
    int w = ScreenWidth();
    return (x < w / 2) ? 0 : 1;
}

int
hit_thumbnail(int x, int y)
{
    int top, bot, cell_w, cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int cols = view_cols();
    int rows = view_rows();
    int page_start = g_state.page * view_pagesize();
    for (int row = 0; row < rows; row++) {
        for (int col = 0; col < cols; col++) {
            int idx = page_start + row * cols + col;
            if (idx >= g_view_count)
                return -1;
            int tx = 8 + col * cell_w;
            int ty = top + 4 + row * cell_h;
            int tw = cell_w - 8;
            int th = cell_h - 6;
            if (x >= tx && x < tx + tw && y >= ty && y < ty + th)
                return idx;
        }
    }
    return -1;
}

int
hit_pager(int x, int y)
{
    int h = ScreenHeight();
    int y0 = h - PAGER_H;
    if (y < y0 || y >= y0 + PAGER_H)
        return 0;
    int w = ScreenWidth();
    /* Prev — 96px wide starting at x=12 */
    if (g_state.page > 0 && x >= 12 && x < 12 + 96)
        return -1;
    /* Next — 96px wide ending at x=w-12 */
    int pages = current_pages();
    if (g_state.page + 1 < pages && x >= w - 108 && x < w - 12)
        return -2;
    return 0;
}

/* ── tap handlers ────────────────────────────────────────────────────── */

void
on_tap_overlay_menu(int x, int y)
{
    int y0 = 96, item_h = 88;
    int pw = ScreenWidth() * 3 / 4;
    if (x < 0 || x >= pw) {
        g_state.menu_open = 0;
        return;
    }
    y -= g_state.panel_h;
    for (int i = 0; i < 4; i++) {
        if (y >= y0 + i * item_h && y < y0 + i * item_h + item_h) {
            g_state.group = (GroupMode)i;
            g_drilled_series[0] = '\0';
            g_state.menu_open = 0;
            do_sync();
        }
    }
    g_state.menu_open = 0;
}

void
on_tap_overlay_more(int x, int y)
{
    int pw = ScreenWidth() * 3 / 4;
    int px = ScreenWidth() - pw;
    if (x < px || x >= ScreenWidth()) {
        g_state.more_open = 0;
        return;
    }
    y -= g_state.panel_h;
    if (y >= MORE_Y0 && y < MORE_Y0 + MORE_ITEM_H) {
        g_state.more_open = 0;
        do_sync();
        return;
    }
    /* Settings row opens the full-screen settings page. */
    if (y >= MORE_Y0 + MORE_SETTINGS_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_SETTINGS_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        g_state.settings_open = 1;
        g_settings_edit = 0;
        draw_overlay_settings();
        FullUpdate();
        return;
    }
    /* System menu row launches the firmware's control panel dropdown. */
    if (y >= MORE_Y0 + MORE_SYSTEM_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_SYSTEM_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        LOG("[bookshelf] opening system control panel\n");
        OpenControlPanel(NULL);
        return;
    }
    /* Applications row opens the in-app launcher overlay. */
    if (y >= MORE_Y0 + MORE_APPS_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_APPS_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        launcher_open_set();
        return;
    }
    /* Download-all row queues every book in the library and jumps to the
     * Downloads tab so the user watches the queue drain. */
    if (y >= MORE_Y0 + MORE_DLALL_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_DLALL_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        for (int i = 0; i < g_lib_count; i++)
            enqueue_download(&g_lib[i]);
        LOG("[bookshelf] download-all queued=%d\n", g_lib_count);
        g_state.tab = TAB_DOWNLOADS;
        redraw_shelf();
        return;
    }
    for (int i = 1; i < MORE_DLALL_IDX; i++) {
        if (y >= MORE_Y0 + i * MORE_ITEM_H && y < MORE_Y0 + i * MORE_ITEM_H + MORE_ITEM_H) {
            g_state.more_open = 0;
            if (i == MORE_GRID_IDX) {
                g_state.view_mode = VIEW_GRID;
                g_state.page = 0;
            } else if (i == MORE_LIST_IDX) {
                g_state.view_mode = VIEW_LIST;
                g_state.page = 0;
            } else {
                /* i = 1..5 → the five sort modes (title↑/↓, author,
                 * series, recent). */
                g_state.sort = (SortMode)(i - 1);
                apply_filter_and_sort();
            }
            return;
        }
    }
    g_state.more_open = 0;
}

/* Close the settings overlay and repaint the shelf beneath it. */
void
settings_close(void)
{
    g_state.settings_open = 0;
    g_settings_edit = 0;
    redraw_shelf();
}

/* Persist settings, rebuild the endpoint URLs from the (possibly edited)
 * api_base / api_token, then re-sync so the shelf reflects the new
 * server immediately. */
void
settings_apply(void)
{
    save_config_file();
    build_endpoint_urls();
    g_state.settings_open = 0;
    g_settings_edit = 0;
    do_sync();
    redraw_shelf();
}

void
on_tap_overlay_settings(int x, int y)
{
    (void)x; /* rows span the full content width; only y matters */
    y -= g_state.panel_h;

    int y_row1 = 112;
    int y_row2 = y_row1 + SETTINGS_ROW_H;
    int y_row3 = y_row2 + SETTINGS_ROW_H;
    int y_save = y_row3 + SETTINGS_ROW_H + 24;
    int y_back = y_save + SETTINGS_BTN_H;

    if (y >= y_row1 && y < y_row1 + SETTINGS_ROW_H - 12) {
        g_settings_edit = 1;
        snprintf(g_settings_kb_buf, sizeof g_settings_kb_buf, "%s", g_state.api_base);
        draw_overlay_settings();
        FullUpdate();
        OpenKeyboard(i18n("settings.api_host"),
                     g_settings_kb_buf,
                     sizeof g_settings_kb_buf - 1,
                     0,
                     settings_keyboard_handler);
        return;
    }
    if (y >= y_row2 && y < y_row2 + SETTINGS_ROW_H - 12) {
        g_settings_edit = 2;
        snprintf(g_settings_kb_buf, sizeof g_settings_kb_buf, "%s", g_state.api_token);
        draw_overlay_settings();
        FullUpdate();
        OpenKeyboard(i18n("settings.api_key"),
                     g_settings_kb_buf,
                     sizeof g_settings_kb_buf - 1,
                     0,
                     settings_keyboard_handler);
        return;
    }
    if (y >= y_row3 && y < y_row3 + SETTINGS_ROW_H - 12) {
        /* Cycle Auto → reader[0] → reader[1] → … → Auto. */
        g_state.reader_pref = (g_state.reader_pref + 1) % (g_reader_count + 1);
        draw_overlay_settings();
        FullUpdate();
        return;
    }
    if (y >= y_save && y < y_save + SETTINGS_BTN_H - 12) {
        settings_apply();
        return;
    }
    if (y >= y_back && y < y_back + SETTINGS_BTN_H - 12) {
        settings_close();
        return;
    }
}

