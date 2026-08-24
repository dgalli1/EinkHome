//! ui — the Slint presentation bridge.
//!
//! Screens are Slint markup (main.slint + components); hit-testing is Slint
//! TouchAreas; painting goes through the software renderer straight into
//! the backend framebuffer (any [`eh_hal::PixelFormat`]).  The App keeps
//! owning every decision: Slint reports SEMANTIC intents (which button,
//! which tile, press vs release) via callbacks that push [`Action`]s onto
//! a queue the app drains right after dispatching input — no re-entrancy,
//! no shared ownership.
//!
//! The window is a [`MinimalSoftwareWindow`] driven entirely by the app's
//! event loop (no Slint runtime loop, no timers — the firmware keyboard
//! and long-press timing stay App-side, exactly as before).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ops::Range;
use std::rc::Rc;

use eh_hal::{Framebuffer, PixelFormat, Rect};

use slint::platform::software_renderer::{
    LineBufferProvider, MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
    SoftwareRenderer, TargetPixel,
};
use slint::platform::{Platform, WindowAdapter, WindowEvent};

use icons::Icons;

pub mod icons;

slint::include_modules!();

static FONT_REGULAR: &[u8] = include_bytes!("../../../../fonts/DejaVuSans.ttf");
static FONT_BOLD: &[u8] = include_bytes!("../../../../fonts/DejaVuSans-Bold.ttf");

// ── intent queue ────────────────────────────────────────────────────────

/// One semantic user intent reported by the Slint tree.  The App maps
/// these onto the same handlers the old coordinate hit-tests drove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Top-bar left button: back chevron (search/drilled) or house.
    Home,
    /// Top-bar source button.
    Source,
    /// Top-bar search icon.
    Search,
    /// Top-bar layout toggle.
    Layout,
    /// Top-bar sync icon.
    Sync,
    /// Top-bar hamburger.
    Menu,
    /// Pager button: 0=prev 1=first 2=last 3=next (the C -1/-3/-4/-2).
    Pager(usize),
    /// A shelf tile / list row was RELEASED (press started there).  The
    /// App classifies tap vs long-press from its own raw-event timing.
    TileRelease(usize),
    /// Tap in the self-drawn status strip band → firmware control panel.
    SystemBar,
    /// The search input row.
    SearchInput,
    /// A search history / suggestion row.
    SearchRow(usize),
    /// A folder-browser row (index is absolute: scroll + row).
    BrowseRow(usize),
}

thread_local! {
    static ACTIONS: RefCell<VecDeque<Action>> = const { RefCell::new(VecDeque::new()) };
}

/// Push one intent (called from Slint callbacks).
pub fn push_action(a: Action) {
    ACTIONS.with(|q| q.borrow_mut().push_back(a));
}

/// Drain every pending intent (the App applies them in order).
pub fn drain_actions() -> Vec<Action> {
    ACTIONS.with(|q| q.borrow_mut().drain(..).collect())
}

// ── gray / RGB pixel targets ────────────────────────────────────────────

/// 8bpp grayscale target pixel (Rec.601 luma of the blended color).
#[repr(transparent)]
#[derive(Clone, Copy, Default)]
struct Gray8(pub u8);

fn luma(r: u8, g: u8, b: u8) -> u32 {
    (r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000
}

impl TargetPixel for Gray8 {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let a = color.alpha as u32;
        let l = luma(color.red, color.green, color.blue);
        self.0 = ((self.0 as u32 * (255 - a)) / 255) as u8 + ((l * a) / 255) as u8;
    }
    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Gray8(luma(red, green, blue) as u8)
    }
}

/// 24bpp straight RGB target pixel.
#[repr(transparent)]
#[derive(Clone, Copy, Default)]
struct Rgb8s([u8; 3]);

impl TargetPixel for Rgb8s {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let a = color.alpha as u32;
        for (i, c) in [color.red, color.green, color.blue].into_iter().enumerate() {
            self.0[i] = ((self.0[i] as u32 * (255 - a)) / 255) as u8
                + ((c as u32 * a) / 255) as u8;
        }
    }
    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Rgb8s([red, green, blue])
    }
}

/// 32bpp straight RGBA target pixel (alpha pinned opaque — the canvas is
/// the panel, not a composited layer).
#[repr(transparent)]
#[derive(Clone, Copy, Default)]
struct Rgba8s([u8; 4]);

impl TargetPixel for Rgba8s {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let a = color.alpha as u32;
        for (i, c) in [color.red, color.green, color.blue].into_iter().enumerate() {
            self.0[i] = ((self.0[i] as u32 * (255 - a)) / 255) as u8
                + ((c as u32 * a) / 255) as u8;
        }
        self.0[3] = 0xff;
    }
    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Rgba8s([red, green, blue, 0xff])
    }
}

trait BytesPixel: TargetPixel {
    const BPP: usize;
}

impl BytesPixel for Gray8 {
    const BPP: usize = 1;
}
impl BytesPixel for Rgb8s {
    const BPP: usize = 3;
}
impl BytesPixel for Rgba8s {
    const BPP: usize = 4;
}

/// Renders one line of the software renderer straight into the backend's
/// byte buffer (handles any stride/padding: the line's pixel range is
/// mapped to `line*stride + range*BPP`).
struct FbLines<'a, P: BytesPixel> {
    bytes: &'a mut [u8],
    stride: usize,
    _pd: std::marker::PhantomData<P>,
}

impl<'a, P: BytesPixel> LineBufferProvider for FbLines<'a, P> {
    type TargetPixel = P;

    fn process_line(
        &mut self,
        line: usize,
        range: Range<usize>,
        render_fn: impl FnOnce(&mut [Self::TargetPixel]),
    ) {
        let bpp = P::BPP;
        let start = line * self.stride + range.start * bpp;
        let end = line * self.stride + range.end * bpp;
        let slice = &mut self.bytes[start..end];
        // Sound: P is a #[repr(transparent)] wrapper over a u8 array with
        // alignment 1, so every complete element of the byte slice is a
        // valid P and align_to_mut's middle slice covers it all.
        let (_, pixels, _) = unsafe { slice.align_to_mut::<P>() };
        render_fn(pixels);
    }
}

fn render_fb(
    renderer: &SoftwareRenderer,
    bytes: &mut [u8],
    stride: usize,
    fmt: PixelFormat,
) -> slint::platform::software_renderer::PhysicalRegion {
    match fmt {
        PixelFormat::Grayscale8 => renderer.render_by_line(FbLines::<Gray8> {
            bytes,
            stride,
            _pd: std::marker::PhantomData,
        }),
        PixelFormat::Rgb24 => renderer.render_by_line(FbLines::<Rgb8s> {
            bytes,
            stride,
            _pd: std::marker::PhantomData,
        }),
        PixelFormat::Rgba32 => renderer.render_by_line(FbLines::<Rgba8s> {
            bytes,
            stride,
            _pd: std::marker::PhantomData,
        }),
    }
}

fn region_to_rect(
    region: &slint::platform::software_renderer::PhysicalRegion,
    w: u32,
    h: u32,
) -> Rect {
    let o = region.bounding_box_origin();
    let s = region.bounding_box_size();
    let x0 = (o.x as u32).min(w);
    let y0 = (o.y as u32).min(h);
    Rect {
        x: x0,
        y: y0,
        w: (s.width as u32).min(w - x0),
        h: (s.height as u32).min(h - y0),
    }
}

// ── platform ────────────────────────────────────────────────────────────

thread_local! {
    static PLATFORM_SET: Cell<bool> = const { Cell::new(false) };
    static FONTS_SET: Cell<bool> = const { Cell::new(false) };
    static LAST_WINDOW: RefCell<Option<Rc<MinimalSoftwareWindow>>> =
        const { RefCell::new(None) };
}

struct EhPlatform;

impl Platform for EhPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::platform::PlatformError> {
        // ReusedBuffer: the renderer tracks what changed since the previous
        // frame — the dirty region that drives the e-ink partial refresh.
        let w = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        LAST_WINDOW.with(|s| *s.borrow_mut() = Some(w.clone()));
        Ok(w)
    }
}

// ── the handle the App owns ─────────────────────────────────────────────

pub struct Ui {
    window: Rc<MinimalSoftwareWindow>,
    comp: EhAppWindow,
    icons: Icons,
}

impl Ui {
    /// Install the platform (once per thread), create the window and wire
    /// every callback to the intent queue.
    pub fn new(width: u32, height: u32) -> Self {
        if !PLATFORM_SET.get() {
            slint::platform::set_platform(Box::new(EhPlatform))
                .expect("set slint platform");
            PLATFORM_SET.set(true);
        }
        let comp = EhAppWindow::new().expect("create EhAppWindow");
        let window = LAST_WINDOW
            .with(|s| s.borrow_mut().take())
            .expect("platform created no window");
        if !FONTS_SET.get() {
            window
                .renderer()
                .register_font_from_memory(FONT_REGULAR)
                .expect("register DejaVuSans");
            window
                .renderer()
                .register_font_from_memory(FONT_BOLD)
                .expect("register DejaVuSans-Bold");
            FONTS_SET.set(true);
        }
        comp.window().set_size(slint::PhysicalSize::new(width, height));

        comp.on_home(|| push_action(Action::Home));
        comp.on_source_btn(|| push_action(Action::Source));
        comp.on_search(|| push_action(Action::Search));
        comp.on_layout(|| push_action(Action::Layout));
        comp.on_sync(|| push_action(Action::Sync));
        comp.on_menu(|| push_action(Action::Menu));
        comp.on_pager(|a| push_action(Action::Pager(a as usize)));
        comp.on_tile(|i, down| {
            if !down {
                push_action(Action::TileRelease(i as usize));
            }
        });
        comp.on_system_bar(|| push_action(Action::SystemBar));
        comp.on_search_input(|| push_action(Action::SearchInput));
        comp.on_history_row(|i| push_action(Action::SearchRow(i as usize)));
        comp.on_browse_row(|i| push_action(Action::BrowseRow(i as usize)));

        let icons = icons::bake_all();
        comp.set_house_icon(icons.house.clone());
        comp.set_back_icon(icons.back.clone());
        comp.set_source_icon(icons.source_kavita.clone());
        comp.set_search_icon(icons.search.clone());
        comp.set_layout_grid_icon(icons.layout_grid.clone());
        comp.set_layout_list_icon(icons.layout_list.clone());
        comp.set_sync_icon(icons.sync.clone());
        comp.set_input_icon(icons.input.clone());
        comp.set_input_icon_inv(icons.input_inv.clone());
        comp.set_bulb_icon(icons.bulb.clone());

        Ui { window, comp, icons }
    }

    /// The generated component (property + callback access).
    pub fn comp(&self) -> &EhAppWindow {
        &self.comp
    }

    /// Live resolution switch (SDL F11): resize + relayout.
    pub fn set_size(&self, width: u32, height: u32) {
        self.comp.window().set_size(slint::PhysicalSize::new(width, height));
    }
    /// Dispatch one raw pointer event into the Slint tree (TouchAreas
    /// fire their callbacks synchronously, pushing intents).
    pub fn dispatch(&self, ev: WindowEvent) {
        self.comp.window().dispatch_event(ev);
    }

    /// Paint the current property state into the framebuffer.  Returns
    /// the dirty region (bounding box) when anything was painted, None
    /// when the frame was unchanged (the present-skip).  `full` forces a
    /// whole-window repaint (used when the canvas carries stale pixels,
    /// e.g. right after an overlay closed).
    pub fn render_full(&self, fb: &mut impl Framebuffer, full: bool) -> Option<Rect> {
        let fmt = fb.format();
        let stride = fb.stride();
        let scr = fb.screen();
        let mut out: Option<Rect> = None;
        self.window.draw_if_needed(|renderer| {
           if full {
               renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
           }
            let bytes = fb.surface_mut();
            let region = render_fb(renderer, bytes, stride, fmt);
           if full {
               renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
           }
            out = Some(region_to_rect(&region, scr.width, scr.height));
        });
        out
    }
    /// Source icon by active source (set at source switches).
    pub fn source_image(&self, source: crate::app::Source) -> slint::Image {
        match source {
            crate::app::Source::Kavita => self.icons.source_kavita.clone(),
            crate::app::Source::Local => self.icons.source_local.clone(),
            crate::app::Source::Folder => self.icons.source_folder.clone(),
        }
    }
}

// Re-export for the app's WindowEvent construction.
pub use slint::platform::PointerEventButton;

