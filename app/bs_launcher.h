#ifndef BS_LAUNCHER_H
#define BS_LAUNCHER_H

/* bs_launcher.h — App launcher + device-profile resolution (bs_launcher.c): the Apps
 * overlay and the lc_* conditional-visibility helpers. */

#include "bookshelf.h"
#include "cJSON.h"

extern const LcProfile g_lcprof;

extern const char *const lc_dims[];

/* Number of profile dimensions (LC_NDIMS uses lc_dims, so it lives here). */
#define LC_NDIMS ((int)(sizeof lc_dims / sizeof lc_dims[0]))

extern LauncherItem g_launcher_items[LAUNCHER_MAX_ITEMS];

extern int g_launcher_count;

extern int g_launcher_body_h; /* total laid-out body height */

extern int g_launcher_built;

const char *lc_prof_val(const char *dim);

const char *lc_pick_key(const cJSON *obj, const char *want);

void lc_resolve(const cJSON *v, const char *cur_dim, char *out, size_t cap);

int lc_resolve_bool(const cJSON *v);

char *read_text_file(const char *path);

const char *lc_token_en(const char *tok);

void lc_translate(const char *raw, char *out, size_t cap);

void launcher_layout(void);

void launcher_add_app(const cJSON *apps, const char *id);

void launcher_build(void);

void launcher_scan_ext1_apps(void);

void draw_launcher_icon(int cx, int cy, const char *icon_name,
                        const char *title);

void draw_overlay_launcher(void);

/* Back-button rect in the launcher header (draw + hit-test share it). */
void launcher_back_rect(int *bx, int *by, int *bw, int *bh);

void launch_app(const LauncherItem *it);

void on_tap_overlay_launcher(int x, int y);

void launcher_open_set(void);

void launcher_close(void);

void drill_back(void);

void on_tap_thumbnail(int vi);

#endif /* BS_LAUNCHER_H */
