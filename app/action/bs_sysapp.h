/* bs_sysapp.h — promote/demote bookshelf to the PocketBook home task.
 * See bs_sysapp.c for the model. */

#ifndef BS_SYSAPP_H
#define BS_SYSAPP_H

/* The system-bin directory the home-task override is written to;
 * $BS_SYSAPP_DIR overrides it (tests). */
const char *bs_sysapp_dir(void);

/* Is the home-task override (BS_HOME_TASK_APP) currently installed? */
int bs_sysapp_detect(void);

/* Copy the running binary + a fresh config to the home-task override so
 * monitor.app boots EinkHome as the home screen.  0 on success. */
int bs_sysapp_promote(void);

/* Remove the home-task override; stock bookshelf returns on reboot. */
int bs_sysapp_unpromote(void);

#endif /* BS_SYSAPP_H */