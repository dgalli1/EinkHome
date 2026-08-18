#ifndef EH_LOCAL_H
#define EH_LOCAL_H

/* eh_local.h — Local book-source import (eh_local.c): the filesystem scan that
 * populates SOURCE_LOCAL. */

#include "eh_core.h"

void eh_local_import_scanner(void);

/* Abort any in-flight local scan chain (settings/source changes call
 * this via sync_abort). */
void eh_local_scan_abort(void);

#endif /* EH_LOCAL_H */
