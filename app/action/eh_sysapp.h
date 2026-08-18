/* eh_sysapp.h — promote/demote bookshelf to the PocketBook home task.
 * See eh_sysapp.c for the model. */

#ifndef EH_SYSAPP_H
#define EH_SYSAPP_H

/* The system-bin directory the home-task override is written to;
 * $EH_SYSAPP_DIR overrides it (tests). */
const char *eh_sysapp_dir(void);

/* Is the home-task override (EH_HOME_TASK_APP) currently installed? */
int eh_sysapp_detect(void);

/* Copy the running binary + a fresh config to the home-task override so
 * monitor.app boots EinkHome as the home screen.  0 on success. */
int eh_sysapp_promote(void);

/* Remove the home-task override; stock bookshelf returns on reboot. */
int eh_sysapp_unpromote(void);

#endif /* EH_SYSAPP_H */