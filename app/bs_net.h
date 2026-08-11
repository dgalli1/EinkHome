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

void build_endpoint_urls(void);

#endif /* BS_NET_H */
