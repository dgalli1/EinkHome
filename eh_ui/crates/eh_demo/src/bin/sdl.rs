//! Host demo binary: run the portable shelf on SDL.
//!
//! `EH_DUMP=/path/x.ppm` writes one frame and exits (headless-ish visual
//! verification / CI); without it, opens a window and runs the loop.

use eh_backend_sdl::SdlFb;
use eh_demo::{build_screen, draw_self_panel};
use eh_hal::{Framebuffer, InputEvent, KeyCode};

fn main() -> Result<(), String> {
    // Resolution from EH_RES="WxH" or a bare width, matched against the
    // three PocketBook screen classes (C sdl_resolve_initial_resolution);
    // default 1072x1448.  F11 cycles them live.
    const RESOLUTIONS: [(u32, u32); 3] = [(758, 1024), (1072, 1448), (1404, 1872)];
    const RES_DEFAULT: usize = 1;
    let mut res_idx = RES_DEFAULT;
    if let Some(e) = std::env::var("EH_RES").ok().filter(|v| !v.is_empty()) {
        let parsed = match e.split_once('x') {
            Some((w, h)) => w
                .parse::<u32>()
                .ok()
                .zip(h.parse::<u32>().ok())
                .map(|(w, h)| (w, Some(h))),
            None => e.parse::<u32>().ok().map(|w| (w, None)),
        };
        match parsed.and_then(|(w, h)| {
            RESOLUTIONS
                .iter()
                .position(|&(rw, rh)| rw == w && h.is_none_or(|h| rh == h))
        }) {
            Some(i) => res_idx = i,
            None => println!("demo: EH_RES={e}: no supported match, keeping default"),
        }
    }
    let (mut width, mut height) = RESOLUTIONS[res_idx];
    let scale = 0.55f32;

    let fb = SdlFb::new("EinkHome (Rust toolkit)", width, height, scale)?;
    let mut screen = build_screen(fb);
    println!("demo: booted bp={:?}", screen.breakpoint);

    screen.redraw_full();
    draw_self_panel(screen.framebuffer_mut());

    if let Ok(dump) = std::env::var("EH_DUMP") {
        let fb = screen.framebuffer();
        // Need a &SdlFb; framebuffer() gives &B = &SdlFb.
        let ppm = {
            // produce the PPM bytes ourselves to avoid a lifetime detour
            let mut out = Vec::with_capacity(fb.stride() * height as usize / 4 * 3 + 32);
            out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
            let b = fb.pixels();
            for i in 0..(width as usize * height as usize) {
                out.push(b[i * 4]);
                out.push(b[i * 4 + 1]);
                out.push(b[i * 4 + 2]);
            }
            out
        };
        std::fs::write(&dump, ppm).map_err(|e| e.to_string())?;
        println!("demo: dumped frame -> {dump}");
        return Ok(());
    }

    // Interactive loop.  F11 (surfaced as Unknown(0x7A) by the backend)
    // reallocs the canvas at the next screen class and rebuilds the
    // widget tree against the new geometry (C sdl_set_resolution).
    let mut running = true;
    while running {
        screen.framebuffer_mut().wait_for_event(16);
        // drain queue
        while let Some(ev) = screen.framebuffer_mut().poll_event() {
            match ev {
                InputEvent::Lifecycle(42) => running = false,
                InputEvent::KeyDown {
                    key: KeyCode::Unknown(0x7A),
                } => {
                    res_idx = (res_idx + 1) % RESOLUTIONS.len();
                    let (w, h) = RESOLUTIONS[res_idx];
                    let mut fb = screen.into_framebuffer();
                    fb.set_resolution(w, h)?;
                    width = w;
                    height = h;
                    screen = build_screen(fb);
                    screen.redraw_full();
                    draw_self_panel(screen.framebuffer_mut());
                    println!("demo: resolution -> {width}x{height}");
                }
                InputEvent::PointerUp { .. } | InputEvent::KeyDown { .. } => {
                    screen.on_event(&ev);
                }
                _ => {}
            }
        }
        screen.redraw_partial();
    }
    Ok(())
}
