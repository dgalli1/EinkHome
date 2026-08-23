//! Headless control socket — the Rust port of the C test-build's
//! `EH_ENABLE_TEST_IPC` plane (`eh_plat_sdl.c`).  A UNIX-socket control
//! plane so the e2e tests can drive the running app without the emulator:
//! pointer/key events, live keyboard text, frame hash / PPM dump, and the
//! overlay:tab:page state query.
//!
//! Protocol (newline-delimited text, one reply line per command):
//!
//! ```text
//!   tap x y             POINTERDOWN then POINTERUP at logical (x,y)
//!   down x y / up x y / move x y
//!   key <0xIVKEY|dec>   key press (par1 = IV_KEY_* code)
//!   keydown <scancode>  a real backend key (F11 resolution cycle, ...)
//!   type TEXT           feed text to the OpenKeyboard buffer
//!   kb_commit           close the keyboard + fire its handler (RETURN)
//!   hash                FNV1a-64 of the RGBA canvas -> "hash=0x%016llx"
//!   shot PATH           write the canvas to PATH as P6 PPM
//!   state               "state=<overlay>:<tab>:<page>"
//!   quit                exit the app cleanly
//! ```
//!
//! Socket path: `$EH_SOCKET`, else `/tmp/bookshelf-<pid>.sock`.  A blank
//! or `"off"` EH_SOCKET disables the control plane.  The socket is
//! per-process so parallel test runs never collide.  One client at a time;
//! a second connection is rejected with `busy`.
//!
//! Commands are polled on the host's main thread ([`Ipc::poll`]) and every
//! command executes inline with main-thread ownership of the app + canvas
//! (same discipline as the C loop's `ipc_poll_and_process`), so replies
//! observe the post-command frame.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

pub struct Ipc {
    listener: UnixListener,
    client: Option<UnixStream>,
    /// Partial line accumulator (a command may arrive split across reads).
    acc: Vec<u8>,
}

impl Ipc {
    /// Bind the control socket per `$EH_SOCKET`.  `None` when disabled
    /// (blank / `"off"`) or the bind fails.
    pub fn bind() -> Option<Ipc> {
        let path = match std::env::var("EH_SOCKET") {
            Ok(v) if v.is_empty() || v == "off" => return None,
            Ok(v) => v,
            Err(_) => format!("/tmp/bookshelf-{}.sock", std::process::id()),
        };
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).ok()?;
        listener.set_nonblocking(true).ok()?;
        Some(Ipc {
            listener,
            client: None,
            acc: Vec::new(),
        })
    }

    /// Non-blocking accept + read.  Returns every complete command line
    /// received since the last poll.
    pub fn poll(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        match self.listener.accept() {
            Ok((mut s, _)) => {
                let _ = s.set_nonblocking(true);
                if self.client.is_some() {
                    // Single-client plane: reject the gate-crasher.
                    let _ = s.write_all(b"busy\n");
                } else {
                    self.client = Some(s);
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {}
            Err(_) => {}
        }
        if let Some(c) = self.client.as_mut() {
            let mut buf = [0u8; 2048];
            match c.read(&mut buf) {
                Ok(0) => {
                    self.client = None;
                    self.acc.clear();
                }
                Ok(n) => {
                    self.acc.extend_from_slice(&buf[..n]);
                    while let Some(pos) = self.acc.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = self.acc.drain(..=pos).collect();
                        out.push(String::from_utf8_lossy(&line[..pos]).into_owned());
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => {
                    self.client = None;
                    self.acc.clear();
                }
            }
        }
        out
    }

    /// Write one reply line to the connected client (no-op when none).
    pub fn reply(&mut self, line: &str) {
        if let Some(c) = self.client.as_mut() {
            let _ = c.write_all(line.as_bytes());
            let _ = c.flush();
        }
    }
}
