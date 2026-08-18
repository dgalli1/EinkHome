#ifndef EH_LICENSES_H
#define EH_LICENSES_H

/* eh_licenses.h — bundled third-party licenses (eh_licenses.c).  Every
 * external component the app bundles or links, with its full license
 * text for the Settings → Licenses viewer. */

typedef struct {
  const char *name;    /* component name, e.g. "cJSON" */
  const char *license; /* short license type, e.g. "MIT" */
  const char *note;    /* one line: what it is used for / where it comes from */
  const char *text;    /* full license text */
} BsLicense;

int eh_license_count(void);
const BsLicense *eh_license(int i);

#endif /* EH_LICENSES_H */