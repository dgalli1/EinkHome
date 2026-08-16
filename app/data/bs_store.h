#ifndef BS_STORE_H
#define BS_STORE_H

/* bs_store.h — On-device library store + view projection (bs_store.c): the SQLite
 * books table and the materialised grid/list view. */

#include "bs_core.h"

void bs_store_open(void);

void bs_store_close(void);

int bs_store_count(void);

long long bs_store_get_cursor(void);

void bs_store_set_cursor(long long cursor);

int bs_store_upsert_book(const BsBook *b);

void bs_store_delete_book(const char *id);

void bs_store_delete_source(const char *source);

int bs_store_local_meta_get(const char *id, char *title, size_t title_cap,
                         char *author, size_t author_cap);

void bs_store_local_meta_put(const char *id, const char *title,
                          const char *author);

void bs_store_set_downloaded(const char *id, int downloaded,
                          const char *local_path);

int bs_store_get_book(const char *id, BsBook *out);

int bs_store_begin(void);

void bs_store_set_meta(const char *key, const char *value);

int bs_store_meta_value(const char *key, char *out, size_t cap);

void bs_store_commit(void);

void bs_store_rollback(void);

void bs_store_series_name(const char *series_id, char *out, size_t cap);

int bs_store_count_undownloaded(void);

int bs_store_next_undownloaded(char ids[][BS_MAX_ID_LEN], int cap);

/* One row of the boot download-flag probe: the four fields the scan
 * needs, fetched in one paged query (see store_next_dl_probes). */
typedef struct {
  char id[BS_MAX_ID_LEN];
  char filename[BS_MAX_PATH_LEN];
  char local_path[BS_MAX_PATH_LEN];
  char ext[8];
  int  downloaded;
} BsDownloadProbe;

/* Paged scan over every book in rowid order (keyset on *after_rowid,
 * 0 to start), filling exactly the probe fields — the boot flag
 * refresh must not pay a per-book SELECT.  Returns the number of rows
 * written (< cap = done); *after_rowid advances to the last rowid
 * read. */
int bs_store_next_dl_probes(BsDownloadProbe *out, int cap,
                         long long *after_rowid);

void bs_store_delete_book_file(const char *id);

int bs_store_series_ids(const char *series_id, char ids[][BS_MAX_ID_LEN], int cap,
                     int offset);

void bs_store_search_add(const char *term);

int bs_store_search_count(void);

int bs_store_search_list(char terms[][BS_MAX_QUERY_LEN], int cap, int offset);

void bs_store_suggest_set(const char *book_id,
                       const char terms[][BS_SUGGEST_TERM_MAX], int n);

int bs_store_suggest_list(const char *prefix, char out[][BS_SUGGEST_TERM_MAX],
                       int cap);

void bs_view_rebuild(void);

int bs_view_fetch_page(int page, BsTileRow *rows, int cap);

int bs_view_fetch_row(int idx, BsTileRow *out);

/* Dimension-grouped view support (see bs_store.c). */
int bs_view_dim_available(BsGroupDim dim); /* 1 when the source has data for it */

int bs_view_total(void);

#endif /* BS_STORE_H */
