#ifndef BS_UI_H
#define BS_UI_H

/* bs_ui.h — Drawing (bs_screen.c, bs_topbar.c, bs_grid.c, bs_search.c,
 * bs_popups.c, bs_overlays.c, bs_logview.c): top bar, grid/list, popups,
 * overlays, settings page, and the shared geometry helpers. */

#include "bookshelf.h"

extern int g_self_panel;    /* 1 = we draw the status strip ourselves */

extern int g_display_color; /* 1 = colour display (device_display_colormask) */

extern int g_settings_edit;

extern char g_settings_kb_buf[260];

/* UTF-8 truncation helpers (bs_screen.c), shared by every title/term
 * display path: utf8_cap enforces a byte budget, utf8_fit_width a
 * pixel width — neither ever splits a multibyte character. */
void utf8_cap(char *s, size_t cap);

void utf8_fit_width(char *s, size_t cap, int maxw);

void draw_overlay_source(void);

void source_geom(int *px, int *py, int *pw, int *ph);

void draw_text_centered(ifont *f, int cx, int cy, const char *text, int color);

void draw_button(int x, int y, int w, int h, int selected, const char *label,
                 int label_size, int label_color);

/* Dim the content area with the LGRAY hatch (modal-sheet backing).  `y0`
 * is where the dim starts: popups keep the top bar undimmed (its icons —
 * the spinning sync glyph among them — stay fully visible), full-screen
 * overlays dim from the very top.  Shared by the download/sync popups
 * (bs_popups.c) and the source-chooser overlay (bs_overlays.c). */
void dim_content(int y0);

void draw_top_bar(void);

void draw_search_icon(void);

void draw_search_tab(void);

int downloads_pending(void);

/* Suggestion band geometry + rendering (bs_search.c); hit-test (bs_input.c). */
void suggest_band(int *y_top, int *y_bot);

void draw_suggestions(int y_top, int y_bot);

CoverSlot *cover_slot(const char *id, int create);

int view_cols(void);

void stamp_panel(void);

int content_bottom(void);

int view_rows(void);

int view_pagesize(void);

void grid_geom(int *top, int *bot, int *cell_w, int *cell_h);

int tile_rect_for_index(int idx, int *x, int *y, int *w, int *h);

void cover_rect(int tx, int ty, int tw, int th, int *cx, int *cy, int *cw,
                int *ch);

void draw_system_strip(void);

void cover_schedule_next(void);

void blit_cover(int cx, int cy, int cw, int ch, const Book *b);

void draw_series_stack_back(int cx, int cy, int cw, int ch);

void draw_series_stack_badge(int cx, int cy, int cw, int ch, int count);

void draw_thumbnail(int x, int y, int w, int h, const TileRow *tr, int vi);

int history_pagesize(void);

int current_pages(void);

void draw_dl_popup(void);

void dl_popup_geom(int *px, int *py, int *pw, int *ph);

void refresh_dl_popup(void);

void dl_cancel_rect(int *x, int *y);

void dl_progress_metrics(int *total, int *done, int *failed, int *active);

void draw_sync_popup(void);

void sync_popup_geom(int *px, int *py, int *pw, int *ph);

void sync_popup_open(void);

void sync_popup_close(void);

/* Timer-driven popup close, armed by finish/fail — and re-armed by the
 * cover tick (bs_grid.c) while covers are still loading. */
void sync_popup_auto_close(int delay_ms);

void sync_popup_refresh(void);

void sync_popup_finish(void); /* sync ended: covers/done stage + auto-close */

void sync_popup_fail(void);   /* sync failed: show the error, then close */

void draw_log_view(void);

void draw_scroll_buttons(int up_ok, int down_ok);

void draw_scroll_buttons_at(int up_ok, int down_ok, int y0);

int hit_scroll_button(int x, int y);

int hit_scroll_button_at(int x, int y, int y0);

void redraw_shelf(void);

/* Lighter page-turn repaint: grid/list body + pager band partial
 * updates only (the top bar is untouched by a page flip). */
void flip_page(void);

void show_hourglass(void);

void flush_content(void);

void draw_grid(void);

void cover_tick(void *ctx);

void draw_pager(void);

void draw_overlay_menu(void);

void draw_overlay_more(void);

/* 1 = any modal overlay or popup is up (input routing, long-press
 * arming, and background work like cover fetches should pause). */
int modal_open(void);

void settings_keyboard_handler(char *buffer);

const char *settings_reader_label(void);

void settings_draw_row(int y, const char *label, const char *value,
                       int editing);

void settings_draw_button(int y, const char *label, int filled);

void draw_overlay_settings(void);

void draw_sync_icon(void);

void sync_set_active(int on);

#endif /* BS_UI_H */
