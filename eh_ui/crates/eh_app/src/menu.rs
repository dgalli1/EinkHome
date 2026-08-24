//! The "…" More menu drawer (C eh_draw_overlay_more): a solid BLACK dim
//! over the whole content area, a right-anchored white 3/4-width panel
//! divided by a 1px line, and a plain row list (Group by / Sort by /
//! Download all / Settings / Applications) starting at the first button.
//! Rows are 88px tall, all drawn alike (white bg, black label, DGRAY
//! value).  No row outlines, no header — C eh_draw_overlay_more verbatim.

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

/// A More-menu row tap (the Slint drawer reports the row index; C
/// eh_on_tap_more's row branch).
pub(crate) fn more_row<B: eh_hal::Framebuffer>(app: &mut App<B>, i: usize) {
    let keys = label_keys();
    let Some((row, _)) = keys.get(i) else {
        return;
    };
    match row {
        MenuRow::Settings => app.set_overlay(crate::app::Overlay::Settings),
        MenuRow::Applications => {
            if crate::launcher::build(app) {
                app.set_overlay(crate::app::Overlay::Launcher);
                app.launcher_scroll = 0;
            }
        }
        MenuRow::GroupBy => app.open_group_chooser(),
        MenuRow::SortBy => app.open_sort_chooser(),
        MenuRow::DownloadAll => app.download_all(),
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
