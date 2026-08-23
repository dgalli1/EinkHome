//! The modal download-progress popup (C eh_draw_dl_popup): a dim + a
//! centred white sheet showing the remaining count (the count changes as
//! the queue drains, so the frame changes during a batch — the e2e
//! suite's event-loop-alive proof).  Modal while a batch is in flight.

use eh_hal::{Framebuffer, Rect};

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

pub fn draw_download_popup<B: Framebuffer>(
    surf: &mut eh_render::Surface,
    app: &mut crate::app::App<B>,
    dirty: &mut Vec<Rect>,
) {
    use eh_shell::{GRAY_BLACK, GRAY_WHITE};
    let w = surf.width();
    let h = app.content_bottom;
    // Dim starting BELOW the top bar (C eh_dim_content(EH_TOP_BAR_H)): the
    // icons — the spinning sync glyph among them — stay fully visible.
    // Centred on the full content height like the C popup.
    let sh = super::sheet::open_sheet(
        surf,
        dirty,
        h,
        crate::appui::TOP_BAR_H,
        h,
        h,
        160,
        false,
    );
    let font = crate::shelf::shelf_font();
    let mut g = eh_render::Glyph::new();
    eh_render::draw_text(
        surf,
        font,
        28.0,
        crate::i18n::tr("dl.in_progress"),
        (sh.px + 32) as i32,
        (sh.py + 72) as i32,
        GRAY_BLACK,
        &mut g,
    );
    let label = if app.dl_total > 0 && !app.dl_batch_all {
        format!(
            "{}, {}",
            crate::i18n::trn("dl.complete", &[app.dl_done as i64]),
            crate::i18n::trn("dl.failed_count", &[app.dl_failed as i64])
        )
    } else {
        crate::i18n::trn("dl.remaining", &[app.downloader.pending as i64])
    };
    eh_render::draw_text(surf, font, 24.0, &label, (sh.px + 32) as i32, (sh.py + 120) as i32, GRAY_BLACK, &mut g);
    // Cancel X button (C draw_dl_popup_sheet's boxed X).
    let cr = dl_cancel_rect(w, h);
    surf.fill_gray(cr, GRAY_WHITE);
    surf.rect_outline(cr, 2, GRAY_BLACK);
    surf.line(
        (cr.x + 12) as i32,
        (cr.y + 12) as i32,
        (cr.x + cr.w - 12) as i32,
        (cr.y + cr.h - 12) as i32,
        3,
        GRAY_BLACK,
    );
    surf.line(
        (cr.x + cr.w - 12) as i32,
        (cr.y + 12) as i32,
        (cr.x + 12) as i32,
        (cr.y + cr.h - 12) as i32,
        3,
        GRAY_BLACK,
    );
}
