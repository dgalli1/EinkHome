/* bs_input.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── hit-testing ─────────────────────────────────────────────────────── */

int
hit_top_bar(int x, int y)
{
    int bar_top = 0; /* top bar sits at the very top; the panel is at the bottom */
    int bar_bot = bar_top + TOP_BAR_H;
    if (y < bar_top || y >= bar_bot)
        return -1;
    int w = ScreenWidth();
    /* Left button — 96×96 region, padded 8 px on the left: a back
     * arrow on the Search sub-view or a drilled series, a no-op
     * otherwise (the home icon was removed). */
    if (x >= 8 && x < 8 + 96)
        return 1;
    /* Source button — icon + label right of the house button. */
    if (x >= SOURCE_BTN_X && x < SOURCE_BTN_X + SOURCE_BTN_W)
        return 6;
    /* The Search page has no right-side icons — its corner is empty. */
    if (g_state.tab == TAB_SEARCH)
        return -1;
    /* Right 96×96 region, padded 8 px on the right: the hamburger/More
     * button. */
    if (x >= w - 96 - 8 && x < w - 8)
        return 3;
    /* Sync button — 96×96 region left of the menu button; runs a
     * library sync. */
    if (x >= w - 96 - 8 - 96 && x < w - 96 - 8)
        return 2;
    /* Search icon — 96×96 region left of the sync button; opens the
     * Search sub-page. */
    if (x >= w - 96 - 8 - 2 * 96 && x < w - 96 - 8 - 96)
        return 5;
    return -1;
}

/* 1 when (x, y) is inside the top-bar search icon (the hit region is
 * tab-dependent — see hit_top_bar). */
int
hit_search_icon(int x, int y)
{
    return hit_top_bar(x, y) == 5;
}

/* 1 when (x, y) is inside the search input row on the Search page. */
int
hit_search_input(int x, int y)
{
    int row_top = TOP_BAR_H + TOP_BAR_PAD;
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

/* 0-based index of the history term row tapped on the Search page, or
 * -1 when the tap is outside the history list. */
int
hit_history(int x, int y)
{
    (void)x;
    int top, bot, cell_w, cell_h;
    (void)cell_w;
    (void)cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int y0 = top + SEARCH_ROW_H;
    if (y < y0)
        return -1;
    int ps = history_pagesize();
    if (ps < 1)
        ps = 1;
    int rel = (y - y0) / SEARCH_HISTORY_ROW_H;
    if (rel >= ps)
        return -1;
    int idx = g_state.page * ps + rel;
    if (idx >= store_search_count())
        return -1;
    return idx;
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
            if (idx >= g_view_total)
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
    int y0 = content_bottom() - PAGER_H;
    if (y < y0 || y >= y0 + PAGER_H)
        return 0;
    int w = ScreenWidth();
    int pages = current_pages();
    /* < prev — 96px wide starting at x=12 */
    if (g_state.page > 0 && x >= 12 && x < 12 + 96)
        return -1;
    /* << first page — next 96px slot */
    if (g_state.page > 0 && x >= 116 && x < 116 + 96)
        return -3;
    /* >> last page — 96px slot left of the next button */
    if (g_state.page + 1 < pages && x >= w - 212 && x < w - 116)
        return -4;
    /* > next — 96px wide ending at x=w-12 */
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
    for (int i = 0; i < 4; i++) {
        if (y >= y0 + i * item_h && y < y0 + i * item_h + item_h) {
            g_state.group = (GroupMode)i;
            g_drilled_series[0] = '\0';
            g_state.menu_open = 0;
            view_rebuild();
        }
    }
    g_state.menu_open = 0;
}

/* Handle a tap while the More overlay is open.  Returns 1 when the
 * action already repainted the screen itself (settings, launcher,
 * download-all) — the caller must then skip its follow-up redraw or
 * the whole content area flushes twice per tap. */
int
on_tap_overlay_more(int x, int y)
{
    int pw = ScreenWidth() * 3 / 4;
    int px = ScreenWidth() - pw;
    if (x < px || x >= ScreenWidth()) {
        g_state.more_open = 0;
        return 0;
    }
    if (y >= MORE_Y0 && y < MORE_Y0 + MORE_ITEM_H) {
        g_state.more_open = 0;
        do_sync();
        return 0;
    }
    /* Settings row opens the full-screen settings page. */
    if (y >= MORE_Y0 + MORE_SETTINGS_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_SETTINGS_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        g_state.settings_open = 1;
        g_settings_edit = 0;
        draw_overlay_settings();
        FullUpdate();
        return 1;
    }
    /* Applications row opens the in-app launcher overlay. */
    if (y >= MORE_Y0 + MORE_APPS_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_APPS_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        launcher_open_set();
        return 1;
    }
    /* Download-all row queues every book in the library and opens the
     * download-progress popup so the user watches the queue drain. */
    if (y >= MORE_Y0 + MORE_DLALL_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_DLALL_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        download_all_start();
        return 1;
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
                view_rebuild();
            }
            return 0;
        }
    }
    g_state.more_open = 0;
    return 0;
}

/* Handle a tap on the source chooser.  Row 0/1 switch the library
 * source directly (Kavita / scanner-local); row 2 opens the folder
 * picker — the picked directory becomes the folder source.  A tap
 * outside the sheet dismisses.  Returns 1 (the chooser owns the
 * screen while open). */
int
on_tap_source(int x, int y)
{
    int pw, ph, px, py;
    source_geom(&px, &py, &pw, &ph);
    if (x < px || x >= px + pw || y < py || y >= py + ph) {
        g_source_open = 0;
        redraw_shelf();
        return 1;
    }
    int row = (y - (py + 80)) / 96;
    if (row < 0 || row >= 3)
        return 1;
    g_source_open = 0;
    if (row == 0) {
        g_state.source = SOURCE_KAVITA;
        g_browse_open = 0;
        g_browse_drag = 0;
        save_config_file();
        do_sync();
        redraw_shelf();
    } else if (row == 1) {
        g_state.source = SOURCE_LOCAL;
        g_browse_open = 0;
        g_browse_drag = 0;
        save_config_file();
        do_sync();
        redraw_shelf();
    } else {
        /* Folder source: the browser is always rooted at /mnt/ext1 —
         * the user only has this partition, so there is no base
         * directory to choose. */
        g_state.source = SOURCE_FOLDER;
        save_config_file();
        browse_start(BROWSE_ROOT);
    }
    return 1;
}

/* Close the settings overlay and repaint the shelf beneath it.  A
 * picked-but-unsaved download folder is discarded. */
void
settings_close(void)
{
    g_state.settings_open = 0;
    g_settings_edit = 0;
    g_settings_dl_dir[0] = '\0';
    redraw_shelf();
}

/* Persist settings, rebuild the endpoint URLs from the (possibly edited)
 * api_base / api_token, re-apply the download folder, then re-sync so
 * the shelf reflects the new server immediately. */
void
settings_apply(void)
{
    save_config_file();
    build_endpoint_urls();
    resolve_downloads_dir();
    g_settings_dl_dir[0] = '\0';
    g_state.settings_open = 0;
    g_settings_edit = 0;
    /* Re-sync with the new settings; show the progress sheet (unless
     * the Folder source, which has nothing to sync). */
    if (g_state.source != SOURCE_FOLDER)
        sync_popup_open();
    do_sync();
    redraw_shelf();
}

void
on_tap_overlay_settings(int x, int y)
{
    (void)x; /* rows span the full content width; only y matters */

    int y_row1 = 112;
    int y_row2 = y_row1 + SETTINGS_ROW_H;
    int y_row3 = y_row2 + SETTINGS_ROW_H;
    int y_row4 = y_row3 + SETTINGS_ROW_H;
    int y_save = y_row4 + SETTINGS_ROW_H + 24;
    int y_back = y_save + SETTINGS_BTN_H;
    int y_logs = y_back + SETTINGS_BTN_H;

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
    if (y >= y_row4 && y < y_row4 + SETTINGS_ROW_H - 12) {
        /* Download-folder picker (confined to /mnt/ext1). */
        folder_open();
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
    if (y >= y_logs && y < y_logs + SETTINGS_BTN_H - 12) {
        /* Show the app log directly (Settings → Show logs). */
        g_state.settings_open = 0;
        g_settings_edit = 0;
        g_state.log_open = 1;
        g_state.log_scroll = -1; /* start at the tail */
        draw_log_view();
        FullUpdate();
        return;
    }
}

/* Taps on the full-screen log viewer: Back (top-left) or the corner
 * scroll buttons (up = older, down = newer).  Taps elsewhere are
 * ignored. */
void
on_tap_log_view(int x, int y)
{
    if (x >= LOG_BACK_X && x < LOG_BACK_X + LOG_BACK_W && y >= LOG_BACK_Y &&
        y < LOG_BACK_Y + LOG_BACK_H) {
        g_state.log_open = 0;
        g_state.log_scroll = -1;
        redraw_shelf();
        return;
    }
    int dir = hit_scroll_button(x, y);
    if (dir != 0) {
        int h = content_bottom();
        int btn_y = h - 8 - SCROLL_BTN_H;
        int page = (btn_y - (LOG_BACK_Y + LOG_BACK_H + 16)) / LOG_ROW_H;
        if (page < 1)
            page = 1;
        /* Rows are ordered oldest → newest; up (dir -1) goes older. */
        g_state.log_scroll += dir * page;
        if (g_state.log_scroll < 0)
            g_state.log_scroll = 0;
        draw_log_view();
        FullUpdate();
    }
}
