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
        Self { width, height, content_bottom: height }
    }

    /// A screen with a reserved bottom strip (`panel_h` rows tall).
    pub fn with_panel(width: u32, height: u32, panel_h: u32) -> Self {
        Self {
            width,
            height,
            content_bottom: if panel_h > height { height } else { height - panel_h },
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
        let x1 = self.x.saturating_add(self.w).min(bounds.x.saturating_add(bounds.w));
        let y1 = self.y.saturating_add(self.h).min(bounds.y.saturating_add(bounds.h));
        Rect { x: x0, y: y0, w: x1.saturating_sub(x0), h: y1.saturating_sub(y0) }
    }

    /// True when `(x, y)` (surface pixels) falls inside the rect.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32 && x < (self.x + self.w) as i32 && y >= self.y as i32 && y < (self.y + self.h) as i32
    }
}

/// Pointer/button co-ordinates in surface pixels (origin top-left, matching
/// the drawing surface).  Backends translate OS co-ordinates into this space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    PointerDown { x: i32, y: i32 },
    PointerMove { x: i32, y: i32 },
    PointerUp { x: i32, y: i32 },
    PointerLongPress { x: i32, y: i32 },
    KeyDown { key: KeyCode },
    KeyUp { key: KeyCode },
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

/// Result of one event-loop iteration handed back to the backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopState {
    /// Keep running.
    Continue,
    /// Tear down the event loop and return from the backend.
    Exit,
}

/// The platform backend a UI runs on.  Implemented by eh_backend_* crates;
/// the shell ([`crate::err::Err`] plumbing in eh_shell) is generic over it.
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
    /// next [`present`](crate::frame::Frame::present).  Refreshes collapse
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
    /// Default: not available on this platform.
    fn launch_app(&mut self, _path: &str, _name: &str) -> bool {
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
}