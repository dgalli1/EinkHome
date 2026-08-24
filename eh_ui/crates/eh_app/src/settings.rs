//! The Settings screen (C eh_draw_overlay_settings): full-screen white, a
//! shared overlay header (back chevron + centred title), four editable
//! rows (API host / API key / Reader app / Download folder) + a System app
//! row, then the Save / Show logs / Licenses buttons.  The API host + key
//! rows edit through the firmware's on-screen keyboard (async commit,
//! drained by the app on its next event).

/// Row rhythm (C EH_SETTINGS_*).
pub const MARGIN: u32 = 32;
pub const ROW_H: u32 = 120;
pub const BTN_H: u32 = 96;
pub const ROWS_Y0: u32 = 112;
