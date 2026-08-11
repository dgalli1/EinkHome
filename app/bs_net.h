#ifndef BS_NET_H
#define BS_NET_H

/* bs_net.h — HTTP (bs_net.c): server requests.  JSON parsing lives in
 * cJSON (app/cJSON.c, app/cJSON.h); the model, store and launcher walk
 * the DOM directly. */

#include "bookshelf.h"

int http_post(const char *url, const char *body, char **resp_out,
              int *resp_len);

int http_post_timeout(const char *url, const char *body, int timeout,
                      char **resp_out, int *resp_len);

/* POST and surface the firmware HTTP outcome separately: a non-200
 * status with a body is an error response, not a transport failure
 * (the sync engine keys its failure handling on this).  *status_out
 * receives the outcome (0 when unavailable, e.g. a transport
 * failure); *resp_out / *resp_len report the body exactly as
 * http_post_timeout does. */
int http_post_timeout_status(const char *url, const char *body, int timeout,
                             char **resp_out, int *resp_len,
                             int *status_out);

void build_endpoint_urls(void);

#endif /* BS_NET_H */
