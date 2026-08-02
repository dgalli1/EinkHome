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
http_post(const char *url, const char *body, char **resp_out, int *resp_len)
{
    int   retsize = 0;
    char *resp = QuickDownloadExt(url, &retsize, HTTP_TIMEOUT, NULL, (char *)body);
    if (resp_out)
        *resp_out = resp;
    if (resp_len)
        *resp_len = retsize;
    if (!resp)
        return -1;
    return 0;
}

void
build_endpoint_urls(void)
{
    const char *base = g_state.api_base;
    const char *tok = g_state.api_token;
    snprintf(g_state.url_books,
             sizeof g_state.url_books,
             "%s/api/v1/books?limit=200&access_token=%s",
             base,
             tok);
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
    snprintf(g_state.url_libs,
             sizeof g_state.url_libs,
             "%s/api/v1/libraries?access_token=%s",
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

int
cmp_series_index_hint(const Book *b)
{
    int n = 0, seen = 0;
    for (int i = (int)strlen(b->id) - 1; i >= 0; i--) {
        if (b->id[i] >= '0' && b->id[i] <= '9') {
            n = n * 10 + (b->id[i] - '0');
            seen = 1;
        } else if (seen) {
            break;
        }
    }
    return n;
}

/* Build a comma-separated list of string ids in the JSON array
 * found at `*arr_key`, returning a malloc'd buffer.  Caller frees.
 */
char *
json_collect_id_list(const char *json, const char *arr_key, size_t *out_len)
{
    const char *p = strstr(json, arr_key);
    if (p == NULL) {
        if (out_len)
            *out_len = 0;
        return NULL;
    }
    p = strchr(p, '[');
    if (p == NULL) {
        if (out_len)
            *out_len = 0;
        return NULL;
    }
    /* Collect up to 8KB of ids. */
    char *buf = malloc(8192);
    if (buf == NULL) {
        if (out_len)
            *out_len = 0;
        return NULL;
    }
    buf[0] = '\0';
    size_t      n = 0;
    const char *q = p;
    while (q && n < 8190) {
        char        id[MAX_ID_LEN];
        const char *next = json_next_string(q, id, sizeof id);
        if (id[0] == '\0')
            break;
        int written;
        if (n == 0) {
            written = snprintf(buf + n, 8192 - n, "%s", id);
        } else {
            written = snprintf(buf + n, 8192 - n, ",%s", id);
        }
        if (written < 0 || (size_t)written >= 8192 - n)
            break;
        n += (size_t)written;
        q = next;
        if (q == NULL)
            break;
        /* skip any whitespace */
        while (*q == ' ' || *q == '\t' || *q == '\n')
            q++;
    }
    if (out_len)
        *out_len = n;
    return buf;
}

/* Return 1 if `id` is in the comma-separated list `list`. */
int
id_in_list(const char *id, const char *list)
{
    if (list == NULL || list[0] == '\0')
        return 0;
    size_t      idlen = strlen(id);
    const char *p = list;
    while (p != NULL && *p != '\0') {
        if (strncmp(p, id, idlen) == 0 && (p[idlen] == '\0' || p[idlen] == ','))
            return 1;
        p = strchr(p, ',');
        if (p != NULL)
            p++;
    }
    return 0;
}

int
cmp_title_asc(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    return strcasecmp(ba->title, bb->title);
}
int
cmp_title_desc(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    return strcasecmp(bb->title, ba->title);
}
int
cmp_author(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    int         r = strcasecmp(ba->author, bb->author);
    if (r != 0)
        return r;
    return strcasecmp(ba->title, bb->title);
}
int
cmp_series(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    int         r = strcasecmp(ba->series, bb->series);
    if (r != 0)
        return r;
    int ia = cmp_series_index_hint(ba);
    int ib = cmp_series_index_hint(bb);
    return (ia < ib) ? -1 : (ia > ib) ? 1 : 0;
}

/* Most-recently-added first; ties fall back to title so the order is
 * stable.  added_at is 0 when the server omits it, in which case the
 * title tie-break still yields a deterministic, non-empty ordering. */
int
cmp_recent(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    if (ba->added_at != bb->added_at)
        return (ba->added_at > bb->added_at) ? -1 : 1;
    return strcasecmp(ba->title, bb->title);
}

/* Build the projected grid view from the filtered+sorted books array.
 * When not drilled and group is ALL or BY_SERIES, books sharing a
 * series_id with >1 member collapse into a single series card tile.
 * The card's book_idx points to the member with the highest series_idx
 * (newest volume).  When drilled, only that series' members appear flat.
 * For AUTHOR/RECENT groups, everything is flat (no collapse). */
void
build_view(void)
{
    g_view_count = 0;
    int collapse = (g_drilled_series[0] == '\0') &&
                   (g_state.group == GROUP_ALL || g_state.group == GROUP_BY_SERIES);

    if (g_drilled_series[0] != '\0') {
        /* Drilled: show only members of the drilled series, flat. */
        for (int i = 0; i < g_state.count && g_view_count < MAX_BOOKS; i++) {
            if (strcmp(g_state.books[i].series_id, g_drilled_series) == 0) {
                ViewTile *vt = &g_view[g_view_count++];
                vt->is_series = 0;
                vt->book_idx = i;
                vt->series_id[0] = '\0';
                vt->series_name[0] = '\0';
                vt->series_count = 0;
            }
        }
        return;
    }

    if (!collapse) {
        /* Flat mode: one tile per book. */
        for (int i = 0; i < g_state.count && g_view_count < MAX_BOOKS; i++) {
            ViewTile *vt = &g_view[g_view_count++];
            vt->is_series = 0;
            vt->book_idx = i;
            vt->series_id[0] = '\0';
            vt->series_name[0] = '\0';
            vt->series_count = 0;
        }
        return;
    }

    /* Collapse mode: group by series_id. */
    /* First pass: count members per series_id. */
    typedef struct {
        char sid[MAX_ID_LEN];
        int  count;
        int  best_idx; /* book index with highest series_idx */
    } SerGroup;
    SerGroup groups[MAX_BOOKS];
    int      ngroups = 0;

    for (int i = 0; i < g_state.count; i++) {
        const char *sid = g_state.books[i].series_id;
        if (sid[0] == '\0') {
            /* Standalone book — emit immediately as flat tile. */
            if (g_view_count < MAX_BOOKS) {
                ViewTile *vt = &g_view[g_view_count++];
                vt->is_series = 0;
                vt->book_idx = i;
                vt->series_id[0] = '\0';
                vt->series_name[0] = '\0';
                vt->series_count = 0;
            }
            continue;
        }
        /* Find or create group. */
        int gi = -1;
        for (int g = 0; g < ngroups; g++) {
            if (strcmp(groups[g].sid, sid) == 0) {
                gi = g;
                break;
            }
        }
        if (gi < 0) {
            gi = ngroups++;
            snprintf(groups[gi].sid, sizeof groups[gi].sid, "%s", sid);
            groups[gi].count = 0;
            groups[gi].best_idx = i;
        }
        groups[gi].count++;
        if (g_state.books[i].series_idx > g_state.books[groups[gi].best_idx].series_idx)
            groups[gi].best_idx = i;
    }

    /* Second pass: emit series cards (count>1) or flat tiles (count==1). */
    for (int g = 0; g < ngroups && g_view_count < MAX_BOOKS; g++) {
        ViewTile *vt = &g_view[g_view_count++];
        if (groups[g].count > 1) {
            vt->is_series = 1;
            vt->book_idx = groups[g].best_idx;
            memcpy(vt->series_id, groups[g].sid, MAX_ID_LEN);
            snprintf(vt->series_name,
                     sizeof vt->series_name,
                     "%s",
                     g_state.books[groups[g].best_idx].series);
            vt->series_count = groups[g].count;
        } else {
            /* Single-book series: show as flat tile. */
            vt->is_series = 0;
            vt->book_idx = groups[g].best_idx;
            vt->series_id[0] = '\0';
            vt->series_name[0] = '\0';
            vt->series_count = 0;
        }
    }
}

void
apply_filter_and_sort(void)
{
    /* Rebuild the filtered projection from the full master library so
     * filtering is non-destructive: every search/sort starts from the
     * complete set, never from an already-shrunk previous result. */
    g_state.count = 0;
    for (int i = 0; i < g_lib_count && i < MAX_BOOKS; i++)
        g_state.books[g_state.count++] = g_lib[i];

    /* Filter: search query, downloaded-only / remote-only. */
    int  n = 0;
    char q[MAX_QUERY_LEN];
    snprintf(q, sizeof q, "%s", g_state.query);
    LOG("[bookshelf] apply_filter: lib=%d query=`%s` filter=%d sort=%d\n",
        g_lib_count,
        q,
        (int)g_state.filter,
        (int)g_state.sort);
    for (char *p = q; *p; p++)
        *p = (char)tolower((unsigned char)*p);
    for (int i = 0; i < g_state.count; i++) {
        Book *b = &g_state.books[i];
        if (g_state.filter == FILTER_DOWNLOADED && !b->downloaded)
            continue;
        if (g_state.filter == FILTER_REMOTE && b->downloaded)
            continue;
        if (q[0] != '\0') {
            char title[MAX_TITLE_LEN], author[80];
            snprintf(title, sizeof title, "%s", b->title);
            snprintf(author, sizeof author, "%s", b->author);
            for (char *p = title; *p; p++)
                *p = (char)tolower((unsigned char)*p);
            for (char *p = author; *p; p++)
                *p = (char)tolower((unsigned char)*p);
            if (!strstr(title, q) && !strstr(author, q))
                continue;
        }
        if (n != i)
            g_state.books[n] = *b;
        n++;
    }
    g_state.total = n;
    g_state.count = n;

    /* Sort. */
    int (*cmp)(const void *, const void *);
    switch (g_state.sort) {
    case SORT_TITLE_ASC:
        cmp = cmp_title_asc;
        break;
    case SORT_TITLE_DESC:
        cmp = cmp_title_desc;
        break;
    case SORT_AUTHOR:
        cmp = cmp_author;
        break;
    case SORT_SERIES:
        cmp = cmp_series;
        break;
    case SORT_RECENT:
        cmp = cmp_recent;
        break;
    default:
        cmp = cmp_title_asc;
        break;
    }
    qsort(g_state.books, g_state.count, sizeof(Book), cmp);

    if (g_state.selected >= g_state.count)
        g_state.selected = -1;

    build_view();

    if (g_state.page >= (g_view_count + view_pagesize() - 1) / view_pagesize())
        g_state.page = 0;
}

