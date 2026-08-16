/* bs_net.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_net.h"

/* ── HTTP helpers ────────────────────────────────────────────────────── */

int
bs_http_post_timeout_status(const char *url, const char *body, int timeout,
                         char **resp_out, int *resp_len, int *status_out)
{
    int   retsize = 0;
    int   err = 0;
    /* QuickDownloadExt3 reports the firmware HTTP outcome in its
     * error_code output.  The body may still be non-NULL for an HTTP
     * error response, so callers must key on the status, not just the
     * presence of a body (see bs_net.h). */
    char *resp = QuickDownloadExt3(url, &retsize, timeout, NULL,
                                   (char *)body, &err);
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
bs_http_post(const char *url, const char *body, char **resp_out, int *resp_len)
{
    int   retsize = 0;
    char *resp = QuickDownloadExt(url, &retsize, BS_HTTP_TIMEOUT, NULL,
                                  (char *)body);
    if (resp_out)
        *resp_out = resp;
    if (resp_len)
        *resp_len = retsize;
    if (!resp)
        return -1;
    return 0;
}

void
bs_build_endpoint_urls(void)
{
    const char *base = bs_g_state.api_base;
    const char *tok = bs_g_state.api_token;
    snprintf(bs_g_state.url_delta,
             sizeof bs_g_state.url_delta,
             "%s/api/v1/sync/delta?access_token=%s",
             base,
             tok);
    snprintf(bs_g_state.url_state,
             sizeof bs_g_state.url_state,
             "%s/api/v1/sync/state?access_token=%s",
             base,
             tok);
    snprintf(bs_g_state.url_openwith,
             sizeof bs_g_state.url_openwith,
             "%s/api/v1/open-with?access_token=%s",
             base,
             tok);
}
