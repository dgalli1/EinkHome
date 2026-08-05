/* bs_net.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── HTTP helpers ────────────────────────────────────────────────────── */

int
http_get(const char *url, int *status_out, char **body_out, int *len_out)
{
    int   retsize = 0;
    char *body = QuickDownload(url, &retsize, HTTP_TIMEOUT);
    *status_out = 0;
    *body_out = NULL;
    *len_out = 0;
    if (!body || retsize <= 0) {
        if (body)
            free(body);
        return -1;
    }
    *body_out = body;
    *len_out = retsize;
    *status_out = 200;
    return 0;
}

int
http_post_timeout(const char *url, const char *body, int timeout, char **resp_out, int *resp_len)
{
    int   retsize = 0;
    char *resp = QuickDownloadExt(url, &retsize, timeout, NULL, (char *)body);
    if (resp_out)
        *resp_out = resp;
    if (resp_len)
        *resp_len = retsize;
    if (!resp)
        return -1;
    return 0;
}

int
http_post(const char *url, const char *body, char **resp_out, int *resp_len)
{
    return http_post_timeout(url, body, HTTP_TIMEOUT, resp_out, resp_len);
}

void
build_endpoint_urls(void)
{
    const char *base = g_state.api_base;
    const char *tok = g_state.api_token;
    snprintf(g_state.url_delta,
             sizeof g_state.url_delta,
             "%s/api/v1/sync/delta?access_token=%s",
             base,
             tok);
    snprintf(g_state.url_state,
             sizeof g_state.url_state,
             "%s/api/v1/sync/state?access_token=%s",
             base,
             tok);
    snprintf(g_state.url_openwith,
             sizeof g_state.url_openwith,
             "%s/api/v1/open-with?access_token=%s",
             base,
             tok);
}

/* ── JSON helpers (the SDK doesn't ship a parser) ───────────────────── */

char *
json_find_key(const char *obj, const char *key, char *out, size_t cap)
{
    char pat[80];
    snprintf(pat, sizeof pat, "\"%s\"", key);
    const char *p = strstr(obj, pat);
    if (p == NULL) {
        if (cap > 0)
            out[0] = '\0';
        return NULL;
    }
    p += strlen(pat);
    while (*p == ' ' || *p == ':' || *p == '\t')
        p++;
    if (*p != '"') {
        /* number / bool / null */
        const char *e = p;
        while (*e && *e != ',' && *e != '}' && *e != ' ')
            e++;
        size_t n = (size_t)(e - p);
        if (n + 1 > cap)
            n = cap - 1;
        memcpy(out, p, n);
        out[n] = '\0';
        /* JSON null is not a string value — surface it as empty so callers
         * that test for "no value" (e.g. series_id[0] == '\0') work.  Without
         * this a server-emitted `"seriesId": null` is copied verbatim and
         * every null book collapses into one phantom "null" series card. */
        if (n == 4 && memcmp(out, "null", 4) == 0)
            out[0] = '\0';
        return out;
    }
    p++;
    size_t n = 0;
    while (*p && *p != '"' && n + 1 < cap) {
        if (*p == '\\' && p[1] != '\0') {
            p++;
            if (*p == 'n')
                out[n++] = '\n';
            else if (*p == 't')
                out[n++] = '\t';
            else if (*p == 'r')
                out[n++] = '\r';
            else if (*p == '\\' || *p == '"')
                out[n++] = *p;
            else
                out[n++] = *p;
            p++;
        } else {
            out[n++] = *p++;
        }
    }
    out[n] = '\0';
    return out;
}

int
json_find_int(const char *obj, const char *key, int default_val)
{
    char buf[32];
    if (json_find_key(obj, key, buf, sizeof buf) != NULL)
        return atoi(buf);
    return default_val;
}

float
json_find_float(const char *obj, const char *key, float default_val)
{
    char buf[32];
    if (json_find_key(obj, key, buf, sizeof buf) != NULL)
        return (float)atof(buf);
    return default_val;
}

/* Boolean member lookup: accepts true/false (and 0/1).  Returns
 * default_val when the key is absent. */
int
json_find_bool(const char *obj, const char *key, int default_val)
{
    char tmp[16];
    if (json_find_key(obj, key, tmp, sizeof tmp) == NULL)
        return default_val;
    if (strcmp(tmp, "true") == 0 || strcmp(tmp, "1") == 0)
        return 1;
    if (strcmp(tmp, "false") == 0 || strcmp(tmp, "0") == 0)
        return 0;
    return default_val;
}

/* Strip a string's first JSON-array element.  Looks for the first
 * `"`-quoted string in `arr` and copies it into `out`.  Returns NULL
 * if the array is empty.
 */
const char *
json_next_string(const char *arr, char *out, size_t cap)
{
    const char *p = strchr(arr, '"');
    if (p == NULL)
        return NULL;
    p++;
    size_t n = 0;
    while (*p && *p != '"' && n + 1 < cap) {
        if (*p == '\\' && p[1] != '\0') {
            p++;
            if (*p == 'n')
                out[n++] = '\n';
            else if (*p == 't')
                out[n++] = '\t';
            else if (*p == 'r')
                out[n++] = '\r';
            else
                out[n++] = *p;
            p++;
        } else {
            out[n++] = *p++;
        }
    }
    out[n] = '\0';
    /* advance past closing quote + comma */
    const char *q = strchr(p, '"');
    if (q == NULL)
        return NULL;
    q++;
    while (*q == ' ' || *q == ',' || *q == '\t')
        q++;
    return q;
}
