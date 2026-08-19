//! Host driver for the real `App`: builds the shelf, then replays synthetic
//! taps (top bar, pager, cover → download, More drawer, Settings, Launcher)
//! and dumps one PPM frame per step for visual verification.
//!
//! Run against the mock API:
//!   cd api && python3 ./api/server.py --provider mock --host 127.0.0.1 --port 18765
//! Then:
//!   cargo run -p eh_app --example host_shelf
//!
//! Env:
//!   API          mock base (default http://127.0.0.1:18765)
//!   EH_RES       WxH (default 1072x1448, the U633 canvas)
//!   EH_DUMP_DIR  dump one PPM per step here (skipped when absent)
//!   EH_TAP       comma list of synthetic taps, e.g.
//!                "menu,settings,launcher,cover:0,next"
//!   EH_DESKTOP_DIR / EH_USER_APPS_DIR
//!                launcher fixture overrides (desktop config dir; user apps
//!                dir holding fake .app files)
//!
//! Tap script tokens:
//!   menu        tap the top-bar "…" button → More drawer
//!   settings    tap the drawer's Settings row
//!   launcher    tap the drawer's Applications row
//!   back        the hardware Back key (closes overlays)
//!   next / prev / first / last
//!               pager buttons
//!   cover:N     tap grid tile N (triggers download → reader)
//!   save        Settings Save button
//!   apihost     Settings API-host row (host keyboard commits the initial
//!               text back, exercising the drain path)

use eh_app::app::App;
use eh_app::config::Config;
use eh_hal::{InputEvent, KeyCode};

const W0: u32 = 1072;
const H0: u32 = 1448;

fn main() -> Result<(), String> {
    let (width, height) = std::env::var("EH_RES")
        .ok()
        .and_then(|r| r.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))))
        .unwrap_or((W0, H0));

    let base = std::env::var("API").unwrap_or_else(|_| "http://127.0.0.1:18765".into());
    let dir = std::env::temp_dir().join("eh_app_host");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let cfg_path = dir.join("bookshelf.cfg");

    // Fresh store each run so the page 1 tiles are deterministic.
    let _ = std::fs::remove_file(dir.join("bookshelf_lib.db"));
    for sub in ["covers", "Downloads"] {
        let _ = std::fs::create_dir_all(dir.join(sub));
    }
    let mut config = Config {
        api_url: base.clone(),
        api_token: "pbemu-dev-token".into(),
        ..Default::default()
    };
    config.downloads_dir = Some(dir.join("Downloads").to_string_lossy().into());

    let fb = eh_backend_sdl::SdlFb::new("EinkHome App (host)", width, height, 0.55)?;
    let mut app = App::new(fb, config, Some(cfg_path), &dir);

    let dump_dir = std::env::var("EH_DUMP_DIR").ok();
    let mut step = 0u32;
    let mut dump = |app: &mut App<eh_backend_sdl::SdlFb>, tag: &str| -> Result<(), String> {
        app.present();
        if let Some(d) = &dump_dir {
            let path = format!("{d}/step_{step:02}_{tag}.ppm");
            std::fs::write(&path, frame_ppm(app, width, height)).map_err(|e| e.to_string())?;
            println!("dump: {path}");
        }
        step += 1;
        Ok(())
    };

    dump(&mut app, "shelf")?;

    if std::env::var("EH_GEO").is_ok() {
        let s = app.screen();
        for (i, _w) in s.widgets.iter().enumerate() {
            let r = s.widget_rect(i);
            println!("geo[{i}] = ({},{},{},{})", r.x, r.y, r.w, r.h);
        }
        let ch = app.screen().content_h;
        println!("content_bottom={} content_h={}", app.content_bottom, ch);
    }

    // ── tap script ────────────────────────────────────────────────────
    let script = std::env::var("EH_TAP").unwrap_or_default();
    for tok in script.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let (cmd, arg) = match tok.split_once(':') {
            Some((c, a)) => (c, Some(a)),
            None => (tok, None),
        };
        let mut tap = |app: &mut App<eh_backend_sdl::SdlFb>, x: i32, y: i32| -> Result<(), String> {
            app.on_event(&InputEvent::PointerDown { x, y });
            app.on_event(&InputEvent::PointerUp { x, y });
            dump(app, cmd)
        };
        let geo = |a: &mut App<eh_backend_sdl::SdlFb>| -> (eh_hal::Rect, eh_hal::Rect, eh_hal::Rect) {
            let s = a.screen();
            let n = s.widgets.len();
            (s.widget_rect(0), s.widget_rect(n - 1), s.widget_rect(2))
        };
        match cmd {
            "menu" => {
                // Top bar: the "…" box is the right BTN_SIZE+2*BTN_PAD band.
                let (tb, _, _) = geo(&mut app);
                tap(&mut app, (tb.x + tb.w) as i32 - 56, (tb.y + tb.h / 2) as i32)?;
            }
            "settings" => {
                // Drawer row 4 (Settings): y = 96 + 3*88 = 360..448, card x.
                tap(&mut app, width as i32 - 400, 400)?;
            }
            "launcher" => {
                // Drawer row 5 (Applications): y = 96 + 4*88 = 448..536.
                tap(&mut app, width as i32 - 400, 488)?;
            }
            "back" => {
                app.on_event(&InputEvent::KeyDown { key: KeyCode::Back });
                dump(&mut app, "back")?;
            }
            "next" | "prev" | "first" | "last" => {
                // Pager band buttons: x offsets from the band edges (12/116/
                // w-212/w-108), centered in the 96x64 button box.
                let (_, pg, _) = geo(&mut app);
                let by = (pg.y + pg.h / 2) as i32;
                let bx0 = pg.x as i32;
                let bx1 = (pg.x + pg.w) as i32;
                let x = match cmd {
                    "prev" => bx0 + 12 + 48,
                    "first" => bx0 + 116 + 48,
                    "last" => bx1 - 212 + 48,
                    _ => bx1 - 108 + 48,
                };
                tap(&mut app, x, by)?;
            }
            "cover" => {
                let n: i32 = arg.unwrap_or("0").parse::<i32>().map_err(|e: std::num::ParseIntError| e.to_string())?;
                // The first cover tile's rect defines the grid cell pitch.
                let (_, _, tile) = geo(&mut app);
                let col = n % 3;
                let row = n / 3;
                let tw = tile.w as i32;
                let th = tile.h as i32;
                tap(&mut app, tile.x as i32 + col * tw + tw / 2, tile.y as i32 + row * th + th / 2)?;
            }
            "save" => {
                // Settings buttons sit at y = 112 + 5*120 + 24 = 736..832.
                tap(&mut app, 120, 784)?;
            }
            "apihost" => {
                // Settings row 1 (API host): y = 112..232.
                tap(&mut app, 200, 170)?;
            }
            other => return Err(format!("unknown tap token: {other}")),
        }
        if std::env::var("EH_GEO_LAUNCHER").is_ok() && app.overlay == eh_app::app::Overlay::Launcher {
            for (i, it) in app.launcher_items.iter().enumerate() {
                let r = app.launcher_rects[i];
                println!(
                    "lgeo[{i}] group={} text={:?} rect=({},{},{},{})",
                    it.group,
                    it.text,
                    r.x,
                    r.y,
                    r.w,
                    r.h
                );
            }
        }
    }

    Ok(())
}
fn frame_ppm(app: &mut App<eh_backend_sdl::SdlFb>, width: u32, height: u32) -> Vec<u8> {
    use eh_hal::Framebuffer;
    let fb = app.screen().framebuffer();
    let b = fb.pixels();
    let mut out = Vec::with_capacity(fb.stride() * height as usize / 4 * 3 + 32);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for i in 0..(width as usize * height as usize) {
        out.push(b[i * 4]);
        out.push(b[i * 4 + 1]);
        out.push(b[i * 4 + 2]);
    }
    out
}