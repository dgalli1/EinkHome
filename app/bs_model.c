/* bs_model.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── book record ─────────────────────────────────────────────────────── */







/* A tile in the projected grid view.  At the top level (not drilled),
 * series with >1 book collapse into a single card (is_series=1) showing
 * the newest volume's cover + a triple border + count badge.  Standalone
 * books and drilled-in series members are individual tiles (is_series=0).
 */

ViewTile g_view[MAX_BOOKS];
int      g_view_count;
char     g_drilled_series[MAX_ID_LEN]; /* "" = top level */


State g_state;

/* Full, unfiltered library — the single source of truth that parse/sync
 * mutate.  g_state.books[] is a *filtered projection* rebuilt from this
 * master by apply_filter_and_sort(), so filtering is non-destructive: a
 * second search always starts from the complete library instead of
 * re-filtering an already-shrunk set. */
Book g_lib[MAX_BOOKS];
int  g_lib_count;

/* Edit buffer handed to OpenKeyboard() for the search field.  It MUST be
 * separate from g_state.query: the firmware writes the live keystrokes
 * straight into the buffer we pass, and on commit keyboard_handler()
 * receives that same pointer as `buffer`.  snprintf(g_state.query, ...,
 * buffer) with buffer aliasing g_state.query would copy over a string
 * being simultaneously overwritten, wiping the query (the "search never
 * searches" bug).  A dedicated scratch buffer breaks the alias. */
char g_search_kb_buf[MAX_QUERY_LEN];

/* Forward declarations — defined below grid_geom; needed by
 * apply_filter_and_sort which runs before them in file order. */

/* Per-book cover cache, keyed by id and kept OUTSIDE the Book struct so
 * the wholesale struct copies in parse_books_array() can never leak or
 * double-free a decoded bitmap.  state: 0 untouched, 1 fetch in flight,
 * 2 cover loaded, 3 fetch failed. */

CoverSlot g_covers[MAX_BOOKS];
int       g_cover_armed = 0;

/* One queued/finished download shown on the Downloads tab.  Downloads
 * run synchronously on the event loop, so the queue is drained one item
 * per timer tick; `state` records the outcome so the tab can show a
 * running tally of what finished.  state: 0 queued, 1 in flight,
 * 2 done, 3 failed. */

DownloadItem g_downloads[MAX_DOWNLOADS];
int          g_download_count = 0;
int          g_download_armed = 0;

/* Directory downloads are written to.  Resolved once at startup by
 * resolve_downloads_dir(): LOCAL_DOWNLOADS when the guest can write it
 * (real device), else the /tmp fallback (emulator). */
char g_downloads_dir[MAX_PATH_LEN];

void
resolve_downloads_dir(void)
{
    if (access(LOCAL_DOWNLOADS, W_OK) == 0)
        snprintf(g_downloads_dir, sizeof g_downloads_dir, "%s", LOCAL_DOWNLOADS);
    else
        snprintf(g_downloads_dir, sizeof g_downloads_dir, "%s", LOCAL_DOWNLOADS_FALLBACK);
    LOG("[bookshelf] downloads dir = %s\n", g_downloads_dir);
}

/* Long-press detection state.  POINTERDOWN records the tile under the
 * finger and arms a one-shot timer; if it fires before POINTERUP (and
 * the finger hasn't drifted), the context menu opens for that tile. */
int g_lp_armed = 0;
int g_lp_vi = -1; /* view-tile index held, or -1 */
int g_lp_x = 0;
int g_lp_y = 0;
/* Set when longpress_tick opens the context menu: the finger is still
 * down, so the very next EVT_POINTERUP is the long-press release and must
 * NOT be treated as a tap on the just-opened menu (which would dismiss it
 * immediately).  on_event clears the flag and drops that one UP. */
int g_ctx_suppress_up = 0;

/* argv[0] from main(). */
char g_argv0[256];

/* Reader candidates offered by the settings page.  `path` is the absolute
 * on-device binary used both for the installed-probe and for NewTaskEx();
 * `label` is the human-facing name.  Populated once at startup by
 * detect_readers(); g_reader_count is how many entries are valid. */

ReaderCandidate g_readers[MAX_READERS];
int             g_reader_count = 0;

/* Probe the known reader binaries and fill g_readers[] with the ones that
 * are actually installed (access(X_OK)).  The standard PocketBook reader
 * is always present in the firmware image; KOReader appears only if the
 * user installed it.  Call once at startup before the settings page or
 * reader_pref resolution runs. */
void
detect_readers(void)
{
    static const ReaderCandidate known[] = {
        {READER_STD_PATH, "Standard"},
        {READER_KO_PATH, "KOReader"},
    };
    g_reader_count = 0;
    for (size_t i = 0; i < sizeof known / sizeof known[0] && g_reader_count < MAX_READERS; i++) {
        if (access(known[i].path, X_OK) == 0) {
            g_readers[g_reader_count++] = known[i];
            LOG("[bookshelf] reader detected: %s (%s)\n", known[i].label, known[i].path);
        } else {
            LOG("[bookshelf] reader not found: %s (%s)\n", known[i].label, known[i].path);
        }
    }
}

/* Map a stored reader= value back to a reader_pref index.  "auto"/""/NULL
 * → 0 (server open-with); a path matching a detected reader → its 1-based
 * index; anything else (e.g. a reader that was uninstalled) → 0. */
int
reader_pref_from_path(const char *value)
{
    if (value == NULL || value[0] == '\0' || strcmp(value, "auto") == 0)
        return 0;
    for (int i = 0; i < g_reader_count; i++) {
        if (strcmp(g_readers[i].path, value) == 0)
            return i + 1;
    }
    return 0;
}

/* Persist the current api_base / api_token / reader_pref to the config
 * file.  Written as a plain key=value list so the existing reader picks
 * it straight back up on the next launch.  Returns 0 on success. */
int
save_config_file(void)
{
    FILE *f = fopen(g_config_path, "w");
    if (f == NULL) {
        LOG("[bookshelf] settings: cannot write %s\n", g_config_path);
        return -1;
    }
    fprintf(f, "api_url=%s\n", g_state.api_base);
    fprintf(f, "api_token=%s\n", g_state.api_token);
    if (g_state.reader_pref > 0 && g_state.reader_pref <= g_reader_count)
        fprintf(f, "reader=%s\n", g_readers[g_state.reader_pref - 1].path);
    else
        fprintf(f, "reader=auto\n");
    fclose(f);
    LOG("[bookshelf] settings: saved %s (reader_pref=%d)\n", g_config_path, g_state.reader_pref);
    return 0;
}

/* ── loader (parses /books and /sync/delta JSON) ─────────────────────── */

int
parse_book_obj(const char *obj, Book *b)
{
    memset(b, 0, sizeof *b);
    json_find_key(obj, "id", b->id, sizeof b->id);
    if (b->id[0] == '\0')
        return -1;
    if (json_find_key(obj, "title", b->title, sizeof b->title) == NULL || b->title[0] == '\0') {
        json_find_key(obj, "summary", b->title, sizeof b->title);
    }
    /* authors is a JSON array; take first. */
    char auth[160];
    if (json_find_key(obj, "authors", auth, sizeof auth))
        json_next_string(auth, b->author, sizeof b->author);
    json_find_key(obj, "series", b->series, sizeof b->series);
    json_find_key(obj, "seriesId", b->series_id, sizeof b->series_id);
    b->series_idx = json_find_float(obj, "seriesIdx", 0.0f);
    json_find_key(obj, "format", b->ext, sizeof b->ext);
    /* Strip format string past first non-alnum. */
    for (char *q = b->ext; *q; q++) {
        if (*q >= 'A' && *q <= 'Z')
            *q = (char)(*q + 32);
        if (*q == '/' || *q == '+' || *q == '.') {
            *q = '\0';
            break;
        }
    }
    b->size = json_find_int(obj, "size", 0);
    json_find_key(obj, "blurhash", b->blurhash, sizeof b->blurhash);
    b->added_at = json_find_int(obj, "addedAt", 0);

    /* Check if the file exists on local storage (resolved downloads dir). */
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    FILE *f = fopen(path, "rb");
    if (f) {
        b->downloaded = 1;
        snprintf(b->local_path, sizeof b->local_path, "%s", path);
        fclose(f);
    }
    return 0;
}

/* Parse a JSON array of book objects starting at `arr_start` ('[').
 * For each book, either update an existing in-memory entry (matched by id)
 * or append a new one (up to MAX_BOOKS).  Returns the new count.
 */
int
parse_books_array(const char *arr_start)
{
    int         n = g_lib_count;
    const char *p = strchr(arr_start, '[');
    if (!p)
        return n;
    while (n < MAX_BOOKS) {
        const char *obj = strchr(p, '{');
        if (!obj)
            break;
        const char *end = strchr(obj, '}');
        if (!end)
            break;
        Book b;
        if (parse_book_obj(obj, &b) == 0) {
            /* Update an existing master entry in place (matched by id) so
             * re-syncs refresh metadata without duplicating the book. */
            int found = -1;
            for (int i = 0; i < g_lib_count; i++) {
                if (strcmp(g_lib[i].id, b.id) == 0) {
                    found = i;
                    break;
                }
            }
            if (found >= 0)
                g_lib[found] = b;
            else
                g_lib[g_lib_count++] = b;
            n = g_lib_count;
        }
        p = end + 1;
    }
    return n;
}

void
apply_books_from_added(const char *json, int known_count, const char known_ids[][MAX_ID_LEN])
{
    (void)known_count;
    (void)known_ids;
    /* Parse the "added" array (and "items" for /books).  Existing
     * entries matched by id are updated in place; new entries are
     * appended.  We also honour "removed" by dropping entries.
     */
    const char *p = strstr(json, "\"items\"");
    if (p)
        p = strchr(p, '[');
    if (!p) {
        p = strstr(json, "\"added\"");
        if (p)
            p = strchr(p, '[');
    }
    if (p)
        parse_books_array(p);

    /* Drop books listed in "removed". */
    size_t removed_len = 0;
    char  *removed = json_collect_id_list(json, "\"removed\"", &removed_len);
    if (removed != NULL && removed_len > 0) {
        int write = 0;
        for (int read = 0; read < g_lib_count; read++) {
            if (id_in_list(g_lib[read].id, removed))
                continue;
            if (write != read)
                g_lib[write] = g_lib[read];
            write++;
        }
        g_lib_count = write;
    }
    free(removed);

    /* Drop books listed in "removed" by id. */
    {
        const char *q = strstr(json, "\"removedIds\"");
        if (q) {
            q = strchr(q, '[');
            if (q) {
                char *rids = json_collect_id_list(q, "]", NULL);
                if (rids != NULL) {
                    int write = 0;
                    for (int read = 0; read < g_lib_count; read++) {
                        if (id_in_list(g_lib[read].id, rids))
                            continue;
                        if (write != read)
                            g_lib[write] = g_lib[read];
                        write++;
                    }
                    g_lib_count = write;
                    free(rids);
                }
            }
        }
    }

    LOG("[bookshelf] apply_books_from_added: master count=%d\n", g_lib_count);
}

/* ── /sync/delta POST ────────────────────────────────────────────────── */

void
do_sync(void)
{
    LOG("[bookshelf] do_sync ENTER url_delta=%s\n", g_state.url_delta);
    g_state.sync_state = 1;
    snprintf(g_state.status, sizeof g_state.status, "%s", i18n("status.syncing"));
    /* A previous sync may have hit the server before its cover cache was
     * warm; give failed covers one more chance each sync. */
    for (int i = 0; i < MAX_BOOKS; i++) {
        if (g_covers[i].state == 3)
            g_covers[i].state = 0;
    }

    char req_body[2048];
    int  n = snprintf(req_body, sizeof req_body, "{\"known\":[");
    for (int i = 0; i < g_lib_count && n < (int)sizeof(req_body) - 32; i++) {
        n += snprintf(req_body + n, sizeof req_body - n, "%s\"%s\"", i ? "," : "", g_lib[i].id);
    }
    snprintf(req_body + n, sizeof req_body - n, "]}");

    char *body = NULL;
    int   retsize = 0;
    if (http_post(g_state.url_delta, req_body, &body, &retsize) != 0 || !body) {
        LOG("[bookshelf] do_sync FAILED: url=%s body=%p\n", g_state.url_delta, (void *)body);
        g_state.sync_state = 2;
        snprintf(g_state.status, sizeof g_state.status, "%s", i18n("status.fail"));
        if (body)
            free(body);
        return;
    }
    LOG("[bookshelf] do_sync: body=%p retsize=%d\n", (void *)body, retsize);
    apply_books_from_added(body, 0, NULL);
    free(body);

    apply_filter_and_sort();

    g_state.sync_state = 0;
    /* post state back (best-effort) */
    char state_body[2048];
    n = snprintf(state_body, sizeof state_body, "{\"deviceId\":\"pbemu\",\"known\":[");
    for (int i = 0; i < g_lib_count && n < (int)sizeof(state_body) - 32; i++) {
        n += snprintf(state_body + n, sizeof state_body - n, "%s\"%s\"", i ? "," : "", g_lib[i].id);
    }
    snprintf(state_body + n, sizeof state_body - n, "]}");
    char *resp = NULL;
    int   rl = 0;
    http_post(g_state.url_state, state_body, &resp, &rl);
    if (resp)
        free(resp);
}

/* ── HTTP GET /books (for full re-fetch) ─────────────────────────────── */

void
do_fetch_all(void)
{
    char *body = NULL;
    int   retsize = 0;
    if (http_get(g_state.url_books, &(int){0}, &body, &retsize) != 0 || !body) {
        snprintf(g_state.status, sizeof g_state.status, "%s", i18n("status.fail"));
        return;
    }
    apply_books_from_added(body, 0, NULL);
    free(body);
    apply_filter_and_sort();
}

