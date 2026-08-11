#ifndef BS_LOCAL_H
#define BS_LOCAL_H

/* bs_local.h — Local book-source import (bs_local.c): the filesystem scan that
 * populates SOURCE_LOCAL. */

#include "bookshelf.h"

void local_import_scanner(void);

/* Abort any in-flight local scan chain (settings/source changes call
 * this via sync_abort). */
void local_scan_abort(void);

#endif /* BS_LOCAL_H */
