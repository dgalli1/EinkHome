//! Host shelf: sync the mock API into a store, then render the real shelf on
//! SDL.  Headless frame dump via EH_DUMP=/path.ppm (CI).
//!
//! Run against: cd api && python3 ./api/server.py --provider mock --host 127.0.0.1 --port 18765
//! Then: cargo run -p eh_app --example host_shelf
//!   EH_RES=758x1024 / 1072x1448 / 1404x1872 to step breakpoints.
//!   EH_DUMP=/tmp/shelf.ppm to dump one frame and exit.

use eh_app::client::ApiClient;
use eh_app::config::Config;
use eh_app::cover;
use eh_app::shelf;
use eh_app::store::Store;
use eh_hal::{Framebuffer, InputEvent};

fn main() -> Result<(), String> {
    // Resolve config: API base from env or bookshelf.cfg defaults.
    let base = std::env::var("API").unwrap_or_else(|_| "http://127.0.0.1:18765".into());
    let cfg = Config { api_url: base.clone(), api_token: "pbemu-dev-token".into(), ..Default::default() };

    // A scratch store + covers dir in the host temp tree.
    let dir = std::env::temp_dir().join("eh_app_host");
    let _ = std::fs::create_dir_all(&dir);
    let db = dir.join("bookshelf_lib.db");
    let covers_dir = cover::resolve_covers_dir(&dir);

    let client = ApiClient::new(&base, &cfg.api_token);
    let store = Store::open(&db).map_err(|e| format!("open store: {e}"))?;

    // Sync (idempotent; a fresh store pulls the full library).
    let n = eh_app::sync::sync(&client, &store, 50).map_err(|e| format!("sync: {e}"))?;
    println!("synced {n} books");

    // Page 1 of the shelf with real covers.
    let entries = shelf::load_page(&client, &store, &covers_dir, 24, 0);
    println!("shelf page has {} books", entries.len());

    // Renderer.
    let (width, height) = std::env::var("EH_RES")
        .ok()
        .and_then(|r| r.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
        .unwrap_or((1072, 1448));
    let fb = eh_backend_sdl::SdlFb::new("EinkHome (Rust)", width, height, 0.55)?;
    let mut screen = shelf::build_shelf(fb, &entries);
    println!("shelf: bp={:?}", screen.breakpoint);

    if let Ok(dump) = std::env::var("EH_DUMP") {
        screen.redraw_full();
        let fb = screen.framebuffer();
        let mut out = Vec::with_capacity(fb.stride() * height as usize / 4 * 3 + 32);
        out.extend_from_slice(format!("P6\n{} {}\n255\n", width, height).as_bytes());
        let b = fb.pixels();
        for i in 0..(width as usize * height as usize) {
            out.push(b[i * 4]);
            out.push(b[i * 4 + 1]);
            out.push(b[i * 4 + 2]);
        }
        std::fs::write(&dump, out).map_err(|e| e.to_string())?;
        println!("dumped frame -> {dump}");
        return Ok(());
    }

    let mut running = true;
    while running {
        screen.framebuffer_mut().wait_for_event(16);
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