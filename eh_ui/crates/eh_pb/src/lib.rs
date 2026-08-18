//! eh_pb — PocketBook app facade: links the toolkit to libinkview.
//!
//! This is the ONLY crate that turns the portable toolkit into a runnable
//! PocketBook `.app`.  A tiny C shim (`sdk/pb-demo/main.c`) calls
//! `InkViewMain` (NOT `InitInkview` — the task machinery + shim initialise
//! inkview on load, exactly like the proven `sdk/hello/hello.c`).  Every event
//! is forwarded to [`eh_pb_on_event`]; the first `EVT_INIT`/`WidgetShown`
//! triggers [`init_once`] which binds the inkview canvas and draws the shelf.
//!
//! The inkview backend is used here (not linuxfb) because:
//!   - on a real device the canvas is the physical framebuffer and the native
//!     status panel strip is preserved (content stays above it);
//!   - in pbemu the canvas is the per-task SysV SHM that `frame_dump` reads,
//!     so this path is the observable one.
//! The linuxfb backend is the direct-framebuffer route for device bring-up and
//! the future Kobo/Kindle port.

use std::cell::RefCell;

use eh_backend_inkview::{evt_to_input, InkviewFb};
use eh_hal::{Framebuffer, InputEvent};

type Screen = eh_shell::Screen<InkviewFb>;

thread_local! {
    static SCREEN: RefCell<Option<Screen>> = const { RefCell::new(None) };
}

/// Append a diagnostic line to /tmp/pbdemo.log (guest-writable); stderr of a
/// qemu-arm guest process may be dropped, so a file is more reliable.
fn dlog(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/pbdemo.log") {
        if let Err(e) = writeln!(f, "{}", msg) {
            let _ = e;
        }
    } else {
        eprintln!("[eh_pb] (no log file) {msg}");
    }
}

/// Build the screen, draw the first frame, store it.  Called lazily from the
/// first EVT_INIT / WidgetShown so it runs inside the inkview event context
/// (matching how hello.c draws on EVT_INIT).
fn init_once() {
    dlog("[eh_pb] init_once enter");
    let mut fb = InkviewFb::new();
    let (w, h, cb, panel) = {
        let s = fb.screen();
        (s.width, s.height, s.content_height(), s.height - s.content_height())
    };
    dlog(&format!("[eh_pb] canvas {}x{} content_bottom={} panel={}", w, h, cb, panel));
    // Establish the firmware panel content once at boot (C app's
    // eh_plat_panel_init), so the native clock/battery painters have
    // something to stamp before our first content-area refresh.
    if panel > 0 {
        fb.panel_init("EinkHome");
        dlog("[eh_pb] called panel_init (firmware panel established)");
    }
    let content_h = cb;
    let mut screen = eh_demo::build_screen(fb);
    dlog(&format!(
        "[eh_pb] screen built bp={:?} widgets={}",
        screen.breakpoint,
        screen.widgets.len()
    ));
    screen.content_h = content_h;
    screen.redraw_full();
    dlog("[eh_pb] redraw_full done");
    // Self-draw the status strip ONLY when the firmware panel painter is
    // inactive (PanelHeight() <= 0 on the live device), exactly like the C
    // app's eh_plat_panel_height() self_panel flag.  When the firmware owns
    // the panel (PanelHeight() > 0, e.g. this device reported 106) it draws
    // its own native bar and the app draws nothing below content_bottom.
    if panel == 0 {
        eh_demo::draw_self_panel(screen.framebuffer_mut());
        dlog("[eh_pb] drew self panel (firmware painter inactive)");
    } else {
        dlog("[eh_pb] firmware owns panel strip (panel>0); not self-drawing");
    }
    SCREEN.with(|s| {
        *s.borrow_mut() = Some(screen);
        dlog("[eh_pb] screen stored in thread_local");
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

    // First Show/Init event: lazily build + draw the screen.
    if matches!(ev, InputEvent::WidgetShown) {
        SCREEN.with(|s| {
            if s.borrow().is_none() {
                init_once();
            }
        });
        // Repaint after the first show.
        SCREEN.with(|s| {
            if let Some(sc) = s.borrow_mut().as_mut() {
                sc.redraw_partial();
            }
        });
        return 0;
    }

    let handled = SCREEN.with(|s| {
        let mut guard = s.borrow_mut();
        match guard.as_mut() {
            None => -1,
            Some(screen) => {
                screen.on_event(&ev);
                screen.redraw_partial();
                0
            }
        }
    });
    handled
}

/// Reserved native panel height (0 on live devices where the app self-draws
/// the status strip).  For the shim's diagnostics only.
#[no_mangle]
pub extern "C" fn eh_pb_panel_height() -> i32 {
    let f = InkviewFb::new();
    let (h, cb) = (f.screen().height, f.screen().content_height());
    h as i32 - cb as i32
}