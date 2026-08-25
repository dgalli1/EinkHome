//! eh_hal — the platform contract for the EinkHome GUI toolkit.
//!
//! This is the seam that mirrors KOReader's device abstraction: the whole UI
//! (and every future device backend) speaks to *this* interface only.  A
//! backend supplies a [`framebuffer`](crate::Framebuffer) (canvas + region
//! refresh with an e-ink waveform mode) and an input event source; everything
//! above — layout, widgets, hit-testing — is platform-independent.

#![no_std]

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

/// Physical pixel geometry of a display.
///
/// On PocketBook the firmware's type-1 system panel occupies the bottom strip
/// `[content_bottom, height)` (clock/battery/wifi).  `height - content_bottom`
/// is that reserved region: the app must never draw into it, and a
/// [`Framebuffer`] must clip refreshes to the content area when a native
/// panel painter is active.  Device backends that own the whole panel set
/// `content_bottom == height`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub width: u32,
    pub height: u32,
    /// First row that belongs to the (possibly firmware-owned) status strip.
    /// All app content lives in `[0, content_bottom)`.
    pub content_bottom: u32,
}

impl Screen {
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            content_bottom: height,
        }
    }

    pub fn content_height(&self) -> u32 {
        self.content_bottom
    }
}

/// Pixel format of the off-screen surface the UI renders into.
///
/// Device backends pick the format their panel wants; the renderer writes into
/// whatever surface the backend hands it.  Grayscale8 is the common e-ink
/// monotone panel; Rgb24 is the Kaleido colour canvas (via the framebuffer
/// bypass the current C app already uses through `GetCanvas`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// One byte per pixel; palette is a linear brightness ramp (inkview's 8bpp).
    Grayscale8,
    /// Triple of bytes per pixel (red, green, blue), row padded to 4 bytes.
    /// Matches `icanvas` on a Kaleido panel.
    Rgb24,
    /// 32-bit RGBA, for desktop/emulator surfaces (SDL, pbemu host).
    Rgba32,
}

impl PixelFormat {
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Grayscale8 => 1,
            PixelFormat::Rgb24 => 3,
            PixelFormat::Rgba32 => 4,
        }
    }
}

/// An e-ink waveform / refresh mode, mapped *by the backend* to the panel's
/// native request (mxcfb `UPDATE_MODE`, sunxi `EPD_FULL_GC16`, etc.).  The UI
/// layer asks for intent, not a concrete ioctl constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshMode {
    /// Low-latency A2-style update for transient feedback (cursor blink).
    Fast,
    /// Normal partial update; does not fully clear ghosting.
    Partial,
    /// Full refresh that clears ghosting (page flips, big changes).
    Full,
    /// Highest-quality full refresh (image redraw / anti-ghost), slower.
    FullHq,
}

impl RefreshMode {
    pub fn is_partial(self) -> bool {
        self == RefreshMode::Fast || self == RefreshMode::Partial
    }
}

/// A rectangle in surface coordinates (already clamped to the screen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn from_xy(x: i32, y: i32, w: i32, h: i32) -> Self {
        let x = x.max(0) as u32;
        let y = y.max(0) as u32;
        let w = w.max(0) as u32;
        let h = h.max(0) as u32;
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Intersect with `bounds`, clamping both edges.
    pub fn intersect(&self, bounds: &Rect) -> Rect {
        let x0 = self.x.max(bounds.x);
        let y0 = self.y.max(bounds.y);
        let x1 = self
            .x
            .saturating_add(self.w)
            .min(bounds.x.saturating_add(bounds.w));
        let y1 = self
            .y
            .saturating_add(self.h)
            .min(bounds.y.saturating_add(bounds.h));
        Rect {
            x: x0,
            y: y0,
            w: x1.saturating_sub(x0),
            h: y1.saturating_sub(y0),
        }
    }

    /// True when `(x, y)` (surface pixels) falls inside the rect.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32
            && x < (self.x + self.w) as i32
            && y >= self.y as i32
            && y < (self.y + self.h) as i32
    }
}

/// Pointer/button co-ordinates in surface pixels (origin top-left, matching
/// the drawing surface).  Backends translate OS co-ordinates into this space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    PointerDown {
        x: i32,
        y: i32,
    },
    PointerMove {
        x: i32,
        y: i32,
    },
    PointerUp {
        x: i32,
        y: i32,
    },
    PointerLongPress {
        x: i32,
        y: i32,
    },
    KeyDown {
        key: KeyCode,
    },
    KeyUp {
        key: KeyCode,
    },
    /// App is being shown / brought to foreground (used to (re)draw).
    WidgetShown,
    WidgetHidden,
    /// OS/vendor lifecycle message not otherwise mapped.
    Lifecycle(u32),
}

/// Key codes normalised across devices (KOReader's `key.lua` equivalent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Menu,
    Home,
    NextPage,
    PrevPage,
    Plus,
    Minus,
    Unknown(u32),
}

/// The platform backend a UI runs on.  Implemented by eh_backend_* crates;
/// the shell (whose `Err` plumbing lives in eh_shell) is generic over it.
pub trait Framebuffer {
    /// The logical screen geometry the renderer lays out against.
    fn screen(&self) -> Screen;

    /// Pixel format of the surface [`surface_mut`](Self::surface_mut) writes.
    fn format(&self) -> PixelFormat;

    /// Underlying byte buffer the renderer draws into.  Row `y` starts at
    /// `y * stride()`; `stride()` may exceed `width * bpp`.
    fn surface_mut(&mut self) -> &mut [u8];

    /// Number of bytes per row including any padding.
    fn stride(&self) -> usize;

    /// Present a dirty region to the physical panel with the requested
    /// waveform mode.  Clipped to the content area by the caller.
    fn refresh(&mut self, region: Rect, mode: RefreshMode);

    /// Reserve a dirty region accumulated across draw calls; cleared on the
    /// next `present()` call.  Refreshes collapse
    /// what was actually drawn.
    fn mark_dirty(&mut self, region: Rect);

    /// Poll one input event, or `None` if none is pending.
    fn poll_event(&mut self) -> Option<InputEvent>;

    /// Block until there is something to do (event, timer, or repaint
    /// request); used by the shell to avoid busy-looping.
    fn wait_for_event(&mut self, timeout_ms: u32);

    /// Called by the shell once per frame after all draws, so the backend can
    /// flush its queued dirty regions to the panel.
    fn present(&mut self, mode: RefreshMode);

    /// Open a book file in the platform reader (the firmware's canonical
    /// open path).  Default: not available on this platform.
    fn open_book(&mut self, _path: &str, _title: &str) -> bool {
        false
    }

    /// Launch another on-device application (the launcher's action).
    /// `args` are the item's launch parameters (C NewTaskEx passes the
    /// argv array through as-is: argv\[0\] is the app path).  Default: not
    /// available on this platform.
    fn launch_app(&mut self, _path: &str, _name: &str, _args: &[String]) -> bool {
        false
    }

    /// Open the platform text keyboard, preloaded with `initial`.  On commit
    /// (or cancel) `on_done` receives the buffer contents; on a backend
    /// without a keyboard it is called immediately with `initial`.
    fn open_keyboard(&mut self, _title: &str, initial: &str, on_done: fn(&[u8])) {
        on_done(initial.as_bytes());
    }

    /// The live keyboard buffer while a keyboard is open (the C app polls
    /// `eh_g_search_kb_buf` on its 200 ms suggest tick — the firmware's
    /// change callback never fires).  `None` when no keyboard is open or
    /// the platform cannot expose the buffer.
    fn live_keyboard_text(&self) -> Option<String> {
        None
    }

    /// True when this backend renders NO keyboard UI for
    /// [`Framebuffer::open_keyboard`]: the app must draw its own
    /// on-screen keyboard (PC hosts).  The firmware platforms show their
    /// own keyboard, so the default is false.
    fn needs_app_keyboard(&self) -> bool {
        false
    }

    /// Append text to the open keyboard buffer (the IPC "type" path and
    /// the app-side on-screen keyboard).  No-op when closed.
    fn kb_type_text(&mut self, _text: &str) {}

    /// Pop one UTF-8 scalar from the open keyboard buffer.
    fn kb_backspace(&mut self) {}

    /// Commit the open keyboard exactly like a physical RETURN: close it
    /// and fire the app's `on_done` with the buffer.
    fn kb_commit(&mut self) {}

    /// True when the device has an ACTIVE network connection (the C
    /// `eh_plat_net_active`).  The boot auto-sync and remote cover
    /// fetches are gated on this: an offline launch renders the cached
    /// library instead of nagging / stalling on the network.
    fn net_active(&self) -> bool {
        true
    }

    /// Whether the app must draw its own status strip: true on devices
    /// where the firmware panel painter is absent (C
    /// `eh_plat_panel_height`'s `*self_panel`), false on the SDL/PC build
    /// and where the firmware owns the panel band.
    fn needs_self_panel(&self) -> bool {
        false
    }

    /// Close an open keyboard WITHOUT firing the commit callback (the C
    /// `CloseKeyboard()`: the handler receives the pre-edit text — used by
    /// the suggestion-tap path, where the app commits the tapped term
    /// itself after the keyboard is gone).
    fn cancel_keyboard(&mut self) {}

    /// Battery charge in percent (0..=100), or `None` when the platform
    /// cannot report it (C `eh_plat_battery_power` → `GetBatteryPower`).
    fn battery_level(&self) -> Option<u8> {
        None
    }

    /// True when the frontlight is lit (firmware probe; feeds the self-drawn
    /// status strip's bulb glyph).
    fn frontlight_on(&self) -> bool {
        false
    }

    /// Ban auto-suspend for the next `secs` seconds (C `BanSleep(sec)`:
    /// the ban is re-armed until expiry — used so the device cannot sleep
    /// mid-sync).
    fn ban_sleep(&self, _secs: u32) {}

    /// Ask monitor.app to start the resident firmware services (the stock
    /// bookshelf sends MSG_START_SERVICES over iv_ipc_cmd during init;
    /// without it a fresh boot runs only scanner + this app).
    fn start_services(&self) {}

    /// Open the firmware control panel / task manager (C
    /// `OpenControlPanel(NULL)`: the system-bar tap action).
    fn open_control_panel(&self) {}

    /// Probe the device capability profile (launcher conditional resolution).
    fn device_profile(&self) -> DeviceProfile {
        DeviceProfile::default()
    }

    /// Resolve a named firmware theme bitmap (C `GetResource(name, NULL)`).
    /// `None` when the name is unknown or the platform has no theme store.
    fn theme_resource(&self, _name: &str) -> Option<ThemeBitmap> {
        None
    }

    /// Load an image through the firmware loader (C `LoadPNG(name, 0)`) —
    /// on modern firmware this also resolves bare theme names, so it is
    /// the C launcher's fallback when GetResource misses.
    fn load_png(&self, _name: &str) -> Option<ThemeBitmap> {
        None
    }
}

/// Device capability profile (the C `eh_plat_device_profile` probes).  Raw
/// firmware identity used for launcher conditional resolution; neutral
/// defaults match every conditional ("all").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceProfile {
    /// Firmware device number (`device_number()`; e.g. 1030 = PB Enotes).
    pub device_number: u32,
    pub has_touchpanel: bool,
    pub has_audio: bool,
}

/// A firmware theme bitmap resolved by [`Framebuffer::theme_resource`] (the
/// C `GetResource` seam the stock launcher resolves its icons through).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeBitmap {
    pub width: u16,
    pub height: u16,
    /// Bits per pixel as the firmware reports it: 8 = paletted grayscale
    /// rows (inkview's standard gray palette), 32 = ARGB rows.
    pub depth: u16,
    /// Bytes per row (may exceed `width * depth / 8`).
    pub scanline: u16,
    /// Raw scanline-major pixel data (`scanline * height` bytes).
    pub data: Vec<u8>,
}

impl ThemeBitmap {
    /// Expand the raw bitmap into tightly-packed RGB triples (`width *
    /// height * 3`), the renderer's `blit_image` input.  Depth 8: each byte
    /// IS the gray level (inkview's standard 8bpp bitmaps index a grayscale
    /// palette).  Depth 32: rows are little-endian 0xAARRGGBB (bytes b, g,
    /// r, a — the LoadPNG layout).  Other depths → `None`.
    pub fn to_rgb(&self) -> Option<Vec<u8>> {
        let w = self.width as usize;
        let h = self.height as usize;
        let sl = self.scanline as usize;
        match self.depth {
            8 => {
                let mut out = Vec::with_capacity(w * h * 3);
                for y in 0..h {
                    for x in 0..w {
                        let g = self.data.get(y * sl + x).copied().unwrap_or(255);
                        out.extend_from_slice(&[g, g, g]);
                    }
                }
                Some(out)
            }
            32 => {
                let mut out = Vec::with_capacity(w * h * 3);
                for y in 0..h {
                    for x in 0..w {
                        let o = y * sl + x * 4;
                        let (b, g, r) = (
                            *self.data.get(o)?,
                            *self.data.get(o + 1)?,
                            *self.data.get(o + 2)?,
                        );
                        out.extend_from_slice(&[r, g, b]);
                    }
                }
                Some(out)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    // ── Rect: the geometry every hit-test and dirty-region clip routes
    // through.  An off-by-one here misroutes taps or drops edge rows
    // stack-wide, so the boundary semantics are pinned explicitly.

    #[test]
    fn rect_from_xy_clamps_negatives_to_zero() {
        assert_eq!(
            Rect::from_xy(-10, -5, 30, 20),
            Rect {
                x: 0,
                y: 0,
                w: 30,
                h: 20
            }
        );
        // Negative extents collapse to empty (u32 cast of the max).
        assert_eq!(
            Rect::from_xy(5, 5, -1, -2),
            Rect {
                x: 5,
                y: 5,
                w: 0,
                h: 0
            }
        );
    }

    #[test]
    fn rect_is_empty_on_zero_extent() {
        assert!(Rect::from_xy(0, 0, 0, 10).is_empty());
        assert!(Rect::from_xy(0, 0, 10, 0).is_empty());
        assert!(!Rect::from_xy(0, 0, 1, 1).is_empty());
    }

    #[test]
    fn rect_intersect_clamps_both_edges() {
        let a = Rect::from_xy(10, 10, 50, 40);
        let b = Rect::from_xy(30, 0, 100, 25);
        assert_eq!(a.intersect(&b), Rect::from_xy(30, 10, 30, 15));
        // Intersection is symmetric.
        assert_eq!(b.intersect(&a), a.intersect(&b));
    }

    #[test]
    fn rect_intersect_disjoint_is_empty_not_wrapped() {
        let a = Rect::from_xy(0, 0, 10, 10);
        let b = Rect::from_xy(20, 20, 5, 5);
        let x = a.intersect(&b);
        assert_eq!(
            x,
            Rect {
                x: 20,
                y: 20,
                w: 0,
                h: 0
            }
        );
        assert!(x.is_empty());
    }

    #[test]
    fn rect_intersect_touching_edges_meet_at_a_line() {
        let a = Rect::from_xy(0, 0, 10, 10);
        let b = Rect::from_xy(10, 0, 10, 10);
        assert_eq!(
            a.intersect(&b),
            Rect {
                x: 10,
                y: 0,
                w: 0,
                h: 10
            }
        );
    }

    #[test]
    fn rect_contains_is_inclusive_low_exclusive_high() {
        let r = Rect {
            x: 10,
            y: 20,
            w: 30,
            h: 40,
        };
        assert!(r.contains(10, 20), "low corner inside");
        assert!(r.contains(39, 59), "last pixel inside");
        assert!(!r.contains(40, 20), "x+w exclusive");
        assert!(!r.contains(10, 60), "y+h exclusive");
        assert!(!r.contains(9, 20), "left of");
        assert!(!r.contains(10, 19), "above");
    }

    // ── Screen / PixelFormat / RefreshMode basics.

    #[test]
    fn screen_full_owns_whole_panel() {
        let s = Screen::full(1264, 1680);
        assert_eq!((s.width, s.height), (1264, 1680));
        assert_eq!(s.content_bottom, 1680, "no firmware panel reserved");
        assert_eq!(s.content_height(), 1680);
    }

    #[test]
    fn pixel_format_byte_widths() {
        assert_eq!(PixelFormat::Grayscale8.bytes_per_pixel(), 1);
        assert_eq!(PixelFormat::Rgb24.bytes_per_pixel(), 3);
        assert_eq!(PixelFormat::Rgba32.bytes_per_pixel(), 4);
    }

    #[test]
    fn refresh_mode_partial_covers_fast_and_partial() {
        assert!(RefreshMode::Fast.is_partial());
        assert!(RefreshMode::Partial.is_partial());
        assert!(!RefreshMode::Full.is_partial());
        assert!(!RefreshMode::FullHq.is_partial());
    }

    // ── ThemeBitmap::to_rgb: expands UNTRUSTED firmware bitmaps; the
    // bounds-checked fallbacks (short row → white at depth 8, None at
    // depth 32) are part of the contract.

    #[test]
    fn theme_bitmap_depth8_grays_with_scanline_padding() {
        // width 2, scanline 4 → one padding byte per row is skipped.
        let bm = ThemeBitmap {
            width: 2,
            height: 2,
            depth: 8,
            scanline: 4,
            data: vec![0x00, 0x80, 0xEE, 0xEE, 0xFF, 0x7F, 0xEE, 0xEE],
        };
        assert_eq!(
            bm.to_rgb(),
            Some(vec![
                0, 0, 0, 0x80, 0x80, 0x80, 0xFF, 0xFF, 0xFF, 0x7F, 0x7F, 0x7F
            ])
        );
    }

    #[test]
    fn theme_bitmap_depth8_short_row_falls_back_to_white() {
        let bm = ThemeBitmap {
            width: 2,
            height: 1,
            depth: 8,
            scanline: 2,
            data: vec![0x10], // second pixel beyond the buffer
        };
        assert_eq!(bm.to_rgb(), Some(vec![0x10, 0x10, 0x10, 255, 255, 255]));
    }

    #[test]
    fn theme_bitmap_depth32_argb_rows_to_rgb() {
        // Little-endian 0xAARRGGBB: bytes are b, g, r, a.
        let px = |b: u8, g: u8, r: u8, a: u8| [b, g, r, a];
        let bm = ThemeBitmap {
            width: 2,
            height: 1,
            depth: 32,
            scanline: 8,
            data: [px(0x11, 0x22, 0x33, 0xFF), px(0x44, 0x55, 0x66, 0x80)].concat(),
        };
        assert_eq!(bm.to_rgb(), Some(vec![0x33, 0x22, 0x11, 0x66, 0x55, 0x44]));
    }

    #[test]
    fn theme_bitmap_depth32_truncated_row_is_none() {
        let bm = ThemeBitmap {
            width: 2,
            height: 1,
            depth: 32,
            scanline: 8,
            data: vec![0, 0, 0, 0, 0, 0], // second pixel's blue missing
        };
        assert_eq!(bm.to_rgb(), None);
    }

    #[test]
    fn theme_bitmap_unknown_depth_is_none() {
        let bm = ThemeBitmap {
            width: 1,
            height: 1,
            depth: 16,
            scanline: 2,
            data: vec![0, 0],
        };
        assert_eq!(bm.to_rgb(), None);
    }
}
