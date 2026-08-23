//! Reusable popup widgets (the C app's eh_popups.c sheets).
//!
//! Every modal popup is a [`sheet::open_sheet`] scaffold — dim + centred
//! white panel — plus widget-specific content. The geometry lives in one
//! place so draw and tap hit-testing cannot drift.
pub mod chooser;
pub mod context;
pub mod download;
pub mod menu_row;
pub mod sheet;
pub mod sync_popup;
