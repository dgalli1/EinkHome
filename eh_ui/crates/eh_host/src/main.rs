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

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the SIGINT/SIGTERM handler (Ctrl-C in the launching terminal):
/// the main loop polls it and exits cleanly so the run script's EXIT trap
/// brings the API server down with it.
static SIGNALED: AtomicBool = AtomicBool::new(false);
extern "C" fn on_signal(_sig: libc::c_int) {
    // Async-signal-safe: a SeqCst store compiles to a plain MOV.
    SIGNALED.store(true, Ordering::SeqCst);
}

/// The three PocketBook screen classes the app adapts to (C
/// g_resolutions); F11 cycles them at runtime, EH_RES picks one up front.
const RESOLUTIONS: [(u32, u32); 3] = [(758, 1024), (1072, 1448), (1404, 1872)];
/// Index of the default element (C PC_RES_DEFAULT).
const RES_DEFAULT: usize = 1;

fn main() -> Result<(), String> {
    let (width, height, mut res_idx) = res_from_env();
    let dir = std::env::current_dir().map_err(|e| e.to_string())?;
    // Log chain (C eh_log_open): $PBEMU_LOG_DIR → app dir → /tmp.
    eh_app::logger::init(Some(&dir.to_string_lossy()));
    // The PC build has no firmware panel (C PanelHeight() == 0).
    eh_app::logger::evt_init(0, width, height);

    // Layered load (C eh_load_config_file): argv0-dir cfg (run-visible-sdl
    // writes build/bookshelf.cfg next to the binary) → /etc/pbemu → /tmp
    // write-root, then ./bookshelf.cfg (the e2e harness contract) wins per
    // key.
    let argv0 = std::env::args().next();
    let cfg_path = dir.join("bookshelf.cfg");
    let mut config = Config::load_for_run(&dir, argv0.as_deref());
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
    // Ctrl-C / SIGTERM close the window even when the launching
    // environment inherited a SIGINT-ignored disposition.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    loop {
        if SIGNALED.load(Ordering::SeqCst) || app.fb().close_requested() {
            return Ok(());
        }
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

        // ── SDL events: drain the ENTIRE queue before presenting.  A
        // burst of N events then costs one flush instead of N full
        // redraws, which is what keeps drags and fast tap sequences
        // responsive while frames are expensive.
        app.fb().pump_events();
        let mut handled = false;
        while let Some(ev) = app.fb().poll_event() {
            // F11 (the backend surfaces it as Unknown(0x7A)): cycle the
            // logical canvas to the next screen class (C sdl_set_resolution
            // on EVT key), then have the app relayout against the new
            // ScreenWidth/Height and present.
            if matches!(
                &ev,
                InputEvent::KeyDown {
                    key: KeyCode::Unknown(0x7A)
                }
            ) {
                res_idx = (res_idx + 1) % RESOLUTIONS.len();
                let (w, h) = RESOLUTIONS[res_idx];
                if let Err(e) = app.fb().set_resolution(w, h) {
                    eprintln!("[pc] resolution switch failed: {e}");
                    continue;
                }
                eprintln!("[pc] resolution -> {w}x{h}");
                app.relayout();
                app.present();
                continue;
            }
            app.on_event(&ev);
            handled = true;
        }

        // One flush for the whole drained batch: intermediate states are
        // never visible anyway at these frame costs.
        if handled {
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

/// Resolve the initial resolution from the EH_RES launch flag before the
/// window exists (C sdl_resolve_initial_resolution): "WxH" or a bare
/// width — the first matching element of RESOLUTIONS wins; anything
/// unknown keeps the default.  Returns (w, h, index).
fn res_from_env() -> (u32, u32, usize) {
    let default = RESOLUTIONS[RES_DEFAULT];
    let Some(e) = std::env::var("EH_RES").ok().filter(|v| !v.is_empty()) else {
        return (default.0, default.1, RES_DEFAULT);
    };
    let parsed = match e.split_once('x') {
        Some((w, h)) => w
            .parse::<u32>()
            .ok()
            .zip(h.parse::<u32>().ok())
            .map(|(w, h)| (w, Some(h))),
        None => e.parse::<u32>().ok().map(|w| (w, None)),
    };
    let Some((w, h)) = parsed else {
        eprintln!("[pc] EH_RES={e}: malformed, ignoring");
        return (default.0, default.1, RES_DEFAULT);
    };
    if let Some(i) = RESOLUTIONS
        .iter()
        .position(|&(rw, rh)| rw == w && h.is_none_or(|h| rh == h))
    {
        eprintln!(
            "[pc] EH_RES={e} -> {}x{}",
            RESOLUTIONS[i].0, RESOLUTIONS[i].1
        );
        (RESOLUTIONS[i].0, RESOLUTIONS[i].1, i)
    } else {
        eprintln!("[pc] EH_RES={e}: no supported match, keeping {default:?}");
        (default.0, default.1, RES_DEFAULT)
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
                app.fb().kb_type_text(text);
                Outcome::Reply("ok\n".into())
            }
            None => Outcome::Reply("err unknown cmd\n".into()),
        },
        // Commit the open keyboard exactly like a real RETURN press
        // (close + fire the app's handler with the buffer); the commit
        // itself drains on the present that follows.
        "kb_commit" => {
            app.fb().kb_commit();
            app.present();
            Outcome::Reply("ok\n".into())
        }
        // ── query group
        "key" => match a.map(parse_code) {
            Some(Some(code)) => {
                app.on_event(&InputEvent::KeyDown {
                    key: iv_to_key(code),
                });
                app.present();
                Outcome::Reply("ok\n".into())
            }
            _ => Outcome::Reply("err unknown cmd\n".into()),
        },
        // A real backend key through the scancode path (C sdl_on_key_down):
        // F11 cycles the resolution — the dummy-driver test runs at a fixed
        // 1072x1448, so it is accepted and ignored here.
        "keydown" => match a.and_then(|s| s.parse::<u32>().ok()) {
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
                let fb = app.fb();
                // A failed dump must not masquerade as success: the
                // harness would otherwise fail later on a missing file.
                match fb.dump_ppm(path) {
                    Ok(()) => Outcome::Reply("ok\n".into()),
                    Err(e) => Outcome::Reply(format!("err shot {path}: {e}\n")),
                }
            }
            None => Outcome::Reply("err unknown cmd\n".into()),
        },
        "hash" => {
            let fb = app.fb();
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
        101 => KeyCode::Menu,     // SDL_SCANCODE_MENU
        74 => KeyCode::Home,      // SDL_SCANCODE_HOME
        42 | 41 => KeyCode::Back, // BACKSPACE / ESCAPE
        75 => KeyCode::PrevPage,  // PAGEUP
        76 => KeyCode::NextPage,  // PAGEDOWN
        82 => KeyCode::PrevPage,  // UP → IV_KEY_PREV2
        81 => KeyCode::NextPage,  // DOWN → IV_KEY_NEXT2
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
        // Detail is a new page without a C ordinal; report SETTINGS-like
        // full-screen so the tests' overlay probe stays meaningful.
        Overlay::Detail => 5,
        Overlay::None | Overlay::Download | Overlay::Sync => 0,
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
    if t == Tab::Search {
        1
    } else {
        0
    }
}
