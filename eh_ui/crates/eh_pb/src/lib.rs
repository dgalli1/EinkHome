//! eh_pb — PocketBook app facade: links the Rust app to libinkview.
//!
//! The ONLY crate that turns the portable toolkit into a runnable PocketBook
//! `.app`.  A tiny C shim (`sdk/pb-demo/main.c`) calls `InkViewMain` (NOT
//! `InitInkview` — the task machinery + shim initialise inkview on load,
//! exactly like the proven `sdk/hello/hello.c`).  Every event is forwarded to
//! [`eh_pb_on_event`]; the first `EVT_INIT`/`EVT_SHOW` triggers
//! [`init_once`], which builds the real [`eh_app::app::App`] on the inkview
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

/// Append a diagnostic line to /tmp/pbdemo.log (guest-writable); stderr of a
/// qemu-arm guest process may be dropped, so a file is more reliable.
/// The app's own diagnostics go to /tmp/eh_app.log (eh_app::log).
fn dlog(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/pbdemo.log") {
        let _ = writeln!(f, "{msg}");
    } else {
        eprintln!("[eh_pb] (no log file) {msg}");
    }
}

/// Build the app, sync the library, draw the first frame, store it.  Called
/// lazily from the first EVT_INIT / EVT_SHOW so it runs inside the inkview
/// event context (matching how hello.c draws on EVT_INIT).
fn init_once() {
    dlog("[eh_pb] init_once enter");
    let mut fb = InkviewFb::new();
    let s = fb.screen();
    // The e2e harness reads bookshelf.log + the EVT_INIT geometry line.
    eh_app::logger::init(Some(APP_DIR));
    eh_app::logger::evt_init(s.height.saturating_sub(s.content_height()), s.width, s.height);
    dlog(&format!("[eh_pb] canvas {}x{} panel={}", s.width, s.height, s.height.saturating_sub(s.content_height())));
    // Establish the firmware panel content once at boot (C app's
    // eh_plat_panel_init), so the native clock/battery painters have
    // something to stamp before our first content-area refresh.
    if s.height > s.content_height() {
        fb.panel_init("EinkHome");
        dlog("[eh_pb] called panel_init (firmware panel established)");
    }
    let dir = Path::new(APP_DIR);
    if let Err(e) = std::fs::create_dir_all(dir) {
        dlog(&format!("[eh_pb] create {APP_DIR} failed: {e}"));
    }
    let cfg_path = Path::new(CFG_PATH);
    let config = if cfg_path.exists() {
        match eh_app::config::Config::load(cfg_path) {
            Ok(c) => c,
            Err(e) => {
                dlog(&format!("[eh_pb] config load failed: {e}; using defaults"));
                eh_app::config::Config::default()
            }
        }
    } else {
        dlog("[eh_pb] no bookshelf.cfg; using defaults (ensure_config will persist)");
        eh_app::config::Config::default()
    };
    let mut app = eh_app::app::App::new(fb, config, Some(cfg_path.to_path_buf()), dir);
    app.present();
    dlog("[eh_pb] app booted");
    APP.with(|a| {
        *a.borrow_mut() = Some(app);
        dlog("[eh_pb] app stored in thread_local");
    });
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
    if handled { 0 } else { -1 }
}

/// Reserved native panel height (0 on live devices where the app self-draws
/// the status strip).  For the shim's diagnostics only.
#[no_mangle]
pub extern "C" fn eh_pb_panel_height() -> i32 {
    let f = InkviewFb::new();
    let (h, cb) = (f.screen().height, f.screen().content_height());
    h as i32 - cb as i32
}