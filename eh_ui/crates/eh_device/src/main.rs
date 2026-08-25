//! `bookshelf-device` — the direct-fb device binary: Kobo, reMarkable 1,
//! Cervantes, or any plain Linux framebuffer board.
//!
//! Runtime detection picks the backend (KOReader ships one binary per
//! platform family and detects the model at startup the same way):
//!
//! 1. Kobo (`PRODUCT` / `kobo_config.sh` / `hwdetect.sh` / `.kobo/version`)
//! 2. Cervantes (`ntxinfo /dev/mmcblk0`)
//! 3. reMarkable (`/sys/devices/soc0/machine`)
//! 4. generic `/dev/fb0` (no EPDC ioctls; standard MT-B touch)
//!
//! The loop is the same discipline as every other backend (eh_android,
//! eh_host): block on input with a timeout, drain events into the app, run
//! the 200 ms tick (live-suggest debounce + download drain), present.
//!
//! Deploy: copy the binary to the device (e.g. `/mnt/onboard/.adds/
//! einkhome/` on a Kobo) and run it from there — config (`bookshelf.cfg`)
//! and state (`bookshelf_lib.db`, `covers/`) live in the working
//! directory, exactly like every other port.

use eh_app::app::App;
use eh_app::config::Config;
use eh_backend_linuxfb::{LinuxFb, TouchQuirks};
use eh_hal::Framebuffer;

fn main() -> Result<(), String> {
    let dir = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    // Log chain (C eh_log_open): app dir → /tmp.
    eh_app::logger::init(Some(&dir.to_string_lossy()));

    let (model, fb) = detect().map_err(|e| format!("device detection: {e}"))?;
    eh_app::logger::log(&format!("detected device: {model}"));

    let cfg_path = dir.join("bookshelf.cfg");
    let config = Config::load(&cfg_path).unwrap_or_default();
    let mut app = App::new(fb, config, Some(cfg_path), &dir);
    app.present();

    let mut last_tick = std::time::Instant::now();
    loop {
        // Block on input; the 50 ms cap paces the 200 ms tick and any
        // download/sync drain the app runs between frames.
        app.fb().wait_for_event(50);
        while let Some(ev) = app.fb().poll_event() {
            app.on_event(&ev);
        }
        if last_tick.elapsed() >= std::time::Duration::from_millis(200) {
            app.tick();
            last_tick = std::time::Instant::now();
        }
        app.present();
    }
}

/// Vendor probes in order; the generic direct-fb board is the fallback.
fn detect() -> Result<(&'static str, LinuxFb), String> {
    if let Ok(kobo) = eh_backend_devices::Kobo::detect() {
        let dev = kobo
            .open("/dev/fb0")
            .map_err(|e| format!("kobo {}: {e}", kobo.model))?;
        return Ok((dev.model, dev.fb));
    }
    if let Ok(cervantes) = eh_backend_devices::Cervantes::detect() {
        let dev = cervantes
            .open("/dev/fb0")
            .map_err(|e| format!("cervantes {}: {e}", cervantes.model))?;
        return Ok((dev.model, dev.fb));
    }
    if let Ok(rm) = eh_backend_devices::Remarkable::detect() {
        let dev = rm
            .open("/dev/fb0")
            .map_err(|e| format!("remarkable: {e}"))?;
        return Ok((dev.model, dev.fb));
    }
    // Generic board: standard MT-B touch, no button pad mapping, no EPDC.
    let mut fb = LinuxFb::open("/dev/fb0").map_err(|e| format!("/dev/fb0: {e}"))?;
    let _ = fb.attach_input(
        &[
            "/dev/input/event0",
            "/dev/input/event1",
            "/dev/input/event2",
        ],
        TouchQuirks::default(),
        &[],
    );
    Ok(("generic linuxfb", fb))
}
