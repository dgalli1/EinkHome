//! The "…" More menu drawer (C eh_draw_overlay_more): an LGRAY hatch dim
//! over the shelf (eh_shell::dim_hatch), a right-anchored white
//! 3/4-width card with a 1px left divider, and a plain row list (Group by
//! / Sort by / Download all / Settings / Applications) starting at the
//! first button.  Rows are 88px tall with a 12px gap (the C
//! EH_MORE_ITEM_H rhythm).

use eh_hal::Rect;

use crate::app::{App, MenuRow};

/// Row rhythm (C EH_MORE_*).
pub const Y0: u32 = 96;
pub const ITEM_H: u32 = 88;
pub const ROW_GAP: u32 = 12;

pub(crate) fn labels() -> [(MenuRow, &'static str, Option<&'static str>); 5] {
    // All strings via i18n (C eh_draw_overlay_more): the group summary
    // value mirrors the C `vals[0]` slot, the sort value the C
    // `eh_i18n(sort_label())`.
    [
        (
            MenuRow::GroupBy,
            crate::i18n::tr("action.group_by"),
            Some(crate::i18n::tr("group.none")),
        ),
        (
            MenuRow::SortBy,
            crate::i18n::tr("action.sort_by"),
            Some(crate::i18n::tr("sort.recent")),
        ),
        (MenuRow::DownloadAll, crate::i18n::tr("action.download_all"), None),
        (MenuRow::Settings, crate::i18n::tr("action.settings"), None),
        (MenuRow::Applications, crate::i18n::tr("action.apps"), None),
    ]
}

/// Draw the drawer; records each row's rect into `app.menu_rows` for
/// tap routing (the C app's draw/hit geometry parity).
pub fn draw<B: eh_hal::Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_WHITE};
    let _t0 = std::time::Instant::now();
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });

    // Dim the shelf behind the drawer with the shared LGRAY every-other-
    // line hatch (C eh_dim_content(0)): the background stays readable
    // behind the card, unlike a solid black fill.
    let pw = (w as i32 * 3) / 4;
    let px = w - pw as u32;
    eh_shell::dim_hatch(surf, 0, h);
    surf.fill_gray(Rect { x: px, y: 0, w: pw as u32, h }, GRAY_WHITE);
    surf.vline(px, 0, h, 2, GRAY_BLACK);

    app.menu_rows.clear();
    let _t1 = std::time::Instant::now();
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    for (_i, (row, label, val)) in labels().iter().enumerate() {
        let ry = Y0 + _i as u32 * ITEM_H;
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