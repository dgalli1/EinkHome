//! The modal download-progress popup (C eh_draw_dl_popup): a dim + a
//! centred white sheet showing the remaining count (the count changes as
//! the queue drains, so the frame changes during a batch — the e2e
//! suite's event-loop-alive proof).  Modal while a batch is in flight.

use eh_hal::Rect;

/// Size of the download-popup X button (C EH_DL_CANCEL_SIZE).
pub const DL_CANCEL_SIZE: u32 = 48;

/// The download-popup cancel-button rect (C eh_dl_cancel_rect mirrored
/// onto this popup's sheet geometry): right edge of the sheet, aligned
/// with the status line.  Draw + tap share this, so they never drift.
pub fn dl_cancel_rect(w: u32, h: u32) -> Rect {
    let pw = w * 3 / 4;
    let ph = 160u32;
    let px = (w - pw) / 2;
    let py = h.saturating_sub(ph) / 2;
    Rect {
        x: px + pw - DL_CANCEL_SIZE - 24,
        y: py + 96,
        w: DL_CANCEL_SIZE,
        h: DL_CANCEL_SIZE,
    }
}
