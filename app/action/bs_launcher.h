#ifndef BS_LAUNCHER_H
#define BS_LAUNCHER_H

/* bs_launcher.h — App launcher + device-profile resolution (bs_launcher.c): the Apps
 * overlay and the lc_* conditional-visibility helpers. */

#include "bs_core.h"
#include "cJSON.h"

extern BsLcProfile bs_g_lcprof;

extern const char *const bs_lc_dims[];

/* Number of profile dimensions (LC_NDIMS uses lc_dims, so it lives here). */
#define BS_LC_NDIMS ((int)(sizeof bs_lc_dims / sizeof bs_lc_dims[0]))

extern BsLauncherItem bs_g_launcher_items[BS_LAUNCHER_MAX_ITEMS];

extern int bs_g_launcher_count;

extern int bs_g_launcher_body_h; /* total laid-out body height */

extern int bs_g_launcher_built;

void bs_lc_resolve(const cJSON *v, const char *cur_dim, char *out, size_t cap);

int bs_lc_resolve_bool(const cJSON *v);

char *bs_read_text_file(const char *path);

const char *bs_lc_token_en(const char *tok);

void bs_lc_translate(const char *raw, char *out, size_t cap);

void bs_launcher_build(void);

/* PocketBook data source for the launcher (view.json + apps_db.json +
 * /mnt/ext1/applications scan); called via bs_plat_launcher_build. */
void bs_launcher_build_pb(void);

void bs_draw_launcher_icon(int cx, int cy, const char *icon_name,
                        const char *title);

void bs_launcher_icons_free(void);

void bs_draw_overlay_launcher(void);

void bs_launch_app(const BsLauncherItem *it);

void bs_on_tap_overlay_launcher(int x, int y);

void bs_launcher_open_set(void);

void bs_launcher_close(void);

void bs_drill_back(void);

void bs_on_tap_thumbnail(int vi);

#endif /* BS_LAUNCHER_H */
