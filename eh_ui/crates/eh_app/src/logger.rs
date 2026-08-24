//! E2E log backend: appends the `<app>/bookshelf.log` the test harness
//! reads, with the exact markers the C app emitted (banner line,
//! `[bookshelf] EVT_INIT panel_h=… sw=… sh=…`, `do_sync`, `draw_grid`).
//!
//! The harness (tests/support/bookshelf) couples to nothing but substring
//! tokens in a per-invocation slice of `bookshelf.log` (starting at the
//! latest open banner):
//!   - the banner `--- bookshelf.app log opened (argv0=…) ---` must appear
//!     exactly once per process life (measured via count_log_openings);
//!   - `[bookshelf] EVT_INIT panel_h=N sw=W sh=H` is the geometry source
//!     (_parse_app_geometry) AND the default wait_for_respawn ready marker;
//!   - a `do_sync` token + a `draw_grid` token after the banner gate
//!     _wait_fresh_bookshelf.
//!
//! Path resolution mirrors C `eh_log_open`: `$PBEMU_LOG_DIR/bookshelf.log`,
//! else `<app_dir>/bookshelf.log`, else `/tmp/bookshelf.log`.  The SDL
//! harness sets PBEMU_LOG_DIR to its per-instance run dir; the device/facade
//! passes the app dir.  The log is append-only, one process at a time.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LOGGER: OnceLock<Mutex<Option<File>>> = OnceLock::new();
/// The resolved log path (set by [`init`]); [`crate::viewer`] reads this
/// for "Show logs" so it can never disagree with where lines are written.
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

fn resolve_path(app_dir: Option<&str>) -> PathBuf {
    if let Ok(d) = std::env::var("PBEMU_LOG_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d).join("bookshelf.log");
        }
    }
    if let Some(d) = app_dir {
        if !d.is_empty() {
            return PathBuf::from(d).join("bookshelf.log");
        }
    }
    PathBuf::from("/tmp/bookshelf.log")
}

/// Open the log once (idempotent), writing the C open banner.  A failed
/// open degrades to a silent no-op — the app must never fail to boot over
/// an unwritable log (the C app falls back rather than aborting too).
pub fn init(app_dir: Option<&str>) {
    LOGGER.get_or_init(|| {
        let path = resolve_path(app_dir);
        let _ = LOG_PATH.set(path.clone());
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        if let Some(f) = file.as_mut() {
            let argv0 = std::env::args().next().unwrap_or_else(|| "(null)".into());
            // The banner must appear exactly once per process.
            let _ = writeln!(f, "--- bookshelf.app log opened (argv0={argv0}) ---");
        }
        Mutex::new(file)
    });
}

/// The path `log` writes to (None before [`init`]).
pub fn path() -> Option<&'static PathBuf> {
    LOG_PATH.get()
}

/// Append a line to the e2e log (no-op if the log never opened); mirrors to
/// stderr on the host for the SDL/host runs.
pub fn log(msg: &str) {
    if let Some(mtx) = LOGGER.get() {
        if let Ok(mut guard) = mtx.lock() {
            if let Some(f) = guard.as_mut() {
                let _ = writeln!(f, "{msg}");
            }
        }
    }
    #[cfg(not(target_arch = "arm"))]
    {
        eprintln!("{msg}");
    }
}

/// Emit the C `EVT_INIT panel_h=… sw=… sh=…` geometry line (the harness's
/// geometry + respawn-ready source).
pub fn evt_init(panel_h: u32, sw: u32, sh: u32) {
    log(&format!(
        "[bookshelf] EVT_INIT panel_h={panel_h} sw={sw} sh={sh}"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    // PBEMU_LOG_DIR is a process-global; these tests flip it and so
    // serialize on one mutex (same pattern as i18n's LANG_LOCK).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` with PBEMU_LOG_DIR set to `value` (None = removed),
    /// restoring the previous environment afterwards.
    fn with_log_dir<R>(value: Option<&str>, f: impl FnOnce() -> R) -> R {
        let _g = ENV_LOCK.lock();
        let saved = std::env::var("PBEMU_LOG_DIR").ok();
        match value {
            Some(v) => std::env::set_var("PBEMU_LOG_DIR", v),
            None => std::env::remove_var("PBEMU_LOG_DIR"),
        }
        let out = f();
        match saved {
            Some(v) => std::env::set_var("PBEMU_LOG_DIR", v),
            None => std::env::remove_var("PBEMU_LOG_DIR"),
        }
        out
    }

    // The SDL e2e harness reads THIS file from a per-instance slice;
    // wrong precedence hides the log where the harness can't see it.
    #[test]
    fn env_dir_wins_over_app_dir() {
        with_log_dir(Some("/run/harness"), || {
            assert_eq!(
                resolve_path(Some("/device/appdir")),
                PathBuf::from("/run/harness/bookshelf.log")
            );
        });
    }

    #[test]
    fn empty_env_falls_through_to_app_dir() {
        with_log_dir(Some(""), || {
            assert_eq!(
                resolve_path(Some("/device/appdir")),
                PathBuf::from("/device/appdir/bookshelf.log")
            );
        });
    }

    #[test]
    fn app_dir_used_when_env_unset() {
        with_log_dir(None, || {
            assert_eq!(
                resolve_path(Some("/device/appdir")),
                PathBuf::from("/device/appdir/bookshelf.log")
            );
        });
    }

    #[test]
    fn tmp_fallback_without_any_dir() {
        with_log_dir(None, || {
            assert_eq!(resolve_path(None), PathBuf::from("/tmp/bookshelf.log"));
            // An empty app dir is as good as none.
            assert_eq!(resolve_path(Some("")), PathBuf::from("/tmp/bookshelf.log"));
        });
    }
}
