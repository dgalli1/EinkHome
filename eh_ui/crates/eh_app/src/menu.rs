//! The "…" More menu drawer (C eh_draw_overlay_more): a solid BLACK dim
//! over the whole content area, a right-anchored white 3/4-width panel
//! divided by a 1px line, and a plain row list (Group by / Sort by /
//! Download all / Settings / Applications) starting at the first button.
//! Rows are 88px tall; the Group row inverts (black bg, white text) while
//! a group is active.  No row outlines, no header — C eh_draw_overlay_more
//! verbatim.

use eh_hal::Rect;

use crate::app::{App, MenuRow};

/// Row rhythm (C EH_MORE_*).
pub const Y0: u32 = 96;
pub const ITEM_H: u32 = 88;

/// Row list with the live summary values (C `vals[]`: the group summary
/// and the current sort label).
pub(crate) fn labels(app: &App<impl eh_hal::Framebuffer>) -> [(MenuRow, &'static str, Option<&'static str>); 5] {
    let group_val = if app.group == crate::store::GroupPreset::None {
        Some(crate::i18n::tr("group.none"))
    } else {
        None // the inverted row carries no value slot in C either
    };
    let sort_val = crate::i18n::tr(sort_key(app.sort));
    [
        (
            MenuRow::GroupBy,
            crate::i18n::tr("action.group_by"),
            group_val,
        ),
        (
            MenuRow::SortBy,
            crate::i18n::tr("action.sort_by"),
            Some(sort_val),
        ),
        (MenuRow::DownloadAll, crate::i18n::tr("action.download_all"), None),
        (MenuRow::Settings, crate::i18n::tr("action.settings"), None),
        (MenuRow::Applications, crate::i18n::tr("action.apps"), None),
    ]
}

/// i18n key for the active sort (C sort_label), matching SORT_KEYS order.
pub(crate) fn sort_key(mode: crate::store::SortMode) -> &'static str {
    match mode {
        crate::store::SortMode::Author => "sort.author",
        crate::store::SortMode::Series => "sort.series",
        crate::store::SortMode::Recent => "sort.recent",
        crate::store::SortMode::Title => "sort.title_az",
    }
}

/// Draw the drawer; records each row's rect into `app.menu_rows` for tap
/// routing (the C app's draw/hit geometry parity).
pub fn draw<B: eh_hal::Framebuffer>(surf: &mut eh_render::Surface, app: &mut App<B>, dirty: &mut Vec<Rect>) {
    use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });

    // Solid black over the whole content area, then the white panel
    // (C FillArea(BLACK) + FillArea(WHITE) + DrawLine divider).
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_BLACK);
    let pw = (w as i32 * 3) / 4;
    let px = w - pw as u32;
    surf.fill_gray(Rect { x: px, y: 0, w: pw as u32, h }, GRAY_WHITE);
    surf.vline(px, 0, h, 2, GRAY_BLACK);

    app.menu_rows.clear();
    let font = crate::shelf::shelf_font();
    let mut glyph = eh_render::Glyph::new();
    for (_i, (row, label, val)) in labels(app).iter().enumerate() {
        let ry = Y0 + _i as u32 * ITEM_H;
        // Selected-group row inverts (C: sel ? BLACK : WHITE fill).
        let sel = *row == MenuRow::GroupBy && app.group != crate::store::GroupPreset::None;
        let (bg, fg, vcol) = if sel {
            (GRAY_BLACK, GRAY_WHITE, GRAY_WHITE)
        } else {
            (GRAY_WHITE, GRAY_BLACK, GRAY_DGRAY)
        };
        surf.fill_gray(Rect { x: px + 12, y: ry, w: (pw as u32) - 24, h: ITEM_H - 12 }, bg);
        let mid = ry as i32 + ((ITEM_H - 28) / 2) as i32 - 2;
        // draw_text takes a baseline; C DrawString takes the glyph top.
        eh_render::draw_text(surf, font, 28.0, label, (px + 32) as i32, mid + 21, fg, &mut glyph);
        if let Some(v) = val {
            let vw = font.width(v, 24.0) as i32;
            let vx = (px + pw as u32 - 32) as i32 - vw;
            eh_render::draw_text(surf, font, 24.0, v, vx, mid + 20, vcol, &mut glyph);
        }
        app.menu_rows.push((
            Rect { x: px + 12, y: ry, w: (pw as u32) - 24, h: ITEM_H - 12 },
            *row,
        ));
    }
}
