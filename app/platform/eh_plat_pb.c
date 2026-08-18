/* eh_plat_pb.c — PocketBook platform backend (see app/platform/eh_plat.h).
 *
 * All firmware-specific boot, service, panel, and device-identity logic
 * lives here so the rest of the app stays platform-neutral.  Behaviour is
 * byte-identical to the pre-seam code: each function moves the exact
 * call sequence (and its load-bearing comments) out of eh_main.c /
 * eh_screen.c verbatim. */

#include "eh_core.h"
#include "eh_ui.h"
#include "eh_launcher.h"
#include "eh_config.h"

#include <ctype.h>

/* Height of the self-drawn status strip used when the firmware's panel
 * painter never activates (PanelHeight()==0 on the live device).  Matches
 * the stock collapsed bar height the emulator's PanelHeight() reports. */
#define EH_SELF_PANEL_H 106

/* Exported by the firmware's libinkview but absent from this SDK
 * vintage's headers (and its bundled lib).  Weak so the link succeeds
 * either way; the guard skips the call if the runtime library lacks it. */
extern void IvSetAppCapability(int caps) __attribute__((weak));
extern void SetDefaultOrientation(int n) __attribute__((weak));

/* The stock bookshelf sends MSG_START_SERVICES (0x600) to monitor.app
 * during its init; monitor then launches reader_controller, taskmgr,
 * control_panel_mgr, explorer, update_desktop_data and binds the
 * global-request target.  Without it a fresh boot runs only scanner
 * + bookshelf.  iv_ipc_cmd() is the stock's exact transport. */
void
eh_plat_start_services(void)
{
    iv_ipc_cmd(MSG_START_SERVICES, 0);
}

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
void
eh_plat_boot(int (*on_event)(int, int, int))
{
    InitInkview(0x4110);
    if (IvSetAppCapability != NULL)
        IvSetAppCapability(1);
    SetOrientation(0);
    if (SetDefaultOrientation != NULL)
        SetDefaultOrientation(-1);
    SetPanelType(1); /* the stock bookshelf's literal value */
    InkViewMain(on_event);
}

/* Live device: the firmware's panel painter never activates for this
 * task (PanelHeight()==0), so the stock strip is never drawn and our
 * content would sit flush against the top edge.  Reserve the stock bar
 * height and paint the strip ourselves; draw_system_strip() paints it at
 * the bottom of the logical space, which the firmware's fb_y_offset wrap
 * renders at the TOP of the framebuffer — exactly where the stock bar
 * lives.  Test/debug override: force the self-drawn strip even when the
 * firmware panel would paint, so the fallback path can be exercised in
 * the emulator. */
int
eh_plat_panel_height(int *self_panel)
{
    int h = PanelHeight();
    *self_panel = 0;
    if (h <= 0) {
        h = EH_SELF_PANEL_H;
        *self_panel = 1;
    }
    if (getenv("PBEMU_SELF_PANEL") != NULL) {
        h = EH_SELF_PANEL_H;
        *self_panel = 1;
    }
    return h;
}

/* Populate and render the panel content.  DrawPanel() fills in the
 * panel_conf content fields (the stock bookshelf.app calls
 * DrawPanel(NULL, NULL, NULL, -1) from its CustomDrawPanel() override);
 * iv_update_panel(0) is the function that actually blits the clock /
 * battery / wifi strip into the framebuffer.  The framework only calls
 * it via iv_actualize_panel() when is_state_changed() is true, which it
 * isn't on a fresh launch, so we force it here.  Arg 0 = reading-mode
 * disabled (normal bar).  Repaint() forces an immediate one-shot
 * redraw so the panel rect is not blank on first launch. */
void
eh_plat_panel_init(void)
{
    DrawPanel(NULL, "EinkHome", NULL, -1);
    eh_stamp_panel();
    Repaint();
}

/* Paint the bottom status strip: firmware-painted when the panel painter
 * is active (emulator), self-drawn when it never activates (device). */
void
eh_plat_stamp_panel(int self_panel)
{
    if (self_panel)
        eh_draw_system_strip();
    else
        iv_update_panel(0);
}

/* PocketBook covers are drawn with blit_cover_color24 directly into the
 * shared inkview framebuffer (GetCanvas == the fb), so there is no
 * separate cover overlay to composite: a modal drawn after the grid
 * already paints above the covers.  No-op for parity with the seam. */
void
eh_plat_cover_flush(void)
{
}

/* Device language: the firmware stores it in /mnt/ext1/system/config/
 * global.cfg ("language=de") and does not export it through the
 * environment, so we parse that file.  The extractor callback captures
 * the first "language=" value into a static slot (eh_read_kv_file feeds
 * keys/values in file order, unknown keys ignored). */
static char g_plat_lang[8] = "";

static void
plat_lang_kv_cb(const char *key, const char *value, void *user)
{
    (void)user;
    if (g_plat_lang[0] != '\0' || strcmp(key, "language") != 0)
        return;
    /* Normalise "de" / "de_DE" / "de-DE" / "de_DE.utf8" -> "de".  Only
     * the languages the i18n table ships are accepted; anything else is
     * left to the caller's fallback. */
    char base[8];
    unsigned n = 0;
    for (const char *p = value; *p != '\0' && *p != '.' && *p != '_' && *p != '-';
         p++) {
        if (n + 1 < sizeof base)
            base[n++] = (char)tolower((unsigned char)*p);
    }
    if (n < 2)
        return;
    base[2] = '\0';
    if (!(strcmp(base, "en") == 0 || strcmp(base, "de") == 0 ||
          strcmp(base, "fr") == 0 || strcmp(base, "it") == 0))
        return;
    snprintf(g_plat_lang, sizeof g_plat_lang, "%.2s", base);
}

int
eh_plat_device_language(char *out, unsigned cap)
{
    g_plat_lang[0] = '\0';
    eh_read_kv_file("/mnt/ext1/system/config/global.cfg", plat_lang_kv_cb,
                    NULL);
    if (g_plat_lang[0] == '\0')
        return -1;
    snprintf(out, cap, "%s", g_plat_lang);
    return 0;
}

/* Fill the launcher device profile from runtime probes.
 *
 * The shipped view.json/apps_db.json "device" conditionals are
 * capability-based, not codename-based: keys are only "all", "notouch"
 * (devices without a touch panel) and "1030" (the InkPad One, excluded
 * from the Enotes app).  We resolve them from device_number() and
 * device_has_touchpanel(), not GetDeviceModel(): the codename is only
 * logged for telemetry.
 *
 *   device_number()==1030  -> "1030"  (specific exclusion: PB_Enotes)
 *   !device_has_touchpanel() -> "notouch"  (no-touch devices)
 *   otherwise             -> "all"
 *
 * has_audio uses device_has_audio(); language is the first 2 chars of
 * the resolved eh_g_lang.  partner/has_cloud/localization keep the
 * defaults the shipped configs only ever test as "all"/"WW".  Unknown
 * probes leave "all" which matches every conditional. */
void
eh_plat_device_profile(BsLcProfile *out, const char *lang)
{
    unsigned int dnum = device_number();
    if (dnum == 1030)
        snprintf(out->device, sizeof out->device, "1030");
    else if (!device_has_touchpanel())
        snprintf(out->device, sizeof out->device, "notouch");
    else
        snprintf(out->device, sizeof out->device, "all");

    snprintf(out->has_audio, sizeof out->has_audio, "%s",
             device_has_audio() ? "true" : "false");

    if (lang != NULL && lang[0] != '\0')
        snprintf(out->language, sizeof out->language, "%.2s", lang);

    eh_LOG("[bookshelf] device_profile device=%s audio=%s lang=%s\n",
           out->device, out->has_audio, out->language);
}

/* Log the device model + firmware version once at boot (telemetry: the
 * codename is not used for conditional resolution, only for diagnostics). */
void
eh_plat_log_identity(void)
{
    char *model = GetDeviceModel();
    char *fw = GetSoftwareVersion();
    eh_LOG("[bookshelf] model=%s fw=%s\n",
           (model != NULL && model[0] != '\0') ? model : "?",
           (fw != NULL && fw[0] != '\0') ? fw : "?");
}

/* ── app launcher (PB data source + launch) ─────────────────────────── */
/* The launcher's app list on PocketBook comes from the firmware's
 * view.json / apps_db.json + the /mnt/ext1/applications *.app scan.  That
 * parser (eh_lc_* resolvers + the build walk) lives in eh_launcher.c as
 * eh_launcher_build_pb(); this delegates to it.  eh_plat_launcher_build
 * writes into the passed items array (the app's global eh_g_launcher_items,
 * so the PB parser's own globals are reused) and returns the count. */

int
eh_plat_launcher_build(BsLauncherItem *items, int cap)
{
    (void)items;
    (void)cap;
    eh_launcher_build_pb();
    return eh_g_launcher_count;
}

/* Launch a launcher app on PocketBook via NewTaskEx.  argv[0] is the app
 * path followed by its params; run_as_reader=0 (a launcher tile is a plain
 * app launch, not a book-open).  Flags 0x25 | TASK_MAKEACTIVE: see the
 * load-bearing comment in the pre-seam eh_launch_app — TASK_MAKEACTIVE is
 * what brings the launched task to the foreground. */
int
eh_plat_launch_app(const BsLauncherItem *it, char **argv, int argc)
{
    (void)argc;
    if (!it || !it->path[0])
        return -1;
    const char *base = strrchr(it->path, '/');
    base = base ? base + 1 : it->path;
    if (NewTaskEx(it->path, argv, base, it->text, NULL, 0x25 | TASK_MAKEACTIVE, 0) < 0)
        return -1;
    return 0;
}

/* ── device capabilities ────────────────────────────────────────────── */

/* Colour display: device_display_colormask() reports the panel's colour
 * mask.  The PocketBook Color has a nonzero mask while its fb ioctl
 * claims 8bpp; the stock bookshelf uses the same probe to pick RGB24
 * cover decodes. */
int
eh_plat_display_color(void)
{
    return device_display_colormask() != 0;
}

/* Narrow (≤758 px, 6-inch) panel: the top bar spans the source button. */
int
eh_plat_narrow_screen(void)
{
    return ScreenWidth() <= 758;
}

/* ── platform filesystem layout ─────────────────────────────────────── */

const char *
eh_plat_downloads_dir(void)
{
    return "/mnt/ext1/Downloads";
}

const char *
eh_plat_write_root(void)
{
    return "/tmp";
}

const char *
eh_plat_browse_root(void)
{
    return "/mnt/ext1";
}

int
eh_plat_path_on_storage(const char *p)
{
    return strncmp(p, "/mnt/ext1", 9) == 0 && (p[9] == '/' || p[9] == '\0');
}

const char *
eh_plat_progress_db(void)
{
    return "/mnt/ext1/system/explorer-3/explorer-3.db";
}

const char *
eh_plat_progress_snap(void)
{
    return "/tmp/progress_import.db";
}

const char *
eh_plat_cover_tmp(void)
{
    return "/tmp/.bcov.png";
}

const char *
eh_plat_config_base_dir(void)
{
    return "/etc/pbemu";
}

/* Reader binaries probed by recognize_readers().  The standard reader
 * lives in the firmware image; KOReader is a third-party install under
 * /mnt/ext1/applications. */
const char *
eh_plat_reader_std_path(void)
{
    return "/ebrmain/bin/eink-reader.app";
}

const char *
eh_plat_reader_koreader_path(void)
{
    return "/mnt/ext1/applications/koreader.app";
}

/* Home-task override dir, overridable for tests ($EH_SYSAPP_DIR: the
 * SDL e2e suite has no /mnt/ext1 device paths).  /mnt/ext1 is the user
 * partition the app can write. */
const char *
eh_plat_sysapp_dir(void)
{
    const char *d = getenv("EH_SYSAPP_DIR");
    return (d != NULL && d[0] != '\0') ? d : "/mnt/ext1/system/bin";
}

/* Launcher desktop-config candidates, tried in order (the firmware
 * rewrites the desktop JSONs into both locations; the SDK vintages only
 * ship one). */
static const char *const g_lc_db_paths[] = {
    "/mnt/ext1/system/config/desktop/apps_db.json",
    "/ebrmain/config/desktop/apps_db.json",
    NULL,
};
static const char *const g_lc_view_paths[] = {
    "/mnt/ext1/system/config/desktop/view.json",
    "/ebrmain/config/desktop/view.json",
    NULL,
};

const char *const *
eh_plat_launcher_desktop_paths(const char *kind)
{
    if (kind != NULL && strcmp(kind, "view") == 0)
        return g_lc_view_paths;
    return g_lc_db_paths;
}

const char *
eh_plat_launcher_user_apps_dir(void)
{
    return "/mnt/ext1/applications";
}
