//! eh_android — the Android port of EinkHome.
//!
//! One cdylib, two hats:
//!
//! 1. **Platform backend**: `AndroidFb` implements `eh_hal::Framebuffer`
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
//!
//! The whole crate is Android-only: host builds (workspace clippy/tests
//! on a PC) compile to an empty cdylib via the crate-level cfg below.

#![cfg(target_os = "android")]
use android_activity::input::{InputEvent, MotionAction};
use android_activity::{AndroidApp, MainEvent, PollEvent};
use eh_hal::{Framebuffer, InputEvent as HalInput, KeyCode, PixelFormat, RefreshMode, Screen};
use jni::{jni_sig, jni_str};

/// The Android "framebuffer": an owned RGBA8888 buffer the Slint software
/// renderer paints into, blitted to the `ANativeWindow` on refresh.
struct AndroidFb {
    px: Vec<u8>,
    width: u32,
    height: u32,
    window: ndk::native_window::NativeWindow,
    /// The platform storage layout (C eh_plat_* paths): resolved once at
    /// boot from the activity's data dirs.
    paths: eh_hal::PlatformPaths,
}

impl AndroidFb {
    fn new(window: ndk::native_window::NativeWindow, paths: eh_hal::PlatformPaths) -> Self {
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
            paths,
        }
    }

    /// Copy the rendered buffer into the window (row-by-row: the window's
    /// stride may exceed width*4).
    fn blit(&mut self) {
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

    /// The Android layout: the app's external files dir is the storage
    /// root (adb-pushable without permissions, app-writable); no
    /// PocketBook firmware facilities exist here.
    fn paths(&self) -> eh_hal::PlatformPaths {
        self.paths.clone()
    }

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

/// True when `dir` exists (or was just created) AND the app uid can
/// actually create a file in it.  A mere `create_dir_all` is not enough:
/// a root `adb push` into the internal files dir leaves it root-owned,
/// and every subsequent open would fail (first-launch black screen).
fn is_writable(dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".eh_write_probe");
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The app's writable home: the internal data dir when usable, else the
/// adb scratch dir (`/data/local/tmp`), else the system temp dir.  The
/// internal dir stays the CFG read path either way — a root-owned files
/// dir is still readable, so the staged `bookshelf.cfg` keeps loading.
fn resolve_dirs(droid: &AndroidApp) -> (std::path::PathBuf, std::path::PathBuf) {
    let files_dir = droid
        .internal_data_path()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/data/local/tmp"));
    if is_writable(&files_dir) {
        return (files_dir.clone(), files_dir);
    }
    // adb root push left the files dir unwritable for the app uid: keep
    // reading the staged cfg from it, but run the store/scratch state out
    // of a dir we CAN write, so App::new's store open survives the first
    // launch instead of aborting into a black screen.
    eprintln!(
        "[eh_android] data dir {} unwritable; falling back",
        files_dir.display()
    );
    for fallback in ["/data/local/tmp/einkhome"] {
        let fb = std::path::PathBuf::from(fallback);
        if is_writable(&fb) {
            return (files_dir, fb);
        }
    }
    let tmp = std::env::temp_dir().join("einkhome");
    let _ = std::fs::create_dir_all(&tmp);
    (files_dir, tmp)
}

// ── storage permission (KOReader parity, via JNI — the app has no Java) ──
//
// KOReader (platform/android MainActivity.kt) treats storage as a MANDATORY
// permission: on API 30+ it checks `Environment.isExternalStorageManager()`
// and otherwise launches the system "Manage all files" page; on API 28/29
// it runs the classic `requestPermissions(WRITE_EXTERNAL_STORAGE)` dialog.
// We mirror that split — `android.app.NativeActivity` never sees the grant
// callback, so the result is (re-)checked on focus instead.

/// True when the app may read shared storage: the "All files access"
/// appop on API 30+, the legacy WRITE_EXTERNAL_STORAGE grant below that.
fn storage_granted(env: &mut jni::Env) -> jni::errors::Result<bool> {
    let sdk = env
        .get_static_field(
            jni_str!("android/os/Build$VERSION"),
            jni_str!("SDK_INT"),
            jni_sig!("I"),
        )?
        .i()?;
    if sdk >= 30 {
        // Environment.isExternalStorageManager()
        let v = env.call_static_method(
            jni_str!("android/os/Environment"),
            jni_str!("isExternalStorageManager"),
            jni_sig!("()Z"),
            &[],
        )?;
        v.z()
    } else {
        // Context.checkSelfPermission(WRITE_EXTERNAL_STORAGE) == GRANTED
        let perm = env.new_string("android.permission.WRITE_EXTERNAL_STORAGE")?;
        let granted = env
            .call_method(
                activity_object(env)?,
                jni_str!("checkSelfPermission"),
                jni_sig!("(Ljava/lang/String;)I"),
                &[(&perm).into()],
            )?
            .i()?;
        Ok(granted == 0)
    }
}

/// Ask the user for storage access (see [`storage_granted`]).  API 30+
/// opens the system "Manage all files" page for this package (KOReader's
/// requestSpecialPermission flow, minus the Java dialog); below that the
/// runtime permission dialog is requested directly.
fn request_storage(env: &mut jni::Env) -> jni::errors::Result<()> {
    let sdk = env
        .get_static_field(
            jni_str!("android/os/Build$VERSION"),
            jni_str!("SDK_INT"),
            jni_sig!("I"),
        )?
        .i()?;
    let activity = activity_object(env)?;
    if sdk >= 30 {
        let action = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[(&action).into()],
        )?;
        let pkg = env
            .call_method(
                &activity,
                jni_str!("getPackageName"),
                jni_sig!("()Ljava/lang/String;"),
                &[],
            )?
            .l()?;
        let scheme = env.new_string("package")?;
        let uri = env
            .call_static_method(
                jni_str!("android/net/Uri"),
                jni_str!("fromParts"),
                jni_sig!(
                    "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Landroid/net/Uri;"
                ),
                &[
                    (&scheme).into(),
                    (&pkg).into(),
                    (&jni::objects::JObject::null()).into(),
                ],
            )?
            .l()?;
        // ndk-context exposes the APPLICATION context (not the activity):
        // starting a new task from outside an activity requires this flag.
        env.call_method(
            &intent,
            jni_str!("addFlags"),
            jni_sig!("(I)Landroid/content/Intent;"),
            &[jni::objects::JValue::from(0x1000_0000)], // FLAG_ACTIVITY_NEW_TASK
        )?;
        env.call_method(
            &intent,
            jni_str!("setData"),
            jni_sig!("(Landroid/net/Uri;)Landroid/content/Intent;"),
            &[(&uri).into()],
        )?;
        env.call_method(
            &activity,
            jni_str!("startActivity"),
            jni_sig!("(Landroid/content/Intent;)V"),
            &[(&intent).into()],
        )?;
    } else {
        let perm = env.new_string("android.permission.WRITE_EXTERNAL_STORAGE")?;
        let perms = env.new_object_array(1, jni_str!("java/lang/String"), &perm)?;
        env.call_method(
            &activity,
            jni_str!("requestPermissions"),
            jni_sig!("([Ljava/lang/String;I)V"),
            &[(&perms).into(), jni::objects::JValue::from(1)],
        )?;
    }
    Ok(())
}

/// The launcher activity as a JNI object (ndk-context carries it).
fn activity_object<'local>(
    env: &jni::Env<'local>,
) -> jni::errors::Result<jni::objects::JObject<'local>> {
    let ctx = ndk_context::android_context();
    let activity = ctx.context();
    if activity.is_null() {
        return Err(jni::errors::Error::WrongObjectType);
    }
    // The wrapper does not own the ref: ndk-context holds the activity's
    // GlobalRef, this is just a typed view of it for the calls below.
    Ok(unsafe { jni::objects::JObject::from_raw(env, activity.cast()) })
}

/// Run `f` with a JNI env attached to this (android_main) thread.
fn with_jni<T>(f: impl FnOnce(&mut jni::Env) -> jni::errors::Result<T>) -> jni::errors::Result<T> {
    let vm = unsafe { jni::JavaVM::from_raw(ndk_context::android_context().vm().cast()) };
    vm.attach_current_thread(f)
}

/// One-shot storage grant check (the JNI bootstrapping of
/// [`wait_for_storage`] without the request flow).
fn check_storage(_droid: &AndroidApp) -> bool {
    with_jni(storage_granted).unwrap_or(false)
}

/// Poll until storage is granted (the user toggles it in the settings page
/// that [`request_storage`] opened) or the user backs out of it.  The
/// returned bool drives the browse-root choice; a refusal keeps the app
/// functional on its own external files dir.
fn wait_for_storage(droid: &AndroidApp) -> bool {
    // Already granted (returning install / sideloaded grant).
    match with_jni(storage_granted) {
        Ok(true) => return true,
        Ok(false) => {}
        Err(e) => {
            eprintln!("[eh_android] storage check failed: {e}");
            return false;
        }
    }
    if let Err(e) = with_jni(request_storage) {
        eprintln!("[eh_android] storage request failed: {e}");
        return false;
    }
    // The settings page covers us (LostFocus); the next GainedFocus is the
    // user coming back — grant there, or proceed with the fallback root.
    // A 60s cap guards against the intent never opening at all.
    let mut covered = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let mut focus_back = false;
        droid.poll_events(Some(std::time::Duration::from_millis(250)), |event| {
            if let PollEvent::Main(MainEvent::GainedFocus) = event {
                focus_back = covered;
                covered = true;
            }
            if let PollEvent::Main(MainEvent::LostFocus) = event {
                covered = true;
            }
        });
        match with_jni(storage_granted) {
            Ok(true) => return true,
            Ok(false) => {}
            Err(e) => {
                eprintln!("[eh_android] storage check failed: {e}");
                return false;
            }
        }
        if focus_back || std::time::Instant::now() > deadline {
            return false;
        }
    }
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

    // ── storage permission (KOReader parity) ────────────────────────────
    // Ask BEFORE building the shelf: a grant means the book root can be
    // real shared storage (/sdcard), not just the app's own dirs.  A
    // refusal (user backs out of the settings page) still boots — with the
    // app-external fallback — and the shelf upgrades if storage is granted
    // later (GainedFocus handler below).
    let storage_ok = wait_for_storage(&droid);

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
    let (files_dir, work_dir) = resolve_dirs(&droid);
    let _ = std::fs::create_dir_all(&work_dir);

    // Layered config load: the staged ./bookshelf.cfg in the files dir
    // (pushed via adb), else the writable dir's own, else defaults.
    let staged_cfg = files_dir.join("bookshelf.cfg");
    let cfg_path = if staged_cfg.exists() {
        staged_cfg
    } else {
        work_dir.join("bookshelf.cfg")
    };
    let config = eh_app::config::Config::load(&cfg_path).unwrap_or_default();

    // Storage layout: WITH storage access the book root is real shared
    // storage (/sdcard — KOReader's root), otherwise the app's own
    // external files dir (adb-pushable, no permission needed); a
    // Downloads/ subdir under either is the download destination.
    let ext_root = if storage_ok {
        std::path::PathBuf::from("/sdcard")
    } else {
        droid
            .external_data_path()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from("/sdcard/Android/data/de.einkhome.app/files")
            })
    };
    let paths = eh_hal::PlatformPaths {
        browse_root: ext_root.display().to_string(),
        downloads_dir: format!("{}/Download", ext_root.display()),
        device: false,
        sysapp_dir: String::new(),
        user_apps_dir: String::new(),
    };

    let fb = AndroidFb::new(native, paths);

    let mut app = eh_app::app::App::new(fb, config, Some(cfg_path), &work_dir);
    app.present();

    // ── main loop ───────────────────────────────────────────────────────
    let mut last_tick = std::time::Instant::now();
    let mut press_pos: Option<(i32, i32)> = None;
    let mut fell_back = !storage_ok;

    loop {
        droid.poll_events(
            Some(std::time::Duration::from_millis(50)),
            |event| match event {
                PollEvent::Main(MainEvent::InitWindow { .. }) => {
                    app.relayout();
                    app.present();
                }
                PollEvent::Main(MainEvent::GainedFocus) => {
                    // Back from a settings round-trip: if the user granted
                    // storage after a fallback boot, upgrade the book root
                    // and re-run the Local import against shared storage.
                    if fell_back && check_storage(&droid) {
                        fell_back = false;
                        app.paths.browse_root = "/sdcard".into();
                        app.paths.downloads_dir = "/sdcard/Download".into();
                        eprintln!("[eh_android] storage granted; book root upgraded to /sdcard");
                        if app.source == eh_app::app::Source::Local {
                            eh_app::local::kick_import(&mut app);
                        }
                    }
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
