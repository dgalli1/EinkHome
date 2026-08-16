/* bs_popups.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_model.h"
#include "bs_store.h"
#include "bs_ui.h"

/* Dim the content area with the LGRAY hatch every modal sheet draws
 * behind itself.  `y0` is where the dim starts: popups keep the top
 * bar undimmed (its icons — the spinning sync glyph among them — stay
 * fully visible), full-screen overlays dim from the very top. */
void
bs_dim_content(int y0)
{
    int w = ScreenWidth();
    for (int yy = y0; yy < bs_content_bottom(); yy += 2)
        DrawLine(0, yy, w, yy, LGRAY);
}

/* Store the four tallied counters into the (optional) output pointers. */
static void
progress_set_outs(int *total_out, int total, int *done_out, int done,
                  int *failed_out, int failed, int *active_out, int active)
{
    if (total_out)
        *total_out = total;
    if (done_out)
        *done_out = done;
    if (failed_out)
        *failed_out = failed;
    if (active_out)
        *active_out = active;
}

/* Tally the open download queue (falling back to the whole-batch tally
 * when a download-all batch is active, since the queue only holds the
 * current slice).  Shared by the popup bar and its status line. */
void
bs_dl_progress_metrics(int *total_out, int *done_out, int *failed_out, int *active_out)
{
    int total = 0, done = 0, failed = 0, active = 0;
    for (int i = 0; i < bs_g_download_count; i++) {
        total++;
        if (bs_g_downloads[i].state == 2)
            done++;
        else if (bs_g_downloads[i].state == 3)
            failed++;
        if (bs_g_downloads[i].state == 0 || bs_g_downloads[i].state == 1)
            active++;
    }
    if (bs_g_dl_batch_total > 0) {
        total = bs_g_dl_batch_total;
        done = bs_g_dl_batch_done;
        failed = bs_g_dl_batch_failed;
    }
    /* Retries can settle the same slot twice; keep the fill bounded. */
    if (done > total)
        done = total;
    if (done + failed > total)
        failed = total - done;
    if (bs_g_dl_batch_active)
        active++;
    progress_set_outs(total_out, total, done_out, done,
                      failed_out, failed, active_out, active);
}

/* Single batch progress bar for the download popup: one bar for the
 * whole open batch, filled by done/total, with a striped overlay on the
 * unfilled portion while anything is still in flight.  The bar spans
 * [x, x+w); the label sits above it. */
void
bs_draw_dl_progress(int x, int y, int w)
{
    int total = 0, done = 0, failed = 0, active = 0;
    bs_dl_progress_metrics(&total, &done, &failed, &active);
    if (total <= 0)
        return;

    ifont *f = OpenFont(DEFAULTFONT, 22, 0);
    int    label_h = 26;
    char   label[48];
    if (active > 0)
        snprintf(label, sizeof label, bs_i18n("dl.progress"), done, total);
    else if (failed > 0 && done == 0)
        snprintf(label, sizeof label, bs_i18n("dl.failed_count"), failed);
    else
        snprintf(label, sizeof label, bs_i18n("dl.complete"), done);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(x + 4, y + 2, label);
        CloseFont(f);
    }

    int bar_y = y + label_h;
    int bar_h = BS_DL_BAR_H - label_h - 6;
    if (bar_h < 8)
        bar_h = 8;
    if (w < 16)
        w = 16;
    DrawRect(x, bar_y, w, bar_h, BLACK);
    int settled = done + failed;
    int fill = (settled * w) / total;
    if (fill > 2)
        FillArea(x + 1, bar_y + 1, fill - 2, bar_h - 2, BLACK);
    /* Striped "in progress" overlay across the unfinished portion. */
    if (active > 0) {
        for (int sx = x + 1 + fill; sx < x + w - 1; sx += 6)
            DrawLine(sx, bar_y + 1, sx + 2, bar_y + bar_h - 2, DGRAY);
    }
}

/* Cancel-button rect inside the download popup: directly right of the
 * batch progress bar (shared by the draw path and the tap hit-test).
 * The bar row runs at py + CTX_TITLE_H + 64 and spans the sheet width
 * minus the button column, so the button shares the bar's row instead
 * of sitting in the popup's title corner. */
void
bs_dl_cancel_rect(int *x, int *y)
{
    int px, py, pw, ph;
    bs_dl_popup_geom(&px, &py, &pw, &ph);
    int bar_y = py + BS_CTX_TITLE_H + 64;
    *x = px + pw - BS_CTX_PAD - BS_DL_CANCEL_SIZE;
    *y = bar_y + (BS_DL_BAR_H - BS_DL_CANCEL_SIZE) / 2;
}

/* Sheet-only repaint (no dim) — forward-declared for refresh_dl_popup,
 * which runs before draw_dl_popup in file order. */
static void draw_dl_popup_sheet(void);

/* Repaint just the download-popup sheet (progress bar, current item,
 * status line).  The download job's completion calls this on every
 * queue change: the shelf around the popup is untouched during a
 * download, so a sheet-sized partial keeps the e-ink flicker local
 * instead of re-flashing the whole content area once per item (which
 * is what redraw_shelf() did — three times per finished download). */
void
bs_refresh_dl_popup(void)
{
    int px, py, pw, ph;

    if (!bs_g_state.dl_popup)
        return;
    /* Sheet only: the dim already ran when the popup opened, and
     * re-running the ~570-line dim loop on every finished item just
     * to flush the sheet rect is pure waste. */
    draw_dl_popup_sheet();
    bs_dl_popup_geom(&px, &py, &pw, &ph);
    PartialUpdate(px, py, pw, ph);
}

/* Centred 3/4-width sheet geometry, shared by the dl and sync popups
 * (they only differ in height). */
static void
popup_geom(int *px, int *py, int *pw, int *ph, int ph_const)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    *pw = w * 3 / 4;
    *ph = ph_const;
    *px = (w - *pw) / 2;
    *py = (h - *ph) / 2;
}

/* Download-popup sheet geometry: a centred 3/4-width sheet.  Shared by
 * the draw path, the cancel-button rect, and the popup-only refresh. */
void
bs_dl_popup_geom(int *px, int *py, int *pw, int *ph)
{
    popup_geom(px, py, pw, ph, 320);
}

/* Download-progress popup: a centred modal sheet over a dimmed shelf.
 * Title, the current item, the batch progress bar, and a status line.
 * A cancel (X) button right of the progress bar aborts the whole
 * queue — the in-flight fetch is told to cancel (it will not rename
 * its .part file into place), but QuickDownload still blocks to its
 * timeout, so it is left to finish while every queued item is dropped
 * (see cancel_downloads).  Shown whenever downloads run (book press,
 * context-menu Download, Download all).  While any download is active
 * the popup is non-dismissable — downloads never run in the
 * background; once the queue drains a tap or Back closes it.  When
 * the popup was opened by a single-book press (dl_popup_auto_open),
 * dl_job_done() launches the reader as soon as the queue drains. */
void
bs_draw_dl_popup(void)
{
    /* Dim the shelf body below the top bar and above the panel band,
     * so the top-bar icons (the spinning sync glyph among them) stay
     * fully visible while the download runs. */
    bs_dim_content(BS_TOP_BAR_H);
    draw_dl_popup_sheet();
    bs_LOG("[bookshelf] draw_dl_popup open auto_open=%d count=%d\n",
        bs_g_state.dl_popup_auto_open,
        bs_g_download_count);
}

/* The popup sheet itself, without the dim behind it.  The dim is only
 * repainted on open (and on redraw_shelf) — per-item refresh_dl_popup
 * repaints the sheet alone. */
static void
draw_dl_popup_sheet(void)
{
    int pw, ph, px, py;
    bs_dl_popup_geom(&px, &py, &pw, &ph);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    /* Cancel (X) button right of the progress bar.  Always drawn:
     * while a download is active it aborts the queue; once the queue
     * has drained it just closes the popup, like a tap anywhere else. */
    int cx, cy;
    bs_dl_cancel_rect(&cx, &cy);
    FillArea(cx, cy, BS_DL_CANCEL_SIZE, BS_DL_CANCEL_SIZE, WHITE);
    DrawRect(cx, cy, BS_DL_CANCEL_SIZE, BS_DL_CANCEL_SIZE, BLACK);
    DrawLine(cx + 16, cy + 16, cx + BS_DL_CANCEL_SIZE - 16, cy + BS_DL_CANCEL_SIZE - 16, BLACK);
    DrawLine(cx + BS_DL_CANCEL_SIZE - 16, cy + 16, cx + 16, cy + BS_DL_CANCEL_SIZE - 16, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 30, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + BS_CTX_PAD, py + 18, bs_i18n("dl.title"));
        CloseFont(tf);
    }
    DrawLine(px + BS_CTX_PAD, py + BS_CTX_TITLE_H - 1, px + pw - BS_CTX_PAD, py + BS_CTX_TITLE_H - 1, LGRAY);

    /* Current item: the first queued/in-flight entry, else the last one. */
    const BsDownloadItem *cur = NULL;
    for (int i = 0; i < bs_g_download_count; i++) {
        if (bs_g_downloads[i].state == 0 || bs_g_downloads[i].state == 1) {
            cur = &bs_g_downloads[i];
            break;
        }
    }
    if (cur == NULL && bs_g_download_count > 0)
        cur = &bs_g_downloads[bs_g_download_count - 1];
    if (cur != NULL) {
        ifont *cf = OpenFont(DEFAULTFONTB, 26, 0);
        if (cf != NULL) {
            SetFont(cf, BLACK);
            char trunc[BS_MAX_TITLE_LEN];
            snprintf(trunc, sizeof trunc, "%s", cur->title);
            bs_utf8_fit_width(trunc, sizeof trunc, pw - 2 * BS_CTX_PAD);
            DrawString(px + BS_CTX_PAD, py + BS_CTX_TITLE_H + 22, trunc);
            CloseFont(cf);
        }
    }

    bs_draw_dl_progress(
        px + BS_CTX_PAD, py + BS_CTX_TITLE_H + 64, pw - 2 * BS_CTX_PAD - BS_DL_CANCEL_SIZE - BS_DL_CANCEL_GAP);

    int total = 0, done = 0, failed = 0, active = 0;
    bs_dl_progress_metrics(&total, &done, &failed, &active);
    ifont *sf = OpenFont(DEFAULTFONT, 22, 0);
    if (sf != NULL) {
        SetFont(sf, DGRAY);
        const char *hint;
        if (active > 0)
            hint = bs_i18n("dl.in_progress");
        else if (failed > 0 && done + failed >= total)
            hint = bs_i18n("dl.failed");
        else
            hint = bs_i18n("dl.tap_close");
        DrawString(px + BS_CTX_PAD, py + BS_CTX_TITLE_H + 64 + BS_DL_BAR_H + 12, hint);
        CloseFont(sf);
    }
}

/* ── sync progress popup ─────────────────────────────────────────────── */

void
bs_sync_popup_geom(int *px, int *py, int *pw, int *ph)
{
    popup_geom(px, py, pw, ph, 190);
}

/* Title / status line for the current sync stage.  The sub-line carries
 * the counter (batch number / scanned books / result count). */
static const char *
sync_popup_line(int *sub)
{
    *sub = 0;
    switch (bs_g_state.sync_stage) {
    case BS_SYNC_STAGE_META:
        *sub = 1;
        return bs_i18n("sync.meta");
    case BS_SYNC_STAGE_SCAN:
        *sub = 2;
        return bs_i18n("sync.scan");
    case BS_SYNC_STAGE_COVERS:
        *sub = 3;
        return bs_i18n("sync.covers");
    case BS_SYNC_STAGE_FAIL:
        return bs_i18n("status.fail");
    default:
        return bs_i18n("sync.done");
    }
}

/* The sub-line under the sync stage title, carrying the counter
 * (batch number / scanned books / result count). */
static void
sync_subline(char *subline, size_t n, int sub)
{
    switch (sub) {
    case 1:
        snprintf(subline, n, bs_i18n("sync.batch"), bs_g_state.sync_round);
        break;
    case 2:
        snprintf(subline, n, bs_i18n("sync.books"), bs_g_state.sync_scan);
        break;
    case 3: {
        int done = 0, total = 0;
        bs_cover_warm_progress(&done, &total);
        if (total > 0)
            snprintf(subline, n, bs_i18n("sync.cover_count"), done, total);
        else
            snprintf(subline, n, "%s", bs_i18n("sync.covers"));
        break;
    }
    default:
        snprintf(subline, n, bs_i18n("sync.books"), bs_view_total());
        break;
    }
}

/* Sheet only (no dim): used by sync_popup_refresh, whose frequent
 * stage/counter updates must not re-run the dim loop every time. */
static void
draw_sync_popup_sheet(void)
{
    int px, py, pw, ph;
    bs_sync_popup_geom(&px, &py, &pw, &ph);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 30, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + BS_CTX_PAD, py + 18, bs_i18n("action.sync"));
        CloseFont(tf);
    }
    DrawLine(px + BS_CTX_PAD, py + BS_CTX_TITLE_H - 1, px + pw - BS_CTX_PAD, py + BS_CTX_TITLE_H - 1, LGRAY);

    int         sub;
    const char *line = sync_popup_line(&sub);
    ifont      *lf = OpenFont(DEFAULTFONTB, 28, 0);
    if (lf != NULL) {
        SetFont(lf, BLACK);
        DrawString(px + BS_CTX_PAD, py + BS_CTX_TITLE_H + 24, line);
        CloseFont(lf);
    }
    ifont *sf = OpenFont(DEFAULTFONT, 24, 0);
    if (sf != NULL) {
        SetFont(sf, DGRAY);
        char subline[96];
        sync_subline(subline, sizeof subline, sub);
        DrawString(px + BS_CTX_PAD, py + BS_CTX_TITLE_H + 68, subline);
        CloseFont(sf);
    }

    /* Covers stage: a progress bar under the "N / M" line, filled by the
     * warm pass's examined/total, with a striped overlay while still
     * downloading.  It sits inside the 190px sheet (bar top 168, h 12),
     * sized to leave ~10px of padding below so it does not crowd the
     * bottom edge. */
    if (sub == 3) {
        int done = 0, total = 0;
        bs_cover_warm_progress(&done, &total);
        int bar_x = px + BS_CTX_PAD;
        int bar_w = pw - 2 * BS_CTX_PAD;
        int bar_y = py + BS_CTX_TITLE_H + 96;
        int bar_h = 12;
        /* Clear any earlier bar (the sheet is repainted each refresh). */
        FillArea(bar_x, bar_y, bar_w, bar_h, WHITE);
        DrawRect(bar_x, bar_y, bar_w, bar_h, BLACK);
        if (total > 0) {
            int fill = (done * bar_w) / total;
            if (fill > 2)
                FillArea(bar_x + 1, bar_y + 1, fill - 2, bar_h - 2, BLACK);
            if (done < total && bs_cover_warm_active()) {
                for (int sx = bar_x + 1 + fill; sx < bar_x + bar_w - 1; sx += 6)
                    DrawLine(sx, bar_y + 1, sx + 2, bar_y + bar_h - 2, DGRAY);
            }
        }
    }
}

/* Sync-progress sheet: a centred modal card over the dimmed shelf,
 * telling the user what the in-flight sync is doing (metadata batch,
 * local scan, covers, done/failed).  Only manual syncs open it; boot
 * and timer syncs run silently behind the spinning top-bar icon. */
void
bs_draw_sync_popup(void)
{
    bs_dim_content(BS_TOP_BAR_H);
    draw_sync_popup_sheet();
}

void
bs_sync_popup_refresh(void)
{
    if (!bs_g_state.sync_popup)
        return;
    int px, py, pw, ph;
    bs_sync_popup_geom(&px, &py, &pw, &ph);
    draw_sync_popup_sheet();
    PartialUpdate(px, py, pw, ph);
}

void
bs_sync_popup_open(void)
{
    if (bs_g_state.sync_popup)
        return;
    bs_g_state.sync_popup = 1;
    bs_g_state.sync_stage = BS_SYNC_STAGE_META;
    /* Zero the batch/scan counters only on a fresh start: a tap while
     * a sync is already in flight (do_sync skips it) just re-opens the
     * sheet over the live run, and resetting the counters would make
     * the progress lines jump backwards. */
    if (bs_g_state.sync_state == 0) {
        bs_g_state.sync_round = 0;
        bs_g_state.sync_scan = 0;
    }
    bs_sync_popup_refresh();
}

void
bs_sync_popup_close(void)
{
    if (!bs_g_state.sync_popup)
        return;
    bs_g_state.sync_popup = 0;
    bs_redraw_shelf();
}

/* Close the popup shortly after the sync finished (or failed).  While
 * covers are still loading or the post-sync warm pass is downloading the
 * library, the popup stays on the COVERS stage and this timer re-arms,
 * repainting the sheet each second so the progress bar advances; when
 * the covers finish it flashes "Sync complete" before closing. */
static void
sync_popup_close_tick(void *ctx)
{
    (void)ctx;
    if (!bs_g_state.sync_popup)
        return;
    if (bs_g_state.sync_stage == BS_SYNC_STAGE_COVERS) {
        if (bs_g_cover_armed || bs_cover_warm_active()) {
            /* Still downloading: keep it up and refresh the bar. */
            bs_sync_popup_refresh();
            SetWeakTimerEx("bsyncp", sync_popup_close_tick, NULL, 1000);
            return;
        }
        /* Covers drained (visible page + full-library warm): show the
         * "done" line for a beat, then close. */
        bs_g_state.sync_stage = BS_SYNC_STAGE_DONE;
        bs_sync_popup_refresh();
        SetWeakTimerEx("bsyncp", sync_popup_close_tick, NULL, 900);
        return;
    }
    bs_sync_popup_close();
}

void
bs_sync_popup_auto_close(int delay_ms)
{
    SetWeakTimerEx("bsyncp", sync_popup_close_tick, NULL, delay_ms);
}

void
bs_sync_popup_finish(void)
{
    if (!bs_g_state.sync_popup)
        return;
    bs_g_state.sync_stage = BS_SYNC_STAGE_COVERS;
    bs_sync_popup_refresh();
    bs_cover_schedule_next();
    /* Arm the close/rearm tick promptly (1s) so it both keeps the popup
     * up while covers or the warm pass are running and repaints the
     * progress bar; a short link that is not warm-active closes soon. */
    if (bs_g_cover_armed || bs_cover_warm_active())
        bs_sync_popup_auto_close(1000);
    else
        bs_sync_popup_auto_close(900);
}

void
bs_sync_popup_fail(void)
{
    if (!bs_g_state.sync_popup)
        return;
    bs_g_state.sync_stage = BS_SYNC_STAGE_FAIL;
    bs_sync_popup_refresh();
    bs_sync_popup_auto_close(1500);
}
