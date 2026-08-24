//! The modal sync-progress sheet and its state machine (C eh_draw_sync_
//! popup / draw_sync_popup_sheet + eh_g_state.sync_popup): a dim below
//! the top bar + a centred 190px sheet — title band, the phase line, the
//! counter subline, and during the covers stage a striped progress bar.

/// Stage of the sync-progress sheet (C EH_SYNC_STAGE_META/SCAN/COVERS/
/// DONE/FAIL).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SyncStage {
    /// Pulling metadata batches.
    Meta,
    /// The local-source library scan.
    Scan,
    /// The post-sync cover warm pass.
    Covers,
    /// Flashed briefly before the sheet auto-closes.
    Done,
    /// The chain failed; the error shows before the auto-close.
    Fail,
}

/// State machine for the sync-progress sheet (C eh_g_state.sync_popup +
/// sync_stage/sync_round/sync_scan + the `bsyncp` weak timer).
#[derive(Clone, Debug)]
pub struct SyncPopup {
    pub open: bool,
    pub stage: SyncStage,
    /// Metadata batch counter (C sync_round, shown as `batch N`).
    pub round: u32,
    /// Books scanned by the local import (C sync_scan).
    pub scanned: u32,
    /// Cover-pass counters for the striped bar (C eh_cover_warm_progress).
    pub covers_done: u32,
    pub covers_total: u32,
    /// The failure text for the Fail stage line.
    pub error: String,
    /// When the current stage was entered (drives the auto-close timing).
    pub stage_at: Option<std::time::Instant>,
}

impl Default for SyncPopup {
    fn default() -> Self {
        Self {
            open: false,
            stage: SyncStage::Meta,
            round: 0,
            scanned: 0,
            covers_done: 0,
            covers_total: 0,
            error: String::new(),
            stage_at: None,
        }
    }
}

/// Auto-close delays ported from eh_popups.c: the Done line flashes for
/// 900 ms before the sheet closes; the Fail line shows for 1500 ms.
pub(crate) const SYNC_DONE_CLOSE_MS: u64 = 900;
pub(crate) const SYNC_FAIL_CLOSE_MS: u64 = 1500;
