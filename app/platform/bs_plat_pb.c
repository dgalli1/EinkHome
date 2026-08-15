/* bs_plat_pb.c — PocketBook platform backend (see app/platform/bs_plat.h).
 *
 * All firmware-specific boot, service, panel, and device-identity logic
 * lives here so the rest of the app stays platform-neutral.  Behaviour is
 * byte-identical to the pre-seam code: each function moves the exact
 * call sequence (and its load-bearing comments) out of bs_main.c /
 * bs_screen.c verbatim. */

#include "bs_core.h"
#include "bs_ui.h"
#include "bs_launcher.h"

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
bs_plat_start_services(void)
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
bs_plat_boot(int (*on_event)(int, int, int))
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
bs_plat_panel_height(int *self_panel)
{
    int h = PanelHeight();
    *self_panel = 0;
    if (h <= 0) {
        h = BS_SELF_PANEL_H;
        *self_panel = 1;
    }
    if (getenv("PBEMU_SELF_PANEL") != NULL) {
        h = BS_SELF_PANEL_H;
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
bs_plat_panel_init(void)
{
    DrawPanel(NULL, "EinkHome", NULL, -1);
    bs_stamp_panel();
    Repaint();
}

/* Paint the bottom status strip: firmware-painted when the panel painter
 * is active (emulator), self-drawn when it never activates (device). */
void
bs_plat_stamp_panel(int self_panel)
{
    if (self_panel)
        bs_draw_system_strip();
    else
        iv_update_panel(0);
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
 * the resolved bs_g_lang.  partner/has_cloud/localization keep the
 * defaults the shipped configs only ever test as "all"/"WW".  Unknown
 * probes leave "all" which matches every conditional. */
void
bs_plat_device_profile(BsLcProfile *out, const char *lang)
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

    bs_LOG("[bookshelf] device_profile device=%s audio=%s lang=%s\n",
           out->device, out->has_audio, out->language);
}

/* Log the device model + firmware version once at boot (telemetry: the
 * codename is not used for conditional resolution, only for diagnostics). */
void
bs_plat_log_identity(void)
{
    char *model = GetDeviceModel();
    char *fw = GetSoftwareVersion();
    bs_LOG("[bookshelf] model=%s fw=%s\n",
           (model != NULL && model[0] != '\0') ? model : "?",
           (fw != NULL && fw[0] != '\0') ? fw : "?");
}

/* ── app launcher (PB data source + launch) ─────────────────────────── */
/* The launcher's app list on PocketBook comes from the firmware's
 * view.json / apps_db.json + the /mnt/ext1/applications *.app scan.  That
 * parser (bs_lc_* resolvers + the build walk) lives in bs_launcher.c as
 * bs_launcher_build_pb(); this delegates to it.  bs_plat_launcher_build
 * writes into the passed items array (the app's global bs_g_launcher_items,
 * so the PB parser's own globals are reused) and returns the count. */

int
bs_plat_launcher_build(BsLauncherItem *items, int cap)
{
    (void)items;
    (void)cap;
    bs_launcher_build_pb();
    return bs_g_launcher_count;
}

/* Launch a launcher app on PocketBook via NewTaskEx.  argv[0] is the app
 * path followed by its params; run_as_reader=0 (a launcher tile is a plain
 * app launch, not a book-open).  Flags 0x25 | TASK_MAKEACTIVE: see the
 * load-bearing comment in the pre-seam bs_launch_app — TASK_MAKEACTIVE is
 * what brings the launched task to the foreground. */
int
bs_plat_launch_app(const BsLauncherItem *it, char **argv, int argc)
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
