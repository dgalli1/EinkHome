//! The "…" More menu drawer (C eh_draw_overlay_more): a black dim over the
//! shelf, a right-anchored white 3/4-width card with a 1px left divider,
//! and a plain row list (Group by / Sort by / Download all / Settings /
//! Applications) starting at the first button.  Rows are 88px tall with a
//! 12px gap (the C EH_MORE_ITEM_H rhythm).

use eh_hal::Rect;

use crate::app::{App, MenuRow};

/// Row rhythm (C EH_MORE_*).
pub const Y0: u32 = 96;
pub const ITEM_H: u32 = 88;
pub const ROW_GAP: u32 = 12;

fn labels() -> [(MenuRow, &'static str, Option<&'static str>); 5] {
    [
        (MenuRow::GroupBy, "Group by", Some("None")),
        (MenuRow::SortBy, "Sort by", Some("Recent")),
        (MenuRow::DownloadAll, "Download all", None),
        (MenuRow::Settings, "Settings", None),
        (MenuRow::Applications, "Applications", None),
    ]
}

/// Draw the drawer; records each row's rect into `app.menu_rows` for
/// tap routing (the C app's draw/hit geometry parity).
pub fn draw<B: eh_hal::Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom as u32;
    dirty.push(Rect { x: 0, y: 0, w, h });

    // Dim the shelf behind the drawer (C: a BLACK FillArea of the whole
    // content area under the card — e-ink has no alpha, so the C app
    // draws the dim as a full black fill and the card white on top).
    let pw = (w as i32 * 3) / 4;
    let px = w - pw as u32;
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_BLACK);
    surf.fill_gray(Rect { x: px, y: 0, w: pw as u32, h }, GRAY_WHITE);
    surf.vline(px, 0, h, 2, GRAY_BLACK);

    app.menu_rows.clear();
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    for (i, (row, label, val)) in labels().iter().enumerate() {
        let ry = Y0 + i as u32 * ITEM_H;
        // Row card (C: 12px inset, ITEM_H-12 tall).
        surf.fill_gray(Rect { x: px + 12, y: ry, w: (pw as u32) - 24, h: ITEM_H - ROW_GAP }, GRAY_WHITE);
        surf.rect_outline(Rect { x: px + 12, y: ry, w: (pw as u32) - 24, h: ITEM_H - ROW_GAP }, 2, GRAY_BLACK);
        let mid = ry as i32 + (ITEM_H - ROW_GAP) as i32 / 2;
        eh_render::draw_text(surf, font, 28.0, label, (px + 32) as i32, mid + 10, GRAY_BLACK, &mut glyph);
        if let Some(v) = val {
            let vw = font.width(v, 24.0) as i32;
            let vx = (px + pw as u32 - 32) as i32 - vw;
            eh_render::draw_text(surf, font, 24.0, v, vx, mid + 8, GRAY_DGRAY, &mut glyph);
        }
        app.menu_rows.push((
            Rect { x: px + 12, y: ry, w: (pw as u32) - 24, h: ITEM_H - ROW_GAP },
            *row,
        ));
    }
}