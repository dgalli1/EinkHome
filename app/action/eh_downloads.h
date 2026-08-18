#ifndef EH_DOWNLOADS_H
#define EH_DOWNLOADS_H

/* eh_downloads.h — Download queue, delete, context (long-press) menu (eh_downloads.c). */

#include "eh_core.h"

void eh_download_all_start(void);

/* Download-fetch status (eh_downloads.c): idle = no fetch actively
 * running (a finished job whose settle is pending counts as idle —
 * the file is already on disk); job_pending = an in-flight job whose
 * settle has not run yet. */
int eh_dl_fetch_idle(void);

int eh_dl_job_pending(void);

void eh_cancel_downloads(void);

void eh_book_local_path(const BsBook *b, char *out, size_t cap);

void eh_book_existing_path(const BsBook *b, char *out, size_t cap);

void eh_refresh_downloaded_flags(void);

/* Sliced boot variant of the flag probe (driven by eh_main's
 * "bootslice" weak timer so the full books b-tree walk never stalls
 * the first frame): arm the scan once, then step it in bounded slices
 * across event-loop frames until it returns 1 (finished). */
void eh_refresh_downloaded_flags_boot_start(void);
int eh_refresh_downloaded_flags_boot_step(void);

BsDownloadItem *eh_find_download(const char *id);

void eh_enqueue_download(const BsBook *b);

void eh_launch_reader(BsBook *b);

void eh_book_press_action(BsBook *b);

void eh_download_series(const char *series_id);

void eh_delete_series(const char *series_id);

void eh_context_geom(int *px, int *py, int *pw, int *ph, int n_items);

int eh_context_item_count(void);

void eh_draw_context_menu(void);

void eh_close_context(void);

void eh_open_context_for_tile(int vi);

void eh_longpress_tick(void *ctx);

void eh_on_tap_context(int x, int y);

#endif /* EH_DOWNLOADS_H */
