//! `bookshelf.test` — the headless SDL host behind the e2e control plane
//! (the Rust replacement for the C `EH_ENABLE_TEST_IPC` build of
//! `eh_plat_sdl.c`).
//!
//! Owns the real [`App`] over the SDL backend and runs the same main loop
//! discipline as the C `eh_plat_boot`: poll the IPC socket, pump SDL
//! events, run the app's 200 ms tick (live-suggest debounce + download
//! drain), present.  Every IPC command executes inline on this thread with
//! main-thread ownership of the app + canvas, so replies (hash / shot /
//! state) always observe the post-command frame.
//!
//! Boot mirrors the emulator facade: log to `$PBEMU_LOG_DIR/bookshelf.log`
//! (else the app dir / `/tmp`), stamp the `EVT_INIT` geometry line, load
//! `./bookshelf.cfg` (the harness writes it into the run dir), then sync
//! and draw the first shelf.

use eh_app::app::{App, Overlay, Tab};
use eh_app::config::Config;
use eh_backend_sdl::ipc::Ipc;
use eh_backend_sdl::SdlFb;
use eh_hal::{Framebuffer, InputEvent, KeyCode};

const W0: u32 = 1072;
const H0: u32 = 1448;

fn main() -> Result<(), String> {
    let (width, height) = res_from_env();
    let dir = std::env::current_dir().map_err(|e| e.to_string())?;
    // Log chain (C eh_log_open): $PBEMU_LOG_DIR → app dir → /tmp.
    eh_app::logger::init(Some(&dir.to_string_lossy()));
    // The PC build has no firmware panel (C PanelHeight() == 0).
    eh_app::logger::evt_init(0, width, height);

    let cfg_path = dir.join("bookshelf.cfg");
    let mut config = if cfg_path.exists() {
        Config::load(&cfg_path).unwrap_or_default()
    } else {
        Config::default()
    };
    // Env override (the C app's PBEMU_API_URL; the harness's EH_API_URL).
    if let Ok(url) = std::env::var("PBEMU_API_URL").or_else(|_| std::env::var("EH_API_URL")) {
        if !url.is_empty() {
            config.api_url = url;
        }
    }

    let fb = SdlFb::new("EinkHome", width, height, 0.6)?;
    let mut app = App::new(fb, config, Some(cfg_path), &dir);
    app.present();

    let mut ipc = Ipc::bind();
    let mut last_tick = std::time::Instant::now();
    loop {
        // ── control plane: every command runs to completion (incl. the
        // post-command present) before its reply is written.
        let lines = ipc.as_mut().map(|i| i.poll()).unwrap_or_default();
        let mut quit = false;
        for line in lines {
            match handle_line(&mut app, &line) {
                Outcome::Reply(r) => {
                    if let Some(i) = ipc.as_mut() {
                        i.reply(&r);
                    }
                }
                Outcome::Quit => {
                    if let Some(i) = ipc.as_mut() {
                        i.reply("ok\n");
                    }
                    quit = true;
                }
            }
        }
        if quit {
            return Ok(());
        }

        // ── SDL events
        app.screen().framebuffer_mut().pump_events();
        while let Some(ev) = app.screen().framebuffer_mut().poll_event() {
            app.on_event(&ev);
            app.present();
        }

        // ── 200 ms app tick (live-suggest debounce; downloads drain in
        // present).  The C SDL loop runs its timer list every frame with
        // the same cadence.
        if last_tick.elapsed() >= std::time::Duration::from_millis(200) {
            app.tick();
            last_tick = std::time::Instant::now();
        }
        app.present();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

fn res_from_env() -> (u32, u32) {
    match std::env::var("EH_RES").ok() {
        Some(r) => r
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .unwrap_or((W0, H0)),
        None => (W0, H0),
    }
}

/// One command's result: a reply line, or a clean exit request.
enum Outcome {
    Reply(String),
    Quit,
}

fn handle_line(app: &mut App<SdlFb>, line: &str) -> Outcome {
    let mut toks = line.split_whitespace();
    let Some(cmd) = toks.next() else {
        return Outcome::Reply("err unknown cmd\n".into());
    };
    // sscanf semantics: at most two further whitespace-delimited tokens.
    let a = toks.next();
    let b = toks.next();
    match cmd {
        // ── pointer group
        "tap" => match pointer_args(a, b) {
            Some((x, y)) => {
                app.on_event(&InputEvent::PointerDown { x, y });
                app.on_event(&InputEvent::PointerUp { x, y });
                app.present();
                Outcome::Reply("ok\n".into())
            }
            None => Outcome::Reply("err unknown cmd\n".into()),
        },
        "down" | "up" | "move" => match pointer_args(a, b) {
            Some((x, y)) => {
                let ev = match cmd {
                    "down" => InputEvent::PointerDown { x, y },
                    "up" => InputEvent::PointerUp { x, y },
                    _ => InputEvent::PointerMove { x, y },
                };
                app.on_event(&ev);
                app.present();
                Outcome::Reply("ok\n".into())
            }
            None => Outcome::Reply("err unknown cmd\n".into()),
        },
        // ── text group
        "type" => match a {
            Some(text) => {
                app.screen().framebuffer_mut().kb_type_text(text);
                Outcome::Reply("ok\n".into())
            }
            None => Outcome::Reply("err unknown cmd\n".into()),
        },
        // Commit the open keyboard exactly like a real RETURN press
        // (close + fire the app's handler with the buffer); the commit
        // itself drains on the present that follows.
        "kb_commit" => {
            app.screen().framebuffer_mut().kb_commit();
            app.present();
            Outcome::Reply("ok\n".into())
        }
        // ── query group
        "key" => match a.map(parse_code) {
            Some(Some(code)) => {
                app.on_event(&InputEvent::KeyDown { key: iv_to_key(code) });
                app.present();
                Outcome::Reply("ok\n".into())
            }
            _ => Outcome::Reply("err unknown cmd\n".into()),
        },
        // A real backend key through the scancode path (C sdl_on_key_down):
        // F11 cycles the resolution — the dummy-driver test runs at a fixed
        // 1072x1448, so it is accepted and ignored here.
        "keydown" => match a.map(|s| s.parse::<u32>().ok()).flatten() {
            Some(68) => Outcome::Reply("ok\n".into()), // SDL_SCANCODE_F11
            Some(sc) => {
                if let Some(key) = scancode_to_key(sc) {
                    app.on_event(&InputEvent::KeyDown { key });
                    app.present();
                }
                Outcome::Reply("ok\n".into())
            }
            None => Outcome::Reply("err unknown cmd\n".into()),
        },
        "shot" => match a {
            Some(path) => {
                let fb = app.screen().framebuffer_mut();
                let _ = fb.dump_ppm(path);
                Outcome::Reply("ok\n".into())
            }
            None => Outcome::Reply("err unknown cmd\n".into()),
        },
        "hash" => {
            let fb = app.screen().framebuffer();
            let v = fnv1a_64(fb.pixels());
            Outcome::Reply(format!("hash=0x{v:016x}\n"))
        }
        "state" => Outcome::Reply(format!(
            "state={}:{}:{}\n",
            overlay_int(app.overlay),
            tab_int(app.tab),
            app.page
        )),
        "quit" => Outcome::Quit,
        _ => Outcome::Reply("err unknown cmd\n".into()),
    }
}

fn pointer_args(a: Option<&str>, b: Option<&str>) -> Option<(i32, i32)> {
    Some((a?.parse().ok()?, b?.parse().ok()?))
}

/// `"0x1b"` (hex) or decimal — C `strtol(a, NULL, 16)` / `atoi`.
fn parse_code(s: &str) -> Option<u32> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// IV_KEY_* → shell key code (the inkview backend's `iv_to_key`; PREV2 /
/// NEXT2 are page keys exactly like PREV / NEXT in the C app).
fn iv_to_key(code: u32) -> KeyCode {
    match code {
        0x11 => KeyCode::Up,
        0x12 => KeyCode::Down,
        0x13 => KeyCode::Left,
        0x14 => KeyCode::Right,
        0x15 => KeyCode::Minus,
        0x16 => KeyCode::Plus,
        0x17 => KeyCode::Menu,
        0x18 | 0x1c => KeyCode::PrevPage,
        0x19 | 0x1d => KeyCode::NextPage,
        0x1a => KeyCode::Home,
        0x1b => KeyCode::Back,
        0x0a => KeyCode::Ok,
        other => KeyCode::Unknown(other),
    }
}

/// SDL scancode → key code (C map_scancode_to_ivkey).
fn scancode_to_key(sc: u32) -> Option<KeyCode> {
    Some(match sc {
        101 => KeyCode::Menu,             // SDL_SCANCODE_MENU
        74 => KeyCode::Home,              // SDL_SCANCODE_HOME
        42 | 41 => KeyCode::Back,         // BACKSPACE / ESCAPE
        75 => KeyCode::PrevPage,          // PAGEUP
        76 => KeyCode::NextPage,          // PAGEDOWN
        82 => KeyCode::PrevPage,          // UP → IV_KEY_PREV2
        81 => KeyCode::NextPage,          // DOWN → IV_KEY_NEXT2
        _ => return None,
    })
}

/// FNV-1a-64 over the RGBA canvas (the C ipc fnv1a_64; the same constants
/// pbemu's frame_dump --hash uses, so hashes are cross-backend stable).
fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &byte in data {
        h ^= u64::from(byte);
        h = h.wrapping_mul(1099511628211);
    }
    h
}

/// Overlay → the C `EH_OV_*` ordinal the tests compare against
/// (eh_core.h: NONE, SOURCE, MORE, GROUP, SORT, SETTINGS, LOG, LICENSES,
/// LAUNCHER, FOLDER, CTX).  The download popup is no overlay in C (the
/// base state stays), so Download reports NONE.
fn overlay_int(o: Overlay) -> i32 {
    match o {
        Overlay::None | Overlay::Download => 0,
        Overlay::Source => 1,
        Overlay::More => 2,
        Overlay::GroupChooser => 3,
        Overlay::SortChooser => 4,
        Overlay::Settings => 5,
        Overlay::LogViewer => 6,
        Overlay::Licenses => 7,
        Overlay::Launcher => 8,
        Overlay::Context => 10,
        // The C build has no license-detail state; it stays LICENSES.
        Overlay::LicenseDetail => 7,
    }
}

/// Tab → the C `EH_TAB_LIBRARY`/`EH_TAB_SEARCH` ordinals.
fn tab_int(t: Tab) -> i32 {
    if t == Tab::Search { 1 } else { 0 }
}
