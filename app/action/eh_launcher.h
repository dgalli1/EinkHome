#ifndef EH_LAUNCHER_H
#define EH_LAUNCHER_H

/* eh_launcher.h — App launcher + device-profile resolution (eh_launcher.c): the Apps
 * overlay and the lc_* conditional-visibility helpers. */

#include "eh_core.h"
#include "cJSON.h"

extern BsLcProfile eh_g_lcprof;

extern const char *const eh_lc_dims[];

/* Number of profile dimensions (LC_NDIMS uses lc_dims, so it lives here). */
#define EH_LC_NDIMS ((int)(sizeof eh_lc_dims / sizeof eh_lc_dims[0]))

extern BsLauncherItem eh_g_launcher_items[EH_LAUNCHER_MAX_ITEMS];

extern int eh_g_launcher_count;

extern int eh_g_launcher_body_h; /* total laid-out body height */

extern int eh_g_launcher_built;

void eh_lc_resolve(const cJSON *v, const char *cur_dim, char *out, size_t cap);

int eh_lc_resolve_bool(const cJSON *v);

char *eh_read_text_file(const char *path);

const char *eh_lc_token_en(const char *tok);

void eh_lc_translate(const char *raw, char *out, size_t cap);

void eh_launcher_build(void);

/* PocketBook data source for the launcher (view.json + apps_db.json +
 * /mnt/ext1/applications scan); called via eh_plat_launcher_build. */
void eh_launcher_build_pb(void);

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
