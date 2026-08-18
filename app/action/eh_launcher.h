#ifndef EH_LAUNCHER_H
#define EH_LAUNCHER_H

/* eh_launcher.h — App launcher (eh_launcher.c): the Apps overlay's layout,
 * draw and tap handling.  The launcher's item source is platform-owned: the
 * PocketBook backend parses the firmware view.json/apps_db.json + the
 * /mnt/ext1/applications scan in app/platform/eh_plat_pb_launcher.c (its
 * eh_lc_* conditional-visibility + @-token helpers are internal there), and
 * the SDL backend reads freedesktop .desktop files (eh_plat_sdl.c). */

#include "eh_core.h"

extern BsLcProfile eh_g_lcprof;

extern BsLauncherItem eh_g_launcher_items[EH_LAUNCHER_MAX_ITEMS];

extern int eh_g_launcher_count;

extern int eh_g_launcher_body_h; /* total laid-out body height */

extern int eh_g_launcher_built;

char *eh_read_text_file(const char *path);

void eh_launcher_build(void);

void eh_draw_launcher_icon(int cx, int cy, const char *icon_name,
                        const char *title);

void eh_launcher_icons_free(void);

void eh_draw_overlay_launcher(void);

void eh_launch_app(const BsLauncherItem *it);

void eh_on_tap_overlay_launcher(int x, int y);

void eh_launcher_open_set(void);

void eh_launcher_close(void);

void eh_on_tap_thumbnail(int vi);

#endif /* EH_LAUNCHER_H */
