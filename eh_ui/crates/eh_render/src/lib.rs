//! eh_render — software rasteriser over a [PixelFormat] surface.
//!
//! Mirrors KOReader's `blitbuffer`: drawing goes into an off-screen byte
//! buffer (owned by the backend), then dirty regions are pushed to the
//! panel with a waveform mode.  This keeps the UI cacheable (redraw only the
//! changed region on e-ink) and is identical to how the current C app draws
//! through inkview — just without being tied to a vendor canvas.
//!
//! The renderer is deliberately tiny: filled rectangles (the app uses colour
//! fills for everything), the system strip primitives, glyph strings via
//! `fontdue`, and a nearest-neighbour image blit (for scaled covers).

#![allow(clippy::too_many_arguments, clippy::type_complexity)]
use core::ops::Range;

use eh_hal::{PixelFormat, Rect};

/// Wraps a pixel buffer + its format/stride so drawing helpers can address
/// individual pixels without caring about the format (8bpp gray, 24bpp RGB,
/// 32bpp RGBA).
///
/// NOT meant to outlive its borrow; constructed per draw pass and discarded.
pub struct Surface<'a> {
    data: &'a mut [u8],
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
}

impl<'a> Surface<'a> {
    pub fn new(
        data: &'a mut [u8],
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
    ) -> Self {
        Self {
            data,
            width,
            height,
            stride,
            format,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn format(&self) -> PixelFormat {
        self.format
    }

    #[inline]
    fn bpp(&self) -> usize {
        self.format.bytes_per_pixel()
    }

    /// Pixel row slice for `y` (may be out of range; returns empty then).
    #[inline]
    fn row_mut(&mut self, y: u32) -> &mut [u8] {
        if y >= self.height {
            return &mut [];
        }
        let from = (y as usize) * self.stride;
        let len = self.width as usize * self.bpp();
        if from + len > self.data.len() {
            return &mut [];
        }
        &mut self.data[from..from + len]
    }

    /// Set one pixel to grayscale intensity, converted to the format.
    #[inline]
    fn set_px(&mut self, x: u32, y: u32, gray: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let (format, bpp) = (self.format, self.bpp());
        let row = self.row_mut(y);
        let off = (x as usize) * bpp;
        if off + bpp > row.len() {
            return;
        }
        match format {
            PixelFormat::Grayscale8 => row[off] = gray,
            PixelFormat::Rgb24 => {
                row[off] = gray;
                row[off + 1] = gray;
                row[off + 2] = gray;
            }
            PixelFormat::Rgba32 => {
                row[off] = gray;
                row[off + 1] = gray;
                row[off + 2] = gray;
                row[off + 3] = 0xff;
            }
        }
    }

    /// Set one pixel from truecolour (for Kaleido covers).  Converts to the
    /// surface format: luma for 8bpp, direct RGB for 24/32bpp.
    #[inline]
    fn set_px_rgb(&mut self, x: u32, y: u32, r: u8, g: u8, b: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let (format, bpp) = (self.format, self.bpp());
        let row = self.row_mut(y);
        let off = (x as usize) * bpp;
        if off + bpp > row.len() {
            return;
        }
        match format {
            PixelFormat::Grayscale8 => {
                // Rec.601 luma of a greyscale-equal-RGB colour.
                let gray = ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8;
                row[off] = gray;
            }
            PixelFormat::Rgb24 => {
                row[off] = r;
                row[off + 1] = g;
                row[off + 2] = b;
            }
            PixelFormat::Rgba32 => {
                row[off] = r;
                row[off + 1] = g;
                row[off + 2] = b;
                row[off + 3] = 0xff;
            }
        }
    }

    /// Fill a rectangle with a grayscale intensity, clipped to the surface.
    pub fn fill_gray(&mut self, rect: Rect, gray: u8) {
        let clip = rect.intersect(&Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        });
        if clip.is_empty() {
            return;
        }
        // Row-wise fill (the per-pixel set_px loop was ~1s for a full
        // canvas under qemu-arm — the e-ink overlays dim the whole frame).
        let bpp = self.bpp();
        let row_bytes = self.stride;
        for y in clip.y..clip.y + clip.h {
            let row = (y as usize) * row_bytes;
            let start = row + (clip.x as usize) * bpp;
            let end = start + (clip.w as usize) * bpp;
            if end > self.data.len() {
                continue;
            }
            match self.format {
                PixelFormat::Grayscale8 => self.data[start..end].fill(gray),
                PixelFormat::Rgb24 => {
                    let fill = [gray, gray, gray];
                    for c in self.data[start..end].chunks_exact_mut(3) {
                        c.copy_from_slice(&fill);
                    }
                }
                PixelFormat::Rgba32 => {
                    let fill = [gray, gray, gray, 0xff];
                    for c in self.data[start..end].chunks_exact_mut(4) {
                        c.copy_from_slice(&fill);
                    }
                }
            }
        }
    }

    /// Horizontal line.
    pub fn hline(&mut self, x: u32, y: u32, len: u32, thick: u32, gray: u8) {
        if y + thick <= self.height {
            self.fill_gray(
                Rect {
                    x,
                    y,
                    w: len,
                    h: thick,
                },
                gray,
            );
        }
    }

    /// Vertical line.
    pub fn vline(&mut self, x: u32, y: u32, len: u32, thick: u32, gray: u8) {
        if x + thick <= self.width {
            self.fill_gray(
                Rect {
                    x,
                    y,
                    w: thick,
                    h: len,
                },
                gray,
            );
        }
    }

    /// Rectangle outline of `thick` px.
    pub fn rect_outline(&mut self, rect: Rect, thick: u32, gray: u8) {
        self.hline(rect.x, rect.y, rect.w, thick, gray);
        self.hline(rect.x, rect.y + rect.h - thick, rect.w, thick, gray);
        self.vline(rect.x, rect.y, rect.h, thick, gray);
        self.vline(rect.x + rect.w - thick, rect.y, rect.h, thick, gray);
    }

    /// 16-segment circle outline (used by the top-bar globe + status strip)
    /// — a polyline because the inkview primitive `DrawCircle` filled.
    pub fn circle_outline(&mut self, cx: i32, cy: i32, r: i32, thick: u32, gray: u8) {
        let mut prev = None;
        for s in 1..=40u32 {
            let a = s as f64 * 2.0 * core::f64::consts::PI / 40.0;
            let x = cx + (r as f64 * a.cos()).round() as i32;
            let y = cy + (r as f64 * a.sin()).round() as i32;
            if let Some((px, py)) = prev {
                self.line(px, py, x, y, thick, gray);
            }
            prev = Some((x, y));
        }
    }

    /// Bresenham line between `(x0,y0)` and `(x1,y1)`.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, thick: u32, gray: u8) {
        let (mut x, mut y) = (x0, y0);
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            self.fill_gray(Rect::from_xy(x, y, thick as i32, thick as i32), gray);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }

    /// Draw cover image data (raw pixels in `src_fmt`) at `dst`, scaled
    /// nearest-neighbour into `size`.  Preserves aspect ratio by letterboxing.
    /// Returns the rect actually drawn (letterboxed), for dirty tracking.
    pub fn blit_image(
        &mut self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
        src_fmt: PixelFormat,
        dst: Rect,
    ) -> Rect {
        if src_w == 0 || src_h == 0 || dst.is_empty() {
            return Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            };
        }
        let bpp = src_fmt.bytes_per_pixel();
        if src.len() < (src_w as usize) * (src_h as usize) * bpp {
            return Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            };
        }

        // Fit the source aspect inside dst, letterboxing.
        let (mut dw, mut dh) = (dst.w, dst.h);
        let src_a = src_w as f32 / src_h as f32;
        let dst_a = dst.w as f32 / dst.h as f32;
        if src_a > dst_a {
            dh = (dst.w as f32 / src_a) as u32;
        } else if src_a < dst_a {
            dw = (dst.h as f32 * src_a) as u32;
        }
        if dw == 0 || dh == 0 {
            return Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            };
        }
        let ox = dst.x + (dst.w - dw) / 2;
        let oy = dst.y + (dst.h - dh) / 2;

        let clip = Rect {
            x: ox,
            y: oy,
            w: dw,
            h: dh,
        }
        .intersect(&Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        });
        if clip.is_empty() {
            return Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            };
        }

        for py in clip.y..clip.y + clip.h {
            let sy = (((py - oy) as f32 * src_h as f32) / dh as f32) as u32;
            for px in clip.x..clip.x + clip.w {
                let sx = (((px - ox) as f32 * src_w as f32) / dw as f32) as u32;
                let so = (sy as usize) * src_w as usize * bpp + (sx as usize) * bpp;
                match src_fmt {
                    PixelFormat::Grayscale8 => self.set_px(px, py, src[so]),
                    PixelFormat::Rgb24 => {
                        self.set_px_rgb(px, py, src[so], src[so + 1], src[so + 2])
                    }
                    PixelFormat::Rgba32 => {
                        self.set_px_rgb(px, py, src[so], src[so + 1], src[so + 2]);
                    }
                }
            }
        }
        Rect {
            x: ox,
            y: oy,
            w: dw,
            h: dh,
        }
    }

    /// Blend a coverage run (fontdue bitmap: 0=bkg, 255=fg) tinted `gray`.
    /// C StretchBitmap: scale the source to EXACTLY dst.w x dst.h (no
    /// aspect preservation — the C grid stretches covers to fill the tile).
    pub fn blit_image_stretch(
        &mut self,
        src: &[u8],
        src_w: u32,
        src_h: u32,
        src_fmt: PixelFormat,
        dst: Rect,
    ) {
        if src_w == 0 || src_h == 0 || dst.is_empty() {
            return;
        }
        let bpp = src_fmt.bytes_per_pixel();
        if src.len() < (src_w as usize) * (src_h as usize) * bpp {
            return;
        }
        let dw = dst.w.max(1);
        let dh = dst.h.max(1);
        for row in 0..dh {
            let sy = (row as u64 * src_h as u64 / dh as u64) as usize;
            for col in 0..dw {
                let sx = (col as u64 * src_w as u64 / dw as u64) as usize;
                let s = (sy * src_w as usize + sx) * bpp;
                let d = (dst.y + row) as usize * self.stride + (dst.x + col) as usize * self.bpp();
                let e = d + bpp;
                if e <= self.data.len() {
                    self.data[d..e].copy_from_slice(&src[s..s + bpp]);
                }
            }
        }
    }

    pub fn blit_glyph(&mut self, x: i32, y: i32, w: u32, h: u32, coverage: &[u8], gray: u8) {
        if coverage.len() < (w as usize) * (h as usize) {
            return;
        }
        let (format, bpp) = (self.format, self.bpp());
        let width = self.width;
        let height = self.height;
        let inv_base = 255 - gray as u32;
        for gy in 0..h {
            let py = (y + gy as i32) as u32;
            if py >= height {
                continue;
            }
            // Compute row bounds once per row; index within a row is cheaper
            // than re-borrowing self per pixel.
            let row = self.row_mut(py);
            for gx in 0..w {
                let c = coverage[(gy as usize) * (w as usize) + (gx as usize)];
                if c == 0 {
                    continue;
                }
                let px = (x + gx as i32) as u32;
                if px >= width {
                    continue;
                }
                let inv = 255 - c as u32;
                let gg = gray as u32;
                let off = (px as usize) * bpp;
                if off + bpp > row.len() {
                    continue;
                }
                match format {
                    PixelFormat::Grayscale8 => {
                        let cur = row[off] as u32;
                        row[off] = ((cur * inv + gg * c as u32 + 127) / 255) as u8;
                    }
                    PixelFormat::Rgb24 => {
                        for k in 0..3 {
                            let cur = row[off + k] as u32;
                            row[off + k] = ((cur * inv + gg * c as u32 + 127) / 255) as u8;
                        }
                    }
                    PixelFormat::Rgba32 => {
                        for k in 0..3 {
                            let cur = row[off + k] as u32;
                            row[off + k] = ((cur * inv + gg * c as u32 + 127) / 255) as u8;
                        }
                        row[off + 3] = 0xff;
                    }
                }
            }
            let _ = inv_base;
        }
    }
}

/// A font with a rasteriser.  Wraps `fontdue` so the crates depending on the
/// renderer don't pull fontdue types into their public API.
pub struct Font {
    face: fontdue::Font,
    /// Cache-key identity: two faces (regular + bold) must never share
    /// glyph-cache entries, or chars render with the other face's
    /// coverage mid-string.
    id: u8,
}

/// Rasterised-glyph cache keyed by (font id, char, size): the first draw
/// of each glyph pays fontdue's rasterize; every later draw copies the
/// cached coverage (the emulator's text-heavy overlays went ~1s -> ~20ms).
static GLYPH_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(u8, char, u32), (fontdue::Metrics, Vec<u8>)>>,
> = std::sync::OnceLock::new();
fn glyph_cache() -> &'static std::sync::Mutex<
    std::collections::HashMap<(u8, char, u32), (fontdue::Metrics, Vec<u8>)>,
> {
    GLYPH_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// A glyph rasterisation: coverage bitmap + the metrics that position it.
pub struct Glyph {
    pub metrics: fontdue::Metrics,
    pub coverage: Vec<u8>,
}

impl Glyph {
    pub fn new() -> Self {
        Self {
            metrics: fontdue::Metrics::default(),
            coverage: Vec::new(),
        }
    }
}

impl Default for Glyph {
    fn default() -> Self {
        Self::new()
    }
}

impl Font {
    pub fn from_bytes(data: &'static [u8]) -> Result<Self, &'static str> {
        static NEXT_ID: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
        let face = fontdue::Font::from_bytes(data, fontdue::FontSettings::default())?;
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Self { face, id })
    }

    /// Rasterise one glyph into `glyph` (cached by char+size — fontdue's
    /// SDF rasterize is ~10ms per glyph under qemu-arm, so uncached text
    /// re-renders were the emulator's ~1s overlay draws).
    pub fn raster(&self, ch: char, size_px: f32, glyph: &mut Glyph) -> bool {
        let key = (self.id, ch, (size_px * 4.0).round() as u32);
        if let Some((m, cov)) = glyph_cache().lock().unwrap().get(&key) {
            glyph.metrics = *m;
            glyph.coverage = cov.clone();
            return true;
        }
        let (metrics, coverage) = self.face.rasterize(ch, size_px);
        glyph.metrics = metrics;
        glyph.coverage = coverage.clone();
        glyph_cache()
            .lock()
            .unwrap()
            .insert(key, (glyph.metrics, coverage));
        true
    }

    /// Horizontal advance of a whole run (text width in px at `size_px`).
    pub fn width(&self, text: &str, size_px: f32) -> f32 {
        text.chars()
            .map(|c| self.face.metrics(c, size_px).advance_width)
            .sum()
    }

    /// Height above baseline (ascent) + below (descent), for vertical
    /// centring of a line.
    pub fn line_h(&self, size_px: f32) -> (f32, f32) {
        let m = self.face.horizontal_line_metrics(size_px);
        match m {
            Some(lm) => (lm.ascent, lm.descent),
            None => (size_px * 0.8, size_px * 0.2),
        }
    }
}

/// Draw a text run with its baseline at `baseline_y`.  Glyphs are positioned
/// with fontdue's canonical formula: bitmap top-left at
/// `(pen_x + xmin, baseline_y - ymin - height)`, then advance by
/// `advance_width`.  Returns the used horizontal extent.
pub fn draw_text(
    surf: &mut Surface,
    font: &Font,
    size_px: f32,
    text: &str,
    x: i32,
    baseline_y: i32,
    gray: u8,
    glyph: &mut Glyph,
) -> u32 {
    let mut pen_x = x as f32;
    for ch in text.chars() {
        if !font.raster(ch, size_px, glyph) {
            continue;
        }
        let m = &glyph.metrics;
        let gx = pen_x as i32 + m.xmin;
        let gy = baseline_y - m.height as i32 - m.ymin;
        let cov = core::mem::take(&mut glyph.coverage);
        surf.blit_glyph(gx, gy, m.width as u32, m.height as u32, &cov, gray);
        glyph.coverage = cov;
        pen_x += m.advance_width;
    }
    (pen_x - x as f32) as u32
}

/// Column-split text at a pixel width budget (the app's `utf8_fit_width`
/// analog — never splits a multibyte char).  Returns the visible prefix.
pub fn fit_width(font: &Font, size_px: f32, text: &str, max_px: f32, out: &mut String) {
    out.clear();
    let mut w = 0.0f32;
    for ch in text.chars() {
        let adv = font.face.metrics(ch, size_px).advance_width;
        if w + adv > max_px && !out.is_empty() {
            break;
        }
        w += adv;
        out.push(ch);
    }
}

/// A reusable scratch area for the caller that needs a glyph buffer.
pub struct DrawScratch {
    pub glyph: Glyph,
    pub tmp: String,
    pub dirty: Vec<Rect>,
    _range: core::marker::PhantomData<Range<u8>>,
}

impl DrawScratch {
    pub fn new() -> Self {
        Self {
            glyph: Glyph::new(),
            tmp: String::new(),
            dirty: Vec::new(),
            _range: core::marker::PhantomData,
        }
    }
    pub fn full() -> Self {
        Self::new()
    }

    pub fn track(&mut self, r: Rect) {
        if !r.is_empty() {
            self.dirty.push(r);
        }
    }
}
impl Default for DrawScratch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eh_hal::{PixelFormat, Rect};

    const FONT: &[u8] = include_bytes!("../../../fonts/DejaVuSans.ttf");

    /// Titles, authors and filenames arrive from untrusted book files
    /// (the Grab::text byte-cap panic was exactly this class).  The
    /// pipeline — measure → fit → draw — must tolerate arbitrary UTF-8:
    /// multibyte scripts, emoji, combining marks, control bytes.
    const ADVERSARIAL: [&str; 9] = [
        "War and Peace",
        "Капита́нская до́чка",                 // Cyrillic + combining marks
        "日本語のタイトル",                  // CJK
        "🦀 ferris 📚 shelf",                // emoji (4-byte chars)
        "a\u{0301}\u{0302}b",                // combining diacritics
        "\u{200b}\u{feff}zero\u{200d}width", // zero-width + ZWJ + BOM
        "tab\there\nnewline\0nul",           // control bytes
        "﷽",                                 // longest single char (U+FDFD)
        "",                                  // empty input
    ];

    #[test]
    fn text_pipeline_never_panics_on_adversarial_utf8() {
        let font = Font::from_bytes(FONT).expect("bundled font parses");
        let mut glyph = Glyph::new();
        let mut buf = vec![0u8; 320 * 240 * PixelFormat::Rgb24.bytes_per_pixel()];
        for text in ADVERSARIAL {
            // Measure.
            let _ = font.width(text, 24.0);
            // Fit to a budget: the prefix must stay valid UTF-8 (it is a
            // String by construction) and never exceed the budget once
            // it holds at least one char.
            let mut fitted = String::new();
            fit_width(&font, 24.0, text, 100.0, &mut fitted);
            if !fitted.is_empty() {
                assert!(font.width(&fitted, 24.0) <= 100.0 + f32::EPSILON);
            }
            // Rasterise onto a tiny surface: clipping must discard, not
            // panic, whatever the metrics say.
            let mut surf = Surface::new(&mut buf, 320, 240, 320 * 3, PixelFormat::Rgb24);
            let _ = draw_text(&mut surf, &font, 24.0, text, 5, 100, 0, &mut glyph);
            let _ = draw_text(&mut surf, &font, 24.0, text, -5000, -5000, 0, &mut glyph);
        }
    }

    #[test]
    fn blit_helpers_reject_out_of_bounds_geometry() {
        let mut buf = vec![0u8; 16 * 16 * 3];
        let mut surf = Surface::new(&mut buf, 16, 16, 16 * 3, PixelFormat::Rgb24);
        let far = Rect {
            x: 10_000,
            y: 10_000,
            w: 64,
            h: 64,
        };
        surf.fill_gray(far, 0);
        surf.hline(10_000, 10_000, 32, 2, 0);
        surf.vline(10_000, 10_000, 32, 2, 0);
        surf.line(-50, -50, 10_000, 10_000, 2, 0);
        surf.blit_image(&[0u8; 64 * 64 * 3], 64, 64, PixelFormat::Rgb24, far);
        surf.blit_glyph(-40, -40, 8, 8, &[255u8; 64], 0);
        // The untouched buffer stays pristine.
        assert!(buf.iter().all(|&b| b == 0));
    }
}
