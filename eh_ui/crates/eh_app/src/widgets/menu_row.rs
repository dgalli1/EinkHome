//! One More-menu drawer row (C eh_draw_overlay_more's row block): a bold
//! label on the left, an optional right-aligned DGRAY current-value
//! ("Sort by  Title A-Z"), on the drawer's 88px rhythm.  No outline, no
//! selection tint (the C app's active-group inversion was dropped).

use eh_hal::Rect;
use eh_render::Font;

/// Draw one drawer row at absolute `ry` inside a panel whose left edge is
/// `px` and width `pw`; return its hit rect for tap routing.
///
/// `value` is the row's live summary (C `vals[]`): right-aligned, 24pt,
/// DGRAY — e.g. the active grouping or sort mode.
pub(crate) fn draw_menu_row(
    surf: &mut eh_render::Surface,
    font: &'static Font,
    glyph: &mut eh_render::Glyph,
    px: u32,
    pw: u32,
    ry: u32,
    label: &str,
    value: Option<&str>,
) -> Rect {
    use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_WHITE};
    const ITEM_H: u32 = crate::menu::ITEM_H;
    surf.fill_gray(
        Rect {
            x: px + 12,
            y: ry,
            w: pw - 24,
            h: ITEM_H - 12,
        },
        GRAY_WHITE,
    );
    let mid = ry as i32 + ((ITEM_H - 28) / 2) as i32 - 2;
    // draw_text takes a baseline; C DrawString takes the glyph top.
    eh_render::draw_text(
        surf,
        font,
        28.0,
        label,
        (px + 32) as i32,
        mid + 21,
        GRAY_BLACK,
        glyph,
    );
    if let Some(v) = value {
        let vw = font.width(v, 24.0) as i32;
        let vx = (px + pw - 32) as i32 - vw;
        eh_render::draw_text(surf, font, 24.0, v, vx, mid + 20, GRAY_DGRAY, glyph);
    }
    Rect {
        x: px + 12,
        y: ry,
        w: pw - 24,
        h: ITEM_H - 12,
    }
}
