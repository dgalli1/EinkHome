/* bs_model.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── book record ─────────────────────────────────────────────────────── */

/* A tile in the projected grid view.  At the top level (not drilled),
 * series with >1 book collapse into a single card (is_series=1) showing
 * the newest volume's cover + a triple border + count badge.  Standalone
 * books and drilled-in series members are individual tiles (is_series=0).
 */

char g_drilled_series[MAX_ID_LEN]; /* "" = top level */
char g_drilled_series_name[48];    /* display name while drilled */

/* Current page of view rows, shared by the draw loop and the cover
 * fetcher.  Single-threaded event loop, so one static page buffer is
 * safe and bounds RAM to O(page) regardless of library size. */
TileRow g_rows[MAX_ROWS * COLS];
int     g_row_count = 0;
int     g_view_total = 0;

State g_state;

/* The library itself lives in SQLite (bs_store.c); there is no
 * in-memory master array.  g_state only carries UI state. */

/* Edit buffer handed to OpenKeyboard() for the search field.  It MUST be
 * separate from g_state.query: the firmware writes the live keystrokes
 * straight into the buffer we pass, and on commit keyboard_handler()
 * receives that same pointer as `buffer`.  snprintf(g_state.query, ...,
 * buffer) with buffer aliasing g_state.query would copy over a string
 * being simultaneously overwritten, wiping the query (the "search never
 * searches" bug).  A dedicated scratch buffer breaks the alias. */
char g_search_kb_buf[MAX_QUERY_LEN];

/* Forward declarations — defined below grid_geom; needed by do_sync
 * which runs before them in file order. */

/* LRU cover cache, keyed by id and kept OUTSIDE the Book struct so a
 * decoded bitmap can never leak or double-free.  state: 0 untouched,
 * 1 fetch in flight, 2 cover loaded, 3 fetch failed.  A handful of
 * slots bounds decoded-cover RAM regardless of library size. */

CoverSlot g_covers[NCOVER_SLOTS];
int       g_cover_armed = 0;

/* One queued/finished download in the drain queue.  Downloads run
 * synchronously on the event loop, so the queue is drained one item
 * per timer tick; `state` records the outcome so the popup can show a
 * running tally of what finished.  state: 0 queued, 1 in flight,
 * 2 done, 3 failed. */

DownloadItem g_downloads[MAX_DOWNLOADS];
int          g_download_count = 0;
int          g_download_armed = 0;

/* Download-all batch bookkeeping: total = undownloaded count at queue
 * time, done/failed = settled downloads. */
int g_dl_batch_active = 0;
int g_dl_batch_total = 0;
int g_dl_batch_done = 0;
int g_dl_batch_failed = 0;

/* Directory downloads are written to.  Resolved once at startup by
 * resolve_downloads_dir(): LOCAL_DOWNLOADS when the guest can write it
 * (real device), else the /tmp fallback (emulator). */
char g_downloads_dir[128];

void
resolve_downloads_dir(void)
{
    if (access(LOCAL_DOWNLOADS, W_OK) == 0)
        snprintf(g_downloads_dir, sizeof g_downloads_dir, "%s", LOCAL_DOWNLOADS);
    else
        snprintf(g_downloads_dir, sizeof g_downloads_dir, "%s", LOCAL_DOWNLOADS_FALLBACK);
    LOG("[bookshelf] downloads dir = %s\n", g_downloads_dir);
}

/* Directory for cached cover PNGs.  Same parent as the config file
 * (writable app dir on device, /tmp in the emulator). */
char g_covers_dir[COVERS_DIR_CAP];

void
resolve_covers_dir(void)
{
    char dir[160];
    dirname_of(g_config_path, dir, sizeof dir);
    snprintf(g_covers_dir, sizeof g_covers_dir, "%s/" COVERS_SUBDIR, dir);
    /* World-writable: in the emulator the guest is a mapped non-root UID
     * and the host-side tooling (tests, staging) must still be able to
     * manage the cache under the sticky /tmp.  chmod covers the EEXIST
     * case where mkdir ignores the mode. */
    if (mkdir(g_covers_dir, 0777) != 0)
        chmod(g_covers_dir, 0777);
    LOG("[bookshelf] covers dir = %s\n", g_covers_dir);
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
    /* authors is a JSON array; take first.  If the server emits a
     * plain string instead of an array, fall back to copying it
     * directly. */
    char auth[sizeof b->author];
    if (json_find_key(obj, "authors", auth, sizeof auth)) {
        if (json_next_string(auth, b->author, sizeof b->author) == NULL && auth[0] != '\0' &&
            auth[0] != '[')
            snprintf(b->author, sizeof b->author, "%s", auth);
    }
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

/* ── /sync/delta POST ────────────────────────────────────────────────── */

void
do_sync(void)
{
    LOG("[bookshelf] do_sync ENTER url_delta=%s\n", g_state.url_delta);
    g_state.sync_state = 1;
    sync_set_active(1);
    snprintf(g_state.status, sizeof g_state.status, "%s", i18n("status.syncing"));
    /* A previous sync may have hit the server before its cover cache was
     * warm; give failed covers one more chance each sync. */
    for (int i = 0; i < NCOVER_SLOTS; i++) {
        if (g_covers[i].state == 3)
            g_covers[i].state = 0;
    }

    /* Cursor-based delta: each round fetches at most SYNC_BATCH books,
     * writes them in one transaction and persists the cursor, so a
     * 100k-book library syncs in bounded-RAM rounds and resumes after a
     * crash. */
    long long cursor = store_get_cursor();
    int       more = 1;
    int       rounds = 0;
    while (more && rounds < 400) { /* 400 * SYNC_BATCH = 200k ceiling */
        char body[128];
        snprintf(body, sizeof body, "{\"cursor\":%lld,\"limit\":%d}", cursor, SYNC_BATCH);
        char *resp = NULL;
        int   rlen = 0;
        if (http_post_timeout(g_state.url_delta, body, 60, &resp, &rlen) != 0 || resp == NULL) {
            LOG("[bookshelf] do_sync FAILED: url=%s body=%p\n", g_state.url_delta, (void *)resp);
            g_state.sync_state = 2;
            snprintf(g_state.status, sizeof g_state.status, "%s", i18n("status.fail"));
            sync_set_active(0);
            if (resp)
                free(resp);
            return;
        }
        LOG("[bookshelf] do_sync: body=%p retsize=%d cursor=%lld\n", (void *)resp, rlen, cursor);

        store_begin();
        const char *added = strstr(resp, "\"added\"");
        if (added != NULL) {
            const char *p = strchr(added, '[');
            const char *end = NULL;
            Book        tmp;
            while (p != NULL && (p = json_next_object(p, &end)) != NULL) {
                if (parse_book_obj(p, &tmp) == 0)
                    store_upsert_book(&tmp);
                p = end + 1;
            }
        }
        const char *rem = strstr(resp, "\"removed\"");
        if (rem != NULL) {
            const char *p = strchr(rem, '[');
            char        id[MAX_ID_LEN];
            while (p != NULL && (p = json_next_string(p, id, sizeof id)) != NULL)
                store_delete_book(id);
        }
        long long next = (long long)json_find_int(resp, "nextCursor", (int)cursor);
        more = json_find_bool(resp, "more", 0);
        cursor = next;
        store_set_cursor(cursor);
        store_commit();
        free(resp);
        rounds++;
    }
    LOG("[bookshelf] do_sync: rounds=%d cursor=%lld\n", rounds, cursor);

    view_rebuild();
    if (g_state.page * view_pagesize() >= view_total())
        g_state.page = 0;

    g_state.sync_state = 0;
    sync_set_active(0);
    /* post state back (best-effort) */
    char state_body[160];
    snprintf(state_body,
             sizeof state_body,
             "{\"deviceId\":\"pbemu\",\"cursor\":%lld,\"books\":%d}",
             cursor,
             store_count());
    char *resp = NULL;
    int   rl = 0;
    http_post(g_state.url_state, state_body, &resp, &rl);
    if (resp)
        free(resp);
}

/* ── cover PNG cache ─────────────────────────────────────────────────── */

/* Build the on-disk path for a cached cover PNG. */
void
cover_cache_path(const char *id, char *out, size_t cap)
{
    /* Sanitise id: replace '/' with '_' so it's safe as a filename. */
    char safe[MAX_ID_LEN];
    snprintf(safe, sizeof safe, "%s", id);
    for (char *p = safe; *p; p++)
        if (*p == '/')
            *p = '_';
    snprintf(out, cap, "%s/%s.png", g_covers_dir, safe);
}

/* Decode a cover PNG scaled to 240x360.  On a colour display the decode
 * stays RGB24 — the same choice the stock bookshelf.app makes via
 * device_display_colormask() — so covers keep their colour; on a
 * greyscale display the 8-bit decode is used as before.  The caller
 * frees the returned bitmap. */
ibitmap *
load_cover_scaled(const char *path)
{
    if (!g_display_color)
        return LoadPNGStretch(path, 240, 360, 0, 0);
    ibitmap *full = LoadPNGToFormat(path, kFmtRGB24);
    if (full == NULL)
        return NULL;
    LOG("[bookshelf] load_cover_scaled RGB24 full depth=%d %dx%d\n",
        full->depth,
        full->width,
        full->height);
    ibitmap *small = BitmapStretchCopy(full, 0, 0, full->width, full->height, 240, 360);
    free(full);
    if (small != NULL)
        LOG("[bookshelf] load_cover_scaled RGB24 small depth=%d\n", small->depth);
    return small;
}
/* Try to load a cached cover PNG from disk.  Returns 0 on success
 * (bitmap written to *out_bmp), -1 if no cache exists. */
int
cover_cache_load(const char *id, ibitmap **out_bmp)
{
    char path[MAX_PATH_LEN];
    cover_cache_path(id, path, sizeof path);
    if (access(path, R_OK) != 0) {
        LOG("[bookshelf] cover_cache_load miss: access %s errno=%d\n", path, errno);
        return -1;
    }
    ibitmap *bmp = load_cover_scaled(path);
    if (bmp == NULL) {
        LOG("[bookshelf] cover_cache_load miss: load_cover_scaled NULL for %s\n", path);
        return -1;
    }
    *out_bmp = bmp;
    return 0;
}

/* Write raw PNG bytes to the cover cache.  Creates the covers
 * directory if needed. */
void
cover_cache_save(const char *id, const char *png_data, int len)
{
    if (png_data == NULL || len <= 0)
        return;
    char path[MAX_PATH_LEN];
    cover_cache_path(id, path, sizeof path);
    FILE *f = fopen(path, "wb");
    if (f == NULL) {
        LOG("[bookshelf] cover_cache_save: cannot write %s\n", path);
        return;
    }
    fwrite(png_data, 1, (size_t)len, f);
    fclose(f);
}