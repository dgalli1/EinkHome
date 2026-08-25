//! eh_android — the Android port of EinkHome.
//!
//! One cdylib, two hats:
//!
//! 1. **Platform backend**: [`AndroidFb`] implements `eh_hal::Framebuffer`
//!    over an RGBA buffer; `refresh` blits it into the `ANativeWindow`.
//! 2. **Host**: `android_main` owns the lifecycle (surface gained/lost,
//!    pause/resume), translates android-activity's touch/key events into
//!    `eh_hal::InputEvent`, drives `App::on_event` / `tick` / `present`.
//!
//! The Slint bridge is platform-less: the window is the RGBA buffer, so
//! the software renderer paints straight into memory we control, and the
//! present step is a plain row-copy into the locked window buffer.
//!
//! The APK needs no Java/dex: the manifest declares android.app.NativeActivity
//! with `android.app.lib_name = "eh_android"`, and android-activity's
//! native-activity glue provides ANativeActivity_onCreate inside the .so.

use android_activity::input::{InputEvent, MotionAction};
use android_activity::{AndroidApp, MainEvent, PollEvent};
use eh_hal::{Framebuffer, InputEvent as HalInput, KeyCode, PixelFormat, RefreshMode, Screen};
use std::ops::DerefMut as _;

/// The Android "framebuffer": an owned RGBA8888 buffer the Slint software
/// renderer paints into, blitted to the `ANativeWindow` on refresh.
struct AndroidFb {
    px: Vec<u8>,
    width: u32,
    height: u32,
    window: ndk::native_window::NativeWindow,
}

impl AndroidFb {
    fn new(window: ndk::native_window::NativeWindow) -> Self {
        use ndk::hardware_buffer_format::HardwareBufferFormat;
        // Request RGBA8888 so the blit is a straight row-copy.
        let _ = window.set_buffers_geometry(0, 0, Some(HardwareBufferFormat::R8G8B8A8_UNORM));
        let width = window.width().max(1) as u32;
        let height = window.height().max(1) as u32;
        Self {
            px: vec![0xFF; (width as usize * height as usize * 4).max(1)],
            width,
            height,
            window,
        }
    }

    /// Copy the rendered buffer into the window (row-by-row: the window's
    /// stride may exceed width*4).
    fn blit(&mut self) {
        use std::ops::Deref;
        let Ok(mut locked) = self.window.lock(None) else {
            return;
        };
        // lines() yields one visible row each (stride padding skipped),
        // so the copy is a straight width*4 blit per row.
        let row_bytes = self.width as usize * 4;
        let rows = locked.height().min(self.px.len() / row_bytes);
        let Some(dst_rows) = locked.lines() else {
            return;
        };
        for (y, drow) in dst_rows.enumerate() {
            if y >= rows {
                break;
            }
            let src = &self.px[y * row_bytes..y * row_bytes + drow.len()];
            unsafe {
                std::ptr::copy_nonoverlapping(src.as_ptr(), drow.as_mut_ptr().cast(), drow.len());
            }
        }
    }
}

impl Framebuffer for AndroidFb {
    fn screen(&self) -> Screen {
        Screen::full(self.width, self.height)
    }

    fn format(&self) -> PixelFormat {
        PixelFormat::Rgba32
    }

    fn surface_mut(&mut self) -> &mut [u8] {
        &mut self.px
    }

    fn stride(&self) -> usize {
        self.width as usize * 4
    }

    /// The e-ink discipline collapses to "blit the frame": Android has no
    /// waveform, and the present-skip already avoids redundant blits.
    fn refresh(&mut self, _region: eh_hal::Rect, _mode: RefreshMode) {
        self.blit();
    }

    fn mark_dirty(&mut self, _region: eh_hal::Rect) {}

    fn poll_event(&mut self) -> Option<HalInput> {
        None // events arrive through android-activity's loop instead
    }

    fn wait_for_event(&mut self, _timeout_ms: u32) {}

    fn present(&mut self, _mode: RefreshMode) {}
}

/// Map android-activity's key codes onto the HAL's normalised set (the
/// hardware keys the UI understands: back + page flips).
fn key_code(code: android_activity::input::Keycode) -> Option<KeyCode> {
    Some(match code {
        android_activity::input::Keycode::Back => KeyCode::Back,
        android_activity::input::Keycode::PageUp => KeyCode::PrevPage,
        android_activity::input::Keycode::PageDown => KeyCode::NextPage,
        _ => return None,
    })
}

#[no_mangle]
fn android_main(droid: AndroidApp) {
    // Immersive reader window: hide the status bar (its overlay would
    // swallow taps in the app's top bar) and keep the screen awake.
    droid.set_window_flags(
        android_activity::WindowManagerFlags::FULLSCREEN
            | android_activity::WindowManagerFlags::LAYOUT_IN_SCREEN
            | android_activity::WindowManagerFlags::KEEP_SCREEN_ON,
        android_activity::WindowManagerFlags::empty(),
    );

    // ── wait for the surface, then size the app to it ───────────────────
    let mut ready = false;
    while !ready {
        droid.poll_events(Some(std::time::Duration::from_millis(250)), |event| {
            if let PollEvent::Main(MainEvent::InitWindow { .. }) = event {
                ready = true;
            }
        });
    }
    let Some(native) = droid.native_window() else {
        return;
    };

    // ── app state ───────────────────────────────────────────────────────
    let data_dir = droid
        .internal_data_path()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/data/local/tmp"));
    let _ = std::fs::create_dir_all(&data_dir);

    let fb = AndroidFb::new(native);

    // Layered config load: ./bookshelf.cfg in the files dir (pushed via
    // adb for the emulator), else defaults (offline shelf).
    let cfg_path = data_dir.join("bookshelf.cfg");
    let config = eh_app::config::Config::load(&cfg_path).unwrap_or_default();
    let mut app = eh_app::app::App::new(fb, config, Some(cfg_path), &data_dir);
    app.present();

    // ── main loop ───────────────────────────────────────────────────────
    let mut last_tick = std::time::Instant::now();
    let mut press_pos: Option<(i32, i32)> = None;

    loop {
        droid.poll_events(
            Some(std::time::Duration::from_millis(50)),
            |event| match event {
                PollEvent::Main(MainEvent::InitWindow { .. }) => {
                    app.relayout();
                    app.present();
                }
                PollEvent::Main(MainEvent::InputAvailable) => {
                    let Ok(mut it) = droid.input_events_iter() else {
                        return;
                    };
                    loop {
                        let mut handled_any = false;
                        it.next(|ev| {
                            handled_any = true;
                            match ev {
                                InputEvent::MotionEvent(m) => {
                                    let Some(p) = m.pointers().next() else {
                                        return android_activity::InputStatus::Handled;
                                    };
                                    let (x, y) = (p.x() as i32, p.y() as i32);
                                    match m.action() {
                                        MotionAction::Down => {
                                            press_pos = Some((x, y));
                                            app.on_event(&HalInput::PointerDown { x, y });
                                        }
                                        MotionAction::Move => {
                                            app.on_event(&HalInput::PointerMove { x, y });
                                        }
                                        MotionAction::Up | MotionAction::Cancel => {
                                            app.on_event(&HalInput::PointerUp { x, y });
                                            press_pos = None;
                                        }
                                        _ => {}
                                    }
                                }
                                InputEvent::KeyEvent(k) => {
                                    if let Some(code) = key_code(k.key_code()) {
                                        app.on_event(&HalInput::KeyDown { key: code });
                                    }
                                }
                                _ => {}
                            }
                            android_activity::InputStatus::Handled
                        });
                        if !handled_any {
                            break;
                        }
                    }
                }
                _ => {}
            },
        );

        if last_tick.elapsed() >= std::time::Duration::from_millis(200) {
            app.tick();
            last_tick = std::time::Instant::now();
        }
        app.present();
    }
}
