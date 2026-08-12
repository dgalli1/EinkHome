#ifndef BS_DOWNLOADS_H
#define BS_DOWNLOADS_H

/* bs_downloads.h — Download queue, delete, context (long-press) menu (bs_downloads.c). */

#include "bs_core.h"

void bs_download_all_start(void);

/* Download-fetch status (bs_downloads.c): idle = no fetch actively
 * running (a finished job whose settle is pending counts as idle —
 * the file is already on disk); job_pending = an in-flight job whose
 * settle has not run yet. */
int bs_dl_fetch_idle(void);

int bs_dl_job_pending(void);

void bs_cancel_downloads(void);

void bs_book_local_path(const BsBook *b, char *out, size_t cap);

void bs_book_existing_path(const BsBook *b, char *out, size_t cap);

void bs_refresh_downloaded(BsBook *b);

void bs_refresh_downloaded_flags(void);

/* Sliced boot variant of the flag probe (driven by bs_main's
 * "bootslice" weak timer so the full books b-tree walk never stalls
 * the first frame): arm the scan once, then step it in bounded slices
 * across event-loop frames until it returns 1 (finished). */
void bs_refresh_downloaded_flags_boot_start(void);
int bs_refresh_downloaded_flags_boot_step(void);

BsDownloadItem *bs_find_download(const char *id);

void bs_enqueue_download(const BsBook *b);

void bs_launch_reader(BsBook *b);

void bs_book_press_action(BsBook *b);

void bs_download_series(const char *series_id);

void bs_delete_series(const char *series_id);

void bs_context_geom(int *px, int *py, int *pw, int *ph, int n_items);

int bs_context_item_count(void);

void bs_draw_context_menu(void);

void bs_close_context(void);

void bs_open_context_for_tile(int vi);

void bs_longpress_tick(void *ctx);

void bs_on_tap_context(int x, int y);

#endif /* BS_DOWNLOADS_H */
