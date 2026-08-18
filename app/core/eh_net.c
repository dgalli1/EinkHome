/* eh_net.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_net.h"

/* ── HTTP helpers ────────────────────────────────────────────────────── */

int
eh_http_post_timeout_status(const char *url, const char *body, int timeout,
                         char **resp_out, int *resp_len, int *status_out)
{
    int   retsize = 0;
    int   err = 0;
    /* The platform transport reports the HTTP outcome; the body may
     * still be non-NULL for an error response, so callers key on the
     * status, not just the presence of a body (see eh_net.h). */
    char *resp = eh_plat_http_post(url, body, timeout, &retsize, &err);
    if (status_out)
        *status_out = err;
    if (resp_out)
        *resp_out = resp;
    if (resp_len)
        *resp_len = retsize;
    if (!resp)
        return -1;
    return 0;
}

int
eh_http_post(const char *url, const char *body, char **resp_out, int *resp_len)
{
    int   retsize = 0;
    char *resp = eh_plat_http_post(url, body, EH_HTTP_TIMEOUT, &retsize, NULL);
    if (resp_out)
        *resp_out = resp;
    if (resp_len)
        *resp_len = retsize;
    if (!resp)
        return -1;
    return 0;
}

void
eh_build_endpoint_urls(void)
{
    const char *base = eh_g_state.api_base;
    const char *tok = eh_g_state.api_token;
    snprintf(eh_g_state.url_delta,
             sizeof eh_g_state.url_delta,
             "%s/api/v1/sync/delta?access_token=%s",
             base,
             tok);
    snprintf(eh_g_state.url_state,
             sizeof eh_g_state.url_state,
             "%s/api/v1/sync/state?access_token=%s",
             base,
             tok);
    snprintf(eh_g_state.url_openwith,
             sizeof eh_g_state.url_openwith,
             "%s/api/v1/open-with?access_token=%s",
             base,
             tok);
}
