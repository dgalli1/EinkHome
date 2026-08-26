//! The source chooser's state changes (C eh_on_tap_source's row branch):
//! switching source aborts any in-flight sync chain, persists the choice,
//! and runs the source's data path — Kavita syncs, Local kicks the
//! storage-root import, Folder opens the directory browser as the shelf
//! body.  (The sheet itself is Slint markup — `ui/source.slint`.)

use crate::app::{App, Source};

/// A source-chooser row tap (0=Kavita 1=Local 2=Folder; C eh_on_tap_source).
pub fn apply_source<B: eh_hal::Framebuffer>(app: &mut App<B>, row: usize) {
    if row > 2 {
        return;
    }
    app.set_overlay(crate::app::Overlay::None);
    // Source switch: abort any in-flight sync chain BEFORE the source
    // changes / config saves (C eh_sync_abort), and drop a still-running
    // local import scan — its result would otherwise land under the new
    // source (or hold `syncing` so resync() silently no-ops).
    app.sync_abort();
    crate::local::cancel_scan(app);
    let new = match row {
        0 => Source::Kavita,
        1 => Source::Local,
        _ => Source::Folder,
    };
    app.source = new;
    app.config.source = Some(new.config_value());
    app.save_config();
    crate::log(&format!(
        "[eh_app] source switched to {}",
        new.ui_label_key()
    ));
    // A source switch always drills back to the shelf root: the drilled
    // group scope belongs to the previous source's view — "Robert Blaise"
    // selected on Kavita must not stay selected after jumping to Local
    // (C eh_on_tap_source resets the group drill).
    app.drill = 0;
    app.drill_values = Default::default();
    app.drill_names = Default::default();
    app.drill_saved_pages = [0; 2];
    app.context.dismiss();
    app.tab = crate::app::Tab::Library;
    app.page = 0;
    match new {
        Source::Kavita => {
            app.browser.open = false;
            // Re-project the view under Kavita NOW (C rebuilds when
            // g_view_source != source even if the delta applies nothing):
            // the shelf must drop local rows immediately, not whenever
            // the async sync happens to land.
            app.rebuild_view();
            app.resync();
        }
        Source::Local => {
            app.browser.open = false;
            // A populated local source shows its cached rows NOW — the
            // chooser no longer re-runs the import on every switch (the
            // walk + extraction of a 20k library is minutes of sheet).
            // Freshness stays reachable: every boot rescans once (C
            // EVT_INIT) and the top-bar sync button re-imports on
            // demand.  Only a first-ever selection (no local rows yet)
            // pays the import here; its apply rebuilds the view on a
            // later tick.
            if app.store.count_source("local").unwrap_or(0) == 0 {
                crate::local::kick_import(app);
            }
            app.rebuild_view();
            app.refresh_shelf();
        }
        Source::Folder => {
            crate::local::start_browse(app);
        }
    }
}
