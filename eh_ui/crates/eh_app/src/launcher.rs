//! The Applications launcher (C eh_launcher.c + eh_plat_pb_launcher.c): a
//! full-screen overlay with the shared header, a scrolling 3-column grid
//! (group headers span the width, app cells flow three per row), corner
//! scroll buttons when the column overflows, and a tap → NewTaskEx launch.
//!
//! Split by concern (see ARCHITECTURE.md): `discover` resolves the item
//! list — the firmware desktop configs (apps_db.json + view.json), the
//! /mnt/ext1/applications scan, and the eh_lc_* conditional-resolution
//! engine; `ui` lays out, paints and routes taps for the overlay screen.
mod discover;
mod ui;

/// Grid rhythm (C EH_LAUNCHER_*).
pub const COLS: u32 = 3;
pub const CELL_H: u32 = 232;
/// Per-item launch arguments (C EH_LAUNCHER_MAX_PARAMS).
pub const LAUNCHER_MAX_PARAMS: usize = 4;
pub const ICON_SZ: u32 = 120;
pub const GROUP_H: u32 = 64;
pub const MARGIN: u32 = 16;

pub use discover::build;
pub(crate) use ui::{body_rects, layout, scroll_of, split_label};
pub use ui::{drag_move, DRAG_SLOP};
