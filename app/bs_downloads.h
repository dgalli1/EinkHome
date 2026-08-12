#ifndef BS_DOWNLOADS_H
#define BS_DOWNLOADS_H

/* bs_downloads.h — Download queue, delete, context (long-press) menu (bs_downloads.c). */

#include "bookshelf.h"

void download_all_start(void);

/* Download-fetch status (bs_downloads.c): idle = no fetch actively
 * running (a finished job whose settle is pending counts as idle —
 * the file is already on disk); job_pending = an in-flight job whose
 * settle has not run yet. */
int dl_fetch_idle(void);

int dl_job_pending(void);

void cancel_downloads(void);

void book_local_path(const Book *b, char *out, size_t cap);

void book_existing_path(const Book *b, char *out, size_t cap);

void refresh_downloaded(Book *b);

void refresh_downloaded_flags(void);

/* Sliced boot variant of the flag probe (driven by bs_main's
 * "bootslice" weak timer so the full books b-tree walk never stalls
 * the first frame): arm the scan once, then step it in bounded slices
 * across event-loop frames until it returns 1 (finished). */
void refresh_downloaded_flags_boot_start(void);
int refresh_downloaded_flags_boot_step(void);

DownloadItem *find_download(const char *id);

void enqueue_download(const Book *b);

void launch_reader(Book *b);

void book_press_action(Book *b);

void download_series(const char *series_id);

void delete_series(const char *series_id);

void context_geom(int *px, int *py, int *pw, int *ph, int n_items);

int context_item_count(void);

void draw_context_menu(void);

void close_context(void);

void open_context_for_tile(int vi);

void longpress_tick(void *ctx);

void on_tap_context(int x, int y);

#endif /* BS_DOWNLOADS_H */
