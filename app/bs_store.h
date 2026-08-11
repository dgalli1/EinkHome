#ifndef BS_STORE_H
#define BS_STORE_H

/* bs_store.h — On-device library store + view projection (bs_store.c): the SQLite
 * books table and the materialised grid/list view. */

#include "bookshelf.h"

void store_open(void);

void store_close(void);

int store_count(void);

long long store_get_cursor(void);

void store_set_cursor(long long cursor);

int store_upsert_book(const Book *b);

void store_delete_book(const char *id);

void store_delete_source(const char *source);

int store_local_meta_get(const char *id, char *title, size_t title_cap,
                         char *author, size_t author_cap);

void store_local_meta_put(const char *id, const char *title,
                          const char *author);

void store_set_downloaded(const char *id, int downloaded,
                          const char *local_path);

int store_get_book(const char *id, Book *out);

void store_begin(void);

void store_set_meta(const char *key, const char *value);

int store_meta_value(const char *key, char *out, size_t cap);

void store_commit(void);

void store_rollback(void);

void store_series_name(const char *series_id, char *out, size_t cap);

int store_count_undownloaded(void);

int store_next_undownloaded(char ids[][MAX_ID_LEN], int cap);

int store_next_ids(char ids[][MAX_ID_LEN], int cap, long long *after_rowid);

void store_delete_book_file(const char *id);

int store_series_ids(const char *series_id, char ids[][MAX_ID_LEN], int cap,
                     int offset);

void store_search_add(const char *term);

int store_search_count(void);

int store_search_list(char terms[][MAX_QUERY_LEN], int cap, int offset);

void store_suggest_set(const char *book_id,
                       const char terms[][SUGGEST_TERM_MAX], int n);

int store_suggest_list(const char *prefix, char out[][SUGGEST_TERM_MAX],
                       int cap);

void view_rebuild(void);

int view_fetch_page(int page, TileRow *rows, int cap);

int view_fetch_row(int idx, TileRow *out);

int view_total(void);

#endif /* BS_STORE_H */
