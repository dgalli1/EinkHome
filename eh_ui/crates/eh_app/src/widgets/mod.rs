//! Popup state machines that outlive their old draw halves (the sheets
//! are Slint markup now — `ui/*.slint`): the chooser display-key maps and
//! the pure progress-bar math.
pub mod chooser;
pub mod context;
pub mod download;
pub mod progress_bar;
pub mod sync_popup;
