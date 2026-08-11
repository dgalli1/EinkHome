#ifndef BS_PROGRESS_H
#define BS_PROGRESS_H

/* bs_progress.h — Reading progress (bs_progress.c): percent-read per book from the
 * firmware's explorer-3.db. */

#include "bookshelf.h"

void progress_reload(void);

int progress_percent(const char *path);

#endif /* BS_PROGRESS_H */
