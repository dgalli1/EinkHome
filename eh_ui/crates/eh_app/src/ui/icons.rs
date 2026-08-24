//! Icon baking: the top-bar / input line-art glyphs are rasterised ONCE at
//! boot through the same `eh_render` draw code the previous per-frame
//! renderer used, into RGBA buffers handed to Slint as images.  This keeps
//! the icon pixels identical to the old renderer without re-drawing them
//! as vector paths.

use eh_hal::PixelFormat;
use eh_render::{Glyph, Surface};
use eh_shell::DrawCtx;
use slint::Image;

use crate::app::{Source, ViewMode};
use crate::appui::{
    circle_outline, draw_back_chevron, draw_book_icon, draw_folder_icon, draw_globe_icon,
    draw_house, draw_layout_icon, draw_search_icon, draw_sync_icon,
};

/// Bake one icon: `size` (w, h) RGBA tile, `draw` paints with a DrawCtx
/// whose surface covers exactly the tile.  `white_bg` false starts from a
/// black tile (the inverted input magnifier).
fn bake(size: (u32, u32), white_bg: bool, draw: impl FnOnce(&mut DrawCtx)) -> Image {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut buf = vec![0xff_u8; w * h * 4];
    if !white_bg {
        for px in buf.chunks_exact_mut(4) {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        }
    }
    let mut glyph = Glyph::new();
    {
        let font = crate::shelf::load_font();
        let bold = eh_shell::bold_font();
        let mut surf = Surface::new(&mut buf, w as u32, h as u32, w * 4, PixelFormat::Rgba32);
        let mut dirty = Vec::new();
        let mut ctx = DrawCtx {
            surf: &mut surf,
            font,
            bold,
            glyph: &mut glyph,
            dirty: &mut dirty,
        };
        draw(&mut ctx);
    }
    Image::from_rgba8(slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        &buf,
        w as u32,
        h as u32,
    ))
}

/// All baked icons for one boot.
pub struct Icons {
    pub house: Image,
    pub back: Image,
    pub source_kavita: Image,
    pub source_local: Image,
    pub source_folder: Image,
    pub search: Image,
    pub layout_grid: Image,
    pub layout_list: Image,
    pub sync: Image,
    pub input: Image,
    pub input_inv: Image,
    pub bulb: Image,
}

/// Bake the full icon set (a few ms at boot).
pub fn bake_all() -> Icons {
    let house = bake((96, 96), true, |ctx| draw_house(ctx, 48, 48, 0));
    let back = bake((96, 96), true, |ctx| draw_back_chevron(ctx, 48, 48, 0));
    let source_kavita = bake((52, 52), true, |ctx| draw_globe_icon(ctx, 0, 0, 0));
    let source_local = bake((52, 52), true, |ctx| draw_book_icon(ctx, 0, 0, 0));
    let source_folder = bake((52, 52), true, |ctx| draw_folder_icon(ctx, 0, 0, 0));
    let search = bake((96, 96), true, |ctx| draw_search_icon(ctx, 48, 48, 0));
    let layout_grid = bake((96, 96), true, |ctx| {
        draw_layout_icon(ctx, 48, 48, ViewMode::Grid, 0)
    });
    let layout_list = bake((96, 96), true, |ctx| {
        draw_layout_icon(ctx, 48, 48, ViewMode::List, 0)
    });
    let sync = bake((96, 96), true, |ctx| draw_sync_icon(ctx, 48, 48, 0, 0));
    // Search-input magnifier: ring centre at (30, 34) in a 60x60 tile
    // (C: circle at bx+30, by+bh/2 of the 68px box).
    let input = bake((60, 60), true, |ctx| {
        circle_outline(ctx, 30, 34, 13, 0);
        ctx.line(39, 44, 52, 57, 2, 0);
        ctx.line(40, 43, 53, 56, 2, 0);
    });
    let input_inv = bake((60, 60), false, |ctx| {
        circle_outline(ctx, 30, 34, 13, 0xff);
        ctx.line(39, 44, 52, 57, 2, 0xff);
        ctx.line(40, 43, 53, 56, 2, 0xff);
    });
    // Frontlight bulb: circle r12 + 8 rays to r22, centred.
    let bulb = bake((48, 48), true, |ctx| {
        circle_outline(ctx, 24, 24, 12, 0);
        for a in 0..8u32 {
            let ang = a as f64 * core::f64::consts::PI / 4.0 + core::f64::consts::PI / 8.0;
            ctx.line(
                24 + (16.0 * ang.cos()) as i32,
                24 + (16.0 * ang.sin()) as i32,
                24 + (22.0 * ang.cos()) as i32,
                24 + (22.0 * ang.sin()) as i32,
                2,
                0,
            );
        }
    });
    let _ = Source::Kavita;
    Icons {
        house,
        back,
        source_kavita,
        source_local,
        source_folder,
        search,
        layout_grid,
        layout_list,
        sync,
        input,
        input_inv,
        bulb,
    }
}
