#ifndef BS_MODEL_H
#define BS_MODEL_H

/* bs_model.h — Model globals + sync engine (bs_model.c): page rows, covers, readers,
 * download batch state, the sync engine, and the cover cache. */

#include "bs_core.h"
#include "cJSON.h"

extern char bs_g_drilled_series[BS_MAX_ID_LEN];

extern char bs_g_search_kb_buf[BS_MAX_QUERY_LEN];

/* Live suggestion band state: filled by the debounce tick in
 * bs_main.c, drawn by the ui/ draw modules, hit-tested by bs_input.c. */
extern int bs_g_nsuggest;

extern char bs_g_suggestions[BS_SUGGEST_MAX_HITS][BS_SUGGEST_TERM_MAX];

extern BsCoverSlot bs_g_covers[BS_NCOVER_SLOTS];

extern BsTileRow bs_g_rows[BS_MAX_ROWS * BS_COLS]; /* current page rows */

extern int bs_g_row_count;                 /* rows on the page */

extern int bs_g_view_total;                /* tiles in the view */

extern int bs_g_sync_changed;              /* view dirty flag (finish_sync) */

extern int bs_g_view_source;               /* source the view was projected for */

extern int bs_g_dl_batch_active;           /* download-all batch mode */

extern int bs_g_dl_batch_total;

extern int bs_g_dl_batch_done;

extern int bs_g_dl_batch_failed;

extern int bs_g_cover_armed;

extern BsDownloadItem bs_g_downloads[BS_MAX_DOWNLOADS];

extern int bs_g_download_count;

extern char bs_g_downloads_dir[128];

/* Raw `downloads_dir=` from the config file (validated against /mnt/ext1
 * by resolve_downloads_dir). */
extern char bs_g_cfg_downloads_dir[256];

/* Folder picked in Settings → Download folder, pending the Save tap. */
extern char bs_g_settings_dl_dir[256];

extern char bs_g_covers_dir[BS_COVERS_DIR_CAP];

extern int bs_g_lp_armed;

extern int bs_g_lp_vi;

extern int bs_g_lp_x;

extern int bs_g_lp_y;

extern int bs_g_ctx_suppress_up;

extern BsReaderCandidate bs_g_readers[BS_MAX_READERS];

extern int bs_g_reader_count;

void bs_resolve_covers_dir(void);

void bs_resolve_downloads_dir(void);

void bs_detect_readers(void);

int bs_reader_pref_from_path(const char *value);

int bs_save_config_file(void);

ibitmap *bs_load_image_scaled(const char *path);

int bs_parse_book_obj(const cJSON *obj, BsBook *b, int probe_fs);

void bs_do_sync(void);

/* Re-arm the device sleep ban while a sync chain is running (see
 * bs_model.c); called from the sync engine and the local importer. */
void bs_sync_keep_awake(void);

/* Abort any in-flight sync chain (settings/source changes must call
 * this before rebuilding endpoint URLs; see bs_model.c). */
void bs_sync_abort(void);

/* Terminal bookkeeping for the async local scan chain (bs_local.c). */
void bs_sync_local_finish(void);

void bs_cover_cache_path(const char *id, char *out, size_t cap);

void bs_cover_raw_path(const char *id, char *out, size_t cap);

int bs_cover_cache_load(const char *id, ibitmap **out_bmp);

void bs_cover_cache_save(const char *id, const char *png_data, int len);

ibitmap *bs_load_cover_scaled(const char *path);

void bs_sync_set_hooks(const BsSyncUiHooks *hooks);

#endif /* BS_MODEL_H */
