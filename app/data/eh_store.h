#ifndef EH_STORE_H
#define EH_STORE_H

/* eh_store.h — On-device library store + view projection (eh_store.c): the SQLite
 * books table and the materialised grid/list view. */

#include "eh_core.h"

void eh_store_open(void);

void eh_store_close(void);

int eh_store_count(void);

long long eh_store_get_cursor(void);

void eh_store_set_cursor(long long cursor);

int eh_store_upsert_book(const BsBook *b);

void eh_store_delete_book(const char *id);

void eh_store_delete_source(const char *source);

int eh_store_local_meta_get(const char *id, char *title, size_t title_cap,
                         char *author, size_t author_cap);

void eh_store_local_meta_put(const char *id, const char *title,
                          const char *author);

void eh_store_set_downloaded(const char *id, int downloaded,
                          const char *local_path);

int eh_store_get_book(const char *id, BsBook *out);

int eh_store_begin(void);

void eh_store_set_meta(const char *key, const char *value);

int eh_store_meta_value(const char *key, char *out, size_t cap);

int eh_store_commit(void);

void eh_store_rollback(void);

void eh_store_series_name(const char *series_id, char *out, size_t cap);

int eh_store_count_undownloaded(void);

int eh_store_next_undownloaded(char ids[][EH_MAX_ID_LEN], int cap);

/* Cover warm-up: the next remote (kavita) book id in rowid order
 * (keyset on *after_rowid, 0 to start).  Returns 1 and writes one id
 * when a row remains, 0 when exhausted. */
int eh_store_next_warm_book(char *id, int id_cap, long long *after_rowid);

/* One row of the boot download-flag probe: the four fields the scan
 * needs, fetched in one paged query (see store_next_dl_probes). */
typedef struct {
  char id[EH_MAX_ID_LEN];
  char filename[EH_MAX_PATH_LEN];
  char local_path[EH_MAX_PATH_LEN];
  char ext[8];
  int  downloaded;
} BsDownloadProbe;

/* Paged scan over every book in rowid order (keyset on *after_rowid,
 * 0 to start), filling exactly the probe fields — the boot flag
 * refresh must not pay a per-book SELECT.  Returns the number of rows
 * written (< cap = done); *after_rowid advances to the last rowid
 * read. */
int eh_store_next_dl_probes(BsDownloadProbe *out, int cap,
                         long long *after_rowid);

void eh_store_delete_book_file(const char *id);

int eh_store_series_ids(const char *series_id, char ids[][EH_MAX_ID_LEN], int cap,
                     int offset);

void eh_store_search_add(const char *term);

int eh_store_search_count(void);

int eh_store_search_list(char terms[][EH_MAX_QUERY_LEN], int cap, int offset);

void eh_store_suggest_set(const char *book_id,
                       const char terms[][EH_SUGGEST_TERM_MAX], int n);

int eh_store_suggest_list(const char *prefix, char out[][EH_SUGGEST_TERM_MAX],
                       int cap);

void eh_view_rebuild(void);

int eh_view_fetch_page(int page, BsTileRow *rows, int cap);

int eh_view_fetch_row(int idx, BsTileRow *out);

/* Dimension-grouped view support (see eh_store.c). */
int eh_view_dim_available(BsGroupDim dim); /* 1 when the source has data for it */

int eh_view_total(void);

#endif /* EH_STORE_H */
