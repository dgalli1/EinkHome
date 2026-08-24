//! The "…" More menu drawer (C eh_draw_overlay_more): a solid BLACK dim
//! over the whole content area, a right-anchored white 3/4-width panel
//! divided by a 1px line, and a plain row list (Group by / Sort by /
//! Download all / Settings / Applications) starting at the first button.
//! Rows are 88px tall, all drawn alike (white bg, black label, DGRAY
//! value).  No row outlines, no header — C eh_draw_overlay_more verbatim.

use eh_hal::Rect;

use crate::app::{App, MenuRow};

/// Row rhythm (C EH_MORE_*).
pub const Y0: u32 = 96;
pub const ITEM_H: u32 = 88;

/// Static row list (C `labels[]`); the live summary values are attached
/// at draw time (see [`draw`]).
pub(crate) fn label_keys() -> [(MenuRow, &'static str); 5] {
    [
        (MenuRow::GroupBy, crate::i18n::tr("action.group_by")),
        (MenuRow::SortBy, crate::i18n::tr("action.sort_by")),
        (MenuRow::DownloadAll, crate::i18n::tr("action.download_all")),
        (MenuRow::Settings, crate::i18n::tr("action.settings")),
        (MenuRow::Applications, crate::i18n::tr("action.apps")),
    ]
}

/// The persisted config value of a grouping preset (lowercase preset
/// name, mirroring the Source persistence precedent).
pub(crate) fn group_config_value(g: crate::store::GroupPreset) -> String {
    match g {
        crate::store::GroupPreset::None => "none",
        crate::store::GroupPreset::AuthorSeries => "author_series",
        crate::store::GroupPreset::Author => "author",
        crate::store::GroupPreset::Year => "year",
        crate::store::GroupPreset::Genre => "genre",
        crate::store::GroupPreset::Series => "series",
    }
    .to_string()
}

/// Map a stored `group=` value back to a preset; anything else → None.
pub(crate) fn group_from_config(s: &Option<String>) -> crate::store::GroupPreset {
    match s.as_deref() {
        Some("author_series") => crate::store::GroupPreset::AuthorSeries,
        Some("author") => crate::store::GroupPreset::Author,
        Some("year") => crate::store::GroupPreset::Year,
        Some("genre") => crate::store::GroupPreset::Genre,
        Some("series") => crate::store::GroupPreset::Series,
        _ => crate::store::GroupPreset::None,
    }
}

/// Draw the drawer; records each row's rect into `app.menu_rows` for tap
/// routing (the C app's draw/hit geometry parity).
pub fn draw<B: eh_hal::Framebuffer>(
    surf: &mut eh_render::Surface,
    app: &mut App<B>,
    dirty: &mut Vec<Rect>,
) {
    use eh_shell::{GRAY_BLACK, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom;
    dirty.push(Rect { x: 0, y: 0, w, h });

    // Solid black over the whole content area, then the white panel
    // (C FillArea(BLACK) + FillArea(WHITE) + DrawLine divider).
    surf.fill_gray(Rect { x: 0, y: 0, w, h }, GRAY_BLACK);
    let pw = (w as i32 * 3) / 4;
    let px = w - pw as u32;
    surf.fill_gray(
        Rect {
            x: px,
            y: 0,
            w: pw as u32,
            h,
        },
        GRAY_WHITE,
    );
    surf.vline(px, 0, h, 2, GRAY_BLACK);

    app.menu_rows.clear();
    // C opens DEFAULTFONTB for the drawer rows.
    let font = eh_shell::bold_font();
    let mut glyph = eh_render::Glyph::new();
    // Both rows always show their live selection (C vals[]: group_summary
    // + sort_label — the Group-by value was wrongly hidden when a grouping
    // was active).
    let group_val = crate::widgets::chooser::group_display_key(app.group);
    let sort_val = crate::widgets::chooser::sort_display_key(app.sort);
    for (_i, (row, label)) in label_keys().iter().enumerate() {
        let val = match row {
            MenuRow::GroupBy => Some(crate::i18n::tr(group_val)),
            MenuRow::SortBy => Some(crate::i18n::tr(sort_val)),
            _ => None,
        };
        let ry = Y0 + _i as u32 * ITEM_H;
        let rect = crate::widgets::menu_row::draw_menu_row(
            surf, font, &mut glyph, px, pw as u32, ry, label, val,
        );
        app.menu_rows.push((rect, *row));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::GroupPreset;

    // The `group=` cfg value is what persists a user's shelf grouping
    // across restarts: one typo'd or reordered arm here silently resets
    // everyone's grouping to None on load.  Every variant must serialize
    // to its exact C-config string and parse back identically.
    #[test]
    fn group_config_values_round_trip() {
        for g in [
            GroupPreset::None,
            GroupPreset::AuthorSeries,
            GroupPreset::Author,
            GroupPreset::Year,
            GroupPreset::Genre,
            GroupPreset::Series,
        ] {
            let stored = group_config_value(g);
            assert_eq!(
                group_from_config(&Some(stored.clone())),
                g,
                "round-trip {stored:?}"
            );
        }
    }

    #[test]
    fn group_config_strings_match_the_c_keys() {
        assert_eq!(group_config_value(GroupPreset::None), "none");
        assert_eq!(
            group_config_value(GroupPreset::AuthorSeries),
            "author_series"
        );
        assert_eq!(group_config_value(GroupPreset::Author), "author");
        assert_eq!(group_config_value(GroupPreset::Year), "year");
        assert_eq!(group_config_value(GroupPreset::Genre), "genre");
        assert_eq!(group_config_value(GroupPreset::Series), "series");
    }

    #[test]
    fn unknown_group_config_falls_back_to_none() {
        // Missing key, legacy junk, or an uninstalled preset spelling.
        assert_eq!(group_from_config(&None), GroupPreset::None);
        assert_eq!(group_from_config(&Some(String::new())), GroupPreset::None);
        assert_eq!(
            group_from_config(&Some("authors".into())),
            GroupPreset::None
        );
        assert_eq!(
            group_from_config(&Some("AUTHOR".into())),
            GroupPreset::None,
            "match is exact-lowercase like the C app"
        );
    }
}
