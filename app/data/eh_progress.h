#ifndef EH_PROGRESS_H
#define EH_PROGRESS_H

/* eh_progress.h — Reading progress (eh_progress.c): percent-read per book from the
 * firmware's explorer-3.db. */

#include "eh_core.h"

void eh_progress_reload(void);

int eh_progress_percent(const char *path);

#endif /* EH_PROGRESS_H */
