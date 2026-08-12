#ifndef BS_LOCAL_H
#define BS_LOCAL_H

/* bs_local.h — Local book-source import (bs_local.c): the filesystem scan that
 * populates SOURCE_LOCAL. */

#include "bs_core.h"

void bs_local_import_scanner(void);

/* Abort any in-flight local scan chain (settings/source changes call
 * this via sync_abort). */
void bs_local_scan_abort(void);

#endif /* BS_LOCAL_H */
