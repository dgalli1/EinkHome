#ifndef BS_INPUT_H
#define BS_INPUT_H

/* bs_input.h — Hit-testing and tap routing (bs_input.c): the per-element hit tests and
 * the overlay/settings tap handlers. */

#include "bookshelf.h"

int on_tap_source(int x, int y);

int hit_suggestion(int x, int y);

void on_tap_log_view(int x, int y);

int hit_top_bar(int x, int y);

int hit_search_icon(int x, int y);

int hit_search_input(int x, int y);

int hit_history(int x, int y);

int hit_thumbnail(int x, int y);

int hit_pager(int x, int y);

void on_tap_overlay_menu(int x, int y);

int on_tap_overlay_more(int x, int y);

void settings_close(void);

void settings_apply(void);

void on_tap_overlay_settings(int x, int y);

#endif /* BS_INPUT_H */
