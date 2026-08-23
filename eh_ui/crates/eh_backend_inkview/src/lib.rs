//! eh_backend_inkview — PocketBook backend over libinkview.
//!
//! Two things this backend must do that the linuxfb one cannot in pbemu:
//!
//! 1. **Observable output in the emulator**: pbemu's fake `/dev/fb0` is a
//!    private memfd no observer reads; the reliable frame source is the
//!    inkview task canvas (registered via `InitInkview`, published as SysV
//!    SHM).  Drawing into `GetCanvas()` and calling `FullUpdate`/`PartialUpdate`
//!    is what pbemu's `frame_dump` actually sees (informer reads task->fbshmkey).
//!    On a real device, `GetCanvas()` is the physical framebuffer.
//!
//! 2. **The native status bar**: the firmware's type-1 panel strip occupies
//!    `[content_bottom, height)`.  This backend maps [`RefreshMode`] to
//!    inkview `PartialUpdate`/`FullUpdate`, and clamps every app refresh to
//!    `[0, content_bottom)` so the panel painter's strip is never clobbered.
//!    On the live device (PanelHeight()==0) the app draws its own strip — the
//!    backend reports `content_bottom == height` then, and the shell paints it.
//!
//! This is the same division KOReader uses: inkview for lifecycle/panel, the
//! app for pixels.  At the FAR end of the migration the drawing can switch to
//! direct fb0 (see `eh_backend_linuxfb`), keeping this backend only for the
//! panel/lifecycle — exactly KOReader's `framebuffer_pocketbook` + inkview split.

use std::marker::PhantomData;

use eh_hal::{Framebuffer, InputEvent, KeyCode, PixelFormat, Rect, RefreshMode, Screen};
use eh_render::Surface;

/// Canvas layout returned by `GetCanvas()` on the firmware.
#[repr(C)]
struct ICanvas {
    width: i32,
    height: i32,
    scanline: i32,
    depth: i32,
    clipx1: i32,
    clipx2: i32,
    clipy1: i32,
    clipy2: i32,
    addr: *mut u8,
}

/// Header of the firmware `ibitmap` (width/height/depth/scanline u16s,
/// flexible `data[]` at offset 8 — layout contract of the SDK header).
#[repr(C)]
struct IBitmapHdr {
    width: u16,
    height: u16,
    depth: u16,
    scanline: u16,
}

// The firmware symbols only exist on-arm; host builds stub them so the crate
// still compiles (the SDL/linuxfb backends are the host path anyway).
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
#[allow(non_snake_case)]
mod imp {
    use super::{IBitmapHdr, ICanvas};
    pub unsafe extern "C" fn InkViewMain(cb: extern "C" fn(i32, i32, i32) -> i32) { let _ = cb; }
    pub unsafe extern "C" fn InitInkview(_f: i32) {}
    pub unsafe extern "C" fn GetCanvas() -> *const ICanvas { std::ptr::null() }
    pub unsafe extern "C" fn FullUpdate() {}
    pub unsafe extern "C" fn PartialUpdate(_x: i32, _y: i32, _w: i32, _h: i32) {}
    pub unsafe extern "C" fn iv_update_panel(_reading_mode: i32) {}
    pub unsafe extern "C" fn DrawPanel(_icon: *const core::ffi::c_void, _text: *const u8, _title: *const u8, _percent: i32) -> i32 { 0 }
    pub unsafe extern "C" fn Repaint() {}
    pub unsafe extern "C" fn ScreenWidth() -> i32 { 758 }
    pub unsafe extern "C" fn ScreenHeight() -> i32 { 1024 }
    pub unsafe extern "C" fn PanelHeight() -> i32 { 0 }
    pub unsafe extern "C" fn OpenBook(_path: *const u8, _params: *const u8, _flags: i32) -> i32 { 0 }
    pub unsafe extern "C" fn NewTaskEx(_path: *const u8, _args: *mut *mut u8, _appname: *const u8, _name: *const u8, _icon: *const core::ffi::c_void, _flags: u32, _as_reader: i32) -> i32 { 0 }
    #[allow(dead_code)]
    pub unsafe extern "C" fn OpenKeyboard(_title: *const u8, _buf: *mut i8, _max: i32, _flags: i32, _h: extern "C" fn(*mut i8)) {}
    #[allow(dead_code)]
    pub unsafe extern "C" fn CloseKeyboard() {}
    #[allow(dead_code)]
    pub unsafe extern "C" fn SetWeakTimerEx(_name: *const u8, _h: extern "C" fn(*mut core::ffi::c_void), _d: *mut core::ffi::c_void, _ms: i32) -> i32 { 0 }
    #[allow(dead_code)]
    pub unsafe extern "C" fn QueryNetwork() -> i32 { 0 }
    #[allow(dead_code)]
    pub unsafe extern "C" fn GetBatteryPower() -> i32 { -1 }
    #[allow(dead_code)]
    pub unsafe extern "C" fn BanSleep(_sec: i32) {}
    #[allow(dead_code)]
    pub unsafe extern "C" fn GetDeviceModel() -> *mut u8 { std::ptr::null_mut() }
    #[allow(dead_code)]
    pub unsafe extern "C" fn GetSoftwareVersion() -> *mut u8 { std::ptr::null_mut() }
    #[allow(dead_code)]
    pub unsafe extern "C" fn device_number() -> u32 { 0 }
    #[allow(dead_code)]
    pub unsafe extern "C" fn device_has_touchpanel() -> bool { false }
    #[allow(dead_code)]
    pub unsafe extern "C" fn device_has_audio() -> bool { false }
    #[allow(dead_code)]
    pub unsafe extern "C" fn iv_ipc_cmd(_typ: core::ffi::c_long, _param: core::ffi::c_long) -> core::ffi::c_long { 0 }
    #[allow(dead_code)]
    pub unsafe extern "C" fn OpenControlPanel(_ctx: *mut core::ffi::c_void) {}
    #[allow(dead_code)]
    pub unsafe extern "C" fn GetResource(_name: *const u8, _deflt: *const IBitmapHdr) -> *mut IBitmapHdr { std::ptr::null_mut() }
    #[allow(dead_code)]
    pub unsafe extern "C" fn LoadPNG(_name: *const u8, _deflt: i32) -> *mut IBitmapHdr { std::ptr::null_mut() }
    #[allow(dead_code)]
    pub unsafe extern "C" fn GetFrontlightState() -> i32 { -1 }
    #[allow(dead_code)]
    pub unsafe extern "C" fn SetOrientation(_n: i32) {}
    #[allow(dead_code)]
    pub unsafe extern "C" fn SetPanelType(_t: i32) {}
    #[allow(dead_code)]
    pub unsafe extern "C" fn GetFrontlightEnabled() -> i32 { 0 }
}
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
#[allow(non_snake_case)]
mod imp {
    use super::*;
    unsafe extern "C" {
        pub(super) fn InitInkview(reg_flags: i32);
        pub(super) fn GetCanvas() -> *const ICanvas;
        pub(super) fn PartialUpdate(x: i32, y: i32, w: i32, h: i32);
        pub(super) fn FullUpdate();
        pub(super) fn ScreenWidth() -> i32;
        pub(super) fn ScreenHeight() -> i32;
        pub(super) fn PanelHeight() -> i32;
        pub(super) fn iv_update_panel(reading_mode: i32);
        pub(super) fn DrawPanel(icon: *const core::ffi::c_void, text: *const u8, title: *const u8, percent: i32) -> i32;
        pub(super) fn Repaint();
        pub(super) fn InkViewMain(cb: extern "C" fn(i32, i32, i32) -> i32);
        pub(super) fn OpenBook(path: *const u8, parameters: *const u8, flags: i32) -> i32;
        pub(super) fn NewTaskEx(path: *const u8, args: *mut *mut u8, appname: *const u8, name: *const u8, icon: *const core::ffi::c_void, flags: u32, run_as_reader: i32) -> i32;
        pub(super) fn OpenKeyboard(title: *const u8, buffer: *mut i8, maxlen: i32, flags: i32, hproc: extern "C" fn(*mut i8));
        pub(super) fn CloseKeyboard();
        pub(super) fn SetWeakTimerEx(name: *const u8, handler: extern "C" fn(*mut core::ffi::c_void), data: *mut core::ffi::c_void, ms: i32) -> i32;
        pub(super) fn QueryNetwork() -> i32;
        pub(super) fn GetBatteryPower() -> i32;
        pub(super) fn BanSleep(sec: i32);
        pub(super) fn GetDeviceModel() -> *mut u8;
        pub(super) fn GetSoftwareVersion() -> *mut u8;
        pub(super) fn device_number() -> u32;
        pub(super) fn device_has_touchpanel() -> bool;
        pub(super) fn device_has_audio() -> bool;
        pub(super) fn iv_ipc_cmd(typ: core::ffi::c_long, param: core::ffi::c_long) -> core::ffi::c_long;
        pub(super) fn SetOrientation(n: i32);
        pub(super) fn SetPanelType(type_: i32);
        pub(super) fn OpenControlPanel(ctx: *mut core::ffi::c_void);
        pub(super) fn GetResource(name: *const u8, deflt: *const IBitmapHdr) -> *mut IBitmapHdr;
        pub(super) fn LoadPNG(name: *const u8, deflt: i32) -> *mut IBitmapHdr;
        pub(super) fn GetFrontlightState() -> i32;
        pub(super) fn GetFrontlightEnabled() -> i32;
    }
}
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
use imp::{BanSleep, CloseKeyboard, DrawPanel, FullUpdate, GetBatteryPower, GetCanvas, GetDeviceModel, GetFrontlightEnabled, GetFrontlightState, GetResource, GetSoftwareVersion, InitInkview, InkViewMain, LoadPNG, NewTaskEx, OpenBook, OpenControlPanel, OpenKeyboard, PanelHeight, PartialUpdate, QueryNetwork, Repaint, ScreenHeight, ScreenWidth, SetOrientation, SetPanelType, SetWeakTimerEx, device_has_audio, device_has_touchpanel, device_number, iv_ipc_cmd, iv_update_panel};
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
use imp::{BanSleep, DrawPanel, FullUpdate, GetBatteryPower, GetCanvas, GetDeviceModel, GetFrontlightEnabled, GetFrontlightState, GetResource, GetSoftwareVersion, InitInkview, InkViewMain, LoadPNG, NewTaskEx, OpenBook, OpenControlPanel, PanelHeight, PartialUpdate, QueryNetwork, Repaint, ScreenHeight, ScreenWidth, SetOrientation, SetPanelType, SetWeakTimerEx, device_has_audio, device_has_touchpanel, device_number, iv_ipc_cmd, iv_update_panel};

/// Boot the inkview library exactly like the stock bookshelf: register, then
/// hand the event loop a callback.  `on_event` receives raw (evt, par1, par2)
/// and returns the RES_* result code.
pub fn iv_main(on_event: extern "C" fn(i32, i32, i32) -> i32) -> ! {
    unsafe {
        InitInkview(0x4110);
        // C eh_plat_boot (the stock bookshelf's main): register the
        // orientation/panel BEFORE InkViewMain attaches the task — the
        // theme store GetResource reads resolves per-orientation resource
        // sets and misses without it (launcher icons came back NULL).
        SetOrientation(0);
        SetPanelType(1);
        InkViewMain(on_event);
    }
    std::process::abort();
}

/// The inkview canvas-backed framebuffer.
pub struct InkviewFb {
    /// Height of the native panel strip (0 on live devices where the app
    /// self-draws it; nonzero when the firmware painter owns it).
    panel_h: u32,
    width: u32,
    height: u32,
    #[allow(dead_code)]
    _p: PhantomData<usize>,
}

impl Default for InkviewFb {
    fn default() -> Self {
        Self::new()
    }
}

impl InkviewFb {
    /// Bind to the firmware canvas.  Call after `InitInkview`.
    ///
    /// `ee_seen` — actually: if `PanelHeight()>0` the firmware owns a bottom
    /// strip and we must stay above it; else `content_bottom==height` and the
    /// app draws its own strip.
    pub fn new() -> Self {
        let (width, height, panel) = unsafe {
            (
                ScreenWidth() as u32,
                ScreenHeight() as u32,
                PanelHeight().max(0) as u32,
            )
        };
        Self { panel_h: panel, width, height, _p: PhantomData }
    }

    fn canvas(&self) -> ICanvas {
        unsafe { std::ptr::read(GetCanvas()) }
    }
}

impl Framebuffer for InkviewFb {
    fn screen(&self) -> Screen {
        let content = self.height.saturating_sub(self.panel_h);
        Screen { width: self.width, height: self.height, content_bottom: content }
    }

    fn format(&self) -> PixelFormat {
        // Kaleido colour devices: 24bpp canvas.  Monochrome: 8bpp gray.
        match self.canvas().depth {
            24 => PixelFormat::Rgb24,
            32 => PixelFormat::Rgba32,
            _ => PixelFormat::Grayscale8,
        }
    }

    fn surface_mut(&mut self) -> &mut [u8] {
        let cv = self.canvas();
        let stride = cv.scanline.max(cv.width) as usize;
        let bytes = stride * cv.height.max(0) as usize;
        unsafe {
            core::slice::from_raw_parts_mut(cv.addr, bytes)
        }
    }

    fn stride(&self) -> usize {
        self.canvas().scanline.max(self.canvas().width) as usize
    }

    fn refresh(&mut self, region: Rect, mode: RefreshMode) {
        let limit = Rect { x: 0, y: 0, w: self.width, h: self.height.saturating_sub(self.panel_h) };
        let r = region.intersect(&limit);
        if r.is_empty() {
            return;
        }
        unsafe {
            // Panel-safe refresh (mirrors the C app's eh_flush_content):
            // a PartialUpdate of the content area never touches the firmware
            // panel band.  FullUpdate() wipes the whole canvas INCLUDING the
            // panel — so when the firmware owns the panel (panel_h>0) a
            // "full" content repaint must be a content-area partial instead,
            // otherwise the my-enhance native strip is erased every frame and
            // iv_update_panel cannot restore it before the next flush.  We
            // only issue a true FullUpdate() when there is no firmware panel
            // to preserve (panel_h==0, the self-drawn-strip case).
            let full = !mode.is_partial();
            if full && self.panel_h == 0 {
                FullUpdate();
            } else {
                PartialUpdate(r.x as i32, r.y as i32, r.w as i32, r.h as i32);
            }
        }
    }

    /// Panic-free no-op: inkview refreshes are immediate.
    fn mark_dirty(&mut self, _region: Rect) {}

    /// Poll inkview key events through the born-from-buffer event loop is not
    /// used; the C runtime routes events through `on_event`.
    fn poll_event(&mut self) -> Option<InputEvent> {
        None
    }
    fn wait_for_event(&mut self, _timeout_ms: u32) {}
    fn present(&mut self, mode: RefreshMode) {
        self.refresh(Rect { x: 0, y: 0, w: self.width, h: self.height.saturating_sub(self.panel_h) }, mode);
    }

    fn open_book(&mut self, path: &str, title: &str) -> bool {
        InkviewFb::open_book(self, path, title)
    }

    fn launch_app(&mut self, path: &str, name: &str, args: &[String]) -> bool {
        InkviewFb::launch_app(self, path, name, args)
    }
    fn open_keyboard(&mut self, title: &str, initial: &str, on_done: fn(&[u8])) {
        #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
        {
            // KBD_PASSEVENTS keeps pointer events flowing to the app while
            // the keyboard is up (the C app relies on this for the
            // suggestion-tap / outside-tap-dismiss handling).
            const KBD_PASSEVENTS: i32 = 0x8000;
            let t = std::ffi::CString::new(title).unwrap_or_default();
            KB.with(|k| {
                let mut g = k.borrow_mut();
                if g.is_none() {
                    let mut buf = vec![0u8; 260];
                    let n = initial.len().min(259);
                    buf[..n].copy_from_slice(&initial.as_bytes()[..n]);
                    *g = Some((buf, on_done));
                    let (b, _) = g.as_ref().unwrap();
                                        unsafe {
                        OpenKeyboard(t.as_ptr() as *const u8, b.as_ptr() as *mut i8, 260,
                                     KBD_PASSEVENTS, kb_commit_handler);
                    }
                }
            });
        }
        #[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
        {
            let _ = title;
            on_done(initial.as_bytes()); // host: no keyboard — cancel with initial value
        }
    }

    fn live_keyboard_text(&self) -> Option<String> {
        #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
        {
            KB.with(|k| {
                k.borrow().as_ref().and_then(|(buf, _)| {
                    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    std::str::from_utf8(&buf[..end])
                        .ok()
                        .map(|s| s.to_string())
                })
            })
        }
        #[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
        {
            None
        }
    }

    fn cancel_keyboard(&mut self) {
        #[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
        {
            // CloseKeyboard() fires the handler with the pre-edit text on
            // the firmware; dropping our KB entry first makes the handler
            // a no-op so the commit callback never runs (the C app's
            // suggestion-tap path commits the term itself afterwards).
            KB.with(|k| {
                if k.borrow_mut().take().is_some() {
                    unsafe { CloseKeyboard(); }
                }
            });
        }
        #[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
        {}
    }

    fn needs_self_panel(&self) -> bool {
        // C eh_plat_panel_height: no firmware panel painter (live device,
        // PanelHeight()==0) → the app draws the 106px strip itself.
        // PBEMU_SELF_PANEL forces it on for emulator testing.
        if std::env::var_os("PBEMU_SELF_PANEL").is_some() {
            return true;
        }
        self.panel_h == 0
    }

    /// C eh_plat_net_active: `(QueryNetwork() & 0xf00) != 0`.  The hal
    /// default `true` would make the device sync with WiFi off.
    fn net_active(&self) -> bool {
        // C eh_plat_net_active: the high byte carries the connection-state
        // flags; nonzero means an active connection.
        let q = unsafe { QueryNetwork() };
        q & 0xf00 != 0
    }

    /// C eh_plat_battery_power: `GetBatteryPower()` (percent; negative =
    /// unknown on this vintage).
    fn battery_level(&self) -> Option<u8> {
        let v = unsafe { GetBatteryPower() };
        if v < 0 { None } else { Some(v.min(100) as u8) }
    }

    /// Firmware frontlight probe: `GetFrontlightState()` (0..=100, negative
    /// = unreadable/unsupported) with the `GetFrontlightEnabled()` flag as
    /// the fallback when the state cannot be read.
    fn frontlight_on(&self) -> bool {
        let st = unsafe { GetFrontlightState() };
        if st > 0 {
            return true;
        }
        if st == 0 {
            return false;
        }
        unsafe { GetFrontlightEnabled() != 0 }
    }

    /// C BanSleep: the anti-suspend ban is re-armed until expiry.
    fn ban_sleep(&self, secs: u32) {
        unsafe { BanSleep(secs.min(i32::MAX as u32) as i32) }
    }

    /// C eh_plat_start_services verbatim: `iv_ipc_cmd(MSG_START_SERVICES,
    /// 0)` — the stock bookshelf's exact transport to monitor.app (the
    /// monitor socket lives inside libinkview's iv_ipc_cmd).  Without it a
    /// fresh boot runs only scanner + this app.
    fn start_services(&self) {
        const MSG_START_SERVICES: core::ffi::c_long = 0x600;
        unsafe { iv_ipc_cmd(MSG_START_SERVICES, 0) };
    }

    /// C OpenControlPanel(NULL): the system-bar tap action.
    fn open_control_panel(&self) {
        unsafe { OpenControlPanel(std::ptr::null_mut()) }
    }

    /// C eh_plat_device_profile probes (capability-based launcher
    /// conditionals: "1030" / "notouch" / "all" + has_audio).
    fn device_profile(&self) -> eh_hal::DeviceProfile {
        unsafe {
            eh_hal::DeviceProfile {
                device_number: device_number(),
                has_touchpanel: device_has_touchpanel(),
                has_audio: device_has_audio(),
            }
        }
    }

    /// C GetResource(name, NULL): the firmware theme store the stock
    /// launcher resolves its icons through before LoadPNG.
    fn theme_resource(&self, name: &str) -> Option<eh_hal::ThemeBitmap> {
        let c = std::ffi::CString::new(name).ok()?;
        unsafe { parse_ibitmap(GetResource(c.as_ptr() as *const u8, std::ptr::null())) }
    }

    /// C LoadPNG(name, 0): inkview's image loader — on modern firmware it
    /// also resolves bare theme names, so it is the C launcher's fallback
    /// when GetResource misses (C eh_launcher.c launcher_icon_get).
    fn load_png(&self, name: &str) -> Option<eh_hal::ThemeBitmap> {
        let c = std::ffi::CString::new(name).ok()?;
        unsafe { parse_ibitmap(LoadPNG(c.as_ptr() as *const u8, 0)) }
    }
}

/// Copy a firmware `ibitmap*` into an owned [`eh_hal::ThemeBitmap`] (the
/// pixel rows follow the 8-byte header, scanline-strided).
unsafe fn parse_ibitmap(bm: *mut IBitmapHdr) -> Option<eh_hal::ThemeBitmap> {
    if bm.is_null() {
        return None;
    }
    let hdr: IBitmapHdr = std::ptr::read(bm as *const IBitmapHdr);
    let len = hdr.scanline as usize * hdr.height as usize;
    let data = core::slice::from_raw_parts((bm as *const u8).add(core::mem::size_of::<IBitmapHdr>()), len).to_vec();
    Some(eh_hal::ThemeBitmap { width: hdr.width, height: hdr.height, depth: hdr.depth, scanline: hdr.scanline, data })
}


fn launch_app_impl(path: &str, name: &str, args: &[String]) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    // NewTaskEx may read argv asynchronously (the emulator task
    // system / launched app), so the C strings must outlive the call:
    // keep the owners in a static until the next launch (the C app
    // passes a heap argv it never frees).
    let mut holder = LAUNCH_ARG_OWN.lock().unwrap();
    let p = std::ffi::CString::new(path).unwrap_or_default();
    let b = std::ffi::CString::new(base).unwrap_or_default();
    let n = std::ffi::CString::new(name).unwrap_or_default();
    let cargs: Vec<std::ffi::CString> = args
        .iter()
        .map(|a| std::ffi::CString::new(a.as_str()).unwrap_or_default())
        .collect();
    let mut ptrs: Vec<*mut u8> = Vec::with_capacity(cargs.len() + 2);
    ptrs.push(p.as_ptr() as *mut u8);
    ptrs.extend(cargs.iter().map(|c| c.as_ptr() as *mut u8));
    ptrs.push(std::ptr::null_mut());
    let rc = unsafe {
        NewTaskEx(
            p.as_ptr() as *const u8,
            ptrs.as_ptr() as *mut *mut u8,
            b.as_ptr() as *const u8,
            n.as_ptr() as *const u8,
            std::ptr::null(),
            0xA5, // TASK_HIDDEN|NOUPDATEONFOCUS|SINGLEINSTANCE|OUTOFSTACK|MAKEACTIVE
            0,
        )
    };
    // Keep the buffers alive until the next launch.
    let mut owned = Vec::with_capacity(cargs.len() + 3);
    owned.push(p);
    owned.push(b);
    owned.push(n);
    owned.extend(cargs);
    *holder = Some(owned);
    rc == 0
}

/// Device model + firmware version for the boot identity log (C
/// `eh_plat_log_identity`; "?" when a probe returns nothing — the codename
/// is diagnostics only, conditionals resolve from device_profile).
pub fn device_identity() -> (String, String) {
    unsafe fn c_str(p: *mut u8) -> Option<String> {
        if p.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *p.add(len) != 0 {
            len += 1;
        }
        std::str::from_utf8(core::slice::from_raw_parts(p, len))
            .ok()
            .map(|s| s.to_string())
    }
    unsafe {
        (
            c_str(GetDeviceModel()).filter(|s| !s.is_empty()).unwrap_or_else(|| "?".into()),
            c_str(GetSoftwareVersion()).filter(|s| !s.is_empty()).unwrap_or_else(|| "?".into()),
        )
    }
}

impl InkviewFb {
    /// Establish the firmware panel (clock/battery) and refresh it.  The C
    /// app's eh_plat_panel_init does `DrawPanel(NULL,"EinkHome",NULL,-1);
    /// eh_stamp_panel(); Repaint();`.  We call this once at boot so the
    /// panel painter has content; afterwards content-area partials (present)
    /// never wipe it.  Safe no-op when the firmware owns no panel.
    pub fn panel_init(&mut self, title: &str) {
        if self.panel_h == 0 {
            return;
        }
        let c = std::ffi::CString::new(title).unwrap_or_default();
        let text: *const u8 = c.as_ptr() as *const u8;
        unsafe {
            DrawPanel(std::ptr::null(), text, std::ptr::null(), -1);
            iv_update_panel(0);
            Repaint();
        }
    }

    /// Open a downloaded book in the firmware reader (the stock bookshelf's
    /// canonical path: `OpenBook(path, NULL, 1)` routes through
    /// monitor.app/reader_controller).
    pub fn open_book(&mut self, path: &str, _title: &str) -> bool {
        let c = std::ffi::CString::new(path).unwrap_or_default();
        unsafe { OpenBook(c.as_ptr() as *const u8, std::ptr::null(), 1) == 0 }
    }

    pub fn launch_app(&mut self, path: &str, name: &str, args: &[String]) -> bool {
        launch_app_impl(path, name, args)
    }
}

/// Owners of the C strings passed to NewTaskEx (kept alive until the next
/// launch — the task system may read argv asynchronously).
static LAUNCH_ARG_OWN: std::sync::Mutex<Option<Vec<std::ffi::CString>>> = std::sync::Mutex::new(None);

// Keyboard commit state: the firmware's keyboard handler is a single
// global function pointer, so the in-flight (buffer, on_done) pair lives
// in a thread_local the static handler drains.
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
thread_local! {
    static KB: std::cell::RefCell<Option<(std::vec::Vec<u8>, fn(&[u8]))>> = const { std::cell::RefCell::new(None) };
}

#[allow(non_snake_case)]
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
extern "C" fn kb_commit_handler(buf: *mut i8) {
        if buf.is_null() {
        return;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(buf as *const u8) }.to_bytes().to_vec();
    KB.with(|k| {
        if let Some((_, f)) = k.borrow_mut().take() {
            f(&s);
        }
    });
}

/// Paint the native panel content (frontlight icon + battery), delegating to
/// the firmware's panel painter.  Safe no-op when the painter isn't active.
///
/// NOTE: requires `DrawPanel` which is arm-only (the firmware exports it, the
/// host SDL shim no-ops it).  Kept generic over the symbol so a host build
/// with a real inkview shim links; we currently call Partial/FullUpdate for
/// the strip and leave the painter to the firmware.
pub fn stamp_panel(_text: Option<&str>, _title: Option<&str>, _percent: i32) {}

/// Translate an inkview key/pointer event into a shell [`InputEvent`].
/// `par1/par2` semantics follow the SDK event codes.
pub fn evt_to_input(evt: i32, par1: i32, par2: i32) -> Option<InputEvent> {
    match evt {
        29 => Some(InputEvent::PointerUp { x: par1, y: par2 }),
        30 => Some(InputEvent::PointerDown { x: par1, y: par2 }),
        31 => Some(InputEvent::PointerMove { x: par1, y: par2 }),
        34 => Some(InputEvent::PointerLongPress { x: par1, y: par2 }),
        25 => Some(InputEvent::KeyDown { key: iv_to_key(par1) }),
        // EVT_INIT (21) and EVT_SHOW (23) both mean "screen is ready to draw".
        21 | 23 => Some(InputEvent::WidgetShown),
        _ => None,
    }
}

fn iv_to_key(code: i32) -> KeyCode {
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

/// A full surface over the inkview canvas for one draw pass (re-exported so
/// the shell can rasterise; the shell already builds it from `surface_mut`).
pub type IvSurface<'a> = Surface<'a>;
/// Arm an inkview weak timer (C SetWeakTimerEx).  `name` must be a
/// NUL-terminated static buffer kept alive for the timer's lifetime.
/// Public wrapper around the crate-internal SDK import.
pub fn arm_weak_timer(name: &'static std::ffi::CStr, handler: extern "C" fn(*mut std::ffi::c_void), ms: i32) {
    unsafe {
        SetWeakTimerEx(name.as_ptr() as *const u8, handler, std::ptr::null_mut(), ms);
    }
}
