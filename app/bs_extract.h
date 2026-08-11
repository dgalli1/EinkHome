#ifndef BS_EXTRACT_H
#define BS_EXTRACT_H

/* bs_extract.h — Book-file metadata/cover extraction (bs_extract.c): epub/pdf/fb2 title,
 * author and embedded cover. */

#include "bookshelf.h"

int extract_book_meta(const char *path, const char *ext, char *title,
                      size_t title_cap, char *author, size_t author_cap);

int extract_book_cover(const char *path, const char *ext, char *out_path,
                       size_t out_cap);

#endif /* BS_EXTRACT_H */
