//! Device demo binary: run the portable shelf directly on /dev/fb0.
//!
//! The KOReader-style direct-framebuffer path for PocketBook / Kobo / Kindle /
//! reMarkable.  On real hardware it writes visible pixels with the e-ink
//! refresh ioctl; in pbemu the fake fb0 is a private memfd so no observer
//! sees those pixels (that route uses eh_backend_inkview instead).

use eh_backend_linuxfb::LinuxFb;
use eh_demo::{build_screen, draw_self_panel};
use eh_hal::{Framebuffer, RefreshMode};

fn main() -> Result<(), String> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "/dev/fb0".to_owned());
    let mut fb = LinuxFb::open(&path).map_err(|e| format!("open {path}: {e}"))?;

    // Let callers reserve a native panel strip (firmware paints it).
    if let Ok(panel_h) = std::env::var("EH_PANEL_H") {
        fb.set_panel(panel_h.parse().unwrap_or(0));
    }

    let scr = fb.screen();
    println!(
        "linuxfb: {}x{} ({}bpp) stride={} content_bottom={}",
        scr.width,
        scr.height,
        fb.format().bytes_per_pixel() * 8,
        fb.stride(),
        scr.content_height()
    );

    let mut screen = build_screen(fb);
    screen.redraw_full();
    draw_self_panel(screen.framebuffer_mut());
    screen.framebuffer_mut().present(RefreshMode::Full);

    println!("linuxfb: drew one frame; sleeping");
    std::thread::sleep(std::time::Duration::from_secs(3600));
    Ok(())
}