#ifndef EH_MODEL_H
#define EH_MODEL_H

/* eh_model.h — Model globals + sync engine (eh_model.c): page rows, covers, readers,
 * download batch state, the sync engine, and the cover cache. */

#include "eh_core.h"
#include "cJSON.h"

/* The active grouping preset (EH_GROUP_NONE = All books) and the drill
 * state: eh_g_drill_level counts how many group cards have been tapped
 * into (0 = top), with the tapped group's value per level.  At
 * drill_level == preset level count the view shows flat books. */
extern BsGroupPreset eh_g_group;
extern int eh_g_drill_level;
extern char eh_g_drill_values[EH_GROUP_MAX_LEVELS][EH_MAX_TITLE_LEN];

/* The library page to restore when popping drill-back INTO each level
 * (saved_pages[L] = page of level L's view, remembered right before
 * drilling deeper into L+1).  Nested so a level-2 leaf pops back
 * through the intermediate group level at its own page, not page 0. */
extern int eh_g_saved_pages[EH_GROUP_MAX_LEVELS];

extern char eh_g_search_kb_buf[EH_MAX_QUERY_LEN];

/* Live suggestion band state: filled by the debounce tick in
 * eh_main.c, drawn by the ui/ draw modules, hit-tested by eh_input.c. */
extern int eh_g_nsuggest;

extern char eh_g_suggestions[EH_SUGGEST_MAX_HITS][EH_SUGGEST_TERM_MAX];

extern BsCoverSlot eh_g_covers[EH_NCOVER_SLOTS];

extern BsTileRow eh_g_rows[EH_MAX_ROWS * EH_COLS]; /* current page rows */

extern int eh_g_row_count;                 /* rows on the page */

extern int eh_g_view_total;                /* tiles in the view */

extern int eh_g_sync_changed;              /* view dirty flag (finish_sync) */

extern int eh_g_view_source;               /* source the view was projected for */

extern int eh_g_dl_batch_active;           /* download-all batch mode */

extern int eh_g_dl_batch_total;

extern int eh_g_dl_batch_done;

extern int eh_g_dl_batch_failed;

extern int eh_g_cover_armed;

extern BsDownloadItem eh_g_downloads[EH_MAX_DOWNLOADS];

extern int eh_g_download_count;

extern char eh_g_downloads_dir[128];

/* Raw `downloads_dir=` from the config file (validated against /mnt/ext1
 * by resolve_downloads_dir). */
extern char eh_g_cfg_downloads_dir[256];

/* Folder picked in Settings → Download folder, pending the Save tap. */
extern char eh_g_settings_dl_dir[256];

extern char eh_g_covers_dir[EH_COVERS_DIR_CAP];

extern int eh_g_lp_armed;

extern int eh_g_lp_vi;

extern int eh_g_lp_x;

extern int eh_g_lp_y;

extern int eh_g_ctx_suppress_up;

extern BsReaderCandidate eh_g_readers[EH_MAX_READERS];

extern int eh_g_reader_count;

void eh_resolve_covers_dir(void);

void eh_resolve_downloads_dir(void);

void eh_detect_readers(void);

int eh_reader_pref_from_path(const char *value);

int eh_save_config_file(void);

/* Write the current settings as a KV config to *path*.  Used for the
 * main config (via eh_save_config_file) and for the promoted home task's
 * cfg (eh_sysapp_promote). */
int eh_write_config_file(const char *path);

ibitmap *eh_load_image_scaled(const char *path);

int eh_parse_book_obj(const cJSON *obj, BsBook *b, int probe_fs);

void eh_do_sync(void);

/* Re-arm the device sleep ban while a sync chain is running (see
 * eh_model.c); called from the sync engine and the local importer. */
void eh_sync_keep_awake(void);

/* Abort any in-flight sync chain (settings/source changes must call
 * this before rebuilding endpoint URLs; see eh_model.c). */
void eh_sync_abort(void);

/* Terminal bookkeeping for the async local scan chain (eh_local.c). */
void eh_sync_local_finish(void);

void eh_cover_cache_path(const char *id, char *out, size_t cap);

/* Create the sharded bucket dir for an id (call before writing a cover's
 * ".png"/".raw" so the subdir exists). */
void eh_cover_ensure_bucket(const char *id);

void eh_cover_raw_path(const char *id, char *out, size_t cap);

int eh_cover_cache_load(const char *id, ibitmap **out_bmp);

void eh_cover_cache_save(const char *id, const char *png_data, int len);

ibitmap *eh_load_cover_scaled(const char *path);

void eh_sync_set_hooks(const BsSyncUiHooks *hooks);

#endif /* EH_MODEL_H */
