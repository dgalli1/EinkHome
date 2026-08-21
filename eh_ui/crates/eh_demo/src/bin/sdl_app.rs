//! Headless test runner: the REAL `eh_app::App` on the SDL backend, driven
//! over the same UNIX-socket control plane as the C build's `bookshelf.test`
//! (app/platform/eh_plat_sdl.c, EH_ENABLE_TEST_IPC).  The Python harness
//! (tests/support/bookshelf/ipc_sdl.py) speaks this protocol unchanged.
//!
//! Protocol: newline-delimited commands, one-line text replies.
//!
//!   tap x y / down x y / up x y / move x y
//!   key <0xIVKEY|dec>   EVT_KEYPRESS-equivalent (IV key codes)
//!   type TEXT           append to the OpenKeyboard buffer (live)
//!   kb_commit           close the keyboard + fire its handler (RETURN)
//!   hash                FNV1a-64 of the RGBA canvas -> "hash=0x%016llx"
//!   shot PATH           write the canvas to PATH as P6 PPM
//!   state               "state=<overlay>:<tab>:<page>"
//!   quit                exit cleanly
//!
//! Socket path: $EH_SOCKET (no control plane when unset or "off").
//! The 200 ms suggest tick runs in the main loop (the facade's weak-timer
//! equivalent), so live suggestions behave like the device.

use std::io::{Read, Write};

use eh_app::app::{App, Overlay, Tab};
use eh_app::config::Config;
use eh_backend_sdl::SdlFb;
use eh_hal::{Framebuffer, InputEvent, KeyCode};

fn iv_to_key(code: i32) -> KeyCode {
    // Same table as the inkview backend (C IV_KEY codes).
    match code {
        0x17 => KeyCode::Menu,
        0x18 => KeyCode::Up,
        0x19 => KeyCode::Down,
        0x1a => KeyCode::Home,
        0x1b => KeyCode::Back,
        0x1c => KeyCode::Left,
        0x1d => KeyCode::Right,
        0x20 => KeyCode::Ok,
        n => KeyCode::Unknown(n as u32),
    }
}

fn fnv1a_64(data: &[u8]) -> u64 {
    // Same constants as pbemu's frame_dump --hash / the C test build.
    let mut h: u64 = 14695981039346656037;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(1099511622111);
    }
    h
}

fn dump_ppm(fb: &SdlFb, w: u32, h: u32, path: &str) {
    let b = fb.pixels();
    let mut out = Vec::with_capacity(w as usize * h as usize * 3 + 32);
    out.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for i in 0..(w as usize * h as usize) {
        out.push(b[i * 4]);
        out.push(b[i * 4 + 1]);
        out.push(b[i * 4 + 2]);
    }
    let _ = std::fs::write(path, out);
}

fn main() -> Result<(), String> {
    let (width, height) = std::env::var("EH_RES")
        .ok()
        .and_then(|r| {
            r.split_once('x').and_then(|(w, h)| {
                Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))
            })
        })
        .unwrap_or((1072, 1448));

    // App state dir (store/covers/cfg): per-instance so parallel runs with
    // their own socket never share a database.
    let dir = std::env::var("EH_APP_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("eh_sdl_app"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for sub in ["covers", "Downloads"] {
        let _ = std::fs::create_dir_all(dir.join(sub));
    }
    let cfg_path = dir.join("bookshelf.cfg");
    let mut config = Config {
        api_url: std::env::var("API")
            .unwrap_or_else(|_| "http://127.0.0.1:18765".into()),
        api_token: std::env::var("API_TOKEN")
            .unwrap_or_else(|_| "pbemu-dev-token".into()),
        ..Default::default()
    };
    config.downloads_dir = Some(dir.join("Downloads").to_string_lossy().into());

    let fb = SdlFb::new("EinkHome App (test)", width, height, 0.55)?;
    eh_app::logger::init(Some(&dir.to_string_lossy()));
    let mut app = App::new(fb, config, Some(cfg_path), &dir);
    app.present();

    // Control plane (the EH_ENABLE_TEST_IPC equivalent).
    let listener = std::env::var("EH_SOCKET")
        .ok()
        .filter(|s| !s.is_empty() && s != "off")
        .map(|path| -> Result<std::os::unix::net::UnixListener, String> {
            let _ = std::fs::remove_file(&path);
            let l = std::os::unix::net::UnixListener::bind(&path)
                .map_err(|e| format!("bind {path}: {e}"))?;
            l.set_nonblocking(true).map_err(|e| e.to_string())?;
            eprintln!("[sdl_app] control socket: {path}");
            Ok(l)
        })
        .transpose()?;
    let mut client: Option<std::os::unix::net::UnixStream> = None;
    let mut rx: Vec<u8> = Vec::new();

    let mut last_tick = std::time::Instant::now();
    loop {
        app.screen().framebuffer_mut().wait_for_event(16);
        // Drain queued input into the app.
        loop {
            let ev = app.screen().framebuffer_mut().poll_event();
            match ev {
                Some(InputEvent::Lifecycle(42)) => return Ok(()),
                Some(ev) => {
                    app.on_event(&ev);
                    app.present();
                }
                None => break,
            }
        }
        // The 200 ms suggest tick (facade weak-timer equivalent).
        if last_tick.elapsed() >= std::time::Duration::from_millis(200) {
            if app.tick() {
                app.present();
            }
            last_tick = std::time::Instant::now();
        }

        // Poll the control plane.
        let Some(l) = &listener else { continue };
        if client.is_none() {
            if let Ok((c, _)) = l.accept() {
                let _ = c.set_nonblocking(true);
                client = Some(c);
            }
        }
        let mut alive = true;
        if let Some(c) = &mut client {
            let mut buf = [0u8; 2048];
            match c.read(&mut buf) {
                Ok(0) => alive = false,
                Ok(m) => rx.extend_from_slice(&buf[..m]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => alive = false,
            }
        }
        if !alive {
            client = None;
            rx.clear();
            continue;
        }
        // Handle complete lines.
        while let Some(pos) = rx.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = rx.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            let (reply, quit) = handle_cmd(&mut app, line.trim_end(), width, height);
            if let Some(c) = client.as_mut() {
                let _ = c.write_all(reply.as_bytes());
            }
            if quit {
                return Ok(());
            }
        }
    }
}

fn handle_cmd(app: &mut App<SdlFb>, line: &str, width: u32, height: u32) -> (String, bool) {
    let mut it = line.split_whitespace();
    let cmd = it.next().unwrap_or("");
    let a = it.next().unwrap_or("");
    let b = it.next().unwrap_or("");
    match cmd {
        "tap" | "down" | "up" | "move" => {
            let (Ok(x), Ok(y)) = (a.parse::<i32>(), b.parse::<i32>()) else {
                return ("err coords\n".into(), false);
            };
            if cmd != "tap" {
                let ev = match cmd {
                    "down" => InputEvent::PointerDown { x, y },
                    "move" => InputEvent::PointerMove { x, y },
                    _ => InputEvent::PointerUp { x, y },
                };
                app.on_event(&ev);
            } else {
                app.on_event(&InputEvent::PointerDown { x, y });
                app.on_event(&InputEvent::PointerUp { x, y });
            }
            app.present();
            ("ok\n".into(), false)
        }
        "key" => {
            let code = a.strip_prefix("0x").map_or_else(
                || a.parse::<i32>(),
                |h| i32::from_str_radix(h, 16),
            );
            match code {
                Ok(c) => {
                    app.on_event(&InputEvent::KeyDown { key: iv_to_key(c) });
                    app.present();
                    ("ok\n".into(), false)
                }
                Err(_) => ("err keycode\n".into(), false),
            }
        }
        "type" => {
            // Preserve interior spaces: everything after "type ".
            let text = line.strip_prefix("type ").unwrap_or("");
            app.screen().framebuffer_mut().kb_type_text(text);
            ("ok\n".into(), false)
        }
        "kb_commit" => {
            app.screen().framebuffer_mut().kb_commit();
            app.present(); // drains the pending commit
            ("ok\n".into(), false)
        }
        "hash" => {
            let fb = app.screen().framebuffer();
            (format!("hash=0x{:016x}\n", fnv1a_64(fb.pixels())), false)
        }
        "shot" if !a.is_empty() => {
            let fb = app.screen().framebuffer();
            dump_ppm(fb, width, height, a);
            ("ok\n".into(), false)
        }
        "state" => {
            let o = match app.overlay {
                Overlay::None => 0,
                Overlay::More => 1,
                Overlay::Settings => 2,
                Overlay::Launcher => 3,
                Overlay::Source => 4,
                Overlay::Download => 5,
                Overlay::Context => 6,
                Overlay::GroupChooser => 7,
                Overlay::SortChooser => 8,
                Overlay::LogViewer => 9,
                Overlay::Licenses => 10,
                Overlay::LicenseDetail => 11,
            };
            let t = match app.tab {
                Tab::Library => 0,
                Tab::Search => 1,
            };
            (format!("state={o}:{t}:{}\n", app.page), false)
        }
        "quit" => ("ok\n".into(), true),
        "" => ("\n".into(), false),
        _ => ("err unknown cmd\n".into(), false),
    }
}
