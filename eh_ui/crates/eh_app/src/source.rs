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
            // The import applies on a later tick and rebuilds the view
            // (C eh_local_import_scanner → async apply chain); its
            // progress shows on the sync sheet until it lands.
            app.browser.open = false;
            crate::local::kick_import(app);
            app.refresh_shelf();
        }
        Source::Folder => {
            crate::local::start_browse(app);
        }
    }
}
