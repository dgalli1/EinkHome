//! Sort-by / Group-by chooser actions and the group drill-down stack
//! (split from `app.rs`).  The chooser SHEET rendering lives in
//! `widgets::chooser`; this module owns the state changes a tap triggers:
//! applying the picked preset/mode, resetting drill state, persisting the
//! choice, and the two-level Author > Series drill navigation.

use eh_hal::Framebuffer;

use crate::app::{App, Overlay};
use crate::widgets::chooser::ChooserKind;

impl<B: Framebuffer> App<B> {
    /// Open the Group by chooser sheet.
    pub(crate) fn open_group_chooser(&mut self) {
        self.chooser_rects.clear();
        self.set_overlay(Overlay::GroupChooser);
    }

    /// Open the Sort by chooser sheet.
    pub(crate) fn open_sort_chooser(&mut self) {
        self.chooser_rects.clear();
        self.set_overlay(Overlay::SortChooser);
    }

    /// A chooser-sheet row (or outside) tap: apply the choice, rebuild the
    /// view, close.  Outside the sheet dismisses (C sheet behaviour).
    pub(crate) fn tap_chooser(&mut self, x: i32, y: i32, kind: ChooserKind) {
        for (i, r) in self.chooser_rects.iter().enumerate() {
            if r.contains(x, y) {
                match kind {
                    ChooserKind::Group => {
                        let offer = self.group_offer();
                        if let Some(g) = offer.get(i) {
                            self.set_group(*g);
                        }
                    }
                    ChooserKind::Sort => {
                        let mode = match i {
                            1 => crate::store::SortMode::Author,
                            2 => crate::store::SortMode::Series,
                            3 => crate::store::SortMode::Recent,
                            _ => crate::store::SortMode::Title,
                        };
                        self.sort = mode;
                        self.rebuild_view();
                    }
                }
                self.chooser_rects.clear();
                self.set_overlay(Overlay::None);
                return;
            }
        }
        // Tap outside the sheet → dismiss.
        self.chooser_rects.clear();
        self.set_overlay(Overlay::None);
    }

    /// Apply a chosen grouping preset (C eh_g_group assignment in the
    /// group chooser): reset any drill state, rebuild the view, and
    /// persist the choice so it survives a restart (`group=` cfg key).
    pub(crate) fn set_group(&mut self, g: crate::store::GroupPreset) {
        self.group = g;
        self.config.group = Some(crate::menu::group_config_value(g));
        self.drill = 0;
        self.drill_values = Default::default();
        self.drill_names = Default::default();
        self.rebuild_view();
        self.save_config();
    }

    /// The pinned drill scopes for the store, level 0..drill (C
    /// eh_g_drill_values[0..eh_g_drill_level]).
    pub(crate) fn drill_scopes(&self) -> Vec<&str> {
        self.drill_values[..self.drill as usize]
            .iter()
            .map(String::as_str)
            .collect()
    }

    /// Drill into a tapped stack card (C eh_group_drill): record the
    /// group's value at the next drill level, so the shelf regroups within
    /// that group (or shows flat books at the preset's last level), and
    /// remember the page of the level we're leaving so drill-back lands
    /// back where they were.
    pub(crate) fn drill_into_card(&mut self, view_row: &crate::store::ViewRow) {
        const MAX_LEVELS: u32 = 2; // C EH_GROUP_MAX_LEVELS (Author -> Series)
        if self.drill >= MAX_LEVELS {
            return;
        }
        let lvl = self.drill as usize;
        self.drill_saved_pages[lvl] = self.page;
        self.drill_values[lvl] = view_row.series_id.clone();
        self.drill_names[lvl] = view_row.series_name.clone();
        self.drill += 1;
        self.page = 0;
        self.rebuild_view();
    }

    /// Back: pop the drill level (C eh_group_drill_back), restoring the
    /// saved page of the level we return into, so back from a deep drill
    /// continues where the user left off.
    pub(crate) fn drill_back(&mut self) {
        if self.drill > 0 {
            self.drill -= 1;
            let lvl = self.drill as usize;
            self.drill_values[lvl].clear();
            self.drill_names[lvl].clear();
            self.page = self.drill_saved_pages[lvl];
            self.rebuild_view();
        }
    }
}
