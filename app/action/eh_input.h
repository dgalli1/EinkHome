#ifndef EH_INPUT_H
#define EH_INPUT_H

/* eh_input.h — Hit-testing and tap routing (eh_input.c): the per-element hit tests and
 * the overlay/settings tap handlers. */

#include "eh_core.h"

int eh_on_tap_source(int x, int y);

int eh_hit_suggestion(int x, int y);

void eh_on_tap_log_view(int x, int y);

/* First visible row of the log tail's last full page (see eh_logview.c). */
int eh_log_view_tail_first(void);

void eh_on_tap_licenses_view(int x, int y);

int eh_hit_top_bar(int x, int y);

int eh_hit_search_input(int x, int y);

int eh_hit_history(int x, int y);

int eh_hit_thumbnail(int x, int y);


int eh_hit_pager(int x, int y);

int eh_on_tap_overlay_group(int x, int y);
int eh_on_tap_overlay_sort(int x, int y);

int eh_on_tap_overlay_more(int x, int y);

void eh_settings_close(void);

void eh_settings_apply(void);

/* Settings → Install as system app toggle (see eh_sysapp.c). */
void eh_settings_toggle_sysapp(void);

void eh_on_tap_overlay_settings(int x, int y);

#endif /* EH_INPUT_H */
