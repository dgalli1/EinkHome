//! Reusable widget implementations: modal popups on the [`sheet`]
//! scaffold (the C app's eh_popups.c sheets), shared chrome (the overlay
//! [`header`]), and self-contained elements ([`menu_row`],
//! [`progress_bar`], [`search_input`]).  Geometry lives beside its draw
//! code so paint and tap hit-testing cannot drift.
pub mod chooser;
pub mod context;
pub mod download;
pub mod header;
pub mod menu_row;
pub mod progress_bar;
pub mod search_input;
pub mod sheet;
pub mod sync_popup;
