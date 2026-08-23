//! The modal sync-progress sheet and its state machine (C eh_draw_sync_
//! popup / draw_sync_popup_sheet + eh_g_state.sync_popup): a dim below
//! the top bar + a centred 190px sheet — title band, the phase line, the
//! counter subline, and during the covers stage a striped progress bar.

use eh_hal::{Framebuffer, Rect};

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

/// Sync-sheet height (C popup_geom(..., 190)).
pub(crate) const SYNC_SHEET_H: u32 = 190;
/// Auto-close delays ported from eh_popups.c: the Done line flashes for
/// 900 ms before the sheet closes; the Fail line shows for 1500 ms.
pub(crate) const SYNC_DONE_CLOSE_MS: u64 = 900;
pub(crate) const SYNC_FAIL_CLOSE_MS: u64 = 1500;

pub fn draw_sync_popup<B: Framebuffer>(
    surf: &mut eh_render::Surface,
    app: &mut crate::app::App<B>,
    dirty: &mut Vec<Rect>,
) {
    use eh_shell::{GRAY_BLACK, GRAY_DGRAY, GRAY_LGRAY};
    let h = app.content_bottom;
    // Dim starting BELOW the top bar (C eh_dim_content(EH_TOP_BAR_H)): the
    // icons — the spinning sync glyph among them — stay fully visible.
    let sh = super::sheet::open_sheet(
        surf,
        dirty,
        h,
        crate::appui::TOP_BAR_H,
        h,
        h,
        SYNC_SHEET_H,
        false,
    );
    const PAD: u32 = 24; // C EH_CTX_PAD
    const TITLE_H: u32 = 72; // C EH_CTX_TITLE_H
    let font = crate::shelf::shelf_font();
    let mut g = eh_render::Glyph::new();
    // C DrawString is TOP-anchored (title top at py+18); our draw_text
    // takes a BASELINE — add the face's ascent or the title rides above
    // the sheet border and gets clipped.
    let title_asc = font.line_h(30.0).0 as i32;
    eh_render::draw_text(
        surf,
        font,
        30.0,
        crate::i18n::tr("action.sync"),
        (sh.px + PAD) as i32,
        (sh.py + 18) as i32 + title_asc,
        GRAY_BLACK,
        &mut g,
    );
    surf.hline(sh.px + PAD, sh.py + TITLE_H - 1, sh.pw - 2 * PAD, 2, GRAY_LGRAY);

    // Whether the cover warm pass has drained — computed before the popup
    // borrow (the probe needs &mut self).
    let warm_drained = !app.cover_warm_active();
    let p = &app.sync_popup;
    let line;
    let subline;
    match p.stage {
        SyncStage::Meta => {
            line = crate::i18n::tr("sync.meta").to_string();
            subline = crate::i18n::trn("sync.batch", &[p.round as i64]);
        }
        SyncStage::Scan => {
            line = crate::i18n::tr("sync.scan").to_string();
            subline = crate::i18n::trn("sync.books", &[p.scanned as i64]);
        }
        SyncStage::Covers => {
            line = crate::i18n::tr("sync.covers").to_string();
            if p.covers_total > 0 {
                subline = crate::i18n::trn(
                    "sync.cover_count",
                    &[p.covers_done as i64, p.covers_total as i64],
                );
            } else {
                subline = crate::i18n::tr("sync.covers").to_string();
            }
        }
        SyncStage::Fail => {
            line = crate::i18n::tr("status.fail").to_string();
            subline = p.error.clone();
        }
        SyncStage::Done => {
            line = crate::i18n::tr("sync.done").to_string();
            subline = crate::i18n::trn("sync.books", &[app.store.count().unwrap_or(0)]);
        }
    }
    let asc28 = font.line_h(28.0).0 as i32;
    let asc24 = font.line_h(24.0).0 as i32;
    eh_render::draw_text(surf, font, 28.0, &line, (sh.px + PAD) as i32, (sh.py + TITLE_H + 24) as i32 + asc28, GRAY_BLACK, &mut g);
    eh_render::draw_text(surf, font, 24.0, &subline, (sh.px + PAD) as i32, (sh.py + TITLE_H + 68) as i32 + asc24, GRAY_DGRAY, &mut g);

    // Covers stage: progress bar under the counter (C draw_sync_popup_
    // sheet: bar top TITLE_H+96, h 12), filled by done/total with a
    // striped overlay over the unfilled part while covers still load.
    if p.stage == SyncStage::Covers && p.covers_total > 0 {
        use eh_shell::GRAY_WHITE;
        let bar = Rect { x: sh.px + PAD, y: sh.py + TITLE_H + 96, w: sh.pw - 48, h: 12 };
        surf.fill_gray(bar, GRAY_WHITE);
        surf.rect_outline(bar, 1, GRAY_BLACK);
        let fill = (p.covers_done * (bar.w - 2)) / p.covers_total;
        if fill > 0 {
            surf.fill_gray(
                Rect { x: bar.x + 1, y: bar.y + 1, w: fill.min(bar.w - 2), h: bar.h - 2 },
                GRAY_BLACK,
            );
        }
        let drained = warm_drained;
        let from = bar.x + 1 + fill;
        let mut sx = from;
        while sx + 3 < bar.x + bar.w - 1 && !drained {
            surf.line(sx as i32, (bar.y + 1) as i32, (sx + 2) as i32, (bar.y + bar.h - 2) as i32, 1, GRAY_DGRAY);
            sx += 6;
        }
    }
}
