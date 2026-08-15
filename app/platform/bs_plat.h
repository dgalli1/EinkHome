#ifndef BS_PLAT_H
#define BS_PLAT_H
/*
 * bs_plat.h — platform seam (the contract between the app and its host).
 *
 * PocketBook is the first backend (inkview + hwconfig).  App code includes
 * this header and the inkview drawing/event subset it exposes; it never
 * includes <inkview.h> / <hwconfig.h> directly.  All PocketBook-specific
 * boot, service, and device-identity logic lives in app/platform/bs_plat_pb.c.
 *
 * Adding a future platform (Kobo/Kindle/…) means providing a new backend
 * that implements the functions declared below and the same drawing/event
 * subset; the app code is unchanged.
 */

/* PB backend: this is the only translation unit that pulls in the
 * firmware SDK headers.  The inkview drawing/event API the app uses
 * (DrawString/DrawLine/DrawRect/FillArea/SetFont/OpenFont/CloseFont/
 * StringWidth/PartialUpdate/FullUpdate/DrawBitmap/StretchBitmap/
 * GetCanvas/OpenKeyboard/EVT and IV_KEY constants, ibitmap/ifont/irect) is part
 * of the contract.  Colour constants are the contract too:
 *   BLACK = 0x000000, DGRAY = 0x555555, LGRAY = 0xaaaaaa, WHITE = 0xffffff
 */
#include <inkview.h>
#include <hwconfig.h>

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
 * (same weak pattern as IvSetAppCapability in bs_main.c).  NULL-check
 * before every call; a missing symbol can never crash the app. */
extern void CloseKeyboard(void) __attribute__((weak));
extern void GetKeyboardRect(irect *rect) __attribute__((weak));

/* ── backend functions (implemented in app/platform/bs_plat_pb.c) ──── */

struct BsLcProfile;

/* Register with the firmware exactly like the stock bookshelf's main()
 * (InitInkview/IvSetAppCapability/SetOrientation/SetDefaultOrientation/
 * SetPanelType) then run the event loop.  Does not return until exit. */
void bs_plat_boot(int (*on_event)(int, int, int));

/* Ask monitor.app to start the resident firmware services (the stock
 * bookshelf sends MSG_START_SERVICES over iv_ipc_cmd during init). */
void bs_plat_start_services(void);

/* Probe the firmware panel height; falls back to the self-drawn strip
 * height when PanelHeight()==0 (live device).  Sets *self_panel to 1
 * when the strip must be painted by the app.  Honours the
 * PBEMU_SELF_PANEL test override. */
int bs_plat_panel_height(int *self_panel);

/* Populate and render the panel content for the first frame
 * (DrawPanel + stamp + Repaint). */
void bs_plat_panel_init(void);

/* Stamp the bottom status strip: firmware-painted when the panel
 * painter is active, self-drawn when the app owns it. */
void bs_plat_stamp_panel(int self_panel);

/* Fill the launcher device profile from runtime probes (capability
 * based: device_number / device_has_touchpanel / device_has_audio). */
void bs_plat_device_profile(struct BsLcProfile *out, const char *lang);

/* Log device model + firmware version once at boot. */
void bs_plat_log_identity(void);

#endif /* BS_PLAT_H */
