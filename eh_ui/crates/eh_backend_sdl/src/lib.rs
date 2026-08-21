//! eh_backend_sdl — desktop/emulator backend over SDL2.
//!
//! The host counterpart to the PocketBook backends: a GUI dev loop on the PC
//! (the C app runs its SDL `eh_plat_sdl.c` for the same reason) and visual
//! verification without qemu.
//!
//! Model mirrors the C SDL backend (`g_px`): the app draws into a CPU-side
//! RGBA buffer via [`surface_mut`], and [`present`](Framebuffer::present)
//! uploads it to an SDL streaming texture and blits it.

use eh_hal::{Framebuffer, InputEvent, KeyCode, PixelFormat, Rect, RefreshMode, Screen};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Canvas, Texture};
use sdl2::video::Window;
use sdl2::{EventPump, Sdl};

pub mod ipc;

pub struct SdlFb {
    sdl: Sdl,
    canvas: Canvas<Window>,
    texture: Texture<'static>,
    /// CPU-side RGBA canvas the shell draws into.
    pub buf: Vec<u8>,
    /// Translated events queued by [`pump_events`](Self::pump_events).
    queue: Vec<InputEvent>,
    pub width: u32,
    pub height: u32,
    scale: f32,
    /// Fake keyboard state (the C SDL backend's `g_kb`): the OpenKeyboard
    /// buffer lives here so tests can type into it live (`type_text`) and
    /// commit or cancel it exactly like a RETURN press / dismissal.
    kb_buf: Vec<u8>,
    kb_open: bool,
    kb_on_done: Option<fn(&[u8])>,
}

impl SdlFb {
    pub fn new(title: &str, width: u32, height: u32, scale: f32) -> Result<Self, String> {
        let sdl = sdl2::init().map_err(|e| e.to_string())?;
        let video = sdl.video().map_err(|e| e.to_string())?;
        let win_w = (width as f32 * scale).max(1.0) as u32;
        let win_h = (height as f32 * scale).max(1.0) as u32;
        let window = video
            .window(title, win_w, win_h)
            .position_centered()
            .build()
            .map_err(|e| e.to_string())?;
        let mut canvas = window.into_canvas().build().map_err(|e| e.to_string())?;
        canvas.set_logical_size(width, height).map_err(|e| e.to_string())?;
        // The texture borrows the creator; leak it so we can own Texture<'static>.
        let creator = Box::leak(Box::new(canvas.texture_creator()));
        let texture = creator
            .create_texture_streaming(PixelFormatEnum::RGBA8888, width, height)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            sdl,
            canvas,
            texture,
            buf: vec![0u8; (width as usize) * (height as usize) * 4],
            queue: Vec::new(),
            width,
            height,
            scale,
            kb_buf: Vec::new(),
            kb_open: false,
            kb_on_done: None,
        })
    }

    /// Append text to the open keyboard buffer (the C `AppendIpcText`, the
    /// IPC "type" command).  No-op when no keyboard is open.
    pub fn kb_type_text(&mut self, s: &str) {
        if self.kb_open {
            self.kb_buf.extend_from_slice(s.as_bytes());
        }
    }

    /// Commit the open keyboard exactly like a real RETURN press: close it
    /// and fire the app's handler with the buffer (the IPC "kb_commit").
    pub fn kb_commit(&mut self) {
        if self.kb_open {
            let f = self.kb_on_done.take();
            self.kb_open = false;
            let buf = std::mem::take(&mut self.kb_buf);
            if let Some(f) = f {
                f(&buf);
            }
        }
    }

    /// Drain the SDL event pump into the internal queue; call once per frame.
    pub fn pump_events(&mut self) {
        let Ok(mut pump) = self.sdl.event_pump() else {
            return;
        };
        self.pump_with(&mut pump);
    }

    fn pump_with(&mut self, pump: &mut EventPump) {
        for ev in pump.poll_iter() {
            if let Some(t) = translate(ev, self.scale) {
                self.queue.push(t);
            }
        }
    }

    /// Write the current RGBA buffer to a PPM file (debug / CI dump).
    pub fn dump_ppm(&self, path: &str) -> std::io::Result<()> {
        dump_ppm(&self.buf, self.width, self.height, path)
    }

    pub fn pixels(&self) -> &[u8] {
        &self.buf
    }
}

impl Framebuffer for SdlFb {
    fn screen(&self) -> Screen {
        Screen::full(self.width, self.height)
    }
    fn format(&self) -> PixelFormat {
        PixelFormat::Rgba32
    }
    fn surface_mut(&mut self) -> &mut [u8] {
        &mut self.buf
    }
    fn stride(&self) -> usize {
        self.width as usize * 4
    }
    fn refresh(&mut self, _region: Rect, _mode: RefreshMode) {
        // Upload happens in present().
    }
    fn mark_dirty(&mut self, _region: Rect) {}
    fn poll_event(&mut self) -> Option<InputEvent> {
        if self.queue.is_empty() {
            None
        } else {
            Some(self.queue.remove(0))
        }
    }
    fn wait_for_event(&mut self, timeout_ms: u32) {
        self.pump_events();
        if self.queue.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(timeout_ms as u64));
        }
    }
    fn present(&mut self, _mode: RefreshMode) {
        let _ = self.texture.update(None, &self.buf, self.stride());
        let _ = self.canvas.copy(&self.texture, None, None);
        self.canvas.present();
    }
    fn open_keyboard(&mut self, _title: &str, initial: &str, on_done: fn(&[u8])) {
        // No on-screen keyboard is rendered (like the C SDL build): the
        // buffer is armed so tests can type/commit over the control plane.
        self.kb_buf = initial.as_bytes().to_vec();
        self.kb_on_done = Some(on_done);
        self.kb_open = true;
    }
    fn live_keyboard_text(&self) -> Option<String> {
        if self.kb_open {
            Some(String::from_utf8_lossy(&self.kb_buf).into_owned())
        } else {
            None
        }
    }
    fn cancel_keyboard(&mut self) {
        if self.kb_open {
            self.kb_open = false;
            self.kb_on_done = None;
            self.kb_buf.clear();
        }
    }
    fn net_active(&self) -> bool {
        // The C SDL QueryNetwork: EH_OFFLINE reports no active connection
        // so the app boots from the on-disk store (the offline e2e suite).
        std::env::var("EH_OFFLINE").map(|v| v.is_empty()).unwrap_or(true)
    }
}

/// Translate an SDL event into a shell InputEvent.  With logical sizing SDL
/// reports pointer co-ords in the logical w×h space (matches the C backend).
fn translate(ev: Event, _scale: f32) -> Option<InputEvent> {
    match ev {
        Event::MouseButtonDown { x, y, .. } => Some(InputEvent::PointerDown { x, y }),
        Event::MouseButtonUp { x, y, .. } => Some(InputEvent::PointerUp { x, y }),
        Event::MouseMotion { x, y, .. } => Some(InputEvent::PointerMove { x, y }),
        Event::KeyDown { keycode: Some(k), .. } => {
            key_to_code(k).map(|key| InputEvent::KeyDown { key })
        }
        Event::Quit { .. } => Some(InputEvent::Lifecycle(42)),
        _ => None,
    }
}

fn key_to_code(k: Keycode) -> Option<KeyCode> {
    Some(match k {
        Keycode::Up | Keycode::W => KeyCode::Up,
        Keycode::Down | Keycode::S => KeyCode::Down,
        Keycode::Left | Keycode::A => KeyCode::Left,
        Keycode::Right | Keycode::D => KeyCode::Right,
        Keycode::PageUp => KeyCode::PrevPage,
        Keycode::PageDown => KeyCode::NextPage,
        Keycode::Return | Keycode::Space => KeyCode::Ok,
        Keycode::Backspace | Keycode::Escape => KeyCode::Back,
        Keycode::Home => KeyCode::Home,
        _ => return None,
    })
}

/// Convert RGBA->RGB PPM (P6).
pub fn dump_ppm(buf: &[u8], w: u32, h: u32, path: &str) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(buf.len() * 3 / 4 + 32);
    out.extend_from_slice(format!("P6\n{} {}\n255\n", w, h).as_bytes());
    for i in 0..(w as usize * h as usize) {
        out.push(buf[i * 4]);
        out.push(buf[i * 4 + 1]);
        out.push(buf[i * 4 + 2]);
    }
    std::fs::write(path, out)
}