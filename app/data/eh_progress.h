#ifndef EH_PROGRESS_H
#define EH_PROGRESS_H

/* eh_progress.h — Reading progress (eh_progress.c): percent-read per book from the
 * firmware's explorer-3.db. */

#include "eh_core.h"

/* One {path, percent} reading-progress entry.  path is the absolute
 * folder + "/" + filename of a book on device. */
typedef struct BsProgressEntry {
    char path[EH_MAX_PATH_LEN];
    int  percent; /* 0..100 */
} BsProgressEntry;

void eh_progress_reload(void);

int eh_progress_percent(const char *path);

#endif /* EH_PROGRESS_H */
