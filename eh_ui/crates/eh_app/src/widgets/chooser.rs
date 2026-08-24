//! The Group by / Sort by chooser sheet (C eh_draw_group / eh_draw_sort):
//! a dim + a centred sheet with a title band and N rows.  Row geometry
//! matches the harness's `_chooser_py` (centred on the CONTENT area).

/// Which chooser sheet is open (group vs sort) — both share the same
/// centered-row sheet layout.
#[derive(Clone, Copy)]
pub enum ChooserKind {
    Group,
    Sort,
}

/// i18n keys of the group-chooser rows, in the C order (None,
/// [Author>Series], Series, Author, Year, Genre) — the harness reads the
/// store to map a chosen dimension to its row index, so the order must
/// match; the drawn text comes from crate::i18n::tr at draw time.
/// Indexed by [`crate::store::GroupPreset`] value (None=0, AuthorSeries=1,
/// Author=2, Year=3, Genre=4, Series=5).
pub(crate) const GROUP_KEYS: [&str; 6] = [
    "group.all",
    "group.author_series",
    "group.author",
    "group.year",
    "group.genre",
    "group.series",
];
pub(crate) const SORT_KEYS: [&str; 4] =
    ["sort.title_az", "sort.author", "sort.series", "sort.recent"];

/// Display key of a grouping preset — the More-menu row value AND the
/// chooser header's current-value line (C group_label / group_summary).
pub(crate) fn group_display_key(g: crate::store::GroupPreset) -> &'static str {
    GROUP_KEYS[g as usize]
}

/// Display key of a sort mode (C sort_label); SortMode's discriminants
/// match the SORT_KEYS order.
pub(crate) fn sort_display_key(mode: crate::store::SortMode) -> &'static str {
    SORT_KEYS[mode as usize]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{GroupPreset, SortMode};

    // Both tables are indexed by enum discriminant (`g as usize`), so a
    // reordered const row or enum variant mislabels every chooser row or
    // panics with an index-out-of-bounds.  The C order is load-bearing:
    // the e2e harness maps a chosen dimension to its ROW INDEX.
    #[test]
    fn group_keys_follow_the_discriminant_order() {
        for (g, key) in [
            (GroupPreset::None, "group.all"),
            (GroupPreset::AuthorSeries, "group.author_series"),
            (GroupPreset::Author, "group.author"),
            (GroupPreset::Year, "group.year"),
            (GroupPreset::Genre, "group.genre"),
            (GroupPreset::Series, "group.series"),
        ] {
            assert_eq!(group_display_key(g), key);
            // The table itself must stay aligned with the cast too.
            assert_eq!(GROUP_KEYS[g as usize], key);
        }
    }

    #[test]
    fn sort_keys_follow_the_discriminant_order() {
        for (m, key) in [
            (SortMode::Title, "sort.title_az"),
            (SortMode::Author, "sort.author"),
            (SortMode::Series, "sort.series"),
            (SortMode::Recent, "sort.recent"),
        ] {
            assert_eq!(sort_display_key(m), key);
            assert_eq!(SORT_KEYS[m as usize], key);
        }
    }
}
