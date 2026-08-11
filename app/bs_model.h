#ifndef BS_MODEL_H
#define BS_MODEL_H

/* bs_model.h — Model globals + sync engine (bs_model.c): page rows, covers, readers,
 * download batch state, the sync engine, and the cover cache. */

#include "bookshelf.h"

extern char g_drilled_series[MAX_ID_LEN];

extern char g_search_kb_buf[MAX_QUERY_LEN];

/* Live suggestion band state: filled by the debounce tick in
 * bs_main.c, drawn by bs_ui.c, hit-tested by bs_input.c. */
extern int g_nsuggest;

extern char g_suggestions[SUGGEST_MAX_HITS][SUGGEST_TERM_MAX];

extern CoverSlot g_covers[NCOVER_SLOTS];

extern TileRow g_rows[MAX_ROWS * COLS]; /* current page rows */

extern int g_row_count;                 /* rows on the page */

extern int g_view_total;                /* tiles in the view */

extern int g_dl_batch_active;           /* download-all batch mode */

extern int g_dl_batch_total;

extern int g_dl_batch_done;

extern int g_dl_batch_failed;

extern char g_dl_batch_failed_ids[MAX_DOWNLOADS * 4][MAX_ID_LEN];

extern int g_dl_batch_failed_count;

extern int g_cover_armed;

extern DownloadItem g_downloads[MAX_DOWNLOADS];

extern int g_download_count;

extern char g_downloads_dir[128];

/* Raw `downloads_dir=` from the config file (validated against /mnt/ext1
 * by resolve_downloads_dir). */
extern char g_cfg_downloads_dir[256];

/* Folder picked in Settings → Download folder, pending the Save tap. */
extern char g_settings_dl_dir[256];

extern char g_covers_dir[COVERS_DIR_CAP];

extern int g_lp_armed;

extern int g_lp_vi;

extern int g_lp_x;

extern int g_lp_y;

extern int g_ctx_suppress_up;

extern ReaderCandidate g_readers[MAX_READERS];

extern int g_reader_count;

void resolve_covers_dir(void);

void resolve_downloads_dir(void);

void detect_readers(void);

int reader_pref_from_path(const char *value);

int save_config_file(void);

ibitmap *load_image_scaled(const char *path);

int parse_book_obj(const char *obj, Book *b);

void do_sync(void);

void cover_cache_path(const char *id, char *out, size_t cap);

void cover_raw_path(const char *id, char *out, size_t cap);

int cover_cache_load(const char *id, ibitmap **out_bmp);

void cover_cache_save(const char *id, const char *png_data, int len);

ibitmap *load_cover_scaled(const char *path);

void sync_set_hooks(const SyncUiHooks *hooks);

#endif /* BS_MODEL_H */
