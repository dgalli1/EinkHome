/* bs_main.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_browser.h"
#include "bs_config.h"
#include "bs_downloads.h"
#include "bs_input.h"
#include "bs_launcher.h"
#include "bs_local.h"
#include "bs_model.h"
#include "bs_net.h"
#include "bs_progress.h"
#include "bs_store.h"
#include "bs_ui.h"
#include "bs_worker.h"



/* Sync-engine → UI hook table (see bs_core.h): the sync engine
 * (bs_model.c) drives the spinner, the sync popup and the shelf
 * repaint through these pointers instead of calling the ui/ modules by name.
 * Registered once on EVT_INIT, before the deferred boot sync can run. */
static const BsSyncUiHooks g_sync_ui_hooks = {
    .set_active = bs_sync_set_active,
    .popup_refresh = bs_sync_popup_refresh,
    .popup_finish = bs_sync_popup_finish,
    .popup_fail = bs_sync_popup_fail,
    .repaint = bs_redraw_shelf,
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
   * + bookshelf.  bs_plat_start_services() is the stock's exact
   * iv_ipc_cmd() transport (see app/platform/bs_plat_pb.c). */
  bs_plat_start_services();
  /* Startup must never ask to enable WiFi: the firmware's
   * QuickDownload() pops the "Turn on WiFi" dialog whenever it sees
   * no ACTIVE connection — even with the adapter enabled — so an
   * unconditional first sync would nag on every launch.  Only
   * auto-sync when the device is already connected (the same
   * connection bits, 0xf00, that QuickDownload itself tests before
   * prompting) or when the sync is local-only and never touches the
   * network.  Pressing Sync still attempts the connection and may
   * ask; an offline launch just renders the cached library. */
  if (bs_g_state.source == BS_SOURCE_LOCAL || bs_g_state.source == BS_SOURCE_FOLDER ||
      (QueryNetwork() & 0xf00))
    bs_do_sync();
  bs_redraw_shelf();
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
  if (bs_refresh_downloaded_flags_boot_step()) {
    /* Probe finished: rebuild the view once and paint the shelf. */
    bs_view_rebuild();
    bs_draw_top_bar();
    bs_draw_grid();
    bs_draw_pager();
    FullUpdate();
    return; /* done: do not re-arm */
  }
  SetWeakTimerEx("bootslice", bootslice_tick, NULL, 16);
}

/* ── live search suggestions (see plan: suggest-completion) ─────────── */

/* Last buffer the tick acted on; a keystroke batch only re-queries the
 * store when the buffer moved (200 ms re-arm gives the debounce). */
static char g_last_suggest_q[BS_SUGGEST_TERM_MAX] = "";

/* Poll the keyboard buffer (the firmware's setKeyboardTextChangeCallback
 * never fires on this build — verified in the emulator spike) and keep
 * the suggestion band above the keyboard live.  Re-arms itself while
 * the keyboard is open; the keyboard handler clears it on close. */
static void suggest_debounce_tick(void *ctx) {
  (void)ctx;
  if (!bs_g_state.search_kb)
    return; /* keyboard closed: the handler cleared us; do not re-arm */
  SetWeakTimerEx("suggest_debounce", suggest_debounce_tick, NULL, 200);

  char q[BS_SUGGEST_TERM_MAX];
  snprintf(q, sizeof q, "%s", bs_g_search_kb_buf);
  if (strcmp(q, g_last_suggest_q) == 0)
    return; /* nothing typed since the last tick */
  snprintf(g_last_suggest_q, sizeof g_last_suggest_q, "%s", q);

  char rows[BS_SUGGEST_MAX_HITS][BS_SUGGEST_TERM_MAX];
  int  n = bs_store_suggest_list(q, rows, BS_SUGGEST_MAX_HITS);
  int  changed = n != bs_g_nsuggest ||
                 (n > 0 && memcmp(rows, bs_g_suggestions,
                                  sizeof bs_g_suggestions[0] * (size_t)n) != 0);
  if (!changed)
    return;
  bs_g_nsuggest = n;
  for (int i = 0; i < n; i++) {
    /* Bounded copy: never feed a "%s" snprintf from a source that may
     * exceed the destination (triggers -Wformat-truncation).  memcpy
     * of the term budget + explicit NUL, same as the sibling copy in
     \* the ui/ draw modules. */
    memcpy(bs_g_suggestions[i], rows[i], BS_SUGGEST_TERM_MAX - 1);
    bs_g_suggestions[i][BS_SUGGEST_TERM_MAX - 1] = '\0';
  }

  int y_top, y_bot;
  bs_suggest_band(&y_top, &y_bot);
  if (y_bot <= y_top + 16)
    return;
  if (n > 0)
    bs_draw_suggestions(y_top, y_bot);
  else
    bs_draw_search_tab(); /* restore the history rows the band covered */
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
  if (*moved || dy > BS_LAUNCHER_DRAG_SLOP || dy < -BS_LAUNCHER_DRAG_SLOP) {
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

int bs_on_event(int type, int par1, int par2) {
  if (type == EVT_INIT) {
    memset(&bs_g_state, 0, sizeof bs_g_state);
    /* Wire the sync engine to the UI before anything can trigger a
     * sync: the boot sync is deferred to the one-shot init_sync_tick
     * timer below, so the hook table is always in place first. */
    bs_sync_set_hooks(&g_sync_ui_hooks);
    bs_g_state.sort = BS_SORT_TITLE_ASC;
    bs_g_group_dim = BS_GROUP_ALL;   /* All books (no dimension grouping) */

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

    /* Orientation and panel type are registered in bs_plat_boot()
     * BEFORE InkViewMain(), exactly where the stock bookshelf does it
     * (SetOrientation(0); SetDefaultOrientation(-1); SetPanelType(1)).
     * Doing it inside EVT_INIT corrupts the per-task fbinfo on the
     * live device (ScreenHeight() then reports the panel height and
     * the layout collapses into the system bar's rows).  The probe,
     * the self-drawn-strip fallback, and the PBEMU_SELF_PANEL test
     * override live in the PB backend (bs_plat_panel_height). */
    bs_g_state.panel_h = bs_plat_panel_height(&bs_g_self_panel);
    bs_LOG("[bookshelf] panel_h=%d self_panel=%d\n", bs_g_state.panel_h,
        bs_g_self_panel);
    bs_plat_panel_init();
    bs_LOG("[bookshelf] EVT_INIT panel_h=%d sw=%d sh=%d\n", bs_g_state.panel_h,
        ScreenWidth(), ScreenHeight());

    struct bs_cfg_out cfg = {
        .api_url = bs_g_state.api_base,
        .url_cap = sizeof bs_g_state.api_base,
        .api_token = bs_g_state.api_token,
        .token_cap = sizeof bs_g_state.api_token,
    };
    bs_g_state.api_base[0] = '\0';
    bs_load_config_file(bs_g_argv0, &cfg);
    bs_resolve_config_path(bs_g_argv0);
    bs_detect_readers();
    bs_resolve_downloads_dir();
    bs_resolve_covers_dir();
    bs_store_open();
    /* The download-flag probe and the view rebuild are deferred to the
     * "bootslice" weak timer below (see bootslice_tick): they walk the
     * whole books b-tree / project the whole view — tens of seconds of
     * synchronous work before the first frame at 100k books.  Both now
     * run in bounded slices across event-loop frames so the grid paints
     * early and completes incrementally. */
    SetWeakTimerEx("bootslice", bootslice_tick, NULL, 16);
    /* Reader progress from the explorer DB.  The snapshot copy runs on
     * the worker thread (see bs_progress.c); the map is published when
     * the copy+read settle. */
    bs_progress_reload();
    /* A local source renders from the on-device library directly;
     * the Local source imports it, the Folder source opens a file
     * browser (drawn on EVT_SHOW). */
    if (bs_g_state.source == BS_SOURCE_LOCAL)
      bs_local_import_scanner();
    else if (bs_g_state.source == BS_SOURCE_FOLDER) {
      /* The Folder source is a live browser now; drop any rows
       * the old per-folder import left behind.  The browser is
       * always rooted at /mnt/ext1. */
      bs_store_delete_source("folder");
      bs_browse_start(BS_BROWSE_ROOT);
    }
    bs_LOG("[bookshelf] config_path=%s\n", bs_g_config_path);
    bs_g_state.reader_pref = bs_reader_pref_from_path(bs_g_cfg_reader);
    /* Colour display?  The PB Color reports a nonzero colormask
     * while the fb ioctl claims 8bpp; the stock bookshelf uses
     * device_display_colormask() to pick RGB24 cover decodes, so do
     * the same (see load_cover_scaled). */
    bs_g_display_color = (device_display_colormask() != 0);
    bs_LOG("[bookshelf] display_colormask=%d\n", bs_g_display_color);
    bs_LOG("[bookshelf] reader_pref=%d (cfg `%s`)\n", bs_g_state.reader_pref,
        bs_g_cfg_reader);

    /* Try firmware language env (PB sets LANG=en_US.utf8 etc). */
    const char *env_lang = getenv("LANG");
    if (env_lang != NULL && env_lang[0] != '\0') {
      if (strncmp(env_lang, "de", 2) == 0)
        snprintf(bs_g_lang, sizeof bs_g_lang, "de");
      else if (strncmp(env_lang, "fr", 2) == 0)
        snprintf(bs_g_lang, sizeof bs_g_lang, "fr");
      else if (strncmp(env_lang, "it", 2) == 0)
        snprintf(bs_g_lang, sizeof bs_g_lang, "it");
      else
        snprintf(bs_g_lang, sizeof bs_g_lang, "en");
    }

    /* Device identity: fill the launcher profile from runtime probes
     * (capability-based device key + has_audio + language) and log the
     * model/firmware once for telemetry.  See bs_plat_device_profile. */
    bs_plat_device_profile(&bs_g_lcprof, bs_g_lang);
    bs_plat_log_identity();

    /* Resolve API URL via env vars if config didn't set it. */
    if (bs_g_state.api_base[0] == '\0') {
      const char *env_url = getenv("PBEMU_API_URL");
      const char *env_host = getenv("PBEMU_API_HOST");
      const char *url =
          env_url ? env_url : (env_host ? env_host : BS_API_BASE_DEFAULT);
      if (strncmp(url, "http://", 7) != 0 && strncmp(url, "https://", 8) != 0) {
        char tmp[200];
        snprintf(tmp, sizeof tmp, "http://%s:8765", url);
        snprintf(bs_g_state.api_base, sizeof bs_g_state.api_base, "%s", tmp);
      } else {
        snprintf(bs_g_state.api_base, sizeof bs_g_state.api_base, "%s", url);
      }
    }

    bs_build_endpoint_urls();
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
    bs_draw_top_bar();
    bs_draw_grid();
    bs_draw_pager();
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
    bs_stamp_panel();
    /* The user may have been reading with the integrated reader or
     * KOReader while we were away — refresh their progress. */
    bs_progress_reload();
    if (bs_g_state.overlay == BS_OV_SOURCE) {
      bs_draw_overlay_source();
      FullUpdate();
      return 1;
    }
    if (bs_g_state.overlay == BS_OV_LAUNCHER) {
      bs_draw_overlay_launcher();
      FullUpdate();
      return 1;
    }
    if (bs_g_state.overlay == BS_OV_FOLDER) {
      bs_draw_overlay_folder();
      FullUpdate();
      return 1;
    }
    if (bs_g_state.overlay == BS_OV_SETTINGS) {
      bs_draw_overlay_settings();
      FullUpdate();
      return 1;
    }
    if (bs_g_state.overlay == BS_OV_LOG) {
      bs_draw_log_view();
      FullUpdate();
      return 1;
    }
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
    if (bs_g_state.overlay == BS_OV_MORE)
      bs_draw_overlay_more();
    else if (bs_g_state.overlay == BS_OV_GROUP)
      bs_draw_overlay_group();
    else if (bs_g_state.overlay == BS_OV_SORT)
      bs_draw_overlay_sort();
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
    if (bs_g_state.overlay == BS_OV_SOURCE)
      return 1;
    /* The download-folder picker body and the launcher body are
     * drag-scrolled: anchor the press point so POINTERMOVE can
     * translate the finger travel into scroll. */
    if (bs_g_state.overlay == BS_OV_FOLDER) {
      drag_scroll_press(y, &bs_g_browser_drag, &bs_g_browser_drag_y, &bs_g_browser_moved);
      return 1;
    }
    if (bs_g_state.overlay == BS_OV_LAUNCHER) {
      drag_scroll_press(y, &bs_g_state.launcher_drag, &bs_g_state.launcher_drag_y,
                        &bs_g_state.launcher_moved);
      return 1;
    }
    /* The file browser body is drag-scrolled like the launcher; a
     * press on the top bar above it is a button press, not a
     * scroll. */
    if (bs_g_browse_open && bs_g_state.source == BS_SOURCE_FOLDER &&
        y >= BS_TOP_BAR_H + BS_TOP_BAR_PAD) {
      drag_scroll_press(y, &bs_g_browser_drag, &bs_g_browser_drag_y, &bs_g_browser_moved);
      return 1;
    }
    /* Arm a long-press only on the Library tab's grid, and only when
     * no modal overlay or popup is up (source, folder and launcher
     * were already swallowed above).  The timer (longpress_tick)
     * opens the context menu if the finger stays put. */
    bs_g_lp_armed = 0;
    bs_g_lp_vi = -1;
    if (bs_g_state.tab == BS_TAB_LIBRARY && !bs_modal_open()) {
      int vi = bs_hit_thumbnail(x, y);
      if (vi >= 0) {
        bs_g_lp_armed = 1;
        bs_g_lp_vi = vi;
        bs_g_lp_x = x;
        bs_g_lp_y = y;
        SetWeakTimerEx("blp", bs_longpress_tick, NULL, BS_LONGPRESS_MS);
      }
    }
    return 1;
  }

  if (type == EVT_POINTERMOVE) {
    if (bs_g_browse_open && bs_g_state.source == BS_SOURCE_FOLDER) {
      drag_scroll_move(par2, &bs_g_browse_scroll, &bs_g_browser_drag,
                       &bs_g_browser_drag_y, &bs_g_browser_moved, bs_draw_browse);
      return 1;
    }
    if (bs_g_state.overlay == BS_OV_FOLDER) {
      drag_scroll_move(par2, &bs_g_browse_scroll, &bs_g_browser_drag,
                       &bs_g_browser_drag_y, &bs_g_browser_moved, bs_draw_overlay_folder);
      return 1;
    }
    if (bs_g_state.overlay == BS_OV_LAUNCHER) {
      drag_scroll_move(par2, &bs_g_state.launcher_scroll, &bs_g_state.launcher_drag,
                       &bs_g_state.launcher_drag_y, &bs_g_state.launcher_moved,
                       bs_draw_overlay_launcher);
      return 1;
    }
    /* A drag away from the press point cancels the pending long-press
     * so scrolling/scrubbing never pops the context menu. */
    if (bs_g_lp_armed) {
      int dx = par1 - bs_g_lp_x, dy = par2 - bs_g_lp_y;
      if (dx * dx + dy * dy > BS_LONGPRESS_SLOP * BS_LONGPRESS_SLOP) {
        bs_g_lp_armed = 0;
        bs_g_lp_vi = -1;
        ClearTimerByName("blp");
      }
    }
    return 0;
  }

  if (type == EVT_POINTERUP) {
    int x = par1, y = par2;
    bs_LOG("[bookshelf] EVT_POINTERUP x=%d y=%d overlay=%d tab=%d\n", x, y,
        (int)bs_g_state.overlay, (int)bs_g_state.tab);
    bs_g_lp_armed = 0;
    bs_g_lp_vi = -1;
    ClearTimerByName("blp");
    /* Drop the release that opened the context menu (see longpress_tick). */
    if (bs_g_ctx_suppress_up) {
      bs_g_ctx_suppress_up = 0;
      return 1;
    }

    /* The file browser body is drag-scrolled; a lift that ended a
     * scroll drag is not a tap.  Plain taps fall through to the
     * normal top-bar / body routing below. */
    if (bs_g_browse_open && bs_g_state.source == BS_SOURCE_FOLDER) {
      if (drag_scroll_lift(&bs_g_browse_scroll, &bs_g_browser_drag,
                           &bs_g_browser_moved, bs_draw_browse, bs_flush_content))
        return 1;
    }
    /* The source chooser owns all taps while open. */
    if (bs_g_state.overlay == BS_OV_SOURCE) {
      bs_on_tap_source(x, y);
      return 1;
    }
    /* The download-folder picker owns the screen while open (it
     * sits on top of the settings page).  A lift that ended a
     * scroll drag is not a tap. */
    if (bs_g_state.overlay == BS_OV_FOLDER) {
      int was_drag = drag_scroll_lift(&bs_g_browse_scroll, &bs_g_browser_drag,
                                      &bs_g_browser_moved, bs_draw_overlay_folder,
                                      bs_flush_content);
      if (!was_drag)
        bs_on_tap_folder(x, y);
      return 1;
    }

    /* Settings overlay owns the whole screen and repaints itself. */
    if (bs_g_state.overlay == BS_OV_SETTINGS) {
      bs_on_tap_overlay_settings(x, y);
      return 1;
    }
    /* The log viewer owns all taps while open. */
    if (bs_g_state.overlay == BS_OV_LOG) {
      bs_on_tap_log_view(x, y);
      return 1;
    }
    /* The sync-progress sheet is modal during the sync (which is
     * synchronous anyway); once the sync is done or failed a tap
     * dismisses it. */
    if (bs_g_state.sync_popup) {
      bs_g_state.sync_popup = 0;
      bs_redraw_shelf();
      return 1;
    }
    /* Launcher overlay owns the whole screen while open.  A lift that
     * ended a scroll drag is not a tap (draw_overlay_launcher clamps
     * the offset to the laid-out body height itself). */
    if (bs_g_state.overlay == BS_OV_LAUNCHER) {
      int was_drag = drag_scroll_lift(&bs_g_state.launcher_scroll,
                                      &bs_g_state.launcher_drag,
                                      &bs_g_state.launcher_moved,
                                      bs_draw_overlay_launcher, FullUpdate);
      if (!was_drag)
        bs_on_tap_overlay_launcher(x, y);
      return 1;
    }

    /* Context (long-press) menu owns all taps while open: a tap on
     * an item runs it, anything else dismisses the sheet. */
    if (bs_g_state.overlay == BS_OV_CTX) {
      bs_on_tap_context(x, y);
      return 1;
    }

    /* The download popup owns all taps while open.  The X button
     * aborts the whole queue (batch, series, or single download);
     * the rest of the popup is modal while any download is active
     * — downloads never run in the background — so a tap is
     * swallowed; once the queue drains (finished or failed) a tap
     * closes it. */
    if (bs_g_state.dl_popup) {
      int cx, cy;
      bs_dl_cancel_rect(&cx, &cy);
      if (x >= cx && x < cx + BS_DL_CANCEL_SIZE && y >= cy &&
          y < cy + BS_DL_CANCEL_SIZE) {
        bs_cancel_downloads();
        return 1;
      }
      if (bs_downloads_pending() == 0 && !bs_g_dl_batch_active) {
        /* Single-book press whose fetch just finished: the settle has
         * not run yet, so swallow the tap — dl_advance() closes the
         * popup and launches the reader itself. */
        if (bs_g_state.dl_popup_auto_open && bs_dl_job_pending())
          return 1;
        bs_g_state.dl_popup = 0;
        bs_g_state.dl_popup_auto_open = 0;
        bs_redraw_shelf();
      }
      return 1;
    }

    /* Overlay taps take priority; outside-of-panel taps close. */
    if (bs_g_state.overlay == BS_OV_MORE) {
      /* on_tap_overlay_more reports 1 when its action already
       * repainted (settings / launcher / download-all); without
       * that, this follow-up redraw would flush the whole content
       * area a second time in the same tap. */
      int repainted = bs_on_tap_overlay_more(x, y);
      /* If Settings was opened, it already drew itself; don't
       * repaint the shelf over it. */
      if (bs_g_state.overlay != BS_OV_SETTINGS && !repainted) {
        bs_redraw_shelf();
      }
      return 1;
    }
    /* The group/sort chooser sheets own taps while open.  Toggling a
     * grouping keeps the sheet up (multi-level selection); a dismiss
     * (outside / All / a sort choice) falls to a full shelf redraw. */
    if (bs_g_state.overlay == BS_OV_GROUP) {
      bs_on_tap_overlay_group(x, y);
      if (bs_g_state.overlay == BS_OV_NONE)
        bs_redraw_shelf();
      return 1;
    }
    if (bs_g_state.overlay == BS_OV_SORT) {
      bs_on_tap_overlay_sort(x, y);
      if (bs_g_state.overlay == BS_OV_NONE)
        bs_redraw_shelf();
      return 1;
    }
    /* Bottom system strip (the status bar with clock, battery,
     * etc.).  Tapping anywhere on it opens the firmware control
     * panel — the same gesture as the real device. */
    if (y >= bs_content_bottom()) {
      bs_LOG("[bookshelf] system bar tapped -> control panel\n");
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
    int which = bs_hit_top_bar(x, y);
    if (which == 1) {
      if (bs_g_state.tab == BS_TAB_SEARCH) {
        bs_g_state.tab = BS_TAB_LIBRARY;
        bs_g_state.page = 0;
        bs_g_state.search_kb = 0;
        bs_redraw_shelf();
        return 1;
      }
      if (bs_g_drilled_series[0] != '\0') {
        bs_drill_back();
        return 1;
      }
      /* Group drill-in: the back affordance pops one level toward the
       * All-books top. */
      if (bs_g_drill_value[0] != '\0') {
        bs_group_drill_back();
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
      bs_g_state.tab = BS_TAB_SEARCH;
      bs_g_state.page = 0;
      bs_g_state.search_kb = 0;
      bs_redraw_shelf();
      return 1;
    }
    if (which == 7) {
      /* Layout switch: toggle grid / list.  The top-bar glyph reflects
       * the new layout on the redraw below. */
      bs_g_state.view_mode =
          (bs_g_state.view_mode == BS_VIEW_GRID) ? BS_VIEW_LIST : BS_VIEW_GRID;
      bs_g_state.page = 0;
      bs_redraw_shelf();
      return 1;
    }
    if (which == 6) {
      /* Source chooser (Kavita / Local / Folder). */
      bs_g_state.overlay = BS_OV_SOURCE;
      bs_draw_overlay_source();
      bs_flush_content();
      return 1;
    }
    if (which == 3) {
      bs_g_state.overlay = BS_OV_MORE;
      bs_draw_overlay_more();
      bs_flush_content();
      return 1;
    }
    if (which == 2) {
      /* The download popup's cancel X is the topmost control while it
       * is open; a sync popup drawn on top would cover it and trap
       * the user, so ignore the tap until the downloads drain. */
      if (bs_g_state.dl_popup) {
        bs_LOG("[bookshelf] sync tap ignored: download popup open\n");
        return 1;
      }
      /* Manual sync: show what the sync is doing (metadata
       * batches / local scan / covers).  The Folder source has
       * nothing to sync, so no popup there. */
      if (bs_g_state.source != BS_SOURCE_FOLDER)
        bs_sync_popup_open();
      bs_do_sync();
      bs_redraw_shelf();
      return 1;
    }

    /* Folder-source file browser: the top-bar buttons were handled
     * above; any other body tap navigates or opens an entry. */
    if (bs_g_browse_open && bs_g_state.source == BS_SOURCE_FOLDER) {
      bs_on_tap_browse(x, y);
      return 1;
    }

    /* Pager — the page count is per-tab (library grid / search
     * history). */
    int pg = bs_hit_pager(x, y);
    if (pg == -1) {
      bs_g_state.page--;
      bs_flip_page();
      return 1;
    }
    if (pg == -2) {
      bs_g_state.page++;
      bs_flip_page();
      return 1;
    }
    if (pg == -3) {
      bs_g_state.page = 0;
      bs_flip_page();
      return 1;
    }
    if (pg == -4) {
      bs_g_state.page = bs_current_pages() - 1;
      bs_flip_page();
      return 1;
    }

    /* Below the pager the body is tab-specific.  The Search page
     * owns its whole body: the input row opens the keyboard, a
     * history term re-runs that search, anything else is swallowed.
     * While the keyboard is open (KBD_PASSEVENTS passes pointer
     * events through), the suggestion band above it is hit-tested
     * first; any other tap returns 0 so the firmware keyboard sees
     * it (it may be a key press). */
    if (bs_g_state.tab == BS_TAB_SEARCH) {
      if (bs_g_state.search_kb) {
        if (bs_g_nsuggest > 0) {
          int si = bs_hit_suggestion(x, y);
          if (si >= 0 && si < bs_g_nsuggest) {
            bs_LOG("[bookshelf] suggest tap: term=`%s`\n", bs_g_suggestions[si]);
            /* CloseKeyboard() CANCELS the edit: the handler receives
             * the keyboard's pre-edit text (empty here) and its
             * else-branch keeps the Search page — it never commits,
             * so the app performs the commit (history-tap sequence)
             * after the keyboard is gone. */
            if (CloseKeyboard)
              CloseKeyboard();
            snprintf(bs_g_state.query, sizeof bs_g_state.query, "%s",
                     bs_g_suggestions[si]);
            bs_store_search_add(bs_g_state.query);
            bs_g_state.search_kb = 0;
            bs_g_state.tab = BS_TAB_LIBRARY;
            bs_g_state.page = 0;
            bs_view_rebuild();
            bs_redraw_shelf();
            return 1;
          }
        } else {
          /* No suggestions: the band shows the history list; a tap
           * there runs that search (keyboard closes first). */
          int hi = bs_hit_history(x, y);
          if (hi >= 0) {
            char terms[BS_SEARCH_HISTORY_MAX][BS_MAX_QUERY_LEN];
            int got = bs_store_search_list(terms, BS_SEARCH_HISTORY_MAX, 0);
            if (hi < got) {
              bs_LOG("[bookshelf] search history tap: query=`%s`\n", terms[hi]);
              if (CloseKeyboard)
                CloseKeyboard();
              snprintf(bs_g_state.query, sizeof bs_g_state.query, "%s", terms[hi]);
              bs_store_search_add(bs_g_state.query);
              bs_g_state.search_kb = 0;
              bs_g_state.tab = BS_TAB_LIBRARY;
              bs_g_state.page = 0;
              bs_view_rebuild();
              bs_redraw_shelf();
            }
            return 1;
          }
        }
        /* Outside the band: a tap above the keyboard dismisses it
         * (KBD_PASSEVENTS stopped the stock outside-tap close, so the
         * app restores it); a tap on the keyboard itself returns 0 so
         * the firmware key handling acts (keys, return, shift...). */
        int y_top, y_bot;
        (void)y_top;
        bs_suggest_band(&y_top, &y_bot);
        if (y < y_bot) {
          if (CloseKeyboard)
            CloseKeyboard();
          return 1;
        }
        return 0;
      }
      if (bs_hit_search_input(x, y) == 1) {
        bs_g_state.search_kb = 1;
        snprintf(bs_g_search_kb_buf, sizeof bs_g_search_kb_buf, "%s", bs_g_state.query);
        g_last_suggest_q[0] = '\0';
        OpenKeyboard("Search", bs_g_search_kb_buf, sizeof bs_g_search_kb_buf - 1,
                     KBD_PASSEVENTS, bs_keyboard_handler);
        SetWeakTimerEx("suggest_debounce", suggest_debounce_tick, NULL, 200);
        return 1;
      }
      int hi = bs_hit_history(x, y);
      if (hi >= 0) {
        char terms[BS_SEARCH_HISTORY_MAX][BS_MAX_QUERY_LEN];
        int got = bs_store_search_list(terms, BS_SEARCH_HISTORY_MAX, 0);
        if (hi < got) {
          snprintf(bs_g_state.query, sizeof bs_g_state.query, "%s", terms[hi]);
          bs_store_search_add(bs_g_state.query);
          bs_LOG("[bookshelf] search history tap: query=`%s`\n", bs_g_state.query);
          bs_g_state.search_kb = 0;
          bs_g_state.tab = BS_TAB_LIBRARY;
          bs_g_state.page = 0;
          bs_view_rebuild();
          bs_redraw_shelf();
        }
      }
      return 1;
    }

    /* Book / card tap */
    int idx = bs_hit_thumbnail(x, y);
    if (idx >= 0) {
      bs_on_tap_thumbnail(idx);
      /* book_press_action already flushed the download popup when
       * the book had to be fetched; repainting the grid here would
       * wipe it. */
      if (!bs_g_state.dl_popup) {
        bs_draw_grid();
        PartialUpdate(0, BS_TOP_BAR_H + BS_TOP_BAR_PAD, ScreenWidth(),
                      bs_content_bottom() - BS_TOP_BAR_H - BS_TOP_BAR_PAD);
      }
      return 1;
    }
    return 0;
  }

  if (type == EVT_KEYPRESS) {
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
    if (is_page_key && !bs_modal_open() && !bs_g_browse_open) {
      int pages = bs_current_pages();
      if ((par1 == IV_KEY_NEXT || par1 == IV_KEY_NEXT2) &&
          bs_g_state.page + 1 < pages) {
        bs_g_state.page++;
        bs_flip_page();
      } else if ((par1 == IV_KEY_PREV || par1 == IV_KEY_PREV2) &&
                 bs_g_state.page > 0) {
        bs_g_state.page--;
        bs_flip_page();
      }
      return 1;
    }

    if (par1 == IV_KEY_BACK || is_page_key) {
      /* The file browser: Back ascends, at the root it opens the
       * source chooser; page keys scroll the list. */
      if (bs_g_browse_open && bs_g_state.source == BS_SOURCE_FOLDER) {
        if (is_page_key) {
          int fwd = par1 == IV_KEY_NEXT || par1 == IV_KEY_NEXT2;
          bs_browse_page(fwd ? 1 : -1);
        } else if (!bs_browse_up()) {
          bs_g_browse_open = 0;
          bs_g_state.overlay = BS_OV_SOURCE;
          bs_draw_overlay_source();
          bs_flush_content();
        }
        return 1;
      }
      if (bs_g_state.overlay == BS_OV_SOURCE) {
        bs_g_state.overlay = BS_OV_NONE;
        bs_redraw_shelf();
        return 1;
      }
      if (bs_g_state.overlay == BS_OV_CTX) {
        bs_close_context();
        return 1;
      }
      if (bs_g_state.dl_popup) {
        /* Modal while downloading; Back only closes a finished
         * popup. */
        if (bs_downloads_pending() == 0 && !bs_g_dl_batch_active) {
          /* Single-book press whose fetch just finished: let
           * dl_advance() close the popup and launch the reader. */
          if (bs_g_state.dl_popup_auto_open && bs_dl_job_pending())
            return 1;
          bs_g_state.dl_popup = 0;
          bs_g_state.dl_popup_auto_open = 0;
          bs_redraw_shelf();
        }
        return 1;
      }
      if (bs_g_state.overlay == BS_OV_FOLDER) {
        bs_folder_close();
        return 1;
      }
      if (bs_g_state.overlay == BS_OV_SETTINGS) {
        bs_settings_close();
        return 1;
      }
      if (bs_g_state.overlay == BS_OV_LAUNCHER) {
        bs_launcher_close();
        return 1;
      }
      if (bs_g_state.overlay == BS_OV_MORE || bs_g_state.overlay == BS_OV_GROUP ||
          bs_g_state.overlay == BS_OV_SORT) {
        bs_g_state.overlay = BS_OV_NONE;
        bs_redraw_shelf();
        return 1;
      }
      if (bs_g_state.tab == BS_TAB_SEARCH) {
        /* Back from the Search page returns to the library, keeping
         * the active query filter in place.  A still-open keyboard
         * must close first (KBD_PASSEVENTS keeps it up on outside
         * taps; its handler then tears the suggestions down). */
        if (bs_g_state.search_kb) {
          ClearTimerByName("suggest_debounce");
          bs_g_nsuggest = 0;
          if (CloseKeyboard)
            CloseKeyboard();
          bs_g_state.search_kb = 0;
        }
        bs_g_state.tab = BS_TAB_LIBRARY;
        bs_g_state.page = 0;
        bs_g_state.search_kb = 0;
        bs_redraw_shelf();
        return 1;
      }
      if (bs_g_drilled_series[0] != '\0') {
        bs_drill_back();
        return 1;
      }
      /* Group drill-in: back pops one level toward All books. */
      if (bs_g_drill_value[0] != '\0') {
        bs_group_drill_back();
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
    /* Tell every in-flight worker job to stop (cooperative flag; the
     * detached threads then get killed by process exit, same as the
     * old download/sync threads).  The log is deliberately NOT closed
     * here: detached workers may still be mid-LOG() (up to 60 s in
     * flight) and would vfprintf into a freed FILE*.  Flush instead —
     * the FILE* stays valid for stragglers and process exit reclaims
     * it. */
    bs_worker_cancel_all();
    bs_store_close();
    bs_launcher_icons_free();
    if (bs_g_log != NULL)
        fflush(bs_g_log);
    return 1;
  }
  return 0;
}

void bs_keyboard_handler(char *buffer) {
  /* The keyboard is closing: tear the live suggestion band down. */
  ClearTimerByName("suggest_debounce");
  bs_g_nsuggest = 0;
  /* buffer aliases g_search_kb_buf (never g_state.query), so this copy
   * is safe and the committed text survives into the filter pass.
   * Only a real edit commits a search and leaves the Search page: a
   * dismissed keyboard (OK / cancel / tap outside) delivers the buffer
   * unchanged, and committing that used to teleport the user home —
   * an empty dismissal even counted as an "edit".  A dismissed,
   * unedited keyboard just closes and the Search page stays put. */
  const char *t = buffer ? buffer : "";
  if (strcmp(t, bs_g_state.query) != 0) {
    snprintf(bs_g_state.query, sizeof bs_g_state.query, "%s", t);
    if (bs_g_state.query[0] != '\0')
      bs_store_search_add(bs_g_state.query);
    bs_LOG("[bookshelf] search commit: query=`%s`\n", bs_g_state.query);
    bs_g_state.search_kb = 0;
    bs_g_state.tab = BS_TAB_LIBRARY;
    bs_g_state.page = 0;
      bs_view_rebuild();
      /* The on-screen keyboard draws full-screen and wipes the bottom
       * status strip; re-stamp it before the draw so the panel survives
       * the commit repaint.  Draw the shelf WITHOUT flushing, then a
       * single full-screen FullUpdate repaints the content area and the
       * panel band the keyboard wiped in one refresh — redraw_shelf()
       * would have flushed the content area as a PartialUpdate first,
       * giving two full refresh cycles per commit. */
      bs_stamp_panel();
      bs_draw_shelf_nofb();
      FullUpdate();
    } else {
      bs_g_state.search_kb = 0;
      bs_stamp_panel();
      bs_draw_shelf_nofb();
      FullUpdate();
    }
}

int main(int argc, char **argv) {
  (void)argc;
  if (argv != NULL && argv[0] != NULL)
    snprintf(bs_g_argv0, sizeof bs_g_argv0, "%s", argv[0]);
  else
    bs_g_argv0[0] = '\0';
  bs_log_open(bs_g_argv0);

  /* Register with the firmware exactly like the stock bookshelf's
   * main() (InitInkview/IvSetAppCapability/SetOrientation/
   * SetDefaultOrientation/SetPanelType) then run the event loop.
   * The orientation/panel registration MUST happen before InkViewMain()
   * attaches the task: on the live device doing it inside EVT_INIT
   * corrupts the per-task fbinfo.  All of this lives in the PB backend
   * (see app/platform/bs_plat_pb.c: bs_plat_boot). */
  bs_plat_boot(bs_on_event);
  /* No log_close(): detached workers may still be mid-LOG() when the
   * event loop unwinds (see the EVT_EXIT comment); freeing g_log
   * under them would be a use-after-free.  Flush and let process
   * exit reclaim the FILE*. */
  if (bs_g_log != NULL)
      fflush(bs_g_log);
  return 0;
}
