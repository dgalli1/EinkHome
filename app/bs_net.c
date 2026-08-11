/* bs_net.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
#include "bs_net.h"

/* ── HTTP helpers ────────────────────────────────────────────────────── */

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

/* ── JSON helpers (the SDK doesn't ship a parser) ─────────────────────
 * One canonical scanner family, shared by the sync engine (bs_model.c),
 * the store loader (bs_store.c) and the launcher's device-profile
 * resolution (bs_launcher.c).  All helpers walk forward over a
 * NUL-terminated buffer; strings and nested values are skipped with
 * escape awareness, so a quote inside a string ("...\"...") never
 * terminates a scan. */

/* Skip JSON whitespace (space, tab, LF, CR). */
const char *
json_skip_ws(const char *p)
{
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')
        p++;
    return p;
}

/* Skip one JSON value (string, object/array, or bare token) starting
 * at or after `p`.  Returns a pointer just past the value, or NULL on
 * malformed input. */
const char *
json_skip_value(const char *p)
{
    p = json_skip_ws(p);
    if (*p == '"') {
        p++;
        while (*p && *p != '"') {
            if (*p == '\\')
                p++;
            p++;
        }
        return *p == '"' ? p + 1 : NULL;
    }
    if (*p == '{' || *p == '[') {
        int depth = 1;
        p++;
        while (*p && depth > 0) {
            if (*p == '"') {
                p = json_skip_value(p);
                if (!p)
                    return NULL;
                continue;
            }
            if (*p == '{' || *p == '[')
                depth++;
            else if (*p == '}' || *p == ']')
                depth--;
            p++;
        }
        return depth == 0 ? p : NULL;
    }
    while (*p && *p != ',' && *p != '}' && *p != ']' && *p != ' ' && *p != '\n' && *p != '\r' &&
           *p != '\t')
        p++;
    return p;
}

/* Return the body of a JSON object (pointer just past the opening
 * '{'), or NULL if `p` does not start an object. */
const char *
json_object_body(const char *p)
{
    p = json_skip_ws(p);
    return *p == '{' ? p + 1 : NULL;
}

/* Copy the quoted JSON string at `p` (which must point at '"') into
 * `out`, unescaping \\ \" \n \t \r (any other escape passes through
 * verbatim).  Returns the position where the scan stopped: just past
 * the closing quote on a well-formed string, else at the truncation or
 * end-of-buffer point.  If *p is not a quote, `out` is set to "" and
 * `p` is returned unchanged. */
const char *
json_copy_string(const char *p, char *out, size_t cap)
{
    if (cap == 0)
        return p;
    if (*p != '"') {
        out[0] = '\0';
        return p;
    }
    p++;
    size_t i = 0;
    while (*p && *p != '"' && i + 1 < cap) {
        if (*p == '\\' && p[1] != '\0') {
            p++;
            if (*p == 'n')
                out[i++] = '\n';
            else if (*p == 't')
                out[i++] = '\t';
            else if (*p == 'r')
                out[i++] = '\r';
            else
                out[i++] = *p;
            p++;
        } else {
            out[i++] = *p++;
        }
    }
    out[i] = '\0';
    return p;
}

/* Find a member `key` inside a JSON object body (pointer just past the
 * opening '{').  Returns a pointer to the member's value, or NULL.
 * Nested objects/arrays are skipped, so a same-named key inside one
 * never matches. */
const char *
json_find_member(const char *p, const char *key)
{
    size_t klen = strlen(key);
    while (*p) {
        p = json_skip_ws(p);
        if (*p == '}')
            return NULL;
        if (*p != '"')
            return NULL;
        const char *ks = ++p;
        while (*p && *p != '"') {
            if (*p == '\\')
                p++;
            p++;
        }
        size_t kl = (size_t)(p - ks);
        if (*p == '"')
            p++;
        p = json_skip_ws(p);
        if (*p == ':')
            p++;
        p = json_skip_ws(p);
        if (kl == klen && memcmp(ks, key, klen) == 0)
            return p;
        p = json_skip_value(p);
        if (!p)
            return NULL;
        p = json_skip_ws(p);
        if (*p == ',')
            p++;
    }
    return NULL;
}

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
    json_copy_string(p, out, cap);
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

/* Strip a string's first JSON-array element.  Looks for the first
 * `"`-quoted string in `arr` and copies it into `out`.  Returns a
 * pointer past the element (closing quote + comma/space), or NULL if
 * the array is empty or the string is unterminated.  Bounded walk: the
 * caller may close the array with ']' so the walk stops there. */
const char *
json_next_string(const char *arr, char *out, size_t cap)
{
    const char *p = strchr(arr, '"');
    if (p == NULL)
        return NULL;
    p = json_copy_string(p, out, cap);
    /* advance past closing quote + comma */
    const char *q = strchr(p, '"');
    if (q == NULL)
        return NULL;
    q++;
    while (*q == ' ' || *q == ',' || *q == '\t')
        q++;
    return q;
}

/* Balanced JSON object scanner — returns pointer to the opening '{'
 * of the next top-level object at or after `p`, and sets *end_out to
 * the matching '}'.  Respects quoted strings (including escapes) and
 * nested braces/brackets so a '}' inside a string value or nested
 * object doesn't terminate the scan early.  Returns NULL when no
 * further object is found. */
const char *
json_next_object(const char *p, const char **end_out)
{
    while (*p && *p != '{')
        p++;
    if (*p != '{')
        return NULL;
    const char *start = p;
    int         depth = 0;
    while (*p) {
        if (*p == '"') {
            p++;
            while (*p && *p != '"') {
                if (*p == '\\')
                    p++;
                p++;
            }
            if (*p == '"')
                p++;
            continue;
        }
        if (*p == '{' || *p == '[')
            depth++;
        else if (*p == '}' || *p == ']') {
            depth--;
            if (depth == 0) {
                if (end_out)
                    *end_out = p;
                return start;
            }
        }
        p++;
    }
    if (end_out)
        *end_out = p;
    return start; /* unterminated; best effort */
}
