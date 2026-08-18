#ifndef EH_PLAT_H
#define EH_PLAT_H
/*
 * eh_plat.h — platform seam (the contract between the app and its host).
 *
 * PocketBook is the first backend (inkview + hwconfig).  App code includes
 * this header and the inkview drawing/event subset it exposes; it never
 * includes <inkview.h> / <hwconfig.h> directly.  All PocketBook-specific
 * logic lives in app/platform/: boot, service, panel and device-identity in
 * eh_plat_pb.c; the launcher data source (firmware view.json/apps_db.json
 * parser + the /mnt/ext1/applications *.app scan) in
 * eh_plat_pb_launcher.c.
 *
 * Adding a future platform (Kobo/Kindle/…) means providing a new backend
 * that implements the functions declared below and the same drawing/event
 * subset; the app code is unchanged.
 *
 * Backend selection: the build defines EH_PLATFORM_SDL to compile the
 * native PC (x64 Wayland/X11 via SDL2) backend; otherwise the PocketBook
 * backend is used.  In the SDL case the inkview/hwconfig headers below
 * resolve to app/platform/sdl/ compat headers (the same struct layouts,
 * event codes and colour constants as the firmware SDK, so the app
 * compiles and behaves unchanged).  The drawing/event API the app uses
 * (DrawString/DrawLine/DrawRect/FillArea/SetFont/OpenFont/CloseFont/
 * StringWidth/PartialUpdate/FullUpdate/DrawBitmap/StretchBitmap/
 * GetCanvas/OpenKeyboard/EVT and IV_KEY constants, ibitmap/ifont/irect)
 * is part of the contract.
 * Colour constants are the contract too:
 *   BLACK = 0x000000, DGRAY = 0x555555, LGRAY = 0xaaaaaa, WHITE = 0xffffff
 */
#ifdef EH_PLATFORM_SDL
#include "sdl/inkview.h"
#include "sdl/hwconfig.h"
#else
/* PB backend: this is the only translation unit that pulls in the
 * firmware SDK headers. */
#include <inkview.h>
#include <hwconfig.h>
#endif

/* libinkview exports iv_update_panel() but the public SDK header omits it.
 * It renders the system status strip (clock / battery / wifi) into the
 * panel region of the framebuffer.  The C++ PBAppFrame framework calls it
 * from the app's CustomDrawPanel() override; a plain-C app must call it
 * itself after DrawPanel() has populated the panel content, otherwise the
 * strip stays blank.  The argument is the reading-mode-enable flag passed
 * through to the panel draw callback (0 for the normal collapsed bar). */
extern void iv_update_panel(int readingModeEnable);

/* libinkview also exports the canvas lock API but the public SDK header
 * omits it.  GetCanvas() (declared in inkview.h) returns the active draw
 * canvas; the QPA bridge (eink-reader) writes RGB24 pixels straight into
 * the canvas to bypass libinkview's 8-bit draw pipeline — the only way
 * an app gets colour on the Kaleido panel. */
extern void lockCanvasDrawing(void);
extern void unlockCanvasDrawing(void);

/* Firmware keyboard exports absent from this SDK vintage's headers
 * (same weak pattern as IvSetAppCapability in eh_main.c).  NULL-check
 * before every call; a missing symbol can never crash the app. */
extern void CloseKeyboard(void) __attribute__((weak));
extern void GetKeyboardRect(irect *rect) __attribute__((weak));

/* ── backend functions (implemented in app/platform/eh_plat_pb.c) ──── */

struct BsLcProfile;
struct BsLauncherItem;
struct BsProgressEntry;
struct sqlite3;

/* Register with the firmware exactly like the stock bookshelf's main()
 * (InitInkview/IvSetAppCapability/SetOrientation/SetDefaultOrientation/
 * SetPanelType) then run the event loop.  Does not return until exit. */
void eh_plat_boot(int (*on_event)(int, int, int));

/* Ask monitor.app to start the resident firmware services (the stock
 * bookshelf sends MSG_START_SERVICES over iv_ipc_cmd during init). */
void eh_plat_start_services(void);

/* Probe the firmware panel height; falls back to the self-drawn strip
 * height when PanelHeight()==0 (live device).  Sets *self_panel to 1
 * when the strip must be painted by the app.  Honours the
 * PBEMU_SELF_PANEL test override. */
int eh_plat_panel_height(int *self_panel);

/* Populate and render the panel content for the first frame
 * (DrawPanel + stamp + Repaint). */
void eh_plat_panel_init(void);

/* Composite the colour-cover overlay into the base framebuffer and
 * clear it, so subsequently drawn 8-bit content (modal popups, dim
 * sheets) paints ABOVE covers.  No-op on platforms where covers are
 * drawn directly into the shared framebuffer (the PocketBook device) —
 * there later draws already win.  Backends with a separate cover
 * overlay (SDL's RGB24 canvas) must implement this so a modal drawn
 * over the grid does not get cover pixels re-stamped on top of it at
 * the next flush.  Call AFTER drawing the shelf body but BEFORE
 * drawing any modal/popup that may overlap it. */
void eh_plat_cover_flush(void);

/* Stamp the bottom status strip: firmware-painted when the panel
 * painter is active, self-drawn when the app owns it. */
void eh_plat_stamp_panel(int self_panel);

/* Fill the launcher device profile from runtime probes (capability
 * based: device_number / device_has_touchpanel / device_has_audio). */
void eh_plat_device_profile(struct BsLcProfile *out, const char *lang);

/* Resolve the on-device system language.  PocketBook stores it in
 * /mnt/ext1/system/config/global.cfg ("language=de") and does NOT export
 * it via the environment, so the PB backend parses that file.  Returns 0
 * with *out set to a 2-letter code (en/de/fr/it) when a known device
 * language is configured, non-zero when there is none (the PC backend
 * has no device config; callers fall back to LANG). */
int eh_plat_device_language(char *out, unsigned cap);

/* Log device model + firmware version once at boot. */
void eh_plat_log_identity(void);

/* Populate the app-launcher item list (the "Apps" overlay).  The items
 * are platform-neutral (name/path/icon/params); where they come from is
 * backend-specific — PocketBook reads its firmware view.json/apps_db.json
 * plus scans /mnt/ext1/applications for *.app files; the PC backend reads
 * the freedesktop .desktop files (Name/Exec/Icon) from the standard
 * application dirs.  Returns the number of items written (<= cap).
 * Callers own the array. */
int eh_plat_launcher_build(struct BsLauncherItem *items, int cap);

/* Launch the app described by `it`.  argv[0] is the app path followed by
 * its params (NULL-terminated, so argv/argc let a native backend exec
 * directly).  Returns 0 on success, non-zero on failure. */
int eh_plat_launch_app(const struct BsLauncherItem *it, char **argv, int argc);

/* Launch a reader on an already-downloaded book file.  `path` is the
 * book's on-disk location; `reader_path` the resolved third-party reader
 * binary (NULL or the standard reader -> the platform's default open-book
 * path); `title` the book title used as the launched task's label.
 * Returns 0 when the launch was initiated, non-zero on failure (the
 * caller then hides its hourglass and repaints).  PB: OpenBook() routes
 * to monitor.app/reader_controller; only an explicitly selected third-party
 * reader (KOReader) is exec'd via NewTaskEx with the firmware flags. */
int eh_plat_launch_reader(const char *path, const char *reader_path,
                          const char *title);

/* Query reading progress from the platform's source store.  `db` is the
 * already-open SQLite handle (the neutral module opens the snapshot);
 * fill `out` with up to `cap` {path, percent} entries and return the
 * count.  PB: reads the firmware explorer-3.db books_settings/files/
 * folders tables.  A future platform stores progress elsewhere. */
int eh_plat_progress_read(struct sqlite3 *db, struct BsProgressEntry *out,
                          int cap);

/* Blit an RGB24 cover into the platform's colour surface, bypassing the
 * 8-bit draw pipeline (iv_area flattens 24-bit sources to grey).  PB: the
 * Kaleido framebuffer canvas (GetCanvas); SDL: the RGB24 cover overlay.
 * Nearest-neighbour scale to (cx,cy,cw,ch); no-op when the surface is not
 * 24bpp. */
void eh_plat_blit_cover(int cx, int cy, int cw, int ch, const ibitmap *src);

/* ── platform device capabilities ──────────────────────────────────────
 * Per-device special rules live HERE, resolved by the backend, never in
 * the neutral app code.  A future platform provides its own answers. */

/* 1 when the panel is colour-capable (covers decode as RGB24).  PB: the
 * firmware device_display_colormask() != 0 (the PocketBook Color reports
 * a colour mask even though the fb ioctl claims 8bpp).  PC: always 1
 * (the SDL canvas is RGB24). */
int eh_plat_display_color(void);

/* 1 on narrow (≤758 px-wide, 6-inch) panels, where the top bar expands
 * the source button to span the whole band.  PB: ScreenWidth() <= 758;
 * PC: the current logical canvas (F11-cycle) width. */
int eh_plat_narrow_screen(void);

/* ── platform filesystem layout ────────────────────────────────────────
 * The platform owns the on-device directory layout: the neutral app
 * never hardcodes a mount point, it asks the backend.  The SDL/PC
 * backend mirrors the PB layout so behaviour stays byte-identical on the
 * host (where /mnt/ext1 simply does not exist and the writable paths
 * fall back to the scratch root). */

/* Default downloads folder (Settings → Download folder default).  PB:
 * /mnt/ext1/Downloads; the writability check in eh_model.c falls back
 * to eh_plat_write_root() when it cannot be created. */
const char *eh_plat_downloads_dir(void);

/* Guest-writable scratch root, used whenever the canonical dir is not
 * writable (log/config/store fallback for the emulator's non-root
 * qemu-arm guest).  PB and PC: /tmp. */
const char *eh_plat_write_root(void);

/* Root of the on-device storage tree the user browses (folder picker,
 * folder-source browser, Local source scan).  PB: /mnt/ext1. */
const char *eh_plat_browse_root(void);

/* 1 when `p` lives under the on-device storage root — a valid download
 * target or browsable path (the config's `downloads_dir=` is re-checked
 * here; the picker never leaves this tree).  PB: a /mnt/ext1 prefix. */
int eh_plat_path_on_storage(const char *p);

/* Reading-progress source DB (the firmware's explorer db, written by
 * both readers) and the writable snapshot the worker refreshes
 * (db + -wal + -shm).  PB: /mnt/ext1/system/explorer-3/explorer-3.db and
 * /tmp/progress_import.db. */
const char *eh_plat_progress_db(void);
const char *eh_plat_progress_snap(void);

/* Scratch cover PNG the cover worker decodes into before scaling.  PB:
 * /tmp/.bcov.png. */
const char *eh_plat_cover_tmp(void);

/* Read-only base config directory (non-writable system config, applied
 * after the app-dir config).  PB/PC: /etc/pbemu. */
const char *eh_plat_config_base_dir(void);

/* Reader binaries the settings page probes (standard + KOReader).  PB:
 * the firmware's eink-reader.app and /mnt/ext1/applications/koreader.app. */
const char *eh_plat_reader_std_path(void);
const char *eh_plat_reader_koreader_path(void);

/* Home-task override dir for promote/demote ("Install as system app").
 * PB: /mnt/ext1/system/bin; honours $EH_SYSAPP_DIR (test hook). */
const char *eh_plat_sysapp_dir(void);

/* Launcher data sources.  Candidate desktop-config files for the PB
 * firmware JSON (`kind` = "db" / "view"), tried in order; returns a
 * NULL-terminated array, or NULL when the backend has no such source.
 * PB: /mnt/ext1/system/config/desktop then /ebrmain/config/desktop. */
const char *const *eh_plat_launcher_desktop_paths(const char *kind);

/* Directory the backend scans for user-installed *.app launcher items
 * (NULL when it has none).  PB: /mnt/ext1/applications. */
const char *eh_plat_launcher_user_apps_dir(void);

#endif /* EH_PLAT_H */
