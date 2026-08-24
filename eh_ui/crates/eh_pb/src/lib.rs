//! eh_pb — PocketBook app facade: links the Rust app to libinkview.
//!
//! The ONLY crate that turns the portable toolkit into a runnable PocketBook
//! `.app`.  A tiny C shim (`sdk/pb-demo/main.c`) calls `InkViewMain` (NOT
//! `InitInkview` — the task machinery + shim initialise inkview on load,
//! exactly like the proven `sdk/hello/hello.c`).  Every event is forwarded to
//! [`eh_pb_on_event`]; the first `EVT_INIT`/`EVT_SHOW` triggers
//! `init_once`, which builds the real [`eh_app::app::App`] on the inkview
//! canvas: library sync, shelf, More drawer, Settings, Launcher.
//!
//! The inkview backend is used (not linuxfb) because:
//!   - on a real device the canvas is the physical framebuffer and the native
//!     status panel strip is preserved (content stays above it);
//!   - in pbemu the canvas is the per-task SysV SHM that `frame_dump` reads,
//!     so this path is the observable one.

use std::cell::RefCell;
use std::path::Path;

use eh_backend_inkview::{evt_to_input, InkviewFb};
use eh_hal::{Framebuffer, InputEvent};

/// The device app dir: the store, covers and `bookshelf.cfg` live here
/// (C app: next to the binary at /mnt/ext1/system/bin).
const APP_DIR: &str = "/mnt/ext1/system/bin";
const CFG_PATH: &str = "/mnt/ext1/system/bin/bookshelf.cfg";

thread_local! {
    static APP: RefCell<Option<eh_app::app::App<InkviewFb>>> = const { RefCell::new(None) };
}

/// Build the app, sync the library, draw the first frame, store it.  Called
/// lazily from the first EVT_INIT / EVT_SHOW so it runs inside the inkview
/// event context (matching how hello.c draws on EVT_INIT).
fn init_once() {
    let mut fb = InkviewFb::new();
    let s = fb.screen();
    // The e2e harness reads bookshelf.log + the EVT_INIT geometry line.
    eh_app::logger::init(Some(APP_DIR));
    // C eh_plat_log_identity: model + firmware version telemetry once at
    // boot (diagnostics only — conditionals resolve from device_profile).
    let (model, fw) = eh_backend_inkview::device_identity();
    let version = env!("CARGO_PKG_VERSION");
    eh_app::logger::log(&format!(
        "[bookshelf] EinkHome v{version} model={model} fw={fw}"
    ));
    eh_app::logger::evt_init(
        s.height.saturating_sub(s.content_height()),
        s.width,
        s.height,
    );
    // C eh_plat_start_services (the stock bookshelf's initsync kick): ask
    // monitor.app to start the resident firmware services
    // (reader_controller/taskmgr/control_panel/explorer) so a fresh boot
    // is not scanner + this app only.
    fb.start_services();
    // Establish the firmware panel content once at boot (C app's
    // eh_plat_panel_init), so the native clock/battery painters have
    // something to stamp before our first content-area refresh.
    if s.height > s.content_height() {
        fb.panel_init("EinkHome");
    }
    let dir = Path::new(APP_DIR);
    let _ = std::fs::create_dir_all(dir);
    let cfg_path = Path::new(CFG_PATH);
    let config = if cfg_path.exists() {
        eh_app::config::Config::load(cfg_path).unwrap_or_default()
    } else {
        eh_app::config::Config::default()
    };
    let mut app = eh_app::app::App::new(fb, config, Some(cfg_path.to_path_buf()), dir);
    app.present();
    APP.with(|a| {
        *a.borrow_mut() = Some(app);
    });
    // Recurring tick that repaints + drains worker downloads while any are
    // in flight (the inkview event loop's only source of periodic work:
    // downloads must not block the UI thread, and the download-progress
    // popup repaints on this cadence — the C app's weak-timer pattern).
    arm_tick();
}

/// Inkview weak-timer handler: poll the live-suggest tick, repaint + drain
/// any in-flight downloads.  The timer is permanently re-armed (a weak
/// one-shot fires once per arm), so work started at ANY time after boot is
/// caught; presenting only runs while something changed.
extern "C" fn eh_pb_tick(_data: *mut std::ffi::c_void) {
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().as_mut() {
            // The 200 ms suggest poll (C suggest_debounce_tick); cheap
            // no-op while the search keyboard is closed.
            app.tick();
            // ALWAYS present: present() drains a keyboard commit that the
            // firmware delivered AFTER the tap's own flush (the return-key
            // tap reaches the app first, the handler fires second), and
            // early-returns for free when nothing changed.
            app.present();
        }
    });
    arm_tick();
}

/// Arm the 200ms weak timer (once per active stretch).
fn arm_tick() {
    unsafe {
        // NUL-terminated static name kept alive for the timer's lifetime.
        static NAME: &[u8] = b"ehtick\0";
        let cname = std::ffi::CStr::from_bytes_with_nul_unchecked(NAME);
        eh_backend_inkview::arm_weak_timer(cname, eh_pb_tick, 200);
    }
}

/// Handle one raw inkview event (evt, par1, par2), redrawing the affected
/// region.  Returns 0 (handled) or -1 (not handled) mirroring the SDK RES_*.
#[no_mangle]
pub extern "C" fn eh_pb_on_event(evt: i32, par1: i32, par2: i32) -> i32 {
    let ev = match evt_to_input(evt, par1, par2) {
        Some(e) => e,
        None => return -1,
    };

    // First Show/Init event: lazily build + draw the app.
    if matches!(ev, InputEvent::WidgetShown) {
        let missing = APP.with(|a| a.borrow().is_none());
        if missing {
            init_once();
        } else {
            // Re-shown (returned from reader/launcher): repaint; the app
            // re-stamps the self panel if the minute rolled over.
            APP.with(|a| {
                if let Some(app) = a.borrow_mut().as_mut() {
                    app.present();
                }
            });
        }
        return 0;
    }

    let mut handled = false;
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().as_mut() {
            app.on_event(&ev);
            handled = true;
        }
    });
    if handled {
        APP.with(|a| {
            if let Some(app) = a.borrow_mut().as_mut() {
                app.present();
            }
        });
    }
    if handled {
        0
    } else {
        -1
    }
}

/// Reserved native panel height (0 on live devices where the app self-draws
/// the status strip).  For the shim's diagnostics only.
#[no_mangle]
pub extern "C" fn eh_pb_panel_height() -> i32 {
    let f = InkviewFb::new();
    let (h, cb) = (f.screen().height, f.screen().content_height());
    h as i32 - cb as i32
}
