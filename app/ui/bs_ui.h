#ifndef BS_UI_H
#define BS_UI_H

/* bs_ui.h — Drawing (bs_screen.c, bs_topbar.c, bs_grid.c, bs_search.c,
 * bs_popups.c, bs_overlays.c, bs_logview.c): top bar, grid/list, popups,
 * overlays, settings page, and the shared geometry helpers. */

#include "bs_core.h"

extern int bs_g_self_panel;    /* 1 = we draw the status strip ourselves */

extern int bs_g_display_color; /* 1 = colour display (device_display_colormask) */

extern int bs_g_settings_edit;

extern char bs_g_settings_kb_buf[260];

/* UTF-8 truncation helpers (bs_screen.c), shared by every title/term
 * display path: utf8_cap enforces a byte budget, utf8_fit_width a
 * pixel width — neither ever splits a multibyte character. */
void bs_utf8_cap(char *s, size_t cap);

void bs_utf8_fit_width(char *s, size_t cap, int maxw);

void bs_draw_overlay_source(void);

void bs_source_geom(int *px, int *py, int *pw, int *ph);

void bs_draw_text_centered(ifont *f, int cx, int cy, const char *text, int color);

void bs_draw_button(int x, int y, int w, int h, int selected, const char *label,
                 int label_size, int label_color);

/* draw_button variant that reuses a pre-opened font (caller owns the
 * OpenFont/CloseFont around the pass). */
void bs_draw_button_font(int x, int y, int w, int h, int selected, const char *label,
                      int label_size, ifont *f, int label_color);

/* Dim the content area with the LGRAY hatch (modal-sheet backing).  `y0`
 * is where the dim starts: popups keep the top bar undimmed (its icons —
 * the spinning sync glyph among them — stay fully visible), full-screen
 * overlays dim from the very top.  Shared by the download/sync popups
 * (bs_popups.c) and the source-chooser overlay (bs_overlays.c). */
void bs_dim_content(int y0);

void bs_draw_top_bar(void);

void bs_draw_search_icon(void);

void bs_draw_search_tab(void);

int bs_downloads_pending(void);

/* Suggestion band geometry + rendering (bs_search.c); hit-test (bs_input.c). */
void bs_suggest_band(int *y_top, int *y_bot);

void bs_draw_suggestions(int y_top, int y_bot);

BsCoverSlot *bs_cover_slot(const char *id, int create);

int bs_view_cols(void);

void bs_stamp_panel(void);

int bs_content_bottom(void);

int bs_view_rows(void);

int bs_view_pagesize(void);

void bs_grid_geom(int *top, int *bot, int *cell_w, int *cell_h);

int bs_tile_rect_for_index(int idx, int *x, int *y, int *w, int *h);

void bs_cover_rect(int tx, int ty, int tw, int th, int *cx, int *cy, int *cw,
                int *ch);

void bs_draw_system_strip(void);

void bs_cover_schedule_next(void);

void bs_blit_cover(int cx, int cy, int cw, int ch, const BsBook *b);

void bs_draw_series_stack_back(int cx, int cy, int cw, int ch);

void bs_draw_series_stack_badge(int cx, int cy, int cw, int ch, int count, ifont *bf);

void bs_draw_thumbnail(int x, int y, int w, int h, const BsTileRow *tr, int vi);

int bs_history_pagesize(void);

int bs_current_pages(void);

void bs_draw_dl_popup(void);

void bs_dl_popup_geom(int *px, int *py, int *pw, int *ph);

void bs_refresh_dl_popup(void);

void bs_dl_cancel_rect(int *x, int *y);

void bs_dl_progress_metrics(int *total, int *done, int *failed, int *active);

void bs_draw_sync_popup(void);

void bs_sync_popup_geom(int *px, int *py, int *pw, int *ph);

void bs_sync_popup_open(void);

void bs_sync_popup_close(void);

/* Timer-driven popup close, armed by finish/fail — and re-armed by the
 * cover tick (bs_grid.c) while covers are still loading. */
void bs_sync_popup_auto_close(int delay_ms);

void bs_sync_popup_refresh(void);

void bs_sync_popup_finish(void); /* sync ended: covers/done stage + auto-close */

void bs_sync_popup_fail(void);   /* sync failed: show the error, then close */

void bs_draw_log_view(void);

void bs_draw_scroll_buttons(int up_ok, int down_ok);

void bs_draw_scroll_buttons_at(int up_ok, int down_ok, int y0);

int bs_hit_scroll_button(int x, int y);

int bs_hit_scroll_button_at(int x, int y, int y0);

void bs_redraw_shelf(void);

/* Draw the shelf content without flushing, so a caller can follow with
 * a single refresh of its choosing (the keyboard-commit path draws here
 * then does one full-screen FullUpdate). */
void bs_draw_shelf_nofb(void);

/* Lighter page-turn repaint: grid/list body + pager band partial
 * updates only (the top bar is untouched by a page flip). */
void bs_flip_page(void);

void bs_show_hourglass(void);

void bs_flush_content(void);

void bs_draw_grid(void);

void bs_cover_tick(void *ctx);

void bs_draw_pager(void);

void bs_draw_overlay_menu(void);

void bs_draw_overlay_more(void);

/* 1 = any modal overlay or popup is up (input routing, long-press
 * arming, and background work like cover fetches should pause). */
int bs_modal_open(void);

void bs_settings_keyboard_handler(char *buffer);

const char *bs_settings_reader_label(void);

void bs_settings_draw_row(int y, const char *label, const char *value,
                       int editing, ifont *lf, ifont *vf);

void bs_settings_draw_button(int y, const char *label, int filled, ifont *f);

/* Back-button rect in the settings header: shared by the draw path
 * (bs_overlays.c) and the tap hit-test (bs_input.c) so the tappable
 * region always matches the painted button. */
void bs_settings_back_rect(int *bx, int *by, int *bw, int *bh);

void bs_draw_overlay_settings(void);

void bs_draw_sync_icon(void);

void bs_sync_set_active(int on);

#endif /* BS_UI_H */
