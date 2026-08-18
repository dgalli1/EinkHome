/* eh_sysapp.h — promote/demote bookshelf to the PocketBook home task.
 * See eh_sysapp.c for the model. */

#ifndef EH_SYSAPP_H
#define EH_SYSAPP_H

/* The platform-owned directory the home-task override is written to
 * (eh_plat_sysapp_dir); $EH_SYSAPP_DIR overrides it (tests). */
const char *eh_sysapp_dir(void);

/* Is the home-task override (bookshelf.app in the sysapp dir) installed? */
int eh_sysapp_detect(void);

/* Copy the running binary + a fresh config to the home-task override so
 * monitor.app boots EinkHome as the home screen.  0 on success. */
int eh_sysapp_promote(void);

/* Remove the home-task override; stock bookshelf returns on reboot. */
int eh_sysapp_unpromote(void);

#endif /* EH_SYSAPP_H */