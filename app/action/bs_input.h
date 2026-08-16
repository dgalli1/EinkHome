#ifndef BS_INPUT_H
#define BS_INPUT_H

/* bs_input.h — Hit-testing and tap routing (bs_input.c): the per-element hit tests and
 * the overlay/settings tap handlers. */

#include "bs_core.h"

int bs_on_tap_source(int x, int y);

int bs_hit_suggestion(int x, int y);

void bs_on_tap_log_view(int x, int y);

int bs_hit_top_bar(int x, int y);

int bs_hit_search_icon(int x, int y);

int bs_hit_search_input(int x, int y);

int bs_hit_history(int x, int y);

int bs_hit_thumbnail(int x, int y);

int bs_hit_group_header(int x, int y);

int bs_hit_pager(int x, int y);

int bs_on_tap_overlay_group(int x, int y);
int bs_on_tap_overlay_sort(int x, int y);

int bs_on_tap_overlay_more(int x, int y);

void bs_settings_close(void);

void bs_settings_apply(void);

void bs_on_tap_overlay_settings(int x, int y);

#endif /* BS_INPUT_H */
