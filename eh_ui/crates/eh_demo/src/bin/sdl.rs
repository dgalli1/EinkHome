//! Host demo binary: run the portable shelf on SDL.
//!
//! `EH_DUMP=/path/x.ppm` writes one frame and exits (headless-ish visual
//! verification / CI); without it, opens a window and runs the loop.

use eh_backend_sdl::SdlFb;
use eh_demo::{build_screen, draw_self_panel};
use eh_hal::{Framebuffer, InputEvent};

fn main() -> Result<(), String> {
    // Resolution from EH_RES="WxH", defaulting to 1072x1448 (Touch HD 3).
    let (width, height) = std::env::var("EH_RES")
        .ok()
        .and_then(|r| {
            r.split_once('x').and_then(|(w, h)| {
                Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))
            })
        })
        .unwrap_or((1072, 1448));
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
            out.extend_from_slice(format!("P6\n{} {}\n255\n", width, height).as_bytes());
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

    // Interactive loop.
    let mut running = true;
    while running {
        screen.framebuffer_mut().wait_for_event(16);
        // drain queue
        while let Some(ev) = screen.framebuffer_mut().poll_event() {
            match ev {
                InputEvent::Lifecycle(42) => running = false,
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