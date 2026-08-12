#ifndef BS_PROGRESS_H
#define BS_PROGRESS_H

/* bs_progress.h — Reading progress (bs_progress.c): percent-read per book from the
 * firmware's explorer-3.db. */

#include "bs_core.h"

void bs_progress_reload(void);

int bs_progress_percent(const char *path);

#endif /* BS_PROGRESS_H */
