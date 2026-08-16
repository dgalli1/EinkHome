/* bs_input.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_browser.h"
#include "bs_downloads.h"
#include "bs_input.h"
#include "bs_launcher.h"
#include "bs_licenses.h"
#include "bs_model.h"
#include "bs_net.h"
#include "bs_store.h"
#include "bs_sysapp.h"
#include "bs_ui.h"

/* ── hit-testing ─────────────────────────────────────────────────────── */

/* Hit-test the top-bar buttons that sit in the right band (menu / sync /
 * layout-switch / search-icon), all TOP_BTN_SIZE×TOP_BTN_SIZE regions
 * padded TOP_BTN_PAD px from the right edge.  Returns the button id, or
 * -1 when x falls outside every region. */
static int
bs_hit_top_bar_right(int x, int w)
{
    /* Right TOP_BTN_SIZE×TOP_BTN_SIZE region, padded TOP_BTN_PAD px on
     * the right: the hamburger/More button. */
    if (x >= w - BS_TOP_BTN_SIZE - BS_TOP_BTN_PAD && x < w - BS_TOP_BTN_PAD)
        return 3;
    /* Sync button — TOP_BTN_SIZE region left of the menu button; runs
     * a library sync. */
    if (x >= w - BS_TOP_BTN_PAD - 2 * BS_TOP_BTN_SIZE && x < w - BS_TOP_BTN_SIZE - BS_TOP_BTN_PAD)
        return 2;
    /* Layout-switch button — TOP_BTN_SIZE region left of the sync
     * button; toggles grid / list view. */
    if (x >= w - BS_TOP_BTN_PAD - 3 * BS_TOP_BTN_SIZE && x < w - BS_TOP_BTN_PAD - 2 * BS_TOP_BTN_SIZE)
        return 7;
    /* Search icon — TOP_BTN_SIZE region left of the layout button;
     * opens the Search sub-page. */
    if (x >= w - BS_TOP_BTN_PAD - 4 * BS_TOP_BTN_SIZE && x < w - BS_TOP_BTN_PAD - 3 * BS_TOP_BTN_SIZE)
        return 5;
    return -1;
}

int
bs_hit_top_bar(int x, int y)
{
    int bar_top = 0; /* top bar sits at the very top; the panel is at the bottom */
    int bar_bot = bar_top + BS_TOP_BAR_H;
    if (y < bar_top || y >= bar_bot)
        return -1;
    int w = ScreenWidth();
    /* Left button — TOP_BTN_SIZE×TOP_BTN_SIZE region, padded
     * TOP_BTN_PAD px on the left: a back arrow on the Search sub-view
     * or a drilled series, a no-op otherwise (the home icon was
     * removed). */
    if (x >= BS_TOP_BTN_PAD && x < BS_TOP_BTN_PAD + BS_TOP_BTN_SIZE)
        return 1;
    /* The Search page has no right-side icons and no source button —
     * its top bar is just the back arrow, so taps there fall through. */
    if (bs_g_state.tab == BS_TAB_SEARCH)
        return -1;
    /* Source button — icon + label right of the house button (grows
     * to fill the band on the 758px panels). */
    if (x >= BS_SOURCE_BTN_X && x < BS_SOURCE_BTN_X + bs_source_btn_w())
        return 6;
    return bs_hit_top_bar_right(x, w);
}

/* 1 when (x, y) is inside the search input row on the Search page. */
int
bs_hit_search_input(int x, int y)
{
    int row_top = BS_TOP_BAR_H + BS_TOP_BAR_PAD;
    int row_bot = row_top + BS_SEARCH_ROW_H;
    if (y < row_top || y >= row_bot)
        return -1;
    int w = ScreenWidth();
    int tx = 16, tw = w - 32; /* full-width bar (see draw_search_tab) */
    int ty = row_top + 10;
    int th = BS_SEARCH_ROW_H - 20;
    if (x < tx || x >= tx + tw)
        return -1;
    if (y < ty || y >= ty + th)
        return -1;
    return 1;
}

/* 0-based index of the history term row tapped on the Search page, or
 * -1 when the tap is outside the history list. */
int
bs_hit_history(int x, int y)
{
    (void)x;
    int top, bot, cell_w, cell_h;
    (void)cell_w;
    (void)cell_h;
    bs_grid_geom(&top, &bot, &cell_w, &cell_h);
    int y0 = top + BS_SEARCH_ROW_H;
    if (y < y0)
        return -1;
    int ps = bs_history_pagesize();
    if (ps < 1)
        ps = 1;
    int rel = (y - y0) / BS_SEARCH_HISTORY_ROW_H;
    if (rel >= ps)
        return -1;
    int idx = bs_g_state.page * ps + rel;
    if (idx >= bs_store_search_count())
        return -1;
    return idx;
}

/* 0-based index of the suggestion row tapped, or -1.  Mirrors
 * hit_history over the live band geometry; only rows that are
 * actually drawn (fit above the keyboard) are hit-testable. */
int
bs_hit_suggestion(int x, int y)
{
    (void)x;
    int y_top, y_bot;
    bs_suggest_band(&y_top, &y_bot);
    if (y < y_top || y >= y_bot)
        return -1;
    int drawn = (y_bot - y_top) / BS_SEARCH_HISTORY_ROW_H;
    int rel = (y - y_top) / BS_SEARCH_HISTORY_ROW_H;
    if (rel >= drawn || rel >= bs_g_nsuggest)
        return -1;
    return rel;
}

int
bs_hit_thumbnail(int x, int y)
{
    int top, bot, cell_w, cell_h;
    bs_grid_geom(&top, &bot, &cell_w, &cell_h);
    int cols = bs_view_cols();
    int rows = bs_view_rows();
    int page_start = bs_g_state.page * bs_view_pagesize();
    int last = page_start + bs_view_pagesize();
    if (last > bs_g_view_total)
        last = bs_g_view_total;
    for (int row = 0; row < rows; row++) {
        for (int col = 0; col < cols; col++) {
            int idx = page_start + row * cols + col;
            if (idx >= last)
                return -1;
            int tx = bs_grid_x0() + col * cell_w;
            int ty = top + 4 + row * cell_h;
            int tw = cell_w - 8;
            int th = cell_h - 6;
            if (x >= tx && x < tx + tw && y >= ty && y < ty + th)
                return idx;
        }
    }
    return -1;
}

/* Left (prev-page) pager buttons: "< prev" and "<< first".  Returns
 * -1/-3 when x is inside a button, 0 otherwise. */
static int
bs_hit_pager_prev(int x)
{
    /* < prev — 96px wide starting at x=12 */
    if (bs_g_state.page > 0 && x >= 12 && x < 12 + 96)
        return -1;
    /* << first page — next 96px slot */
    if (bs_g_state.page > 0 && x >= 116 && x < 116 + 96)
        return -3;
    return 0;
}

/* Right (next-page) pager buttons: ">> last" and "> next".  Returns
 * -4/-2 when x is inside a button, 0 otherwise. */
static int
bs_hit_pager_next(int x, int w, int pages)
{
    /* >> last page — 96px slot left of the next button */
    if (bs_g_state.page + 1 < pages && x >= w - 212 && x < w - 116)
        return -4;
    /* > next — 96px wide ending at x=w-12 */
    if (bs_g_state.page + 1 < pages && x >= w - 108 && x < w - 12)
        return -2;
    return 0;
}

/* 1 = the tap is on the current page's dimension-group header band. */
int
bs_hit_pager(int x, int y)
{
    int y0 = bs_content_bottom() - BS_PAGER_H;
    if (y < y0 || y >= y0 + BS_PAGER_H)
        return 0;
    int w = ScreenWidth();
    int pages = bs_current_pages();
    int r = bs_hit_pager_prev(x);
    if (r != 0)
        return r;
    return bs_hit_pager_next(x, w, pages);
}

/* ── tap handlers ────────────────────────────────────────────────────── */

int
bs_on_tap_overlay_group(int x, int y)
{
    BsGroupPreset opts[1 + 5];
    int n = bs_group_options(opts, 1 + 5);
    int w = ScreenWidth();
    int pw = w * 3 / 4;
    int ph = 72 + n * 96 + 24;
    int px = (w - pw) / 2;
    int py = (bs_content_bottom() - ph) / 2;
    if (x < px || x >= px + pw || y < py || y >= py + ph) {
        bs_g_state.overlay = BS_OV_NONE;
        return 1;
    }
    if (y < py + 84)
        return 1; /* title/header strip: ignore */
    int r = (y - (py + 84)) / 96;
    if (r < 0 || r >= n)
        return 1;
    /* Single-select: choosing a row applies it and closes the sheet. */
    bs_g_group = opts[r];
    bs_g_drill_level = 0;
    for (int L = 0; L < BS_GROUP_MAX_LEVELS; L++)
        bs_g_drill_values[L][0] = '\0';
    bs_g_drilled_series[0] = '\0';
    bs_g_state.page = 0;
    bs_view_rebuild();
    bs_g_state.overlay = BS_OV_NONE;
    return 1;
}

int
bs_on_tap_overlay_sort(int x, int y)
{
    int w = ScreenWidth();
    int pw = w * 3 / 4;
    int ph = 72 + 4 * 96 + 24;
    int px = (w - pw) / 2;
    int py = (bs_content_bottom() - ph) / 2;
    if (x < px || x >= px + pw || y < py || y >= py + ph) {
        bs_g_state.overlay = BS_OV_NONE;
        return 1;
    }
    if (y < py + 84)
        return 1;
    int r = (y - (py + 84)) / 96;
    if (r < 0 || r >= 4)
        return 1;
    bs_g_state.sort = (BsSortMode)r;
    bs_g_state.page = 0;
    bs_view_rebuild();
    bs_g_state.overlay = BS_OV_NONE;
    return 1;
}

/* Handle a tap while the More overlay is open.  Returns 1 when the
 * action already repainted the screen itself (settings, launcher,
 * download-all) — the caller must then skip its follow-up redraw or
 * the whole content area flushes twice per tap. */
int
bs_on_tap_overlay_more(int x, int y)
{
    int pw = ScreenWidth() * 3 / 4;
    int px = ScreenWidth() - pw;
    if (x < px || x >= ScreenWidth()) {
        bs_g_state.overlay = BS_OV_NONE;
        return 0;
    }
    int y0 = BS_MORE_Y0;
    /* The right drawer (burger) hosts the group/sort chooser buttons;
     * each opens its source-chooser-style sheet. */
    if (y >= y0 + BS_MORE_GROUP_IDX * BS_MORE_ITEM_H &&
        y < y0 + (BS_MORE_GROUP_IDX + 1) * BS_MORE_ITEM_H) {
        bs_g_state.overlay = BS_OV_GROUP;
        bs_draw_overlay_group();
        FullUpdate();
        return 1;
    }
    if (y >= y0 + BS_MORE_SORT_IDX * BS_MORE_ITEM_H &&
        y < y0 + (BS_MORE_SORT_IDX + 1) * BS_MORE_ITEM_H) {
        bs_g_state.overlay = BS_OV_SORT;
        bs_draw_overlay_sort();
        FullUpdate();
        return 1;
    }
    /* Settings row opens the full-screen settings page. */
    if (y >= y0 + BS_MORE_SETTINGS_IDX * BS_MORE_ITEM_H &&
        y < y0 + (BS_MORE_SETTINGS_IDX + 1) * BS_MORE_ITEM_H) {
        bs_g_state.overlay = BS_OV_SETTINGS;
        bs_g_settings_edit = 0;
        bs_draw_overlay_settings();
        FullUpdate();
        return 1;
    }
    /* Applications row opens the in-app launcher overlay. */
    if (y >= y0 + BS_MORE_APPS_IDX * BS_MORE_ITEM_H &&
        y < y0 + (BS_MORE_APPS_IDX + 1) * BS_MORE_ITEM_H) {
        bs_g_state.overlay = BS_OV_NONE;
        bs_launcher_open_set();
        return 1;
    }
    /* Download-all row queues every book in the library and opens the
     * download-progress popup. */
    if (y >= y0 + BS_MORE_DLALL_IDX * BS_MORE_ITEM_H &&
        y < y0 + (BS_MORE_DLALL_IDX + 1) * BS_MORE_ITEM_H) {
        bs_g_state.overlay = BS_OV_NONE;
        bs_download_all_start();
        return 1;
    }
    bs_g_state.overlay = BS_OV_NONE;
    return 0;
}

/* Handle a tap on the source chooser.  Row 0/1 switch the library
 * source directly (Kavita / scanner-local); row 2 opens the folder
 * picker — the picked directory becomes the folder source.  A tap
 * outside the sheet dismisses.  Returns 1 (the chooser owns the
 * screen while open). */
int
bs_on_tap_source(int x, int y)
{
    int pw, ph, px, py;
    bs_source_geom(&px, &py, &pw, &ph);
    if (x < px || x >= px + pw || y < py || y >= py + ph) {
        bs_g_state.overlay = BS_OV_NONE;
        bs_redraw_shelf();
        return 1;
    }
    int row = (y - (py + 80)) / 96;
    if (row < 0 || row >= 3)
        return 1;
    bs_g_state.overlay = BS_OV_NONE;
    /* Source switch: abort any in-flight sync chain BEFORE the source
     * changes / config saves, so a stale round never fetches from the
     * old endpoint or applies a response under the new source. */
    bs_sync_abort();
    if (row == 0) {
        bs_g_state.source = BS_SOURCE_KAVITA;
        bs_g_browse_open = 0;
        bs_g_browser_drag = 0;
        bs_save_config_file();
        bs_do_sync();
        bs_redraw_shelf();
    } else if (row == 1) {
        bs_g_state.source = BS_SOURCE_LOCAL;
        bs_g_browse_open = 0;
        bs_g_browser_drag = 0;
        bs_save_config_file();
        bs_do_sync();
        bs_redraw_shelf();
    } else {
        /* Folder source: the browser is always rooted at /mnt/ext1 —
         * the user only has this partition, so there is no base
         * directory to choose. */
        bs_g_state.source = BS_SOURCE_FOLDER;
        bs_save_config_file();
        bs_browse_start(BS_BROWSE_ROOT);
    }
    return 1;
}

/* Close the settings overlay and repaint the shelf beneath it.  A
 * picked-but-unsaved download folder is discarded. */
void
bs_settings_close(void)
{
    bs_g_state.overlay = BS_OV_NONE;
    bs_g_settings_edit = 0;
    bs_g_settings_dl_dir[0] = '\0';
    bs_redraw_shelf();
}

/* Persist settings, rebuild the endpoint URLs from the (possibly edited)
 * api_base / api_token, re-apply the download folder, then re-sync so
 * the shelf reflects the new server immediately. */
void
bs_settings_apply(void)
{
    /* Abort any in-flight sync chain BEFORE the endpoints are rebuilt
     * from the edited api_base/api_token, so the chain never fetches
     * the next round from the new URL with the old cursor. */
    bs_sync_abort();
    bs_save_config_file();
    bs_build_endpoint_urls();
    bs_resolve_downloads_dir();
    bs_g_settings_dl_dir[0] = '\0';
    bs_g_state.overlay = BS_OV_NONE;
    bs_g_settings_edit = 0;
    /* Re-sync with the new settings; show the progress sheet (unless
     * the Folder source, which has nothing to sync). */
    if (bs_g_state.source != BS_SOURCE_FOLDER)
        bs_sync_popup_open();
    bs_do_sync();
    bs_redraw_shelf();
}

/* Settings → Install as system app: promote the RUNNING (verified)
 * binary to the firmware's home-task override (EinkHome becomes the
 * home screen) or remove it (stock home returns).  The toggle is the
 * explicit "this version works" confirmation — a standard-folder copy
 * is never silently promoted.  Detailed outcome goes to the log view. */
void
bs_settings_toggle_sysapp(void)
{
    int want = !bs_g_state.sys_app_on;
    int rc = want ? bs_sysapp_promote() : bs_sysapp_unpromote();
    if (rc != 0) {
        bs_LOG("[bookshelf] sysapp: %s failed (target dir %s)\n",
               want ? "promote" : "unpromote", bs_sysapp_dir());
        return; /* keep showing the previous state */
    }
    bs_g_state.sys_app_on = want;
    bs_draw_overlay_settings();
    /* Only the toggle row changed; refresh it instead of a full-screen
     * flash. */
    PartialUpdate(0, 4 * BS_SETTINGS_ROW_H + 112, ScreenWidth(),
                  BS_SETTINGS_ROW_H - 12);
    bs_LOG("[bookshelf] sysapp: %s — %s\n",
           want ? "installed as system app" : "removed from system",
           want ? "reboot to boot EinkHome as the home screen"
                : "stock home returns after reboot");
}

/* Per-row tap handlers for the settings overlay.  Each returns 1 when
 * the row's y-band was hit (and the row handled the tap), 0 otherwise,
 * so the dispatcher above can fall through to the next row. */
static int
bs_on_tap_settings_api_host(int y, int y_row)
{
    if (y >= y_row && y < y_row + BS_SETTINGS_ROW_H - 12) {
        bs_g_settings_edit = 1;
        snprintf(bs_g_settings_kb_buf, sizeof bs_g_settings_kb_buf, "%s", bs_g_state.api_base);
        bs_draw_overlay_settings();
        FullUpdate();
        OpenKeyboard(bs_i18n("settings.api_host"),
                     bs_g_settings_kb_buf,
                     sizeof bs_g_settings_kb_buf - 1,
                     0,
                     bs_settings_keyboard_handler);
        return 1;
    }
    return 0;
}

static int
bs_on_tap_settings_api_token(int y, int y_row)
{
    if (y >= y_row && y < y_row + BS_SETTINGS_ROW_H - 12) {
        bs_g_settings_edit = 2;
        snprintf(bs_g_settings_kb_buf, sizeof bs_g_settings_kb_buf, "%s", bs_g_state.api_token);
        bs_draw_overlay_settings();
        FullUpdate();
        OpenKeyboard(bs_i18n("settings.api_key"),
                     bs_g_settings_kb_buf,
                     sizeof bs_g_settings_kb_buf - 1,
                     0,
                     bs_settings_keyboard_handler);
        return 1;
    }
    return 0;
}

static int
bs_on_tap_settings_reader(int y, int y_row)
{
    if (y >= y_row && y < y_row + BS_SETTINGS_ROW_H - 12) {
        /* Cycle Auto → reader[0] → reader[1] → … → Auto. */
        bs_g_state.reader_pref = (bs_g_state.reader_pref + 1) % (bs_g_reader_count + 1);
        bs_draw_overlay_settings();
        /* Only the reader row's value text changed; refresh just that
         * row instead of a full-screen flash. */
        PartialUpdate(32, y_row, ScreenWidth() - 64, BS_SETTINGS_ROW_H - 12);
        return 1;
    }
    return 0;
}

static int
bs_on_tap_settings_folder(int y, int y_row)
{
    if (y >= y_row && y < y_row + BS_SETTINGS_ROW_H - 12) {
        /* Download-folder picker (confined to /mnt/ext1). */
        bs_folder_open();
        return 1;
    }
    return 0;
}

static int
bs_on_tap_settings_sysapp(int y, int y_row)
{
    if (y >= y_row && y < y_row + BS_SETTINGS_ROW_H - 12) {
        bs_settings_toggle_sysapp();
        return 1;
    }
    return 0;
}

static int
bs_on_tap_settings_apply(int y, int y_save)
{
    if (y >= y_save && y < y_save + BS_SETTINGS_BTN_H - 12) {
        bs_settings_apply();
        return 1;
    }
    return 0;
}

static int
bs_on_tap_settings_logs(int y, int y_logs)
{
    if (y >= y_logs && y < y_logs + BS_SETTINGS_BTN_H - 12) {
        /* Show the app log directly (Settings → Show logs).  Settings
         * is NOT restored when the log closes — the log's Back goes
         * straight to the shelf (see on_tap_log_view). */
        bs_g_settings_edit = 0;
        bs_g_state.overlay = BS_OV_LOG;
        bs_g_state.log_scroll = -1; /* start at the tail */
        bs_draw_log_view();
        FullUpdate();
        return 1;
    }
    return 0;
}

static int
bs_on_tap_settings_licenses(int y, int y_lic)
{
    if (y >= y_lic && y < y_lic + BS_SETTINGS_BTN_H - 12) {
        /* Open the third-party licenses viewer (Settings → Licenses).
         * Like the log viewer it owns the screen while open; its Back
         * returns to the shelf (via the list). */
        bs_g_settings_edit = 0;
        bs_g_state.overlay = BS_OV_LICENSES;
        bs_g_state.lic_sel = -1; /* start on the entry list */
        bs_g_state.lic_scroll = 0;
        bs_draw_licenses_view();
        FullUpdate();
        return 1;
    }
    return 0;
}

void
bs_on_tap_overlay_settings(int x, int y)
{
    /* Header Back chevron (rows span the full content width; only the
     * button rect uses x). */
    int bx, by, bw, bh;
    bs_overlay_back_rect(&bx, &by, &bw, &bh);
    if (x >= bx && x < bx + bw && y >= by && y < by + bh) {
        bs_settings_close();
        return;
    }

    int y_row1 = 112;
    int y_row2 = y_row1 + BS_SETTINGS_ROW_H;
    int y_row3 = y_row2 + BS_SETTINGS_ROW_H;
    int y_row4 = y_row3 + BS_SETTINGS_ROW_H;
    int y_row5 = y_row4 + BS_SETTINGS_ROW_H;
    int y_save = y_row5 + BS_SETTINGS_ROW_H + 24;
    int y_logs = y_save + BS_SETTINGS_BTN_H;

    if (bs_on_tap_settings_api_host(y, y_row1))
        return;
    if (bs_on_tap_settings_api_token(y, y_row2))
        return;
    if (bs_on_tap_settings_reader(y, y_row3))
        return;
    if (bs_on_tap_settings_folder(y, y_row4))
        return;
    if (bs_on_tap_settings_sysapp(y, y_row5))
        return;
    if (bs_on_tap_settings_apply(y, y_save))
        return;
    if (bs_on_tap_settings_logs(y, y_logs))
        return;

    int y_lic = y_logs + BS_SETTINGS_BTN_H;
    if (bs_on_tap_settings_licenses(y, y_lic))
        return;
}

/* Taps on the full-screen log viewer: Back (top-left) or the corner
 * scroll buttons (up = older, down = newer).  Taps elsewhere are
 * ignored. */
void
bs_on_tap_log_view(int x, int y)
{
    int bx, by, bw, bh;
    bs_overlay_back_rect(&bx, &by, &bw, &bh);
    if (x >= bx && x < bx + bw && y >= by && y < by + bh) {
        bs_g_state.overlay = BS_OV_NONE;
        bs_g_state.log_scroll = -1;
        bs_redraw_shelf();
        return;
    }
    int dir = bs_hit_scroll_button(x, y);
    if (dir != 0) {
        int h = bs_content_bottom();
        int btn_y = h - 8 - BS_SCROLL_BTN_H;
        int page = (btn_y - BS_LOG_BODY_TOP) / BS_LOG_ROW_H;
        if (page < 1)
            page = 1;
        if (bs_g_state.log_scroll < 0) {
            /* Pinned to the tail.  "Newer" (dir > 0) is already at the
             * newest lines, so it stays pinned; "older" (dir < 0) pages
             * up from the tail's last full page. */
            if (dir < 0) {
                int first = bs_log_view_tail_first() - page;
                bs_g_state.log_scroll = first < 0 ? 0 : first;
            }
        } else {
            /* Rows are ordered oldest → newest; up (dir -1) goes older. */
            bs_g_state.log_scroll += dir * page;
            if (bs_g_state.log_scroll < 0)
                bs_g_state.log_scroll = 0;
        }
        bs_draw_log_view();
        /* One page scroll shifts the log body (and the scroll-button
         * state at the extremes); refresh just that region rather than
         * the whole screen.  The header (back button/title/path) is
         * unchanged on scroll. */
        {
            int h = bs_content_bottom();
            int body_top = BS_LOG_BODY_TOP;
            PartialUpdate(0, body_top, ScreenWidth(), h - body_top);
        }
    }
}

/* Taps on the full-screen licenses viewer.  The header Back chevron
 * changes with depth: from a license's detail it returns to the entry
 * list; from the list it closes to the shelf (mirroring the log
 * viewer).  The corner scroll buttons page-scroll the current view; a
 * tap on a list row opens that license's full text. */
/* Handle a tap on the licenses view's corner scroll buttons (up =
 * older, down = newer).  Returns 1 when a button was hit (and the
 * scroll was applied), 0 otherwise. */
static int
bs_on_tap_lic_scroll(int x, int y, int btn_y)
{
    int h = bs_content_bottom();
    int dir = bs_hit_scroll_button(x, y);
    if (dir != 0) {
        int body_top = bs_g_state.lic_sel < 0 ? BS_LIC_LIST_TOP : BS_LOG_BODY_TOP;
        int page = bs_g_state.lic_sel < 0
                       ? (btn_y - body_top - 8) / BS_LIC_LIST_H
                       : (btn_y - body_top) / BS_LOG_ROW_H;
        if (page < 1)
            page = 1;
        bs_g_state.lic_scroll += dir * page;
        if (bs_g_state.lic_scroll < 0)
            bs_g_state.lic_scroll = 0;
        bs_draw_licenses_view();
        /* Only the body moves on a scroll; the header is unchanged. */
        PartialUpdate(0, body_top, ScreenWidth(), h - body_top);
        return 1;
    }
    return 0;
}

/* Handle a tap on a list-view row (opens that license's full text).
 * Returns 1 when in list mode (row selected or tap ignored), 0
 * otherwise (detail mode — body taps are ignored). */
static int
bs_on_tap_lic_list(int n, int y, int btn_y)
{
    /* List view: a tap on a row opens that license's full text. */
    if (bs_g_state.lic_sel < 0) {
        int body_h = btn_y - BS_LIC_LIST_TOP - 8;
        int rows_vis = body_h / BS_LIC_LIST_H;
        if (rows_vis < 1)
            rows_vis = 1;
        int rel = (y - BS_LIC_LIST_TOP) / BS_LIC_LIST_H;
        if (y >= BS_LIC_LIST_TOP && rel >= 0 && rel < rows_vis) {
            int idx = bs_g_state.lic_scroll + rel;
            if (idx >= 0 && idx < n) {
                bs_g_state.lic_sel = idx;
                bs_g_state.lic_scroll = 0;
                bs_draw_licenses_view();
                FullUpdate();
            }
        }
        return 1;
    }
    return 0;
}

void
bs_on_tap_licenses_view(int x, int y)
{
    int n = bs_license_count();
    int bx, by, bw, bh;
    bs_overlay_back_rect(&bx, &by, &bw, &bh);
    if (x >= bx && x < bx + bw && y >= by && y < by + bh) {
        if (bs_g_state.lic_sel >= 0) {
            /* detail → back to the list */
            bs_g_state.lic_sel = -1;
            bs_g_state.lic_scroll = 0;
            bs_draw_licenses_view();
            FullUpdate();
        } else {
            bs_g_state.overlay = BS_OV_NONE;
            bs_g_state.lic_scroll = 0;
            bs_redraw_shelf();
        }
        return;
    }

    int h = bs_content_bottom();
    int btn_y = h - 8 - BS_SCROLL_BTN_H;

    if (bs_on_tap_lic_scroll(x, y, btn_y))
        return;

    bs_on_tap_lic_list(n, y, btn_y);
    /* Detail: taps in the body are ignored (only Back / scroll). */
}
