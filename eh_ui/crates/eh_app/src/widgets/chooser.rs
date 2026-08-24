//! The Group by / Sort by chooser sheet (C eh_draw_group / eh_draw_sort):
//! a dim + a centred sheet with a title band and N rows.  Row geometry
//! matches the harness's `_chooser_py` (centred on the CONTENT area).

use eh_hal::{Framebuffer, Rect};

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

pub fn draw_chooser_sheet<B: Framebuffer>(
    surf: &mut eh_render::Surface,
    app: &mut crate::app::App<B>,
    dirty: &mut Vec<Rect>,
    kind: ChooserKind,
) {
    use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_LGRAY, GRAY_WHITE};
    let h = app.content_bottom;
    // Dim over the content area only; centred on it as well.
    let (n, labels, title): (usize, Vec<String>, &str) = match kind {
        ChooserKind::Group => {
            let offer = app.group_offer();
            (
                offer.len(),
                offer
                    .iter()
                    .map(|g| crate::i18n::tr(GROUP_KEYS[*g as usize]).to_string())
                    .collect(),
                crate::i18n::tr("action.group_by"),
            )
        }
        ChooserKind::Sort => (
            4,
            SORT_KEYS
                .iter()
                .map(|k| crate::i18n::tr(k).to_string())
                .collect(),
            crate::i18n::tr("action.sort_by"),
        ),
    };
    let sh = super::sheet::open_sheet(
        surf,
        dirty,
        h,
        0,
        h,
        h,
        (96 + n as u32 * 96 + 24).max(1),
        true,
    );
    let font = crate::shelf::shelf_font();
    let mut g = eh_render::Glyph::new();
    // Header: bold title + DGRAY current-value line under it (C
    // DEFAULTFONTB 28 title at py+16, value at py+46), then the divider.
    eh_render::draw_text(
        surf,
        eh_shell::bold_font(),
        28.0,
        title,
        (sh.px + 24) as i32,
        (sh.py + 16) as i32 + 22,
        GRAY_BLACK,
        &mut g,
    );
    let current: String = match kind {
        ChooserKind::Group => crate::i18n::tr(group_display_key(app.group)).to_string(),
        ChooserKind::Sort => crate::i18n::tr(sort_display_key(app.sort)).to_string(),
    };
    eh_render::draw_text(
        surf,
        font,
        24.0,
        &current,
        (sh.px + 24) as i32,
        (sh.py + 46) as i32 + 18,
        GRAY_DGRAY,
        &mut g,
    );
    surf.hline(sh.px + 24, sh.py + 76, sh.pw - 48, 2, GRAY_LGRAY);
    app.chooser_rects.clear();
    let selected: i64 = match kind {
        ChooserKind::Group => app.group as i64,
        ChooserKind::Sort => app.sort as i64,
    };
    let offered: Vec<i64> = match kind {
        ChooserKind::Group => app.group_offer().iter().map(|g| *g as i64).collect(),
        ChooserKind::Sort => vec![0, 1, 2, 3],
    };
    for (i, label) in labels.iter().enumerate() {
        let iy = sh.py + 84 + (i as u32) * 96;
        let sel = offered.get(i) == Some(&selected);
        let (bg, fg) = if sel {
            (GRAY_BLACK, GRAY_WHITE)
        } else {
            (GRAY_WHITE, GRAY_BLACK)
        };
        surf.fill_gray(
            Rect {
                x: sh.px + 12,
                y: iy,
                w: sh.pw - 24,
                h: 84,
            },
            bg,
        );
        surf.rect_outline(
            Rect {
                x: sh.px + 12,
                y: iy,
                w: sh.pw - 24,
                h: 84,
            },
            1,
            GRAY_BLACK,
        );
        eh_render::draw_text(
            surf,
            eh_shell::bold_font(),
            26.0,
            label,
            (sh.px + 32) as i32,
            (iy + 30) as i32 + 20,
            fg,
            &mut g,
        );
        app.chooser_rects.push(Rect {
            x: sh.px + 12,
            y: iy,
            w: sh.pw - 24,
            h: 84,
        });
    }
}
