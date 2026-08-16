#ifndef BS_LICENSES_H
#define BS_LICENSES_H

/* bs_licenses.h — bundled third-party licenses (bs_licenses.c).  Every
 * external component the app bundles or links, with its full license
 * text for the Settings → Licenses viewer. */

typedef struct {
  const char *name;    /* component name, e.g. "cJSON" */
  const char *license; /* short license type, e.g. "MIT" */
  const char *note;    /* one line: what it is used for / where it comes from */
  const char *text;    /* full license text */
} BsLicense;

int bs_license_count(void);
const BsLicense *bs_license(int i);

#endif /* BS_LICENSES_H */