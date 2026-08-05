/* bs_main.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── event loop ──────────────────────────────────────────────────────── */

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
         * the system panel drawn over by the firmware.  We also
         * query PanelHeight() once so all subsequent draws can
         * start below it without per-frame work.
         *
         * The SDK docstring for SetCurrentApplicationAttribute notes
         * that APPLICATION_READER "affects behaviour of panel, for
         * proper work, set this attribute before first access to
         * panel API".  Without it the firmware may treat us as a
         * generic "shell" task (no bottom status bar) instead of a
         * reader-style app (with the persistent Tue 23:13 + battery
         * strip).  Setting it matches what the original sudoku.app
         * and dictionary do.
         */
        SetCurrentApplicationAttribute(APPLICATION_READER, 1);

        /* Set the framebuffer orientation FIRST.  SetOrientation()
         * recomputes the per-task iv_fbinfo (clearing the framebuffer to
         * white and resetting fb_y_offset to 0).  We run it before
         * SetPanelType() so the panel config lands on the final fb layout
         * and is not clobbered by the orientation reset. */
        SetOrientation(0);

        /* Enable the reader-style status bar at the TOP of the screen.
         * SetShowPanelReader(1) sets the panel_conf show flag (offset 0x30)
         * and re-applies the current panel type.  SetPanelType() with the
         * PANEL_NO_FB_OFFSET bit (the same value eink-reader.app uses,
         * PANEL_ENABLED | 1<<3 == 10) keeps fb_y_offset at 0 and makes the
         * firmware's panel painter draw the strip at y=0 (top) instead of
         * the bottom.  Our layout offsets every surface below panel_h. */
        SetShowPanelReader(1);
        SetPanelSeparatorEnabled(1);
        SetPanelTransparent(0);
        SetPanelType(PANEL_ENABLED | PANEL_NO_FB_OFFSET);
        g_state.panel_h = PanelHeight();
        if (g_state.panel_h <= 0) {
            /* Live device: the firmware's panel painter never activates
             * for this task (PanelHeight()==0), so the stock strip is
             * never drawn and our content would sit flush against the
             * top edge.  Reserve the stock bar height and paint the
             * strip ourselves (draw_system_strip). */
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
        DrawPanel(NULL, "Bookshelf", NULL, -1);
        stamp_panel();

        /* Force the firmware to actually draw the system panel now.
         * Repaint() enqueues EVT_SHOW (=23) on the event loop, which
         * the firmware handles by calling iv_actualize_panel(), which
         * in turn calls iv_update_panel() (the function that draws the
         * day-of-week + 24h-time strip at the top and the matching
         * strip at the bottom with the down-arrow + lightbulb +
         * battery icons).  Without this call the panel is only
         * redrawn on subsequent state changes (clock minute tick,
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
        view_rebuild();             /* render from the local db even if sync fails */
        LOG("[bookshelf] config_path=%s\n", g_config_path);
        g_state.reader_pref = reader_pref_from_path(g_cfg_reader);
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
         * manual tap.  do_sync() blocks on the network here exactly as
         * it does from the menu path; the draw below then renders the
         * fetched books (and arms the per-tile cover fetcher). */
        do_sync();
        draw_top_bar();
        draw_search_row();
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
        if (g_state.launcher_open) {
            draw_overlay_launcher();
            FullUpdate();
            return 1;
        }
        if (g_state.settings_open) {
            draw_overlay_settings();
            FullUpdate();
            return 1;
        }
        draw_top_bar();
        draw_search_row();
        if (g_state.tab == TAB_DOWNLOADS)
            draw_downloads_tab();
        else
            draw_grid();
        draw_pager();
        if (g_state.menu_open)
            draw_overlay_menu();
        else if (g_state.more_open)
            draw_overlay_more();
        FullUpdate();
        return 1;
    }

    if (type == EVT_POINTERDOWN) {
        int x = par1, y = par2;
        /* The launcher body is drag-scrolled: remember the press point so
         * POINTERMOVE can translate the finger travel into scroll. */
        if (g_state.launcher_open) {
            g_state.launcher_drag = 1;
            g_state.launcher_drag_y = y;
            g_state.launcher_moved = 0;
            return 1;
        }
        /* Arm a long-press only on the Library tab's grid, and only when
         * no modal overlay is up.  The timer (longpress_tick) opens the
         * context menu if the finger stays put. */
        g_lp_armed = 0;
        g_lp_vi = -1;
        if (g_state.tab == TAB_LIBRARY && !g_state.settings_open && !g_state.menu_open &&
            !g_state.more_open && !g_state.ctx_open && !g_state.search_open) {
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
        if (g_state.launcher_open) {
            if (g_state.launcher_drag) {
                int dy = par2 - g_state.launcher_drag_y;
                if (g_state.launcher_moved || dy > LAUNCHER_DRAG_SLOP || dy < -LAUNCHER_DRAG_SLOP) {
                    g_state.launcher_moved = 1;
                    g_state.launcher_scroll -= dy;
                    g_state.launcher_drag_y = par2;
                    draw_overlay_launcher();
                    FullUpdate();
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
        LOG("[bookshelf] EVT_POINTERUP x=%d y=%d menu=%d more=%d search=%d\n",
            x,
            y,
            g_state.menu_open,
            g_state.more_open,
            g_state.search_open);
        /* Finger lifted — a pending long-press becomes a normal tap. */
        g_lp_armed = 0;
        g_lp_vi = -1;
        /* Drop the release that opened the context menu (see longpress_tick). */
        if (g_ctx_suppress_up) {
            g_ctx_suppress_up = 0;
            return 1;
        }

        /* Settings overlay owns the whole screen and repaints itself. */
        if (g_state.settings_open) {
            on_tap_overlay_settings(x, y);
            return 1;
        }
        /* Launcher overlay owns the whole screen while open.  A lift that
         * ended a scroll drag is not a tap. */
        if (g_state.launcher_open) {
            int was_drag = g_state.launcher_moved;
            g_state.launcher_drag = 0;
            g_state.launcher_moved = 0;
            if (!was_drag)
                on_tap_overlay_launcher(x, y);
            return 1;
        }

        /* Context (long-press) menu owns all taps while open: a tap on
         * an item runs it, anything else dismisses the sheet. */
        if (g_state.ctx_open) {
            on_tap_context(x, y);
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
            on_tap_overlay_more(x, y);
            /* If Settings was opened, it already drew itself; don't
             * repaint the shelf over it. */
            if (!g_state.settings_open) {
                redraw_shelf();
            }
            return 1;
        }
        /* Top system strip (the status bar with clock, battery, etc.).
         * Tapping anywhere on it opens the firmware control panel — the
         * same gesture as the real device. */
        if (y < g_state.panel_h) {
            LOG("[bookshelf] system bar tapped -> control panel\n");
            OpenControlPanel(NULL);
            return 1;
        }

        /* Search input */
        if (hit_search(x, y) == 1) {
            g_state.search_open = 1;
            snprintf(g_search_kb_buf, sizeof g_search_kb_buf, "%s", g_state.query);
            OpenKeyboard(
                "Search", g_search_kb_buf, sizeof g_search_kb_buf - 1, 0, keyboard_handler);
            return 1;
        }

        /* Top-bar buttons — shared by both tabs (home/menu must work even
         * while the Downloads tab is showing).  hit_top_bar returns:
         *   1 = home  (left; back on the Downloads view / drilled series)
         *   3 = menu  (right, Library tab; opens the More overlay)
         *   2 = sync  (right, Downloads tab only; runs a library sync)
         *   4 = downloads icon (left of the menu button, Library tab)
         */
        int which = hit_top_bar(x, y);
        if (which == 1) {
            if (g_state.tab == TAB_DOWNLOADS) {
                g_state.tab = TAB_LIBRARY;
                g_state.page = 0;
                redraw_shelf();
                return 1;
            }
            if (g_drilled_series[0] != '\0') {
                drill_back();
                return 1;
            }
            /* Home — close the app and return to the launcher. */
            CloseApp();
            return 1;
        }
        if (which == 3) {
            g_state.more_open = 1;
            draw_overlay_more();
            FullUpdate();
            return 1;
        }
        if (which == 2) {
            do_sync();
            redraw_shelf();
            return 1;
        }
        if (which == 4) {
            g_state.tab = TAB_DOWNLOADS;
            g_state.page = 0;
            redraw_shelf();
            return 1;
        }

        /* Pager — shared by both tabs; the page count is per-tab, so the
         * same buttons page the downloads list on that tab. */
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

        /* Below the pager the body is tab-specific: the Downloads tab has
         * no tappable rows, so swallow the tap; the Library tab falls
         * through to the book-grid hit-test below. */
        if (g_state.tab == TAB_DOWNLOADS)
            return 1;

        /* Book tap */
        int idx = hit_thumbnail(x, y);
        if (idx >= 0) {
            on_tap_thumbnail(idx);
            draw_grid();
            PartialUpdate(0,
                          g_state.panel_h + TOP_BAR_H + SEARCH_ROW_H,
                          ScreenWidth(),
                          ScreenHeight() - g_state.panel_h - TOP_BAR_H - SEARCH_ROW_H);
            return 1;
        }
        return 0;
    }

    if (type == EVT_KEYPRESS) {
        if (par1 == IV_KEY_BACK || par1 == IV_KEY_PREV) {
            if (g_state.ctx_open) {
                close_context();
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
            if (g_state.search_open) {
                g_state.search_open = 0;
                draw_search_row();
                PartialUpdate(0, TOP_BAR_H, ScreenWidth(), SEARCH_ROW_H);
                return 1;
            }
            if (g_drilled_series[0] != '\0') {
                drill_back();
                return 1;
            }
            CloseApp();
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
     * is safe and the committed text survives into the filter pass. */
    snprintf(g_state.query, sizeof g_state.query, "%s", buffer ? buffer : "");
    LOG("[bookshelf] search commit: query=`%s`\n", g_state.query);
    g_state.search_open = 0;
    view_rebuild();
    /* redraw grid + search */
    /* The on-screen keyboard draws full-screen and wipes the top status
     * strip; re-stamp it before redraw_shelf() flushes so the panel
     * survives the commit redraw (redraw_shelf clears only from panel_h). */
    stamp_panel();
    redraw_shelf();
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

    /* Note: the original firmware's bookshelf.app imports
     * SetDefaultOrientation and calls it before InkViewMain(), but
     * calling set_fb_orientation() that early hits a NULL fb on the
     * pbemu shim (and may have issues on real devices where the fb
     * isn't attached until the task is registered).  We instead call
     * SetOrientation(0) inside EVT_INIT, after the shim has attached
     * the main framebuffer (see the attach_shm log lines that precede
     * EVT_INIT).  This produces an identical end-state orientation
     * (portrait) without the early-NULL-fb problem.
     */

    InkViewMain(on_event);
    log_close();
    return 0;
}
