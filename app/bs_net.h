#ifndef BS_NET_H
#define BS_NET_H

/* bs_net.h — HTTP + JSON (bs_net.c): server requests and the JSON walker used to
 * parse the sync responses. */

#include "bookshelf.h"

int http_post(const char *url, const char *body, char **resp_out,
              int *resp_len);

int http_post_timeout(const char *url, const char *body, int timeout,
                      char **resp_out, int *resp_len);

void build_endpoint_urls(void);

const char *json_skip_ws(const char *p);

const char *json_skip_value(const char *p);

const char *json_copy_string(const char *p, char *out, size_t cap);

const char *json_find_member(const char *p, const char *key);

const char *json_object_body(const char *p);

char *json_find_key(const char *obj, const char *key, char *out, size_t cap);

int json_find_int(const char *obj, const char *key, int default_val);

float json_find_float(const char *obj, const char *key, float default_val);

const char *json_next_string(const char *arr, char *out, size_t cap);

const char *json_next_object(const char *p, const char **end_out);

#endif /* BS_NET_H */
