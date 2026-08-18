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

// The firmware symbols only exist on-arm; host builds stub them so the crate
// still compiles (the SDL/linuxfb backends are the host path anyway).
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
#[allow(non_snake_case)]
mod imp {
    use super::ICanvas;
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
    }
}

// Re-expose the imports uniformly.
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
use imp::{DrawPanel, FullUpdate, GetCanvas, InitInkview, InkViewMain, PanelHeight, PartialUpdate, Repaint, ScreenHeight, ScreenWidth, iv_update_panel};
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
use imp::{DrawPanel, FullUpdate, GetCanvas, InitInkview, InkViewMain, PanelHeight, PartialUpdate, Repaint, ScreenHeight, ScreenWidth, iv_update_panel};

/// Boot the inkview library exactly like the stock bookshelf: register, then
/// hand the event loop a callback.  `on_event` receives raw (evt, par1, par2)
/// and returns the RES_* result code.
pub fn iv_main(on_event: extern "C" fn(i32, i32, i32) -> i32) -> ! {
    unsafe {
        InitInkview(0x4110);
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