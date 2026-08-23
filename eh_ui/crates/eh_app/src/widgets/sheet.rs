//! The shared modal-sheet scaffold: a dim band plus a centred white panel
//! with a black border (the C app's eh_dim_content + popup_geom pattern).

use eh_hal::Rect;

/// Geometry of an opened sheet. Draw code positions content relative to
/// these values; tap handlers recompute the same centring via
/// [`open_sheet`]'s documented formula.
#[derive(Clone, Copy, Debug)]
pub struct Sheet {
    pub px: u32,
    pub py: u32,
    pub pw: u32,
    pub ph: u32,
}

/// Dim `[dim_y0, dim_y1)` and open a centred white panel of `ph` height
/// spanning `w * 3 / 4`.
///
/// * `dirty_h` — height of the full-screen dirty rect pushed first (the
///   popups dim the content area but still repaint the status strip).
/// * `dim_y0`/`dim_y1` — the dimmed band (the C eh_dim_content range).
/// * `center_base` — the height the panel is vertically centred on
///   (`py = (center_base - ph) / 2`, clamped at 0). For every popup this
///   is the same value the C app centred on; it is NOT always the dimmed
///   band's end (the download/sync sheets dim below the top bar but
///   centre on the full content height).
/// * `double_border` — the chooser's C inset second outline.
pub fn open_sheet(
    surf: &mut eh_render::Surface,
    dirty: &mut Vec<Rect>,
    dirty_h: u32,
    dim_y0: u32,
    dim_y1: u32,
    center_base: u32,
    ph: u32,
    double_border: bool,
) -> Sheet {
    use eh_shell::{GRAY_BLACK, GRAY_WHITE};
    let w = surf.width();
    dirty.push(Rect { x: 0, y: 0, w, h: dirty_h });
    eh_shell::dim_hatch(surf, dim_y0, dim_y1);
    let pw = w * 3 / 4;
    let px = (w - pw) / 2;
    let py = ((center_base as i32 - ph as i32) / 2).max(0) as u32;
    surf.fill_gray(Rect { x: px, y: py, w: pw, h: ph }, GRAY_WHITE);
    // C draws the chooser border twice (outer + inset); an outline of 2
    // plus a 1px inset covers both.
    surf.rect_outline(Rect { x: px, y: py, w: pw, h: ph }, 2, GRAY_BLACK);
    if double_border {
        surf.rect_outline(Rect { x: px + 1, y: py + 1, w: pw - 2, h: ph - 2 }, 1, GRAY_BLACK);
    }
    Sheet { px, py, pw, ph }
}
