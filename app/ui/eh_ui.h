#ifndef EH_UI_H
#define EH_UI_H

/* eh_ui.h — Drawing (eh_screen.c, eh_topbar.c, eh_grid.c, eh_search.c,
 * eh_popups.c, eh_overlays.c, eh_logview.c): top bar, grid/list, popups,
 * overlays, settings page, and the shared geometry helpers. */

#include "eh_core.h"

extern int eh_g_self_panel;    /* 1 = we draw the status strip ourselves */

extern int eh_g_display_color; /* 1 = colour display (eh_plat_display_color) */

extern int eh_g_settings_edit;

extern char eh_g_settings_kb_buf[260];

/* UTF-8 truncation helpers (eh_screen.c), shared by every title/term
 * display path: utf8_cap enforces a byte budget, utf8_fit_width a
 * pixel width — neither ever splits a multibyte character. */
void eh_utf8_cap(char *s, size_t cap);

void eh_utf8_fit_width(char *s, size_t cap, int maxw);

void eh_draw_overlay_source(void);

void eh_source_geom(int *px, int *py, int *pw, int *ph);

void eh_draw_text_centered(ifont *f, int cx, int cy, const char *text, int color);

/* draw_button variant that reuses a pre-opened font (caller owns the
 * OpenFont/CloseFont around the pass). */
void eh_draw_button_font(int x, int y, int w, int h, int selected, const char *label,
                      int label_size, ifont *f, int label_color);

/* Shared primitive (eh_screen.c): 16-segment circle outline with the
 * 1px-down thickening used by the top-bar globe and the two magnifier
 * icons. */
void eh_draw_circle_outline(int cx, int cy, int r, int col);

/* Shared width of a non-NUL-terminated span (eh_screen.c), used by the
 * log viewer and licenses viewer word-wrap. */
int eh_span_width(const char *p, int len);

/* Dim the content area with the LGRAY hatch (modal-sheet backing).  `y0`
 * is where the dim starts: popups keep the top bar undimmed (its icons —
 * the spinning sync glyph among them — stay fully visible), full-screen
 * overlays dim from the very top.  Shared by the download/sync popups
 * (eh_popups.c) and the source-chooser overlay (eh_overlays.c). */
void eh_dim_content(int y0);

void eh_draw_top_bar(void);

void eh_draw_search_icon(void);
void eh_draw_layout_icon(void);
int eh_source_btn_w(void);

void eh_draw_search_tab(void);

int eh_downloads_pending(void);

/* Suggestion band geometry + rendering (eh_search.c); hit-test (eh_input.c). */
void eh_suggest_band(int *y_top, int *y_bot);

void eh_draw_suggestions(int y_top, int y_bot);

BsCoverSlot *eh_cover_slot(const char *id, int create);

int eh_view_cols(void);

void eh_stamp_panel(void);

int eh_content_bottom(void);

int eh_view_rows(void);

int eh_view_pagesize(void);

void eh_grid_geom(int *top, int *bot, int *cell_w, int *cell_h);
int eh_grid_x0(void);

int eh_tile_rect_for_index(int idx, int *x, int *y, int *w, int *h);

void eh_cover_rect(int tx, int ty, int tw, int th, int *cx, int *cy, int *cw,
                int *ch);

void eh_draw_system_strip(void);

void eh_cover_schedule_next(void);

/* Post-sync cover warm-up: fetch every remote book's cover into the
 * on-disk cache in the background so the library renders offline. */
void eh_cover_warm_start(void);
/* 1 while the warm pass is running (drives the sync-popup progress bar /
 * keep-open decision). */
int eh_cover_warm_active(void);
/* Fill *done / *total with the warm pass's progress; returns whether active. */
int eh_cover_warm_progress(int *done, int *total);

void eh_blit_cover(int cx, int cy, int cw, int ch, const BsBook *b);

void eh_draw_series_stack_back(int cx, int cy, int cw, int ch);

void eh_draw_series_stack_badge(int cx, int cy, int cw, int ch, int count, ifont *bf);

void eh_draw_thumbnail(int x, int y, int w, int h, const BsTileRow *tr, int vi);

/* Dimension-group drill actions (eh_grid.c). */
void eh_group_drill(const char *value);
void eh_group_drill_back(void);

int eh_history_pagesize(void);

int eh_current_pages(void);

void eh_draw_dl_popup(void);

void eh_dl_popup_geom(int *px, int *py, int *pw, int *ph);

void eh_refresh_dl_popup(void);

void eh_dl_cancel_rect(int *x, int *y);

void eh_dl_progress_metrics(int *total, int *done, int *failed, int *active);

void eh_draw_sync_popup(void);

void eh_sync_popup_geom(int *px, int *py, int *pw, int *ph);

void eh_sync_popup_open(void);

void eh_sync_popup_close(void);

/* Timer-driven popup close, armed by finish/fail — and re-armed by the
 * cover tick (eh_grid.c) while covers are still loading. */
void eh_sync_popup_auto_close(int delay_ms);

void eh_sync_popup_refresh(void);

void eh_sync_popup_finish(void); /* sync ended: covers/done stage + auto-close */

void eh_sync_popup_fail(void);   /* sync failed: show the error, then close */

void eh_draw_log_view(void);

void eh_draw_licenses_view(void);

void eh_draw_scroll_buttons(int up_ok, int down_ok);

void eh_draw_scroll_buttons_at(int up_ok, int down_ok, int y0);

int eh_hit_scroll_button(int x, int y);

int eh_hit_scroll_button_at(int x, int y, int y0);

void eh_redraw_shelf(void);

/* Draw the shelf content without flushing, so a caller can follow with
 * a single refresh of its choosing (the keyboard-commit path draws here
 * then does one full-screen FullUpdate). */
void eh_draw_shelf_nofb(void);

/* Lighter page-turn repaint: grid/list body + pager band partial
 * updates only (the top bar is untouched by a page flip). */
void eh_flip_page(void);

void eh_show_hourglass(void);

void eh_flush_content(void);

void eh_draw_grid(void);

void eh_cover_tick(void *ctx);

void eh_draw_pager(void);


void eh_draw_overlay_more(void);

void eh_draw_overlay_group(void);

void eh_draw_overlay_sort(void);

/* Group chooser row list (eh_overlays.c): All + available dimensions. */
int eh_group_options(BsGroupPreset out[], int cap);
int eh_view_dim_available(BsGroupDim dim);

/* 1 = any modal overlay or popup is up (input routing, long-press
 * arming, and background work like cover fetches should pause). */
int eh_modal_open(void);

void eh_settings_keyboard_handler(char *buffer);

const char *eh_settings_reader_label(void);

void eh_settings_draw_row(int y, const char *label, const char *value,
                       int editing, ifont *lf, ifont *vf);

void eh_settings_draw_button(int y, const char *label, int filled, ifont *f);

/* Left-pointing chevron back arrow, centred at (cx, cy) — the same
 * icon the top bar shows on the Search page.  Shared by the top bar,
 * the settings header and the launcher header. */
void eh_draw_back_icon(int cx, int cy, int col);

/* Shared full-screen overlay header (launcher, settings, log viewer):
 * Back chevron + centred title, drawn identically on every page (see
 * EH_OVERLAY_* in eh_core.h).  eh_overlay_back_rect is the shared
 * tap hit-test. */
void eh_draw_overlay_header(const char *title);

void eh_overlay_back_rect(int *bx, int *by, int *bw, int *bh);

void eh_draw_overlay_settings(void);

void eh_draw_sync_icon(void);

void eh_sync_set_active(int on);

#endif /* EH_UI_H */
