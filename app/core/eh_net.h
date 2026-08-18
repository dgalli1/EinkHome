#ifndef EH_NET_H
#define EH_NET_H

/* eh_net.h — HTTP (eh_net.c): server requests.  JSON parsing lives in
 * cJSON (app/cJSON.c, app/cJSON.h); the model, store and launcher walk
 * the DOM directly. */

#include "eh_core.h"

int eh_http_post(const char *url, const char *body, char **resp_out,
              int *resp_len);

/* POST and surface the firmware HTTP outcome separately: a non-200
 * status with a body is an error response, not a transport failure
 * (the sync engine keys its failure handling on this).  *status_out
 * receives the outcome (0 when unavailable, e.g. a transport
 * failure); *resp_out / *resp_len report the body exactly as
 * http_post_timeout does. */
int eh_http_post_timeout_status(const char *url, const char *body, int timeout,
                             char **resp_out, int *resp_len,
                             int *status_out);

void eh_build_endpoint_urls(void);

#endif /* EH_NET_H */
