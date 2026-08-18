/* eh_main.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_browser.h"
#include "eh_config.h"
#include "eh_downloads.h"
#include "eh_input.h"
#include "eh_launcher.h"
#include "eh_local.h"
#include "eh_model.h"
#include "eh_net.h"
#include "eh_progress.h"
#include "eh_store.h"
#include "eh_sysapp.h"
#include "eh_ui.h"
#include "eh_worker.h"



/* Sync-engine → UI hook table (see eh_core.h): the sync engine
 * (eh_model.c) drives the spinner, the sync popup and the shelf
 * repaint through these pointers instead of calling the ui/ modules by name.
 * Registered once on EVT_INIT, before the deferred boot sync can run. */
static const BsSyncUiHooks g_sync_ui_hooks = {
    .set_active = eh_sync_set_active,
    .popup_refresh = eh_sync_popup_refresh,
    .popup_finish = eh_sync_popup_finish,
    .popup_fail = eh_sync_popup_fail,
    .repaint = eh_redraw_shelf,
};

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
 * does, then run the first sync when a connection is already up.  The
 * firmware's main-menu task binding must not wait on the network. */
static void init_sync_tick(void *ctx) {
  (void)ctx;
  /* The stock desktop sends MSG_START_SERVICES (0x600) to monitor
   * during its init; monitor then launches reader_controller, taskmgr,
   * control_panel_mgr, explorer, update_desktop_data and binds the
   * global-request target.  Without it a fresh boot runs only scanner
   * + bookshelf.  eh_plat_start_services() is the stock's exact
   * iv_ipc_cmd() transport (see app/platform/eh_plat_pb.c). */
  eh_plat_start_services();
  /* Startup must never ask to enable WiFi: the firmware's
   * QuickDownload() pops the "Turn on WiFi" dialog whenever it sees
   * no ACTIVE connection — even with the adapter enabled — so an
   * unconditional first sync would nag on every launch.  Only
   * auto-sync when the device is already connected (the same
   * connection bits, 0xf00, that QuickDownload itself tests before
   * prompting) or when the sync is local-only and never touches the
   * network.  Pressing Sync still attempts the connection and may
   * ask; an offline launch just renders the cached library. */
  if (eh_g_state.source == EH_SOURCE_LOCAL || eh_g_state.source == EH_SOURCE_FOLDER ||
      eh_plat_net_active())
    eh_do_sync();
  eh_redraw_shelf();
}

/* Deferred boot init sliced across event-loop frames (see EVT_INIT).
 * refresh_downloaded_flags() pages the whole books b-tree and
 * view_rebuild() projects the whole view — together tens of seconds of
 * synchronous work before the first frame at 100k books.  Instead of
 * running them to completion inline, the "bootslice" weak timer runs
 * the download-flag probe in bounded slices across frames, then
 * rebuilds the view once and paints the shelf.  The grid already
 * painted early in EVT_INIT; this pass completes the flags and view
 * and repaints.  Disarms itself when done. */
static void bootslice_tick(void *ctx) {
  (void)ctx;
  if (eh_refresh_downloaded_flags_boot_step()) {
    /* Probe finished: rebuild the view once and paint the shelf. */
    eh_view_rebuild();
    eh_draw_top_bar();
    eh_draw_grid();
    eh_draw_pager();
    FullUpdate();
    return; /* done: do not re-arm */
  }
  SetWeakTimerEx("bootslice", bootslice_tick, NULL, 16);
}

/* ── live search suggestions (see plan: suggest-completion) ─────────── */

/* Last buffer the tick acted on; a keystroke batch only re-queries the
 * store when the buffer moved (200 ms re-arm gives the debounce). */
static char g_last_suggest_q[EH_SUGGEST_TERM_MAX] = "";

/* Poll the keyboard buffer (the firmware's setKeyboardTextChangeCallback
 * never fires on this build — verified in the emulator spike) and keep
 * the suggestion band above the keyboard live.  Re-arms itself while
 * the keyboard is open; the keyboard handler clears it on close. */
static void suggest_debounce_tick(void *ctx) {
  (void)ctx;
  if (!eh_g_state.search_kb)
    return; /* keyboard closed: the handler cleared us; do not re-arm */
  SetWeakTimerEx("suggest_debounce", suggest_debounce_tick, NULL, 200);

  char q[EH_SUGGEST_TERM_MAX];
  snprintf(q, sizeof q, "%s", eh_g_search_kb_buf);
  if (strcmp(q, g_last_suggest_q) == 0)
    return; /* nothing typed since the last tick */
  snprintf(g_last_suggest_q, sizeof g_last_suggest_q, "%s", q);

  char rows[EH_SUGGEST_MAX_HITS][EH_SUGGEST_TERM_MAX];
  int  n = eh_store_suggest_list(q, rows, EH_SUGGEST_MAX_HITS);
  int  changed = n != eh_g_nsuggest ||
                 (n > 0 && memcmp(rows, eh_g_suggestions,
                                  sizeof eh_g_suggestions[0] * (size_t)n) != 0);
  if (!changed)
    return;
  eh_g_nsuggest = n;
  for (int i = 0; i < n; i++) {
    /* Bounded copy: never feed a "%s" snprintf from a source that may
     * exceed the destination (triggers -Wformat-truncation).  memcpy
     * of the term budget + explicit NUL, same as the sibling copy in
     \* the ui/ draw modules. */
    memcpy(eh_g_suggestions[i], rows[i], EH_SUGGEST_TERM_MAX - 1);
    eh_g_suggestions[i][EH_SUGGEST_TERM_MAX - 1] = '\0';
  }

  int y_top, y_bot;
  eh_suggest_band(&y_top, &y_bot);
  if (y_bot <= y_top + 16)
    return;
  if (n > 0)
    eh_draw_suggestions(y_top, y_bot);
  else
    eh_draw_search_tab(); /* restore the history rows the band covered */
  PartialUpdate(0, y_top, ScreenWidth(), y_bot - y_top);
}

/* ── shared drag-scroll driver (browser + launcher) ──────────────────
 * The folder-source browser (body mode and the download-folder picker
 * overlay) and the launcher body all scroll by dragging: POINTERDOWN
 * anchors the press point, POINTERMOVE feeds the finger travel into
 * the scroll offset once it has passed LAUNCHER_DRAG_SLOP (the slop
 * keeps a stationary tap from jittering the list), and POINTERUP
 * clamps, redraws and flushes when the lift ended a drag — a plain
 * lift falls through to the tap handlers.  Each branch supplies its
 * own scroll state, draw and flush; the draw functions clamp the
 * upper scroll bound themselves. */
static void
drag_scroll_press(int y, int *drag, int *drag_y, int *moved)
{
  *drag = 1;
  *drag_y = y;
  *moved = 0;
}

static void
drag_scroll_move(int y, int *scroll, int *drag, int *drag_y, int *moved,
                 void (*draw)(void))
{
  if (!*drag)
    return;
  int dy = y - *drag_y;
  if (*moved || dy > EH_LAUNCHER_DRAG_SLOP || dy < -EH_LAUNCHER_DRAG_SLOP) {
    *moved = 1;
    *scroll -= dy;
    *drag_y = y;
    /* Draw, do NOT flush; the refresh happens once on lift. */
    draw();
  }
}

/* Returns 1 when the lift ended a drag (the caller then skips the tap
 * handling).  Only the lower scroll bound is kept non-negative here;
 * the upper bound is clamped by the draw functions. */
static int
drag_scroll_lift(int *scroll, int *drag, int *moved, void (*draw)(void),
                 void (*flush)(void))
{
  int was_drag = *moved;
  *drag = 0;
  *moved = 0;
  if (was_drag) {
    if (*scroll < 0)
      *scroll = 0;
    draw();
    flush();
  }
  return was_drag;
}

static void eh_evt_detect_lang(void) {
  char plat_lang[8] = "";
  if (eh_plat_device_language(plat_lang, sizeof plat_lang) == 0) {
      snprintf(eh_g_lang, sizeof eh_g_lang, "%.3s", plat_lang);
  } else {
    const char *env_lang = getenv("LANG");
    if (env_lang != NULL && env_lang[0] != '\0') {
      if (strncmp(env_lang, "de", 2) == 0)
        snprintf(eh_g_lang, sizeof eh_g_lang, "de");
      else if (strncmp(env_lang, "fr", 2) == 0)
        snprintf(eh_g_lang, sizeof eh_g_lang, "fr");
      else if (strncmp(env_lang, "it", 2) == 0)
        snprintf(eh_g_lang, sizeof eh_g_lang, "it");
      else
        snprintf(eh_g_lang, sizeof eh_g_lang, "en");
    }
  }
}

static void eh_evt_resolve_api_url(void) {
  if (eh_g_state.api_base[0] == '\0') {
    const char *env_url = getenv("PBEMU_API_URL");
    const char *env_host = getenv("PBEMU_API_HOST");
    const char *url =
        env_url ? env_url : (env_host ? env_host : EH_API_BASE_DEFAULT);
    if (strncmp(url, "http://", 7) != 0 && strncmp(url, "https://", 8) != 0) {
      char tmp[200];
      snprintf(tmp, sizeof tmp, "http://%s:8765", url);
      snprintf(eh_g_state.api_base, sizeof eh_g_state.api_base, "%s", tmp);
    } else {
      snprintf(eh_g_state.api_base, sizeof eh_g_state.api_base, "%s", url);
    }
  }
}

static int eh_evt_init(void) {
    memset(&eh_g_state, 0, sizeof eh_g_state);
    /* Wire the sync engine to the UI before anything can trigger a
     * sync: the boot sync is deferred to the one-shot init_sync_tick
     * timer below, so the hook table is always in place first. */
    eh_sync_set_hooks(&g_sync_ui_hooks);
    eh_g_state.sort = EH_SORT_TITLE_ASC;
    eh_g_group = EH_GROUP_NONE;   /* All books (no grouping) */
    /* Mirror whether the home-task override is installed so Settings
     * shows the toggle in the correct state at launch. */
    eh_g_state.sys_app_on = eh_sysapp_detect();

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

    /* Orientation and panel type are registered in eh_plat_boot()
     * BEFORE InkViewMain(), exactly where the stock bookshelf does it
     * (SetOrientation(0); SetDefaultOrientation(-1); SetPanelType(1)).
     * Doing it inside EVT_INIT corrupts the per-task fbinfo on the
     * live device (ScreenHeight() then reports the panel height and
     * the layout collapses into the system bar's rows).  The probe,
     * the self-drawn-strip fallback, and the PBEMU_SELF_PANEL test
     * override live in the PB backend (eh_plat_panel_height). */
    eh_g_state.panel_h = eh_plat_panel_height(&eh_g_self_panel);
    eh_LOG("[bookshelf] panel_h=%d self_panel=%d\n", eh_g_state.panel_h,
        eh_g_self_panel);
    eh_plat_panel_init();
    eh_LOG("[bookshelf] EVT_INIT panel_h=%d sw=%d sh=%d\n", eh_g_state.panel_h,
        ScreenWidth(), ScreenHeight());

    struct eh_cfg_out cfg = {
        .api_url = eh_g_state.api_base,
        .url_cap = sizeof eh_g_state.api_base,
        .api_token = eh_g_state.api_token,
        .token_cap = sizeof eh_g_state.api_token,
    };
    eh_g_state.api_base[0] = '\0';
    eh_load_config_file(eh_g_argv0, &cfg);
    eh_resolve_config_path(eh_g_argv0);
    eh_detect_readers();
    eh_resolve_downloads_dir();
    eh_resolve_covers_dir();
    eh_store_open();
    /* The download-flag probe and the view rebuild are deferred to the
     * "bootslice" weak timer below (see bootslice_tick): they walk the
     * whole books b-tree / project the whole view — tens of seconds of
     * synchronous work before the first frame at 100k books.  Both now
     * run in bounded slices across event-loop frames so the grid paints
     * early and completes incrementally. */
    SetWeakTimerEx("bootslice", bootslice_tick, NULL, 16);
    /* Reader progress from the explorer DB.  The snapshot copy runs on
     * the worker thread (see eh_progress.c); the map is published when
     * the copy+read settle. */
    eh_progress_reload();
    /* A local source renders from the on-device library directly;
     * the Local source imports it, the Folder source opens a file
     * browser (drawn on EVT_SHOW). */
    if (eh_g_state.source == EH_SOURCE_LOCAL)
      eh_local_import_scanner();
    else if (eh_g_state.source == EH_SOURCE_FOLDER) {
      /* The Folder source is a live browser now; drop any rows
       * the old per-folder import left behind.  The browser is
       * always rooted at /mnt/ext1. */
      eh_store_delete_source("folder");
      eh_browse_start(eh_plat_browse_root());
    }
    eh_LOG("[bookshelf] config_path=%s\n", eh_g_config_path);
    eh_g_state.reader_pref = eh_reader_pref_from_path(eh_g_cfg_reader);
/* Colour display?  A per-device capability resolved by the platform
         * backend (eh_plat_display_color: the PB Color reports a nonzero
         * colormask while the fb ioctl claims 8bpp; the stock bookshelf
         * uses that probe to pick RGB24 cover decodes). */
    eh_g_display_color = eh_plat_display_color();
    eh_LOG("[bookshelf] display_colormask=%d\n", eh_g_display_color);
    eh_LOG("[bookshelf] reader_pref=%d (cfg `%s`)\n", eh_g_state.reader_pref,
        eh_g_cfg_reader);

    /* Device language.  On PocketBook the firmware keeps the system
     * language in /mnt/ext1/system/config/global.cfg (language=de) and
     * does NOT export it via the environment, so the PB backend parses
     * that file.  Fall back to LANG (the SDL/PC host), then the app's
     * own config loaded above. */
    eh_evt_detect_lang();

    /* Device identity: fill the launcher profile from runtime probes
     * (capability-based device key + has_audio + language) and log the
     * model/firmware once for telemetry.  See eh_plat_device_profile. */
    eh_plat_device_profile(&eh_g_lcprof, eh_g_lang);
    eh_plat_log_identity();

    /* Resolve API URL via env vars if config didn't set it. */
    eh_evt_resolve_api_url();

    eh_build_endpoint_urls();
    /* Auto-sync on first launch so the shelf populates without a
     * manual tap — but only when the device is already online (see
     * init_sync_tick): an offline launch must render the cached
     * library without ever asking to enable WiFi.  The sync is
     * DEFERRED to a one-shot timer: a blocking network sync inside
     * EVT_INIT (up to 60 s per round when the API is unreachable)
     * delays the firmware's main-menu task binding on the real
     * device, which leaves the global-request target unset — the
     * control-panel Task Manager button and the reader_controller
     * service both fail (taskmgr never opens; OpenBook's
     * reader_controller poll times out).  EVT_INIT returns
     * immediately like the stock bookshelf; the shelf renders from
     * the local store first and init_sync_tick refreshes it once the
     * sync settles. */
    SetWeakTimerEx("initsync", init_sync_tick, NULL, 100);
    eh_draw_top_bar();
    eh_draw_grid();
    eh_draw_pager();
    FullUpdate();
    return 1;
}
static int eh_show_draw_overlay(void) {
  if (eh_g_state.overlay == EH_OV_SOURCE) {
    eh_draw_overlay_source();
    FullUpdate();
    return 1;
  }
  if (eh_g_state.overlay == EH_OV_LAUNCHER) {
    eh_draw_overlay_launcher();
    FullUpdate();
    return 1;
  }
  if (eh_g_state.overlay == EH_OV_FOLDER) {
    eh_draw_overlay_folder();
    FullUpdate();
    return 1;
  }
  if (eh_g_state.overlay == EH_OV_SETTINGS) {
    eh_draw_overlay_settings();
    FullUpdate();
    return 1;
  }
  if (eh_g_state.overlay == EH_OV_LOG) {
    eh_draw_log_view();
    FullUpdate();
    return 1;
  }
  if (eh_g_state.overlay == EH_OV_LICENSES) {
    eh_draw_licenses_view();
    FullUpdate();
    return 1;
  }
  return 0;
}

static int eh_evt_show(void) {
    /* Render the system panel strip before drawing app content.
     * The framework's iv_actualize_panel() skips the draw when
     * is_state_changed() returns 0 (no clock/battery/net change),
     * leaving the strip blank after a FullUpdate() flush.  Calling
     * iv_update_panel(0) directly ensures the clock/battery/wifi
     * strip is always present in the framebuffer before we draw
     * our content below it. */
    eh_stamp_panel();
    /* The user may have been reading with the integrated reader or
     * KOReader while we were away — refresh their progress. */
    eh_progress_reload();
    if (eh_show_draw_overlay())
      return 1;
    eh_draw_top_bar();
    if (eh_g_state.tab == EH_TAB_SEARCH)
      eh_draw_search_tab();
    else if (eh_g_state.source == EH_SOURCE_FOLDER && eh_g_browse_open)
      eh_draw_browse();
    else
      eh_draw_grid();
    if (eh_g_state.source != EH_SOURCE_FOLDER)
      eh_draw_pager();
    if (eh_g_state.dl_popup)
      eh_draw_dl_popup();
    if (eh_g_state.sync_popup)
      eh_draw_sync_popup();
    if (eh_g_state.overlay == EH_OV_MORE)
      eh_draw_overlay_more();
    else if (eh_g_state.overlay == EH_OV_GROUP)
      eh_draw_overlay_group();
    else if (eh_g_state.overlay == EH_OV_SORT)
      eh_draw_overlay_sort();
    FullUpdate();
    return 1;
}
static int eh_evt_pointerdown(int par1, int par2) {
    int x = par1, y = par2;
    /* The file browser body is drag-scrolled like the launcher; a
     * press on the top bar above it is a button press, not a
     * scroll. */
    /* The source chooser is tap-only; swallow the press so nothing
     * underneath arms (long-press, drag). */
    if (eh_g_state.overlay == EH_OV_SOURCE)
      return 1;
    /* The download-folder picker body and the launcher body are
     * drag-scrolled: anchor the press point so POINTERMOVE can
     * translate the finger travel into scroll. */
    if (eh_g_state.overlay == EH_OV_FOLDER) {
      drag_scroll_press(y, &eh_g_browser_drag, &eh_g_browser_drag_y, &eh_g_browser_moved);
      return 1;
    }
    if (eh_g_state.overlay == EH_OV_LAUNCHER) {
      drag_scroll_press(y, &eh_g_state.launcher_drag, &eh_g_state.launcher_drag_y,
                        &eh_g_state.launcher_moved);
      return 1;
    }
    /* The file browser body is drag-scrolled like the launcher; a
     * press on the top bar above it is a button press, not a
     * scroll. */
    if (eh_g_browse_open && eh_g_state.source == EH_SOURCE_FOLDER &&
        y >= EH_TOP_BAR_H + EH_TOP_BAR_PAD) {
      drag_scroll_press(y, &eh_g_browser_drag, &eh_g_browser_drag_y, &eh_g_browser_moved);
      return 1;
    }
    /* Arm a long-press only on the Library tab's grid, and only when
     * no modal overlay or popup is up (source, folder and launcher
     * were already swallowed above).  The timer (longpress_tick)
     * opens the context menu if the finger stays put. */
    eh_g_lp_armed = 0;
    eh_g_lp_vi = -1;
    if (eh_g_state.tab == EH_TAB_LIBRARY && !eh_modal_open()) {
      int vi = eh_hit_thumbnail(x, y);
      if (vi >= 0) {
        eh_g_lp_armed = 1;
        eh_g_lp_vi = vi;
        eh_g_lp_x = x;
        eh_g_lp_y = y;
        SetWeakTimerEx("blp", eh_longpress_tick, NULL, EH_LONGPRESS_MS);
      }
    }
    return 1;
}
static int eh_evt_pointermove(int par1, int par2) {
    if (eh_g_browse_open && eh_g_state.source == EH_SOURCE_FOLDER) {
      drag_scroll_move(par2, &eh_g_browse_scroll, &eh_g_browser_drag,
                       &eh_g_browser_drag_y, &eh_g_browser_moved, eh_draw_browse);
      return 1;
    }
    if (eh_g_state.overlay == EH_OV_FOLDER) {
      drag_scroll_move(par2, &eh_g_browse_scroll, &eh_g_browser_drag,
                       &eh_g_browser_drag_y, &eh_g_browser_moved, eh_draw_overlay_folder);
      return 1;
    }
    if (eh_g_state.overlay == EH_OV_LAUNCHER) {
      drag_scroll_move(par2, &eh_g_state.launcher_scroll, &eh_g_state.launcher_drag,
                       &eh_g_state.launcher_drag_y, &eh_g_state.launcher_moved,
                       eh_draw_overlay_launcher);
      return 1;
    }
    /* A drag away from the press point cancels the pending long-press
     * so scrolling/scrubbing never pops the context menu. */
    if (eh_g_lp_armed) {
      int dx = par1 - eh_g_lp_x, dy = par2 - eh_g_lp_y;
      if (dx * dx + dy * dy > EH_LONGPRESS_SLOP * EH_LONGPRESS_SLOP) {
        eh_g_lp_armed = 0;
        eh_g_lp_vi = -1;
        ClearTimerByName("blp");
      }
    }
    return 0;
}
static int eh_pu_handle_modal(int x, int y) {
    if (eh_g_state.overlay == EH_OV_SOURCE) {
      eh_on_tap_source(x, y);
      return 1;
    }
    /* The download-folder picker owns the screen while open (it
     * sits on top of the settings page).  A lift that ended a
     * scroll drag is not a tap. */
    if (eh_g_state.overlay == EH_OV_FOLDER) {
      int was_drag = drag_scroll_lift(&eh_g_browse_scroll, &eh_g_browser_drag,
                                      &eh_g_browser_moved, eh_draw_overlay_folder,
                                      eh_flush_content);
      if (!was_drag)
        eh_on_tap_folder(x, y);
      return 1;
    }

    /* Settings overlay owns the whole screen and repaints itself. */
    if (eh_g_state.overlay == EH_OV_SETTINGS) {
      eh_on_tap_overlay_settings(x, y);
      return 1;
    }
    /* The log viewer owns all taps while open. */
    if (eh_g_state.overlay == EH_OV_LOG) {
      eh_on_tap_log_view(x, y);
      return 1;
    }
    /* The licenses viewer owns all taps while open. */
    if (eh_g_state.overlay == EH_OV_LICENSES) {
      eh_on_tap_licenses_view(x, y);
      return 1;
    }
    /* The sync-progress sheet is modal during the sync (which is
     * synchronous anyway); once the sync is done or failed a tap
     * dismisses it. */
    if (eh_g_state.sync_popup) {
      eh_g_state.sync_popup = 0;
      eh_redraw_shelf();
      return 1;
    }
    /* Launcher overlay owns the whole screen while open.  A lift that
     * ended a scroll drag is not a tap (draw_overlay_launcher clamps
     * the offset to the laid-out body height itself). */
    if (eh_g_state.overlay == EH_OV_LAUNCHER) {
      int was_drag = drag_scroll_lift(&eh_g_state.launcher_scroll,
                                      &eh_g_state.launcher_drag,
                                      &eh_g_state.launcher_moved,
                                      eh_draw_overlay_launcher, FullUpdate);
      if (!was_drag)
        eh_on_tap_overlay_launcher(x, y);
      return 1;
    }

    /* Context (long-press) menu owns all taps while open: a tap on
     * an item runs it, anything else dismisses the sheet. */
    if (eh_g_state.overlay == EH_OV_CTX) {
      eh_on_tap_context(x, y);
      return 1;
    }

  return 0;
}
static int eh_pu_handle_dl(int x, int y) {
    /* The download popup owns all taps while open.  The X button
     * aborts the whole queue (batch, series, or single download);
     * the rest of the popup is modal while any download is active
     * — downloads never run in the background — so a tap is
     * swallowed; once the queue drains (finished or failed) a tap
     * closes it. */
    if (eh_g_state.dl_popup) {
      int cx, cy;
      eh_dl_cancel_rect(&cx, &cy);
      if (x >= cx && x < cx + EH_DL_CANCEL_SIZE && y >= cy &&
          y < cy + EH_DL_CANCEL_SIZE) {
        eh_cancel_downloads();
        return 1;
      }
      if (eh_downloads_pending() == 0 && !eh_g_dl_batch_active) {
        /* Single-book press whose fetch just finished: the settle has
         * not run yet, so swallow the tap — dl_advance() closes the
         * popup and launches the reader itself. */
        if (eh_g_state.dl_popup_auto_open && eh_dl_job_pending())
          return 1;
        eh_g_state.dl_popup = 0;
        eh_g_state.dl_popup_auto_open = 0;
        eh_redraw_shelf();
      }
      return 1;
    }

  return 0;
}
static int eh_pu_handle_popover(int x, int y) {
    /* Overlay taps take priority; outside-of-panel taps close. */
    if (eh_g_state.overlay == EH_OV_MORE) {
      /* on_tap_overlay_more reports 1 when its action already
       * repainted (settings / launcher / download-all); without
       * that, this follow-up redraw would flush the whole content
       * area a second time in the same tap. */
      int repainted = eh_on_tap_overlay_more(x, y);
      /* If Settings was opened, it already drew itself; don't
       * repaint the shelf over it. */
      if (eh_g_state.overlay != EH_OV_SETTINGS && !repainted) {
        eh_redraw_shelf();
      }
      return 1;
    }
    /* The group/sort chooser sheets own taps while open.  Toggling a
     * grouping keeps the sheet up (multi-level selection); a dismiss
     * (outside / All / a sort choice) falls to a full shelf redraw. */
    if (eh_g_state.overlay == EH_OV_GROUP) {
      eh_on_tap_overlay_group(x, y);
      if (eh_g_state.overlay == EH_OV_NONE)
        eh_redraw_shelf();
      return 1;
    }
    if (eh_g_state.overlay == EH_OV_SORT) {
      eh_on_tap_overlay_sort(x, y);
      if (eh_g_state.overlay == EH_OV_NONE)
        eh_redraw_shelf();
      return 1;
    }
  return 0;
}
static int eh_pu_handle_chrome_system(int x, int y) {
    (void)x;
    if (y >= eh_content_bottom()) {
      eh_LOG("[bookshelf] system bar tapped -> control panel\n");
      OpenControlPanel(NULL);
      return 1;
    }
  return 0;
}
static int eh_pu_handle_chrome_which(int x, int y) {
    int which = eh_hit_top_bar(x, y);
    if (which == 1) {
      if (eh_g_state.tab == EH_TAB_SEARCH) {
        eh_g_state.tab = EH_TAB_LIBRARY;
        eh_g_state.page = 0;
        eh_g_state.search_kb = 0;
        eh_redraw_shelf();
        return 1;
      }
      /* Group drill-in: the back affordance pops one level toward the
       * All-books top. */
      if (eh_g_drill_level > 0) {
        eh_group_drill_back();
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
      eh_g_state.tab = EH_TAB_SEARCH;
      eh_g_state.page = 0;
      eh_g_state.search_kb = 0;
      eh_redraw_shelf();
      return 1;
    }
    if (which == 7) {
      /* Layout switch: toggle grid / list.  The top-bar glyph reflects
       * the new layout on the redraw below. */
      eh_g_state.view_mode =
          (eh_g_state.view_mode == EH_VIEW_GRID) ? EH_VIEW_LIST : EH_VIEW_GRID;
      eh_g_state.page = 0;
      eh_redraw_shelf();
      return 1;
    }
    if (which == 6) {
      /* Source chooser (Kavita / Local / Folder). */
      eh_g_state.overlay = EH_OV_SOURCE;
      eh_draw_overlay_source();
      eh_flush_content();
      return 1;
    }
    if (which == 3) {
      eh_g_state.overlay = EH_OV_MORE;
      eh_draw_overlay_more();
      eh_flush_content();
      return 1;
    }
    if (which == 2) {
      /* The download popup's cancel X is the topmost control while it
       * is open; a sync popup drawn on top would cover it and trap
       * the user, so ignore the tap until the downloads drain. */
      if (eh_g_state.dl_popup) {
        eh_LOG("[bookshelf] sync tap ignored: download popup open\n");
        return 1;
      }
      /* Manual sync: show what the sync is doing (metadata
       * batches / local scan / covers).  The Folder source has
       * nothing to sync, so no popup there. */
      if (eh_g_state.source != EH_SOURCE_FOLDER)
        eh_sync_popup_open();
      eh_do_sync();
      eh_redraw_shelf();
      return 1;
    }

  return 0;
}
static int eh_pu_handle_chrome(int x, int y) {
    if (eh_pu_handle_chrome_system(x, y)) return 1;
    if (eh_pu_handle_chrome_which(x, y)) return 1;
    /* Folder-source file browser: the top-bar buttons were handled
     * above; any other body tap navigates or opens an entry. */
    if (eh_g_browse_open && eh_g_state.source == EH_SOURCE_FOLDER) {
      eh_on_tap_browse(x, y);
      return 1;
    }
  return 0;
}
/* A tap on a search-history row runs that stored query again.  Shared
 * between the keyboard-band tap (search_kb) and the plain search page
 * history list so the commit sequence stays in one place. */
static void
search_run_history(int hi)
{
  char terms[EH_SEARCH_HISTORY_MAX][EH_MAX_QUERY_LEN];
  int got = eh_store_search_list(terms, EH_SEARCH_HISTORY_MAX, 0);
  if (hi < got) {
    snprintf(eh_g_state.query, sizeof eh_g_state.query, "%s", terms[hi]);
    eh_store_search_add(eh_g_state.query);
    eh_LOG("[bookshelf] search history tap: query=`%s`\n", eh_g_state.query);
    eh_g_state.search_kb = 0;
    eh_g_state.tab = EH_TAB_LIBRARY;
    eh_g_state.page = 0;
    eh_view_rebuild();
    eh_redraw_shelf();
  }
}

static int eh_pu_handle_search_kb(int x, int y) {
        if (eh_g_nsuggest > 0) {
          int si = eh_hit_suggestion(x, y);
          if (si >= 0 && si < eh_g_nsuggest) {
            eh_LOG("[bookshelf] suggest tap: term=`%s`\n", eh_g_suggestions[si]);
            /* CloseKeyboard() CANCELS the edit: the handler receives
             * the keyboard's pre-edit text (empty here) and its
             * else-branch keeps the Search page — it never commits,
             * so the app performs the commit (history-tap sequence)
             * after the keyboard is gone. */
            if (CloseKeyboard)
              CloseKeyboard();
            snprintf(eh_g_state.query, sizeof eh_g_state.query, "%s",
                     eh_g_suggestions[si]);
            eh_store_search_add(eh_g_state.query);
            eh_g_state.search_kb = 0;
            eh_g_state.tab = EH_TAB_LIBRARY;
            eh_g_state.page = 0;
            eh_view_rebuild();
            eh_redraw_shelf();
            return 1;
          }
        } else {
          /* No suggestions: the band shows the history list; a tap
           * there runs that search (keyboard closes first). */
          int hi = eh_hit_history(x, y);
          if (hi >= 0) {
            if (CloseKeyboard)
              CloseKeyboard();
            search_run_history(hi);
            return 1;
          }
        }
        /* Outside the band: a tap above the keyboard dismisses it
         * (KBD_PASSEVENTS stopped the stock outside-tap close, so the
         * app restores it); a tap on the keyboard itself returns 0 so
         * the firmware key handling acts (keys, return, shift...). */
        int y_top, y_bot;
        (void)y_top;
        eh_suggest_band(&y_top, &y_bot);
        if (y < y_bot) {
          if (CloseKeyboard)
            CloseKeyboard();
          return 1;
        }
        return 0;
}
static int eh_pu_handle_search(int x, int y) {
  if (eh_g_state.search_kb) return eh_pu_handle_search_kb(x, y);
      if (eh_hit_search_input(x, y) == 1) {
        eh_g_state.search_kb = 1;
        snprintf(eh_g_search_kb_buf, sizeof eh_g_search_kb_buf, "%s", eh_g_state.query);
        g_last_suggest_q[0] = '\0';
        OpenKeyboard("Search", eh_g_search_kb_buf, sizeof eh_g_search_kb_buf - 1,
                     KBD_PASSEVENTS, eh_keyboard_handler);
        SetWeakTimerEx("suggest_debounce", suggest_debounce_tick, NULL, 200);
        return 1;
      }
      int hi = eh_hit_history(x, y);
      if (hi >= 0) {
        search_run_history(hi);
      }
      return 1;
}
static int eh_evt_page_key(int par1) {
      int pages = eh_current_pages();
      if ((par1 == IV_KEY_NEXT || par1 == IV_KEY_NEXT2) &&
          eh_g_state.page + 1 < pages) {
        eh_g_state.page++;
        eh_flip_page();
      } else if ((par1 == IV_KEY_PREV || par1 == IV_KEY_PREV2) &&
                 eh_g_state.page > 0) {
        eh_g_state.page--;
        eh_flip_page();
      }
      return 1;
}
static int eh_evt_back_browse(int par1, int is_page_key) {
  if (eh_g_browse_open && eh_g_state.source == EH_SOURCE_FOLDER) {
        if (is_page_key) {
          int fwd = par1 == IV_KEY_NEXT || par1 == IV_KEY_NEXT2;
          eh_browse_page(fwd ? 1 : -1);
        } else if (!eh_browse_up()) {
          eh_g_browse_open = 0;
          eh_g_state.overlay = EH_OV_SOURCE;
          eh_draw_overlay_source();
          eh_flush_content();
        }
        return 1;
  }
  return 0;
}
static int eh_evt_back_modal(int par1, int is_page_key) {
      /* The file browser: Back ascends, at the root it opens the
       * source chooser; page keys scroll the list. */
  if (eh_evt_back_browse(par1, is_page_key)) return 1;
      if (eh_g_state.overlay == EH_OV_SOURCE) {
        eh_g_state.overlay = EH_OV_NONE;
        eh_redraw_shelf();
        return 1;
      }
      if (eh_g_state.overlay == EH_OV_CTX) {
        eh_close_context();
        return 1;
      }
      if (eh_g_state.dl_popup) {
        /* Modal while downloading; Back only closes a finished
         * popup. */
        if (eh_downloads_pending() == 0 && !eh_g_dl_batch_active) {
          /* Single-book press whose fetch just finished: let
           * dl_advance() close the popup and launch the reader. */
          if (eh_g_state.dl_popup_auto_open && eh_dl_job_pending())
            return 1;
          eh_g_state.dl_popup = 0;
          eh_g_state.dl_popup_auto_open = 0;
          eh_redraw_shelf();
        }
        return 1;
      }
      if (eh_g_state.overlay == EH_OV_FOLDER) {
        eh_folder_close();
        return 1;
      }
      if (eh_g_state.overlay == EH_OV_SETTINGS) {
        eh_settings_close();
        return 1;
      }
  return 0;
}
static int eh_evt_back_overlay(int par1) {
    (void)par1;
      if (eh_g_state.overlay == EH_OV_LICENSES) {
        /* Back pops a license detail to its list, then closes the
         * viewer (mirrors the on-screen Back chevron). */
        if (eh_g_state.lic_sel >= 0) {
          eh_g_state.lic_sel = -1;
          eh_g_state.lic_scroll = 0;
          eh_draw_licenses_view();
          FullUpdate();
        } else {
          eh_g_state.overlay = EH_OV_NONE;
          eh_g_state.lic_scroll = 0;
          eh_redraw_shelf();
        }
        return 1;
      }
      if (eh_g_state.overlay == EH_OV_LAUNCHER) {
        eh_launcher_close();
        return 1;
      }
      if (eh_g_state.overlay == EH_OV_MORE || eh_g_state.overlay == EH_OV_GROUP ||
          eh_g_state.overlay == EH_OV_SORT) {
        eh_g_state.overlay = EH_OV_NONE;
        eh_redraw_shelf();
        return 1;
      }
  return 0;
}
static int eh_evt_back_search_drill(int par1) {
    (void)par1;
      if (eh_g_state.tab == EH_TAB_SEARCH) {
        /* Back from the Search page returns to the library, keeping
         * the active query filter in place.  A still-open keyboard
         * must close first (KBD_PASSEVENTS keeps it up on outside
         * taps; its handler then tears the suggestions down). */
        if (eh_g_state.search_kb) {
          ClearTimerByName("suggest_debounce");
          eh_g_nsuggest = 0;
          if (CloseKeyboard)
            CloseKeyboard();
          eh_g_state.search_kb = 0;
        }
        eh_g_state.tab = EH_TAB_LIBRARY;
        eh_g_state.page = 0;
        eh_g_state.search_kb = 0;
        eh_redraw_shelf();
        return 1;
      }
      /* Group drill-in: back pops one level toward All books. */
      if (eh_g_drill_level > 0) {
        eh_group_drill_back();
        return 1;
      }
      /* Back on the plain shelf: no-op, same reasoning as the home
       * button above — closing the home replacement reads as a
       * crash on the live device. */
      return 1;
}
static int eh_evt_back_key(int par1, int is_page_key) {
  if (eh_evt_back_modal(par1, is_page_key)) return 1;
  if (eh_evt_back_overlay(par1)) return 1;
  if (eh_evt_back_search_drill(par1)) return 1;
  return 1;
}
static int eh_pu_handle_tail(int x, int y) {
    /* Pager — the page count is per-tab (library grid / search
     * history). */
    int pg = eh_hit_pager(x, y);
    if (pg == -1) {
      eh_g_state.page--;
      eh_flip_page();
      return 1;
    }
    if (pg == -2) {
      eh_g_state.page++;
      eh_flip_page();
      return 1;
    }
    if (pg == -3) {
      eh_g_state.page = 0;
      eh_flip_page();
      return 1;
    }
    if (pg == -4) {
      eh_g_state.page = eh_current_pages() - 1;
      eh_flip_page();
      return 1;
    }
    if (eh_g_state.tab == EH_TAB_SEARCH) return eh_pu_handle_search(x, y);
    /* Book / card tap */
    int idx = eh_hit_thumbnail(x, y);
    if (idx >= 0) {
      eh_on_tap_thumbnail(idx);
      /* book_press_action already flushed the download popup when
       * the book had to be fetched; repainting the grid here would
       * wipe it. */
      if (!eh_g_state.dl_popup) {
        eh_draw_grid();
        PartialUpdate(0, EH_TOP_BAR_H + EH_TOP_BAR_PAD, ScreenWidth(),
                      eh_content_bottom() - EH_TOP_BAR_H - EH_TOP_BAR_PAD);
      }
      return 1;
    }
    return 0;
}

static int eh_evt_pointerup(int par1, int par2) {
    int x = par1, y = par2;
    eh_LOG("[bookshelf] EVT_POINTERUP x=%d y=%d overlay=%d tab=%d\n", x, y,
        (int)eh_g_state.overlay, (int)eh_g_state.tab);
    eh_g_lp_armed = 0;
    eh_g_lp_vi = -1;
    ClearTimerByName("blp");
    /* Drop the release that opened the context menu (see longpress_tick). */
    if (eh_g_ctx_suppress_up) {
      eh_g_ctx_suppress_up = 0;
      return 1;
    }
    /* The file browser body is drag-scrolled; a lift that ended a
     * scroll drag is not a tap.  Plain taps fall through to the
     * normal top-bar / body routing below. */
    if (eh_g_browse_open && eh_g_state.source == EH_SOURCE_FOLDER) {
      if (drag_scroll_lift(&eh_g_browse_scroll, &eh_g_browser_drag,
                           &eh_g_browser_moved, eh_draw_browse, eh_flush_content))
        return 1;
    }
    if (eh_pu_handle_modal(x, y)) return 1;
    if (eh_pu_handle_dl(x, y)) return 1;
    if (eh_pu_handle_popover(x, y)) return 1;
    if (eh_pu_handle_chrome(x, y)) return 1;
    return eh_pu_handle_tail(x, y);
}

static int eh_evt_keypress(int par1) {
    int is_page_key = (par1 == IV_KEY_PREV || par1 == IV_KEY_NEXT ||
                       par1 == IV_KEY_PREV2 || par1 == IV_KEY_NEXT2);

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
  if (is_page_key && !eh_modal_open() && !eh_g_browse_open)
    return eh_evt_page_key(par1);
  if (par1 == IV_KEY_BACK || is_page_key)
    return eh_evt_back_key(par1, is_page_key);
  return 0;
}
static int eh_evt_exit(void) {
    /* Tell every in-flight worker job to stop (cooperative flag; the
     * detached threads then get killed by process exit, same as the
     * old download/sync threads).  The log is deliberately NOT closed
     * here: detached workers may still be mid-LOG() (up to 60 s in
     * flight) and would vfprintf into a freed FILE*.  Flush instead —
     * the FILE* stays valid for stragglers and process exit reclaims
     * it. */
    eh_worker_cancel_all();
    eh_store_close();
    eh_launcher_icons_free();
    if (eh_g_log != NULL)
        fflush(eh_g_log);
    return 1;
}
int eh_on_event(int type, int par1, int par2) {
  if (type == EVT_INIT) return eh_evt_init();
  if (type == EVT_SHOW || type == EVT_REPAINT || type == EVT_FOREGROUND) return eh_evt_show();
  if (type == EVT_POINTERDOWN) return eh_evt_pointerdown(par1, par2);
  if (type == EVT_POINTERMOVE) return eh_evt_pointermove(par1, par2);
  if (type == EVT_POINTERUP) return eh_evt_pointerup(par1, par2);
  if (type == EVT_KEYPRESS) return eh_evt_keypress(par1);
  if (type == EVT_EXIT) return eh_evt_exit();
  return 0;
}

void eh_keyboard_handler(char *buffer) {
  /* The keyboard is closing: tear the live suggestion band down. */
  ClearTimerByName("suggest_debounce");
  eh_g_nsuggest = 0;
  /* buffer aliases g_search_kb_buf (never g_state.query), so this copy
   * is safe and the committed text survives into the filter pass.
   * Only a real edit commits a search and leaves the Search page: a
   * dismissed keyboard (OK / cancel / tap outside) delivers the buffer
   * unchanged, and committing that used to teleport the user home —
   * an empty dismissal even counted as an "edit".  A dismissed,
   * unedited keyboard just closes and the Search page stays put. */
  const char *t = buffer ? buffer : "";
  if (strcmp(t, eh_g_state.query) != 0) {
    snprintf(eh_g_state.query, sizeof eh_g_state.query, "%s", t);
    if (eh_g_state.query[0] != '\0')
      eh_store_search_add(eh_g_state.query);
    eh_LOG("[bookshelf] search commit: query=`%s`\n", eh_g_state.query);
    eh_g_state.search_kb = 0;
    eh_g_state.tab = EH_TAB_LIBRARY;
    eh_g_state.page = 0;
      eh_view_rebuild();
      /* The on-screen keyboard draws full-screen and wipes the bottom
       * status strip; re-stamp it before the draw so the panel survives
       * the commit repaint.  Draw the shelf WITHOUT flushing, then a
       * single full-screen FullUpdate repaints the content area and the
       * panel band the keyboard wiped in one refresh — redraw_shelf()
       * would have flushed the content area as a PartialUpdate first,
       * giving two full refresh cycles per commit. */
      eh_stamp_panel();
      eh_draw_shelf_nofb();
      FullUpdate();
    } else {
      eh_g_state.search_kb = 0;
      eh_stamp_panel();
      eh_draw_shelf_nofb();
      FullUpdate();
    }
}

int main(int argc, char **argv) {
  (void)argc;
  if (argv != NULL && argv[0] != NULL)
    snprintf(eh_g_argv0, sizeof eh_g_argv0, "%s", argv[0]);
  else
    eh_g_argv0[0] = '\0';
  eh_log_open(eh_g_argv0);

  /* Register with the firmware exactly like the stock bookshelf's
   * main() (InitInkview/IvSetAppCapability/SetOrientation/
   * SetDefaultOrientation/SetPanelType) then run the event loop.
   * The orientation/panel registration MUST happen before InkViewMain()
   * attaches the task: on the live device doing it inside EVT_INIT
   * corrupts the per-task fbinfo.  All of this lives in the PB backend
   * (see app/platform/eh_plat_pb.c: eh_plat_boot). */
  eh_plat_boot(eh_on_event);
  /* No log_close(): detached workers may still be mid-LOG() when the
   * event loop unwinds (see the EVT_EXIT comment); freeing g_log
   * under them would be a use-after-free.  Flush and let process
   * exit reclaim the FILE*. */
  if (eh_g_log != NULL)
      fflush(eh_g_log);
  return 0;
}
