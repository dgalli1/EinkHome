/* bs_main.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* Exported by the firmware's libinkview but absent from this SDK
 * vintage's headers (and its bundled lib).  Weak so the link succeeds
 * either way; the guard skips the call if the runtime library lacks it. */
extern void IvSetAppCapability(int caps) __attribute__((weak));
extern void SetDefaultOrientation(int n) __attribute__((weak));

/* ── event loop ──────────────────────────────────────────────────────── */

/* The stock bookshelf asks monitor.app to start the resident firmware
 * services (reader_controller, taskmgr, control_panel_mgr, explorer,
 * update_desktop_data, ...) by sending MSG_START_SERVICES (0x600) over
 * iv_ipc_cmd() during its init; monitor's main loop then launches each
 * service itself and binds the global-request target (the control-panel
 * Task Manager button's destination, [state+0x439c] in monitor).  A fresh
 * boot whose home task never sends it leaves only scanner + bookshelf
 * running — taskmgr never opens and OpenBook's reader_controller poll
 * times out.  We replicate the stock call verbatim; launching the
 * services ourselves via NewTaskEx() does NOT work (monitor never binds
 * the request target for tasks it didn't start). */

/* One-shot deferred init work (see the EVT_INIT comments): ask monitor
 * to start the resident firmware services the way the stock bookshelf
 * does, then run the first sync.  The firmware's main-menu task binding
 * must not wait on the network. */
static void
init_sync_tick(void *ctx)
{
    (void)ctx;
    /* The stock desktop sends MSG_START_SERVICES (0x600) to monitor
     * during its init; monitor then launches reader_controller, taskmgr,
     * control_panel_mgr, explorer, update_desktop_data and binds the
     * global-request target.  Without it a fresh boot runs only scanner
     * + bookshelf.  iv_ipc_cmd() is the stock's exact transport. */
    iv_ipc_cmd(MSG_START_SERVICES, 0);
    do_sync();
    redraw_shelf();
}

int
on_event(int type, int par1, int par2)
{
    if (type == EVT_INIT) {
        memset(&g_state, 0, sizeof g_state);
        g_state.sort = SORT_TITLE_ASC;
        g_state.group = GROUP_ALL;
        g_state.filter = FILTER_ALL;

        /* Keep the system panel visible (battery / wifi / clock).
         * Calling SetPanelType(PANEL_DISABLED) or iv_fullscreen()
         * would hide it, which is what we explicitly do NOT want —
         * the user wants the original PB-app behaviour of leaving
         * the system panel drawn by the firmware at the TOP of the
         * screen; the guest's logical drawing space starts below it,
         * so we query PanelHeight() once and offset every surface by
         * it (the logical bottom is ScreenHeight() - panel_h).
         *
         * Note: the stock bookshelf does NOT set the
         * APPLICATION_READER attribute (that is the eink-reader's
         * panel mode); we deliberately match the stock registration
         * so the firmware's service startup treats our home task the
         * same way.
         */

        /* Orientation and panel type are registered in main() BEFORE
         * InkViewMain(), exactly where the stock bookshelf does it
         * (SetOrientation(0); SetDefaultOrientation(-1); SetPanelType(1)).
         * Doing it inside EVT_INIT corrupts the per-task fbinfo on the
         * live device (ScreenHeight() then reports the panel height and
         * the layout collapses into the system bar's rows). */
        g_state.panel_h = PanelHeight();
        if (g_state.panel_h <= 0) {
            /* Live device: the firmware's panel painter never activates
             * for this task (PanelHeight()==0), so the stock strip is
             * never drawn and our content would sit flush against the
             * top edge.  Reserve the stock bar height and paint the
             * strip ourselves; draw_system_strip() paints it at the
             * bottom of the logical space, which the firmware's
             * fb_y_offset wrap renders at the TOP of the framebuffer —
             * exactly where the stock bar lives. */
            g_state.panel_h = SELF_PANEL_H;
            g_self_panel = 1;
        }
        /* Test/debug override: force the self-drawn strip even when the
         * firmware panel would paint, so the fallback path can be
         * exercised in the emulator. */
        if (getenv("PBEMU_SELF_PANEL") != NULL) {
            g_state.panel_h = SELF_PANEL_H;
            g_self_panel = 1;
        }
        LOG("[bookshelf] panel_h=%d self_panel=%d\n", g_state.panel_h, g_self_panel);

        /* Populate and render the panel content.  DrawPanel() fills in the
         * panel_conf content fields (the stock bookshelf.app calls
         * DrawPanel(NULL, NULL, NULL, -1) from its CustomDrawPanel()
         * override); iv_update_panel(0) is the function that actually blits
         * the clock / battery / wifi strip into the framebuffer.  The
         * framework only calls it via iv_actualize_panel() when
         * is_state_changed() is true, which it isn't on a fresh launch, so
         * we force it here.  Arg 0 = reading-mode disabled (normal bar). */
        DrawPanel(NULL, "EinkHome", NULL, -1);
        stamp_panel();

        /* Force the firmware to actually draw the system panel now.
         * Repaint() enqueues EVT_SHOW (=23) on the event loop, which
         * the firmware handles by calling iv_actualize_panel(), which
         * in turn calls iv_update_panel() (the function that blits the
         * day-of-week + 24h-time + down-arrow + lightbulb + battery
         * strip into the bottom band).  Without this call the panel is
         * only redrawn on subsequent state changes (clock minute tick,
         * battery percent change, net state change) — on a freshly
         * launched task with no state change yet, the panel rect is
         * blank.  Repaint() forces an immediate one-shot redraw.
         */
        Repaint();
        LOG("[bookshelf] EVT_INIT panel_h=%d sw=%d sh=%d\n",
            g_state.panel_h,
            ScreenWidth(),
            ScreenHeight());

        struct cfg_out cfg = {
            .api_url = g_state.api_base,
            .api_token = g_state.api_token,
            .cap = sizeof g_state.api_base,
        };
        g_state.api_base[0] = '\0';
        load_config_file(g_argv0, &cfg);
        resolve_config_path(g_argv0);
        detect_readers();
        resolve_downloads_dir();
        resolve_covers_dir();
        store_open();
        refresh_downloaded_flags(); /* files may have changed while we were away */
        progress_reload();          /* reader progress from the explorer DB */
        /* A local source renders from the on-device library directly;
         * the Local source imports it, the Folder source opens a file
         * browser (drawn on EVT_SHOW). */
        if (g_state.source == SOURCE_LOCAL)
            local_import_scanner();
        else if (g_state.source == SOURCE_FOLDER) {
            /* The Folder source is a live browser now; drop any rows
             * the old per-folder import left behind.  The browser is
             * always rooted at /mnt/ext1. */
            store_delete_source("folder");
            browse_start(BROWSE_ROOT);
        }
        view_rebuild(); /* render from the local db even if sync fails */
        LOG("[bookshelf] config_path=%s\n", g_config_path);
        g_state.reader_pref = reader_pref_from_path(g_cfg_reader);
        /* Colour display?  The PB Color reports a nonzero colormask
         * while the fb ioctl claims 8bpp; the stock bookshelf uses
         * device_display_colormask() to pick RGB24 cover decodes, so do
         * the same (see load_cover_scaled). */
        g_display_color = (device_display_colormask() != 0);
        LOG("[bookshelf] display_colormask=%d\n", g_display_color);
        LOG("[bookshelf] reader_pref=%d (cfg `%s`)\n", g_state.reader_pref, g_cfg_reader);

        /* Try firmware language env (PB sets LANG=en_US.utf8 etc). */
        const char *env_lang = getenv("LANG");
        if (env_lang != NULL && env_lang[0] != '\0') {
            if (strncmp(env_lang, "de", 2) == 0)
                snprintf(g_lang, sizeof g_lang, "de");
            else if (strncmp(env_lang, "fr", 2) == 0)
                snprintf(g_lang, sizeof g_lang, "fr");
            else if (strncmp(env_lang, "it", 2) == 0)
                snprintf(g_lang, sizeof g_lang, "it");
            else
                snprintf(g_lang, sizeof g_lang, "en");
        }

        /* Resolve API URL via env vars if config didn't set it. */
        if (g_state.api_base[0] == '\0') {
            const char *env_url = getenv("PBEMU_API_URL");
            const char *env_host = getenv("PBEMU_API_HOST");
            const char *url = env_url ? env_url : (env_host ? env_host : API_BASE_DEFAULT);
            if (strncmp(url, "http://", 7) != 0 && strncmp(url, "https://", 8) != 0) {
                char tmp[200];
                snprintf(tmp, sizeof tmp, "http://%s:8765", url);
                snprintf(g_state.api_base, sizeof g_state.api_base, "%s", tmp);
            } else {
                snprintf(g_state.api_base, sizeof g_state.api_base, "%s", url);
            }
        }

        build_endpoint_urls();
        /* Auto-sync on first launch so the shelf populates without a
         * manual tap.  The sync is DEFERRED to a one-shot timer: a
         * blocking network sync inside EVT_INIT (up to 60 s per round
         * when the API is unreachable) delays the firmware's main-menu
         * task binding on the real device, which leaves the
         * global-request target unset — the control-panel Task Manager
         * button and the reader_controller service both fail (taskmgr
         * never opens; OpenBook's reader_controller poll times out).
         * EVT_INIT returns immediately like the stock bookshelf; the
         * shelf renders from the local store first and init_sync_tick
         * refreshes it once the sync settles. */
        SetWeakTimerEx("initsync", init_sync_tick, NULL, 100);
        draw_top_bar();
        draw_grid();
        draw_pager();
        FullUpdate();
        return 1;
    }

    if (type == EVT_SHOW || type == EVT_REPAINT || type == EVT_FOREGROUND) {
        /* Render the system panel strip before drawing app content.
         * The framework's iv_actualize_panel() skips the draw when
         * is_state_changed() returns 0 (no clock/battery/net change),
         * leaving the strip blank after a FullUpdate() flush.  Calling
         * iv_update_panel(0) directly ensures the clock/battery/wifi
         * strip is always present in the framebuffer before we draw
         * our content below it. */
        stamp_panel();
        /* The user may have been reading with the integrated reader or
         * KOReader while we were away — refresh their progress. */
        progress_reload();
        if (g_source_open) {
            draw_overlay_source();
            FullUpdate();
            return 1;
        }
        if (g_state.launcher_open) {
            draw_overlay_launcher();
            FullUpdate();
            return 1;
        }
        if (g_folder_open) {
            draw_overlay_folder();
            FullUpdate();
            return 1;
        }
        if (g_state.settings_open) {
            draw_overlay_settings();
            FullUpdate();
            return 1;
        }
        if (g_state.log_open) {
            draw_log_view();
            FullUpdate();
            return 1;
        }
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
        if (g_state.menu_open)
            draw_overlay_menu();
        else if (g_state.more_open)
            draw_overlay_more();
        FullUpdate();
        return 1;
    }

    if (type == EVT_POINTERDOWN) {
        int x = par1, y = par2;
        /* The file browser body is drag-scrolled like the launcher; a
         * press on the top bar above it is a button press, not a
         * scroll. */
        /* The source chooser is tap-only; swallow the press so nothing
         * underneath arms (long-press, drag). */
        if (g_source_open)
            return 1;
        /* The download-folder picker body is drag-scrolled like the
         * launcher: remember the press point so POINTERMOVE can
         * translate the finger travel into scroll. */
        if (g_folder_open) {
            g_folder_drag = 1;
            g_folder_drag_y = y;
            g_folder_moved = 0;
            return 1;
        }
        /* The launcher body is drag-scrolled: remember the press point so
         * POINTERMOVE can translate the finger travel into scroll. */
        if (g_state.launcher_open) {
            g_state.launcher_drag = 1;
            g_state.launcher_drag_y = y;
            g_state.launcher_moved = 0;
            return 1;
        }
        /* The file browser body is drag-scrolled like the launcher; a
         * press on the top bar above it is a button press, not a
         * scroll. */
        if (g_browse_open && g_state.source == SOURCE_FOLDER && y >= TOP_BAR_H + TOP_BAR_PAD) {
            g_browse_drag = 1;
            g_browse_drag_y = y;
            g_browse_moved = 0;
            return 1;
        }
        /* Arm a long-press only on the Library tab's grid, and only when
         * no modal overlay is up.  The timer (longpress_tick) opens the
         * context menu if the finger stays put. */
        g_lp_armed = 0;
        g_lp_vi = -1;
        if (g_state.tab == TAB_LIBRARY && !g_state.settings_open && !g_state.menu_open &&
            !g_state.more_open && !g_state.ctx_open && !g_state.dl_popup && !g_state.log_open &&
            !g_state.sync_popup) {
            int vi = hit_thumbnail(x, y);
            if (vi >= 0) {
                g_lp_armed = 1;
                g_lp_vi = vi;
                g_lp_x = x;
                g_lp_y = y;
                SetWeakTimerEx("blp", longpress_tick, NULL, LONGPRESS_MS);
            }
        }
        return 1;
    }

    if (type == EVT_POINTERMOVE) {
        if (g_browse_open && g_state.source == SOURCE_FOLDER) {
            if (g_browse_drag) {
                int dy = par2 - g_browse_drag_y;
                if (g_browse_moved || dy > LAUNCHER_DRAG_SLOP || dy < -LAUNCHER_DRAG_SLOP) {
                    g_browse_moved = 1;
                    g_browse_scroll -= dy;
                    g_browse_drag_y = par2;
                    /* Draw, do NOT flush; the refresh happens on lift. */
                    draw_browse();
                }
            }
            return 1;
        }
        if (g_folder_open) {
            if (g_folder_drag) {
                int dy = par2 - g_folder_drag_y;
                if (g_folder_moved || dy > LAUNCHER_DRAG_SLOP || dy < -LAUNCHER_DRAG_SLOP) {
                    g_folder_moved = 1;
                    g_folder_scroll -= dy;
                    g_folder_drag_y = par2;
                    /* Draw the new scroll position into the framebuffer
                     * but do NOT flush; the refresh happens once on
                     * finger lift (see the POINTERUP path). */
                    draw_overlay_folder();
                }
            }
            return 1;
        }
        if (g_state.launcher_open) {
            if (g_state.launcher_drag) {
                int dy = par2 - g_state.launcher_drag_y;
                if (g_state.launcher_moved || dy > LAUNCHER_DRAG_SLOP || dy < -LAUNCHER_DRAG_SLOP) {
                    g_state.launcher_moved = 1;
                    g_state.launcher_scroll -= dy;
                    g_state.launcher_drag_y = par2;
                    /* Draw the new scroll position into the framebuffer
                     * but do NOT flush.  A FullUpdate here per move event
                     * takes 300-500ms on e-ink and looks broken; the
                     * stock firmware draws during the drag and refreshes
                     * once on finger lift (see the POINTERUP path). */
                    draw_overlay_launcher();
                }
            }
            return 1;
        }
        /* A drag away from the press point cancels the pending long-press
         * so scrolling/scrubbing never pops the context menu. */
        if (g_lp_armed) {
            int dx = par1 - g_lp_x, dy = par2 - g_lp_y;
            if (dx * dx + dy * dy > LONGPRESS_SLOP * LONGPRESS_SLOP) {
                g_lp_armed = 0;
                g_lp_vi = -1;
            }
        }
        return 0;
    }

    if (type == EVT_POINTERUP) {
        int x = par1, y = par2;
        LOG("[bookshelf] EVT_POINTERUP x=%d y=%d menu=%d more=%d tab=%d\n",
            x,
            y,
            g_state.menu_open,
            g_state.more_open,
            (int)g_state.tab);
        g_lp_armed = 0;
        g_lp_vi = -1;
        /* Drop the release that opened the context menu (see longpress_tick). */
        if (g_ctx_suppress_up) {
            g_ctx_suppress_up = 0;
            return 1;
        }

        /* The file browser body is drag-scrolled; a lift that ended a
         * scroll drag is not a tap.  Plain taps fall through to the
         * normal top-bar / body routing below. */
        if (g_browse_open && g_state.source == SOURCE_FOLDER && g_browse_moved) {
            g_browse_drag = 0;
            g_browse_moved = 0;
            if (g_browse_scroll < 0)
                g_browse_scroll = 0;
            draw_browse();
            flush_content();
            return 1;
        }
        g_browse_drag = 0;
        g_browse_moved = 0;
        /* The source chooser owns all taps while open. */
        if (g_source_open) {
            on_tap_source(x, y);
            return 1;
        }
        /* The download-folder picker owns the screen while open (it
         * sits on top of the settings page).  A lift that ended a
         * scroll drag is not a tap. */
        if (g_folder_open) {
            int was_drag = g_folder_moved;
            g_folder_drag = 0;
            g_folder_moved = 0;
            if (was_drag) {
                /* The full clamp (list height, ".." row) lives in
                 * draw_overlay_folder; only keep the offset
                 * non-negative here. */
                if (g_folder_scroll < 0)
                    g_folder_scroll = 0;
                draw_overlay_folder();
                flush_content();
            } else {
                on_tap_folder(x, y);
            }
            return 1;
        }

        /* Settings overlay owns the whole screen and repaints itself. */
        if (g_state.settings_open) {
            on_tap_overlay_settings(x, y);
            return 1;
        }
        /* The log viewer owns all taps while open. */
        if (g_state.log_open) {
            on_tap_log_view(x, y);
            return 1;
        }
        /* The sync-progress sheet is modal during the sync (which is
         * synchronous anyway); once the sync is done or failed a tap
         * dismisses it. */
        if (g_state.sync_popup) {
            g_state.sync_popup = 0;
            redraw_shelf();
            return 1;
        }
        /* Launcher overlay owns the whole screen while open.  A lift that
         * ended a scroll drag is not a tap. */
        if (g_state.launcher_open) {
            int was_drag = g_state.launcher_moved;
            g_state.launcher_drag = 0;
            g_state.launcher_moved = 0;
            if (was_drag) {
                /* Clamp the scroll to the laid-out body height, then
                 * flush the framebuffer drawn during the drag — the
                 * single refresh the stock firmware performs on lift. */
                int body_top = LAUNCHER_HEADER_H;
                int body_h = content_bottom() - body_top;
                int max_scroll = g_launcher_body_h - body_h;
                if (max_scroll < 0)
                    max_scroll = 0;
                if (g_state.launcher_scroll < 0)
                    g_state.launcher_scroll = 0;
                if (g_state.launcher_scroll > max_scroll)
                    g_state.launcher_scroll = max_scroll;
                draw_overlay_launcher();
                FullUpdate();
            } else {
                on_tap_overlay_launcher(x, y);
            }
            return 1;
        }

        /* Context (long-press) menu owns all taps while open: a tap on
         * an item runs it, anything else dismisses the sheet. */
        if (g_state.ctx_open) {
            on_tap_context(x, y);
            return 1;
        }

        /* The download popup owns all taps while open.  The X button
         * aborts the whole queue (batch, series, or single download);
         * the rest of the popup is modal while any download is active
         * — downloads never run in the background — so a tap is
         * swallowed; once the queue drains (finished or failed) a tap
         * closes it. */
        if (g_state.dl_popup) {
            int cx, cy;
            dl_cancel_rect(&cx, &cy);
            if (x >= cx && x < cx + DL_CANCEL_SIZE && y >= cy && y < cy + DL_CANCEL_SIZE) {
                cancel_downloads();
                return 1;
            }
            if (downloads_pending() == 0 && !g_dl_batch_active) {
                g_state.dl_popup = 0;
                g_state.dl_popup_auto_open = 0;
                redraw_shelf();
            }
            return 1;
        }

        /* Overlay taps take priority; outside-of-panel taps close. */
        if (g_state.menu_open) {
            on_tap_overlay_menu(x, y);
            /* Clear entire screen then redraw.  The overlay drew a black
             * mask across the whole screen, so we need to repaint
             * everything underneath.
             */
            redraw_shelf();
            return 1;
        }
        if (g_state.more_open) {
            /* on_tap_overlay_more reports 1 when its action already
             * repainted (settings / launcher / download-all); without
             * that, this follow-up redraw would flush the whole content
             * area a second time in the same tap. */
            int repainted = on_tap_overlay_more(x, y);
            /* If Settings was opened, it already drew itself; don't
             * repaint the shelf over it. */
            if (!g_state.settings_open && !repainted) {
                redraw_shelf();
            }
            return 1;
        }
        /* Bottom system strip (the status bar with clock, battery,
         * etc.).  Tapping anywhere on it opens the firmware control
         * panel — the same gesture as the real device. */
        if (y >= content_bottom()) {
            LOG("[bookshelf] system bar tapped -> control panel\n");
            OpenControlPanel(NULL);
            return 1;
        }

        /* Top-bar buttons.  hit_top_bar returns:
         *   1 = back  (left; back on the Search view or a drilled
         *              series, no-op on the library shelf)
         *   2 = sync  (left of the menu button; runs a library sync)
         *   3 = menu  (right; opens the More overlay)
         *   5 = search icon (opens the Search sub-page)
         *   6 = source chooser
         */
        int which = hit_top_bar(x, y);
        if (which == 1) {
            if (g_state.tab == TAB_SEARCH) {
                g_state.tab = TAB_LIBRARY;
                g_state.page = 0;
                g_state.search_kb = 0;
                redraw_shelf();
                return 1;
            }
            if (g_drilled_series[0] != '\0') {
                drill_back();
                return 1;
            }
            /* Home while already on the library shelf is a no-op, not a
             * CloseApp().  This app is the home-screen replacement, so
             * closing it here drops the user into the stock UI and —
             * behind the boot wrapper, which guards respawn by PID file —
             * the app never comes back; that reads as a crash. */
            return 1;
        }
        if (which == 5) {
            /* Open the Search sub-page. */
            g_state.tab = TAB_SEARCH;
            g_state.page = 0;
            g_state.search_kb = 0;
            redraw_shelf();
            return 1;
        }
        if (which == 6) {
            /* Source chooser (Kavita / Local / Folder). */
            g_source_open = 1;
            draw_overlay_source();
            flush_content();
            return 1;
        }
        if (which == 3) {
            g_state.more_open = 1;
            draw_overlay_more();
            flush_content();
            return 1;
        }
        if (which == 2) {
            /* Manual sync: show what the sync is doing (metadata
             * batches / local scan / covers).  The Folder source has
             * nothing to sync, so no popup there. */
            if (g_state.source != SOURCE_FOLDER)
                sync_popup_open();
            do_sync();
            redraw_shelf();
            return 1;
        }

        /* Folder-source file browser: the top-bar buttons were handled
         * above; any other body tap navigates or opens an entry. */
        if (g_browse_open && g_state.source == SOURCE_FOLDER) {
            on_tap_browse(x, y);
            return 1;
        }

        /* Pager — the page count is per-tab (library grid / search
         * history). */
        int pg = hit_pager(x, y);
        if (pg == -1) {
            g_state.page--;
            redraw_shelf();
            return 1;
        }
        if (pg == -2) {
            g_state.page++;
            redraw_shelf();
            return 1;
        }
        if (pg == -3) {
            g_state.page = 0;
            redraw_shelf();
            return 1;
        }
        if (pg == -4) {
            g_state.page = current_pages() - 1;
            redraw_shelf();
            return 1;
        }

        /* Below the pager the body is tab-specific.  The Search page
         * owns its whole body: the input row opens the keyboard, a
         * history term re-runs that search, anything else is swallowed.
         * The Library tab falls through to the book-grid hit-test
         * below. */
        if (g_state.tab == TAB_SEARCH) {
            if (hit_search_input(x, y) == 1) {
                g_state.search_kb = 1;
                snprintf(g_search_kb_buf, sizeof g_search_kb_buf, "%s", g_state.query);
                OpenKeyboard(
                    "Search", g_search_kb_buf, sizeof g_search_kb_buf - 1, 0, keyboard_handler);
                return 1;
            }
            int hi = hit_history(x, y);
            if (hi >= 0) {
                char terms[SEARCH_HISTORY_MAX][MAX_QUERY_LEN];
                int  got = store_search_list(terms, SEARCH_HISTORY_MAX, 0);
                if (hi < got) {
                    snprintf(g_state.query, sizeof g_state.query, "%s", terms[hi]);
                    store_search_add(g_state.query);
                    LOG("[bookshelf] search history tap: query=`%s`\n", g_state.query);
                    g_state.search_kb = 0;
                    g_state.tab = TAB_LIBRARY;
                    g_state.page = 0;
                    view_rebuild();
                    redraw_shelf();
                }
            }
            return 1;
        }

        /* Book tap */
        int idx = hit_thumbnail(x, y);
        if (idx >= 0) {
            on_tap_thumbnail(idx);
            /* book_press_action already flushed the download popup when
             * the book had to be fetched; repainting the grid here would
             * wipe it. */
            if (!g_state.dl_popup) {
                draw_grid();
                PartialUpdate(0,
                              TOP_BAR_H + TOP_BAR_PAD,
                              ScreenWidth(),
                              content_bottom() - TOP_BAR_H - TOP_BAR_PAD);
            }
            return 1;
        }
        return 0;
    }

    if (type == EVT_KEYPRESS) {
        int is_page_key = (par1 == IV_KEY_PREV || par1 == IV_KEY_NEXT || par1 == IV_KEY_PREV2 ||
                           par1 == IV_KEY_NEXT2);

        /* Hamburger button toggles the group-filter drawer (the left
         * overlay).  It stays inert while a full-screen or modal sheet
         * is up so it can never fight the active surface. */
        if (par1 == IV_KEY_MENU) {
            if (!g_state.settings_open && !g_state.ctx_open && !g_state.dl_popup &&
                !g_state.launcher_open && !g_folder_open && !g_browse_open) {
                g_state.menu_open = !g_state.menu_open;
                if (g_state.menu_open) {
                    draw_overlay_menu();
                    flush_content();
                } else {
                    redraw_shelf();
                }
            }
            return 1;
        }

        /* Home: this app is the home task (see bookshelf-wrapper.sh —
         * monitor.app launches it as "bookshelf.app"), so the
         * taskmanager foregrounds us globally when Home is pressed.
         * A Home key that reaches us while we are already foreground
         * is a no-op; closing here would read as a crash. */
        if (par1 == IV_KEY_HOME)
            return 1;

        /* Page-turn buttons paginate the shelf.  With a modal open they
         * fall through to the Back logic below (close the topmost
         * sheet), matching how the stock bookshelf treats them. */
        if (is_page_key && !g_state.ctx_open && !g_state.dl_popup && !g_state.settings_open &&
            !g_state.launcher_open && !g_state.menu_open && !g_state.more_open && !g_folder_open &&
            !g_source_open && !g_browse_open) {
            int pages = current_pages();
            if ((par1 == IV_KEY_NEXT || par1 == IV_KEY_NEXT2) && g_state.page + 1 < pages) {
                g_state.page++;
                redraw_shelf();
            } else if ((par1 == IV_KEY_PREV || par1 == IV_KEY_PREV2) && g_state.page > 0) {
                g_state.page--;
                redraw_shelf();
            }
            return 1;
        }

        if (par1 == IV_KEY_BACK || is_page_key) {
            /* The file browser: Back ascends, at the root it opens the
             * source chooser; page keys scroll the list. */
            if (g_browse_open && g_state.source == SOURCE_FOLDER) {
                if (is_page_key) {
                    int fwd = par1 == IV_KEY_NEXT || par1 == IV_KEY_NEXT2;
                    browse_page(fwd ? 1 : -1);
                } else if (!browse_up()) {
                    g_browse_open = 0;
                    g_source_open = 1;
                    draw_overlay_source();
                    flush_content();
                }
                return 1;
            }
            if (g_source_open) {
                g_source_open = 0;
                redraw_shelf();
                return 1;
            }
            if (g_state.ctx_open) {
                close_context();
                return 1;
            }
            if (g_state.dl_popup) {
                /* Modal while downloading; Back only closes a finished
                 * popup. */
                if (downloads_pending() == 0 && !g_dl_batch_active) {
                    g_state.dl_popup = 0;
                    g_state.dl_popup_auto_open = 0;
                    redraw_shelf();
                }
                return 1;
            }
            if (g_folder_open) {
                folder_close();
                return 1;
            }
            if (g_state.settings_open) {
                settings_close();
                return 1;
            }
            if (g_state.launcher_open) {
                launcher_close();
                return 1;
            }
            if (g_state.menu_open) {
                g_state.menu_open = 0;
                redraw_shelf();
                return 1;
            }
            if (g_state.more_open) {
                g_state.more_open = 0;
                redraw_shelf();
                return 1;
            }
            if (g_state.tab == TAB_SEARCH) {
                /* Back from the Search page returns to the library,
                 * keeping the active query filter in place. */
                g_state.tab = TAB_LIBRARY;
                g_state.page = 0;
                g_state.search_kb = 0;
                redraw_shelf();
                return 1;
            }
            if (g_drilled_series[0] != '\0') {
                drill_back();
                return 1;
            }
            /* Back on the plain shelf: no-op, same reasoning as the home
             * button above — closing the home replacement reads as a
             * crash on the live device. */
            return 1;
        }
        return 0;
    }

    if (type == EVT_EXIT) {
        store_close();
        return 1;
    }
    return 0;
}

void
keyboard_handler(char *buffer)
{
    /* buffer aliases g_search_kb_buf (never g_state.query), so this copy
     * is safe and the committed text survives into the filter pass.
     * Only a real edit counts as a new search: a dismissed keyboard
     * delivers the unchanged buffer, which must not pollute history. */
    const char *t = buffer ? buffer : "";
    if (strcmp(t, g_state.query) != 0 || t[0] == '\0') {
        snprintf(g_state.query, sizeof g_state.query, "%s", t);
        if (g_state.query[0] != '\0')
            store_search_add(g_state.query);
        LOG("[bookshelf] search commit: query=`%s`\n", g_state.query);
    }
    g_state.search_kb = 0;
    g_state.tab = TAB_LIBRARY;
    g_state.page = 0;
    view_rebuild();
    /* The on-screen keyboard draws full-screen and wipes the bottom
     * status strip; re-stamp it before redraw_shelf() flushes so the
     * panel survives the commit redraw.  redraw_shelf() only flushes
     * the content area, so follow with a full-screen flush to repaint
     * the panel band the keyboard wiped. */
    stamp_panel();
    redraw_shelf();
    FullUpdate();
}

int
main(int argc, char **argv)
{
    (void)argc;
    if (argv != NULL && argv[0] != NULL)
        snprintf(g_argv0, sizeof g_argv0, "%s", argv[0]);
    else
        g_argv0[0] = '\0';
    log_open(g_argv0);

    /* Register exactly like the stock bookshelf's main():
     *   InitInkview(0x4110); IvSetAppCapability(1);
     *   SetOrientation(0); SetDefaultOrientation(-1); SetPanelType(1);
     * then run the framework.  The orientation/panel registration MUST
     * happen before InkViewMain() attaches the task: on the live device
     * doing it inside EVT_INIT corrupts the per-task fbinfo (the panel
     * painter then fights our content for the top rows and
     * ScreenHeight() collapses).  SetOrientation()/
     * SetDefaultOrientation() are NULL-fb-safe at this point (the
     * firmware lib logs and returns when hw_getframebuffer() is still
     * NULL), which is exactly why the stock app can call them here. */
    InitInkview(0x4110);
    if (IvSetAppCapability != NULL)
        IvSetAppCapability(1);
    SetOrientation(0);
    if (SetDefaultOrientation != NULL)
        SetDefaultOrientation(-1);
    SetPanelType(1); /* the stock bookshelf's literal value */
    InkViewMain(on_event);
    log_close();
    return 0;
}
