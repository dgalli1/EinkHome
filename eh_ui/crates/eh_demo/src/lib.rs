//! Shared demo shelf screen, portable across backends.
//!
//! Renders a responsive cover grid: the column count follows the screen
//! width breakpoint (2 / 3 / 4 columns like the C app), computed once per
//! frame and fed to taffy as per-item flex basis.  The same [`Screen`] drives
//! the SDL host backend and the linuxfb/inkview device backends.

use eh_hal::Framebuffer;
use eh_layout::{Breakpoint, Style};
use eh_render::Font;
use eh_shell::{Cover, Screen, GRAY_BLACK, GRAY_WHITE};

/// The font embedded for the demo (a permissive OFL font; the real device
/// loads the firmware's system TTF, same as the C app).
pub static FONT: &[u8] = include_bytes!("../../../fonts/DejaVuSans.ttf");

/// Cover tiles per page.
pub const BOOKS: usize = 12;

/// Column count for the active breakpoint (the C app's `eh_view_cols` as a
/// data-driven value instead of inline `if`s).
pub fn columns_for(bp: Breakpoint) -> u32 {
    match bp {
        Breakpoint::Narrow => 2,
        Breakpoint::Std => 3,
        Breakpoint::Wide => 4,
    }
}

/// Build the screen: only the cover grid for this first slice.  Each cover is
/// a flex item sized `100/cols %` wide with a 2:3 aspect, so taffy wraps them
/// into the requested column count.  `present()` then draws each at its
/// computed rect and flushes the dirty region — hit-testing uses the same
/// rects.
pub fn build_screen<B: Framebuffer>(fb: B) -> Screen<B> {
    let font = load_font();
    let mut screen = Screen::new(fb, font);

    // The shelf grid: wrap so covers flow into rows at the breakpoint's
    // column count (like CSS flex-wrap inside the app's content area).
    screen.layout_mut().root_flex_wrap();

    // Re-derive columns each build (the shell recomputes breakpoint+layout
    // on every present; column basis is set here from the initial width).
    let bp = screen.breakpoint;
    build_covers(&mut screen, bp);

    screen
}

fn build_covers<B: Framebuffer>(screen: &mut Screen<B>, bp: Breakpoint) {
    use eh_layout::taffy;
    let cols = columns_for(bp);
    // Fixed 2:3 aspect tile height so taffy sizes the wrap rows correctly;
    // width is `100/cols %` with the active breakpoint.
    let row_h = 300.0f32;
    for i in 0..BOOKS {
        let mut c = Cover::new(format!("Book {}", i + 1));
        c.author = format!("Author {}", (i * 7) % 13);
        c.title_size = 20.0;
        let style = Style {
            size: taffy::geometry::Size {
                width: taffy::Dimension::percent(1.0 / cols as f32),
                height: taffy::Dimension::length(row_h),
            },
            ..Style::default()
        };
        screen.add_styled(Box::new(c), style);
    }
}

fn load_font() -> &'static Font {
    static FACE: std::sync::LazyLock<Font> =
        std::sync::LazyLock::new(|| Font::from_bytes(FONT).expect("embed font"));
    &FACE
}

/// Draw the self-owned status strip (clock + battery).  Portable replacement
/// for the C app's `eh_draw_system_strip`; only invoked when the firmware
/// panel painter isn't active (live device).
pub fn draw_self_panel<B: Framebuffer>(fb: &mut B) {
    let s = fb.screen();
    let y0 = s.content_height();
    let h = s.height - y0;
    if h == 0 {
        return;
    }
    let fmt = fb.format();
    let bpp = fmt.bytes_per_pixel() as u32;
    let stride = fb.stride() as u32;
    {
        let mut surf = eh_render::Surface::new(fb.surface_mut(), s.width, s.height, stride as usize, fmt);
        let mut glyph = eh_render::Glyph::new();
        let font = load_font();
        surf.fill_gray(eh_hal::Rect { x: 0, y: y0, w: s.width, h }, GRAY_WHITE);
        surf.hline(0, y0, s.width, 2, GRAY_BLACK);
        // clock text vertically centred
        let top = y0 + h / 2;
        eh_render::draw_text(&mut surf, font, 40.0, "Sun 12:00", 24, top as i32 - 12, GRAY_BLACK, &mut glyph);
        // battery
        let bw = 84u32;
        let bh = 40u32;
        let bx = s.width - 116;
        let by = y0 + (h - bh) / 2;
        surf.rect_outline(eh_hal::Rect { x: bx, y: by, w: bw, h: bh }, 3, GRAY_BLACK);
        surf.fill_gray(eh_hal::Rect { x: bx + 4, y: by + 4, w: (bw - 8) / 2, h: bh - 8 }, GRAY_BLACK);
        let _ = bpp;
        let _ = glyph;
    }
}