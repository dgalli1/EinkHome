#ifndef EH_EXTRACT_H
#define EH_EXTRACT_H

/* eh_extract.h — Book-file metadata/cover extraction: epub/pdf/fb2 title,
 * author and embedded cover.
 *
 * The implementation is the Rust static library (rust_extract/, linked into
 * the binary as libeh_lib.a) — a drop-in for the former hand-rolled
 * app/data/eh_extract.c.  These two functions are the entire FFI surface. */

#include "eh_core.h"

int eh_extract_book_meta(const char *path, const char *ext, char *title,
                      size_t title_cap, char *author, size_t author_cap);

int eh_extract_book_cover(const char *path, const char *ext, char *out_path,
                       size_t out_cap);

#endif /* EH_EXTRACT_H */
