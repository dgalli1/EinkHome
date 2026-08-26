//! Icon baking: the top-bar / input line-art glyphs are rasterised ONCE at
//! boot through the same `eh_render` draw code the previous per-frame
//! renderer used, into RGBA buffers handed to Slint as images.  This keeps
//! the icon pixels identical to the old renderer without re-drawing them
//! as vector paths.

use eh_hal::PixelFormat;
use eh_render::{Glyph, Surface};

use eh_render::DrawCtx;
use slint::Image;

use crate::app::ViewMode;
use crate::appui::{
    circle_outline, draw_back_chevron, draw_book_icon, draw_folder_icon, draw_globe_icon,
    draw_house, draw_layout_icon, draw_search_icon, draw_sync_icon,
};

/// Paint one icon tile into a fresh RGBA buffer: `size` (w, h), `draw`
/// paints with a DrawCtx whose surface covers exactly the tile.
/// `white_bg` false starts from a black tile (the inverted input magnifier).
fn paint(size: (u32, u32), white_bg: bool, draw: impl FnOnce(&mut DrawCtx)) -> Vec<u8> {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut buf = vec![0xff_u8; w * h * 4];
    if !white_bg {
        for px in buf.as_chunks_mut::<4>().0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        }
    }
    let mut glyph = Glyph::new();
    {
        let font = crate::shelf::load_font();
        let bold = crate::shelf::load_bold_font();
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
    buf
}

/// Wrap a painted RGBA buffer into a Slint image.
fn tile(buf: &[u8], size: (u32, u32)) -> Image {
    Image::from_rgba8(
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(buf, size.0, size.1),
    )
}

/// Bake one icon (+ its color-inverted twin for press feedback).
fn pair(size: (u32, u32), draw: impl FnOnce(&mut DrawCtx)) -> (Image, Image) {
    let buf = paint(size, true, draw);
    let mut inv = buf.clone();
    invert(&mut inv);
    (tile(&buf, size), tile(&inv, size))
}

/// Invert every pixel's RGB channels (alpha untouched): white tile with
/// black ink becomes the pressed-state black tile with white ink.
fn invert(buf: &mut [u8]) {
    for px in buf.as_chunks_mut::<4>().0 {
        px[0] = !px[0];
        px[1] = !px[1];
        px[2] = !px[2];
    }
}

/// Rotate an RGBA buffer 90° clockwise; `n` is both width and height
/// (square tiles only).  Pixel (x, y) lands at (n-1-y, x).
fn rot90(buf: &[u8], n: usize) -> Vec<u8> {
    let mut out = vec![0_u8; buf.len()];
    for y in 0..n {
        for x in 0..n {
            let src = (y * n + x) * 4;
            let dst = (x * n + (n - 1 - y)) * 4;
            out[dst..dst + 4].copy_from_slice(&buf[src..src + 4]);
        }
    }
    out
}

/// Bake one icon (no inverted twin needed: never shown inside a button).
fn bake(size: (u32, u32), white_bg: bool, draw: impl FnOnce(&mut DrawCtx)) -> Image {
    tile(&paint(size, white_bg, draw), size)
}

/// All baked icons for one boot.  Every top-bar button glyph has an
/// inverted twin (`*_inv`: black tile, white ink) shown while the button
/// is held — e-ink press feedback is a hard color reversal.
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
    /// The sync glyph in 90° CW steps (index = angle/90) plus the
    /// inverted twins.  The software renderer cannot rotate images, so
    /// the in-flight spin is baked as four tiles.
    pub sync_rot: Vec<Image>,
    pub sync_inv_rot: Vec<Image>,
    pub house_inv: Image,
    pub back_inv: Image,
    pub source_kavita_inv: Image,
    pub source_local_inv: Image,
    pub source_folder_inv: Image,
    pub search_inv: Image,
    pub layout_grid_inv: Image,
    pub layout_list_inv: Image,
    pub sync_inv: Image,
    pub input: Image,
    pub input_inv: Image,
    pub bulb: Image,
    /// Scroll-button chevron ("^"); `chevron_down` is the 180° twin — the
    /// software renderer does not rasterise Path (or reliably rotate), so
    /// both directions are baked.
    pub chevron: Image,
    pub chevron_down: Image,
}

/// Bake the full icon set (a few ms at boot).
pub fn bake_all() -> Icons {
    let (house, house_inv) = pair((96, 96), |ctx| draw_house(ctx, 48, 48, 0));
    let (back, back_inv) = pair((96, 96), |ctx| draw_back_chevron(ctx, 48, 48, 0));
    let (source_kavita, source_kavita_inv) = pair((52, 52), |ctx| draw_globe_icon(ctx, 0, 0, 0));
    let (source_local, source_local_inv) = pair((52, 52), |ctx| draw_book_icon(ctx, 0, 0, 0));
    let (source_folder, source_folder_inv) = pair((52, 52), |ctx| draw_folder_icon(ctx, 0, 0, 0));
    let (search, search_inv) = pair((96, 96), |ctx| draw_search_icon(ctx, 48, 48, 0));
    let (layout_grid, layout_grid_inv) = pair((96, 96), |ctx| {
        draw_layout_icon(ctx, 48, 48, ViewMode::Grid, 0)
    });
    let (layout_list, layout_list_inv) = pair((96, 96), |ctx| {
        draw_layout_icon(ctx, 48, 48, ViewMode::List, 0)
    });
    // Sync spin: bake the idle glyph once and derive the 90° quadrants by
    // buffer rotation; each gets its inverted twin for press feedback.
    let mut sync_bufs: Vec<Vec<u8>> = vec![paint((96, 96), true, |ctx| {
        draw_sync_icon(ctx, 48, 48, 0, 0)
    })];
    for _ in 1..4 {
        sync_bufs.push(rot90(sync_bufs.last().unwrap(), 96));
    }
    let sync_rot: Vec<Image> = sync_bufs.iter().map(|b| tile(b, (96, 96))).collect();
    let sync_inv_rot: Vec<Image> = sync_bufs
        .iter()
        .map(|b| {
            let mut i = b.clone();
            invert(&mut i);
            tile(&i, (96, 96))
        })
        .collect();
    let sync = sync_rot[0].clone();
    let sync_inv = sync_inv_rot[0].clone();
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
    // Scroll-button chevron: a 3px "^" centred in a 48x48 tile (C
    // eh_draw_scroll_buttons_at drew the same two lines).
    let chevron = bake((48, 48), true, |ctx| {
        ctx.line(10, 31, 24, 17, 3, 0);
        ctx.line(24, 17, 38, 31, 3, 0);
    });
    let chevron_down = bake((48, 48), true, |ctx| {
        ctx.line(10, 17, 24, 31, 3, 0);
        ctx.line(24, 31, 38, 17, 3, 0);
    });
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
        sync_rot,
        sync_inv_rot,
        house_inv,
        back_inv,
        source_kavita_inv,
        source_local_inv,
        source_folder_inv,
        search_inv,
        layout_grid_inv,
        layout_list_inv,
        sync_inv,
        input,
        input_inv,
        bulb,
        chevron,
        chevron_down,
    }
}
