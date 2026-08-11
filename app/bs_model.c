/* bs_model.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
#include "cJSON.h"
#include "bs_config.h"
#include "bs_downloads.h"
#include "bs_local.h"
#include "bs_model.h"
#include "bs_net.h"
#include "bs_store.h"
#include "bs_ui.h"
#include "bs_worker.h"

#include <dirent.h>

/* ── book record ─────────────────────────────────────────────────────── */

/* A tile in the projected grid view.  At the top level (not drilled),
 * series with >1 book collapse into a single card (is_series=1) showing
 * the newest volume's cover + a triple border + count badge.  Standalone
 * books and drilled-in series members are individual tiles (is_series=0).
 */

char g_drilled_series[MAX_ID_LEN]; /* "" = top level */

/* Current page of view rows, shared by the draw loop and the cover
 * fetcher.  Single-threaded event loop, so one static page buffer is
 * safe and bounds RAM to O(page) regardless of library size. */
TileRow g_rows[MAX_ROWS * COLS];
int     g_row_count = 0;
int     g_view_total = 0;

/* Dirty flag for the materialised view: set by any sync/local apply
 * path that changed rows, cleared by finish_sync, which then rebuilds
 * the view exactly once per changed sync instead of unconditionally. */
int     g_sync_changed = 0;

/* Source the materialised view was last projected for (set by
 * view_rebuild).  finish_sync rebuilds on a source mismatch too: a
 * source switch whose sync applies no rows must still re-project the
 * view under the new source (the old projection would keep showing
 * the previous source's books). */
int     g_view_source = -1;

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

/* Live suggestion band: filled by the suggest_debounce_tick in
 * bs_main.c while the search keyboard is open; drawn and hit-tested
 * by bs_ui.c / bs_input.c.  g_nsuggest == 0 = band hidden. */
int  g_nsuggest = 0;
char g_suggestions[SUGGEST_MAX_HITS][SUGGEST_TERM_MAX];

/* Forward declarations — defined below grid_geom; needed by do_sync
 * which runs before them in file order. */

/* LRU cover cache, keyed by id and kept OUTSIDE the Book struct so a
 * decoded bitmap can never leak or double-free.  state: 0 untouched,
 * 1 fetch in flight, 2 cover loaded, 3 fetch failed.  A handful of
 * slots bounds decoded-cover RAM regardless of library size. */

CoverSlot g_covers[NCOVER_SLOTS];
int       g_cover_armed = 0;

/* One queued/finished download in the drain queue.  Each file fetch
 * runs on the shared background worker (bs_worker.c), one download at
 * a time; a completed job's done_cb settles its queue entry and starts
 * the next.  `state` records the outcome so the popup can show a
 * running tally of what finished.  state: 0 queued, 1 in flight,
 * 2 done, 3 failed. */

DownloadItem g_downloads[MAX_DOWNLOADS];
int          g_download_count = 0;

/* Download-all batch bookkeeping: total = undownloaded count at queue
 * time, done/failed = settled downloads.  The set of failed ids (so a
 * slice never re-enqueues them) lives in bs_downloads.c next to its
 * only users, grown on the heap so a batch can exceed 256 failures. */
int g_dl_batch_active = 0;
int g_dl_batch_total = 0;
int g_dl_batch_done = 0;
int g_dl_batch_failed = 0;

/* Directory downloads are written to.  Resolved at startup (and again
 * after a settings save) by resolve_downloads_dir(): the configured
 * `downloads_dir=` (Settings → Download folder) when it is a valid
 * /mnt/ext1 path, else the default /mnt/ext1/Downloads, else — when
 * the guest cannot write /mnt/ext1 at all, e.g. the emulator's
 * non-root qemu-arm — the /tmp fallback. */
char g_downloads_dir[128];

/* Raw `downloads_dir=` from the config file.  Not trusted: the picker
 * confines choices to /mnt/ext1, but the config file is re-validated
 * against that prefix here. */
char g_cfg_downloads_dir[256];

/* Folder picked in Settings → Download folder, pending the Save tap. */
char g_settings_dl_dir[256];

static int
is_mnt_ext1_path(const char *p)
{
    return strncmp(p, "/mnt/ext1", 9) == 0 && (p[9] == '/' || p[9] == '\0');
}

void
resolve_downloads_dir(void)
{
    /* The pending picker choice (before the settings Save has been
     * re-read from the config) wins over the stored config value. */
    const char *wanted = DEFAULT_DOWNLOADS_DIR;
    if (g_settings_dl_dir[0] != '\0' && is_mnt_ext1_path(g_settings_dl_dir))
        wanted = g_settings_dl_dir;
    else if (g_cfg_downloads_dir[0] != '\0' && is_mnt_ext1_path(g_cfg_downloads_dir))
        wanted = g_cfg_downloads_dir;
    /* First run on a real device: the default folder does not exist
     * yet.  Creating it here makes the picker default usable; a
     * non-root guest (emulator) cannot create it and falls through to
     * the /tmp fallback. */
    if (access(wanted, W_OK) != 0)
        mkdir(wanted, 0777);
    if (access(wanted, W_OK) == 0) {
        /* Bounded: book paths are <dir>/<id>.<ext> and must fit
         * MAX_PATH_LEN, so the folder is capped well under it.  A
         * deeper configured path cannot be used: the full path just
         * passed access(W_OK), so a silently truncated prefix would
         * point downloads at the wrong directory — fall back to the
         * /tmp path instead. */
        size_t wlen = strlen(wanted);
        if (wlen >= sizeof g_downloads_dir) {
            LOG("[bookshelf] downloads dir too long (%d bytes, max %d); "
                "falling back to %s\n",
                (int)wlen, (int)(sizeof g_downloads_dir - 1),
                LOCAL_DOWNLOADS_FALLBACK);
            snprintf(g_downloads_dir, sizeof g_downloads_dir, "%s",
                     LOCAL_DOWNLOADS_FALLBACK);
        } else {
            memcpy(g_downloads_dir, wanted, wlen);
            g_downloads_dir[wlen] = '\0';
        }
    } else {
        snprintf(g_downloads_dir, sizeof g_downloads_dir, "%s", LOCAL_DOWNLOADS_FALLBACK);
    }
    LOG("[bookshelf] downloads dir = %s (cfg=%s%s)\n",
        g_downloads_dir,
        g_cfg_downloads_dir[0] != '\0' ? g_cfg_downloads_dir : "(none)",
        g_settings_dl_dir[0] != '\0' ? ", pending" : "");
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

/* Persist the current api_base / api_token / downloads_dir /
 * reader_pref to the config file.  Written as a plain key=value list
 * so the existing reader picks it straight back up on the next
 * launch.  Returns 0 on success. */
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
    const char *dl_dir = g_settings_dl_dir[0] ? g_settings_dl_dir : g_cfg_downloads_dir;
    if (dl_dir[0] != '\0')
        fprintf(f, "downloads_dir=%s\n", dl_dir);
    fprintf(f,
            "source=%s\n",
            g_state.source == SOURCE_LOCAL    ? "local"
            : g_state.source == SOURCE_FOLDER ? "folder"
                                              : "kavita");
    if (g_state.reader_pref > 0 && g_state.reader_pref <= g_reader_count)
        fprintf(f, "reader=%s\n", g_readers[g_state.reader_pref - 1].path);
    else
        fprintf(f, "reader=auto\n");
    fclose(f);
    LOG("[bookshelf] settings: saved %s (reader_pref=%d)\n", g_config_path, g_state.reader_pref);
    return 0;
}

/* ── loader (parses /books and /sync/delta JSON via cJSON) ──────────── */

/* Copy a string member into a fixed buffer (truncating). */
static void
js_copy(const cJSON *obj, const char *key, char *out, size_t cap)
{
    const cJSON *v = cJSON_GetObjectItemCaseSensitive(obj, key);
    if (v != NULL && cJSON_IsString(v) && v->valuestring != NULL)
        snprintf(out, cap, "%s", v->valuestring);
    else if (cap > 0)
        out[0] = '\0';
}

/* Copy a string node into a fixed buffer (truncating). */
static void
js_str(const cJSON *v, char *out, size_t cap)
{
    if (v != NULL && cJSON_IsString(v) && v->valuestring != NULL)
        snprintf(out, cap, "%s", v->valuestring);
    else if (cap > 0)
        out[0] = '\0';
}

static int
js_int(const cJSON *obj, const char *key, int dflt)
{
    const cJSON *v = cJSON_GetObjectItemCaseSensitive(obj, key);
    return (v != NULL && cJSON_IsNumber(v)) ? (int)v->valuedouble : dflt;
}

static float
js_float(const cJSON *obj, const char *key, float dflt)
{
    const cJSON *v = cJSON_GetObjectItemCaseSensitive(obj, key);
    return (v != NULL && cJSON_IsNumber(v)) ? (float)v->valuedouble : dflt;
}

/* "addedAt": the server sends ISO-8601 UTC ("2026-08-11T09:07:00Z",
 * with or without fractional seconds); the store sorts on unix epoch,
 * so convert.  A plain epoch number is accepted as-is. */
static long
js_epoch(const cJSON *obj, const char *key)
{
    const cJSON *v = cJSON_GetObjectItemCaseSensitive(obj, key);
    if (v == NULL)
        return 0;
    if (cJSON_IsNumber(v))
        return (long)v->valuedouble;
    if (!cJSON_IsString(v) || v->valuestring == NULL)
        return 0;
    int y = 0, mo = 0, d = 0, h = 0, mi = 0;
    double sec = 0.0;
    if (sscanf(v->valuestring, "%4d-%2d-%2dT%2d:%2d:%lf",
               &y, &mo, &d, &h, &mi, &sec) < 6)
        return 0;
    /* days-from-civil (Howard Hinnant), UTC (the server emits Z). */
    long long yy = y - (mo <= 2);
    long long era = (yy >= 0 ? yy : yy - 399) / 400;
    unsigned  yoe = (unsigned)(yy - era * 400);
    unsigned  doy = (153 * (mo + (mo > 2 ? -3 : 9)) + 2) / 5 + (unsigned)d - 1;
    unsigned  doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    long long days = era * 146097 + (long long)doe - 719468;
    return (long)(days * 86400 + h * 3600 + mi * 60 + (long long)sec);
}

int
parse_book_obj(const cJSON *obj, Book *b, int probe_fs)
{
    memset(b, 0, sizeof *b);
    js_copy(obj, "id", b->id, sizeof b->id);
    if (b->id[0] == '\0')
        return -1;
    js_copy(obj, "title", b->title, sizeof b->title);
    if (b->title[0] == '\0') {
        js_copy(obj, "summary", b->title, sizeof b->title);
    }
    /* authors is a JSON array; take the first.  If the server emits a
     * plain string instead of an array, fall back to it directly. */
    const cJSON *a = cJSON_GetObjectItemCaseSensitive(obj, "authors");
    if (a != NULL) {
        if (cJSON_IsArray(a) && cJSON_GetArraySize(a) > 0)
            js_str(cJSON_GetArrayItem(a, 0), b->author, sizeof b->author);
        else
            js_str(a, b->author, sizeof b->author);
    }
    js_copy(obj, "series", b->series, sizeof b->series);
    js_copy(obj, "seriesId", b->series_id, sizeof b->series_id);
    b->series_idx = js_float(obj, "seriesIdx", 0.0f);
    /* Folded search blob (delta "searchText"): the server folds, so the
     * device matches LIKE against the same folded text. */
    js_copy(obj, "searchText", b->search_text, sizeof b->search_text);
    js_copy(obj, "format", b->ext, sizeof b->ext);
    /* Strip format string past first non-alnum. */
    for (char *q = b->ext; *q; q++) {
        if (*q >= 'A' && *q <= 'Z')
            *q = (char)(*q + 32);
        if (*q == '/' || *q == '+' || *q == '.') {
            *q = '\0';
            break;
        }
    }
    b->size = js_int(obj, "size", 0);
    b->added_at = js_epoch(obj, "addedAt");
    /* Server books come from the remote library; local imports set
     * their own source. */
    snprintf(b->source, sizeof b->source, "kavita");
    js_copy(obj, "filename", b->filename, sizeof b->filename);
    /* Sanitize to a bare basename right away: the downloads path is
     * built from this and must stay inside the downloads dir. */
    char *slash = strrchr(b->filename, '/');
    if (slash != NULL)
        memmove(b->filename, slash + 1, strlen(slash + 1) + 1);

    /* Check if the file exists on local storage (resolved downloads
     * dir).  Only the local-import path probes the filesystem
     * (probe_fs): a per-book access() on the main thread would block
     * every delta round.  The remote sync path passes probe_fs=0 and
     * keeps an already-downloaded book's state via store_upsert_book's
     * existing-row lookup. */
    if (probe_fs) {
        char path[MAX_PATH_LEN];
        book_local_path(b, path, sizeof path);
        if (access(path, F_OK) == 0) {
            b->downloaded = 1;
            snprintf(b->local_path, sizeof b->local_path, "%s", path);
        }
    }
    return 0;
}

/* ── /sync/delta POST ────────────────────────────────────────────────── */

/* ── async delta sync ──────────────────────────────────────────────────
 * Each /sync/delta round-trip runs as a one-shot job on the shared
 * background worker (bs_worker.c), so the event loop stays responsive
 * during a big first sync (100k books ≈ 200 rounds of HTTP).  The job
 * fn only does the blocking HTTP fetch; its done_cb applies the
 * response on the main thread — store writes stay single-threaded —
 * and chains the next round job, so no pacing timer is needed (the
 * HTTP round-trip paces the loop).  The worker touches no UI and no
 * store state. */

/* Round job argument: the delta URL + body are snapshotted on the main
 * thread (the worker never reads g_state).  gen tags the chain this
 * round belongs to; sync_round_done drops jobs whose gen no longer
 * matches (sync_abort + restart), so a stale response can never be
 * applied to a new chain. */
typedef struct {
    char url[MAX_URL_LEN + 16];
    char body[160];
    int  gen;
} SyncRoundArg;

/* Round job result (worker-allocated; done_cb frees it).  http_status
 * is the firmware HTTP outcome surfaced by http_post_timeout_status
 * (0 when unavailable); a non-200 status with a body is an error
 * response, not a transport failure.  The response body is parsed and
 * shape-checked on the worker, so only the cJSON tree reaches the main
 * thread — the raw bytes are freed in sync_fetch_round. */
typedef struct {
    cJSON *root;      /* parsed delta (worker); NULL when unusable */
    int    parse_ok;  /* 1 = root holds a usable delta response */
    char  *resp;      /* raw body — freed by the worker; always NULL */
    int    rlen;
    int    rc;
    int    http_status;
} SyncRoundResult;

static long long g_sync_cursor; /* main thread only now */
static int       g_sync_rounds; /* main thread only */
static int       g_sync_gen;    /* chain generation: bumped on every
                                   do_sync / sync_abort */

static void sync_submit_round(void);
static void sync_submit_finish(void);
static void sync_round_done(BsJob *job);

/* ── sync-engine → UI hooks ────────────────────────────────────────────
 * Registered once at startup (bs_main.c EVT_INIT) via sync_set_hooks().
 * The sync engine never calls bs_ui.c by name — every UI side effect
 * (spinner state, sync popup, shelf repaint) goes through these
 * NULL-checked wrappers, so bs_model.c depends on the UI only through
 * the hook struct (dependency inversion).  All hook invocations happen
 * on the main thread, exactly where the direct calls used to run. */
static SyncUiHooks g_sync_ui;

void
sync_set_hooks(const SyncUiHooks *hooks)
{
    if (hooks != NULL)
        g_sync_ui = *hooks;
}

static void sync_ui_active(int on) { if (g_sync_ui.set_active) g_sync_ui.set_active(on); }
static void sync_ui_popup_refresh(void) { if (g_sync_ui.popup_refresh) g_sync_ui.popup_refresh(); }
static void sync_ui_popup_finish(void) { if (g_sync_ui.popup_finish) g_sync_ui.popup_finish(); }
static void sync_ui_popup_fail(void) { if (g_sync_ui.popup_fail) g_sync_ui.popup_fail(); }
static void sync_ui_repaint(void) { if (g_sync_ui.repaint) g_sync_ui.repaint(); }

/* Worker: fetch one delta batch (blocking HTTP) and parse it, so the
 * main thread never blocks on the cJSON_Parse of a large delta.  The
 * parsed tree is owned by the job (freed by the done_cb on the main
 * thread); the raw response bytes are freed here. */
static void
sync_fetch_round(BsJob *job)
{
    SyncRoundArg   *a = job->arg;
    char           *resp = NULL;
    int             rlen = 0;
    int             status = 0;
    int             rc = http_post_timeout_status(a->url, a->body, 60,
                                                  &resp, &rlen, &status);
    SyncRoundResult *r = malloc(sizeof *r);
    if (r == NULL) {
        free(resp);
        job->rc = -1;
    } else {
        r->resp = NULL; /* raw bytes freed below; the tree survives */
        r->rlen = rlen;
        r->rc = rc;
        r->http_status = status;
        r->root = NULL;
        r->parse_ok = 0;
        /* rc == 0 implies resp != NULL (http_post_timeout_status
         * returns -1 when there is no body).  Parse + shape-check the
         * exact shape sync_apply_round used to check on the main
         * thread: object, no error/detail member, array added/removed,
         * numeric nextCursor. */
        if (rc == 0 && resp != NULL) {
            r->root = cJSON_Parse(resp);
            if (r->root == NULL) {
                LOG("[bookshelf] sync: delta response not valid JSON "
                    "(%.80s)\n",
                    cJSON_GetErrorPtr() ? cJSON_GetErrorPtr() : "?");
            } else if (!cJSON_IsObject(r->root) ||
                       cJSON_GetObjectItemCaseSensitive(r->root, "error") !=
                           NULL ||
                       cJSON_GetObjectItemCaseSensitive(r->root, "detail") !=
                           NULL) {
                /* An error body (the server's {error,more} 503 reply,
                 * or a proxy JSON error) must not be read as an empty
                 * delta: without this the round "succeeds" with zero
                 * books and the chain reports done. */
                LOG("[bookshelf] sync: delta response is an error body\n");
            } else {
                const cJSON *added =
                    cJSON_GetObjectItemCaseSensitive(r->root, "added");
                const cJSON *rem =
                    cJSON_GetObjectItemCaseSensitive(r->root, "removed");
                const cJSON *nc =
                    cJSON_GetObjectItemCaseSensitive(r->root, "nextCursor");
                if (!cJSON_IsArray(added) || !cJSON_IsArray(rem) ||
                    !cJSON_IsNumber(nc)) {
                    LOG("[bookshelf] sync: delta response missing added/"
                        "removed/nextCursor\n");
                } else {
                    r->parse_ok = 1;
                }
            }
        }
        free(resp);
        job->result = r;
        job->rc = rc;
    }
    __atomic_store_n(&job->done, 1, __ATOMIC_RELEASE);
}

/* Submit one round job (main thread only). */
static void
sync_submit_round(void)
{
    /* Re-arm the sleep ban on every round: a 100k first sync runs for
     * minutes with zero input, and the firmware's auto-sleep would
     * otherwise stretch every batch to a wake cycle.  Auto-expires
     * SYNC_BAN_SLEEP_SEC after the last round — no restore needed. */
    sync_keep_awake();
    SyncRoundArg *a = malloc(sizeof *a);
    if (a == NULL) {
        /* Cannot happen in practice; fail gracefully instead of
         * hanging with sync_state stuck at 1. */
        LOG("[bookshelf] do_sync: worker submit failed\n");
        g_state.sync_state = 0;
        sync_ui_active(0);
        return;
    }
    a->gen = g_sync_gen;
    snprintf(a->url, sizeof a->url, "%s", g_state.url_delta);
    snprintf(a->body, sizeof a->body,
             "{\"cursor\":%lld,\"limit\":%d}", g_sync_cursor, SYNC_BATCH);
    if (bs_worker_submit(sync_fetch_round, sync_round_done, a) == NULL) {
        free(a);
        LOG("[bookshelf] do_sync: worker submit failed\n");
        g_state.sync_state = 0;
        sync_ui_active(0);
        return;
    }
}

/* Round outcome codes from sync_apply_round: 0 = applied cleanly (the
 * cursor was advanced and persisted), 1 = the response was unusable
 * (bad JSON, an error body, or missing added/removed/nextCursor), 2 =
 * a store write failed and the batch was rolled back.  Both failures
 * abort the WHOLE chain visibly (no finish job, no "done") with the
 * cursor un-advanced, so the next sync retries from this delta. */
enum {
    SYNC_ROUND_OK = 0,
    SYNC_ROUND_BAD_RESP = 1,
    SYNC_ROUND_STORE_FAIL = 2,
};

/* Apply one parsed delta response on the main thread: the added/removed
 * arrays plus cursor/more, inside the batch transaction.  The response
 * was parsed and shape-checked on the worker (sync_fetch_round), so the
 * main thread never blocks on the cJSON_Parse of a large delta; root is
 * owned by this function and freed before every return.  Returns a
 * SYNC_ROUND_* outcome; on failure the transaction is never opened (or
 * is rolled back), the cursor is left unchanged, and the caller aborts
 * the chain visibly instead of chaining the finish "done". */
/* Apply one `added` entry: parse + upsert + suggest terms.  Returns 0
 * on success, -1 when the upsert failed (the caller aborts the round
 * and rolls back).  Unparseable entries are skipped, as the old
 * inline loop did. */
static int
sync_apply_added(const cJSON *it, long long cursor)
{
    Book tmp;
    if (parse_book_obj(it, &tmp, 0) != 0)
        return 0;
    if (store_upsert_book(&tmp) != 0) {
        LOG("[bookshelf] sync: upsert failed id=%s; "
            "aborting sync (cursor %lld kept)\n",
            tmp.id, cursor);
        return -1;
    }
    g_sync_changed = 1; /* rows applied: rebuild at finish */
    /* Suggestion terms for this book, straight from the DOM — no
     * bounded-copy tricks needed. */
    char terms[SUGGEST_MAX_TERMS][SUGGEST_TERM_MAX];
    int  n = 0;
    const cJSON *sg = cJSON_GetObjectItemCaseSensitive(it, "suggest");
    if (cJSON_IsArray(sg)) {
        const cJSON *t;
        cJSON_ArrayForEach(t, sg) {
            if (n >= SUGGEST_MAX_TERMS)
                break;
            if (cJSON_IsString(t) && t->valuestring != NULL &&
                t->valuestring[0] != '\0')
                snprintf(terms[n++], SUGGEST_TERM_MAX, "%s", t->valuestring);
        }
    }
    store_suggest_set(tmp.id, n > 0 ? terms : NULL, n);
    return 0;
}

typedef struct {
    const cJSON *it;
    char         id[MAX_ID_LEN];
} AddedItem;

static int
added_item_cmp(const void *a, const void *b)
{
    return strcmp(((const AddedItem *)a)->id, ((const AddedItem *)b)->id);
}

static int
sync_apply_round(cJSON *root, long long cursor, long long *next_out,
                 int *more_out)
{
    *next_out = cursor;
    *more_out = 0;
    /* root is a validated delta here: added/removed arrays, numeric
     * nextCursor, no error/detail member (worker-checked). */
    const cJSON *added = cJSON_GetObjectItemCaseSensitive(root, "added");
    const cJSON *rem = cJSON_GetObjectItemCaseSensitive(root, "removed");
    const cJSON *nc = cJSON_GetObjectItemCaseSensitive(root, "nextCursor");
    store_begin();
    if (cJSON_IsArray(added)) {
        /* Sort the round's added entries by id before applying.  The
         * server streams the catalogue in its own order, but the
         * SQLite b-tree insert cost collapses when the per-round ids
         * are sequential: every insert then hits pages the round
         * already warmed instead of evicting and re-reading them from
         * flash.  Unsorted, a 100k ingest re-reads ~1GB of pages and
         * the per-round time grows with the table (measured 2s -> 9s+
         * per round on device). */
        int n_added = cJSON_GetArraySize(added);
        AddedItem *items = NULL;
        int        n_items = 0;
        if (n_added > 0)
            items = malloc(sizeof *items * (size_t)n_added);
        if (items != NULL) {
            const cJSON *it;
            cJSON_ArrayForEach(it, added) {
                if (!cJSON_IsObject(it))
                    continue;
                js_copy(it, "id", items[n_items].id, sizeof items[n_items].id);
                if (items[n_items].id[0] == '\0')
                    continue; /* parse_book_obj rejects it below */
                items[n_items].it = it;
                n_items++;
            }
            if (n_items > 1)
                qsort(items, (size_t)n_items, sizeof *items, added_item_cmp);
        }
        if (items != NULL) {
            for (int i = 0; i < n_items; i++) {
                if (sync_apply_added(items[i].it, cursor) != 0) {
                    /* A failed upsert aborts the whole sync: roll
                     * back so the half-written batch is not persisted
                     * (a later round would otherwise apply its rows on
                     * top of a partial batch), and leave the cursor
                     * unchanged so the next sync retries from this
                     * same delta.  *next_out / *more_out were already
                     * set to cursor/0 at the top. */
                    store_rollback();
                    free(items);
                    cJSON_Delete(root);
                    return SYNC_ROUND_STORE_FAIL;
                }
            }
            free(items);
        } else {
            /* Allocation failed: apply in server order (same result,
             * worse flash locality). */
            const cJSON *it;
            cJSON_ArrayForEach(it, added) {
                if (sync_apply_added(it, cursor) != 0) {
                    store_rollback();
                    cJSON_Delete(root);
                    return SYNC_ROUND_STORE_FAIL;
                }
            }
        }
    }
    if (cJSON_IsArray(rem)) {
        const cJSON *it;
        cJSON_ArrayForEach(it, rem) {
            if (cJSON_IsString(it) && it->valuestring != NULL &&
                it->valuestring[0] != '\0') {
                store_delete_book(it->valuestring);
                store_suggest_set(it->valuestring, NULL, 0);
                g_sync_changed = 1; /* rows removed: rebuild at finish */
            }
        }
    }
    if (cJSON_IsNumber(nc))
        *next_out = (long long)nc->valuedouble;
    const cJSON *mk = cJSON_GetObjectItemCaseSensitive(root, "more");
    if (cJSON_IsBool(mk))
        *more_out = cJSON_IsTrue(mk);
    store_set_cursor(*next_out);
    store_commit();
    cJSON_Delete(root);
    return SYNC_ROUND_OK;
}

/* Sync keep-awake: the firmware's auto-sleep (reset only by input)
 * would freeze a long sync mid-chain — the worker fetch included —
 * and every batch would then complete only around a wake cycle.  The
 * stock reader uses BanSleep() for exactly this; re-arm it only when
 * the current ban is about to expire (a sync's worth of rounds runs
 * on a handful of calls, not one per round).  The ban auto-expires
 * after the sync ends, so nothing needs restoring. */
#define SYNC_BAN_SLEEP_SEC 1800 /* 30 min per ban */

static long g_sync_ban_until = 0;

void
sync_keep_awake(void)
{
    long now = (long)time(NULL);
    if (now >= g_sync_ban_until - 300) {
        BanSleep(SYNC_BAN_SLEEP_SEC);
        g_sync_ban_until = now + SYNC_BAN_SLEEP_SEC;
    }
}

/* Sync finished (any source): close the popup, rebuild the view, hand
 * the spinner off, repaint, and clear the sync state.  The remote
 * state-report POST runs as a separate finish job (sync_submit_finish)
 * so it does not block the main thread; local/folder sources call this
 * directly. */
static void
finish_sync(void)
{
    sync_ui_popup_finish();
    /* The view only changes when rows were applied or the source
     * filter changed; the dirty flag is set by the apply paths (sync
     * rounds, local slices) and reset here.  A source switch whose
     * sync applies nothing (empty local scan, or a delta the server
     * considers already-synced) must still re-project the view under
     * the new source, so the rebuild also runs when g_state.source
     * differs from the source the current view was projected for
     * (recorded by view_rebuild in g_view_source).  Skipping the
     * rebuild keeps an unchanged re-sync from paying for a full view
     * projection. */
    if (g_sync_changed || g_state.source != g_view_source)
        view_rebuild();
    g_sync_changed = 0;
    if (g_state.page * view_pagesize() >= view_total())
        g_state.page = 0;

    g_state.sync_state = 0;
    sync_ui_active(0);
    /* do_sync is async now: the callers' redraw runs before the sync
     * lands, so the shelf repaints itself here. */
    sync_ui_repaint();
}

/* ── finish job: report the final state back to the server ───────────── */

typedef struct {
    char url[MAX_URL_LEN + 16];
    char body[160];
} SyncFinishArg;

/* Worker: POST the final state (best-effort). */
static void
sync_finish_post(BsJob *job)
{
    SyncFinishArg *a = job->arg;
    char          *resp = NULL;
    int            rl = 0;
    http_post(a->url, a->body, &resp, &rl);
    if (resp)
        free(resp);
    job->rc = 0; /* best-effort; the outcome is not used */
    __atomic_store_n(&job->done, 1, __ATOMIC_RELEASE);
}

/* done_cb for the finish job: the terminal bookkeeping. */
static void
sync_finish_done(BsJob *job)
{
    free(job->arg);
    finish_sync();
}

/* Submit the finish job (main thread).  The state body is built here —
 * store_count() is SQLite and may only run on the main thread; the
 * worker only POSTs the snapshot.  Only ever called from the remote
 * round loop (the server progress report makes no sense otherwise). */
static void
sync_submit_finish(void)
{
    SyncFinishArg *a = malloc(sizeof *a);
    if (a == NULL) {
        finish_sync(); /* best-effort: skip the report POST */
        return;
    }
    snprintf(a->url, sizeof a->url, "%s", g_state.url_state);
    snprintf(a->body, sizeof a->body,
             "{\"device\":\"pbemu\",\"cursor\":%lld,\"books\":%d}",
             g_sync_cursor,
             store_count());
    if (bs_worker_submit(sync_finish_post, sync_finish_done, a) == NULL) {
        free(a);
        finish_sync();
        return;
    }
}

/* done_cb for a round job: apply the batch and chain the next round or
 * the finish job.  The terminal paths (done, failed, capped) do not
 * chain. */
static void
sync_round_done(BsJob *job)
{
    SyncRoundArg *arg = job->arg;
    /* Consume and drop the result when the sync was aborted while the
     * fetch was in flight — the sync_state check catches a plain abort
     * (sync_state reset to 0), the generation check catches an abort
     * followed by an immediate restart (settings_apply does both in one
     * turn), where sync_state is already 1 again but the round belongs
     * to the old chain with the old endpoint/cursor.  A stale response
     * must never be applied to a newer chain. */
    if (!g_state.sync_state || arg->gen != g_sync_gen) {
        SyncRoundResult *r = job->result;
        if (r != NULL) {
            cJSON_Delete(r->root);
            free(r);
        }
        free(arg);
        return;
    }
    SyncRoundResult *r = job->result;
    int   rc = (r != NULL) ? r->rc : job->rc;
    int   rlen = (r != NULL) ? r->rlen : 0;

    /* Transport failure.  rc != 0 is exactly the old "rc != 0 ||
     * resp == NULL": http_post_timeout_status returns -1 whenever it
     * produced no body (see bs_net.c), and the worker frees the raw
     * body after parsing, so only rc reaches the main thread. */
    if (rc != 0) {
        LOG("[bookshelf] do_sync FAILED: url=%s rc=%d\n",
            g_state.url_delta, rc);
        g_state.sync_state = 2;
        sync_ui_active(0);
        free(r);
        free(job->arg);
        sync_ui_popup_fail();
        return;
    }
    LOG("[bookshelf] do_sync: retsize=%d cursor=%lld\n", rlen,
        g_sync_cursor);

    long long next = g_sync_cursor;
    int       more = 0;
    int       outcome;
    if (r != NULL && r->http_status != 0 && r->http_status != 200) {
        /* The server answered with an HTTP error status: the body is
         * an error response, not a delta (a transport failure above
         * yields no body at all).  Treat it exactly like a bad
         * response — abort the chain visibly, cursor un-advanced. */
        LOG("[bookshelf] sync: HTTP status %d with body; error response\n",
            r->http_status);
        cJSON_Delete(r->root);
        outcome = SYNC_ROUND_BAD_RESP;
    } else if (r == NULL || !r->parse_ok) {
        /* Bad JSON or a malformed delta, diagnosed on the worker. */
        cJSON_Delete(r->root);
        outcome = SYNC_ROUND_BAD_RESP;
    } else {
        outcome = sync_apply_round(r->root, g_sync_cursor, &next, &more);
    }
    free(r);
    free(job->arg);
    if (outcome != SYNC_ROUND_OK) {
        /* Bad response or store failure: abort the WHOLE chain here —
         * no finish job, no "done" popup.  The cursor was not
         * advanced, so the next sync retries from this delta. */
        LOG("[bookshelf] do_sync: round failed (outcome=%d); aborting\n",
            outcome);
        g_state.sync_state = 2;
        sync_ui_active(0);
        sync_ui_popup_fail();
        return;
    }
    g_sync_cursor = next;
    g_sync_rounds++;

    if (more && g_sync_rounds < 400) { /* 400 * SYNC_BATCH = 200k ceiling */
        g_state.sync_round = g_sync_rounds + 1;
        /* Repaint the progress sheet every few batches; the round trip
         * itself is the slow part, so the sheet tracks it live. */
        if (g_state.sync_popup && (g_sync_rounds % 5 == 0))
            sync_ui_popup_refresh();
        sync_submit_round();
        return;
    }
    LOG("[bookshelf] do_sync: rounds=%d cursor=%lld\n", g_sync_rounds,
        g_sync_cursor);
    sync_submit_finish();
}

void
do_sync(void)
{
    LOG("[bookshelf] do_sync ENTER url_delta=%s\n", g_state.url_delta);
    if (g_state.sync_state == 1) {
        LOG("[bookshelf] do_sync: already syncing, skipping\n");
        return;
    }
    g_state.sync_state = 1;
    sync_ui_active(1);
    /* A first sync is 200+ rounds: keep the device awake for the whole
     * chain (re-armed per round in sync_submit_round), or the auto-sleep
     * timer, which only input resets, would freeze the app mid-fetch. */
    sync_keep_awake();
    g_sync_gen++; /* new chain generation: stale rounds (see the gen
                     check in sync_round_done) can never apply */
    /* A previous sync may have hit the server before its cover cache was
     * warm; give failed covers one more chance each sync. */
    for (int i = 0; i < NCOVER_SLOTS; i++) {
        if (g_covers[i].state == 3)
            g_covers[i].state = 0;
    }

    /* Local sources have no server: sync = (re)import the on-device
     * library (all of /mnt/ext1), or nothing for the live Folder file
     * browser. */
    if (g_state.source == SOURCE_LOCAL) {
        if (g_state.sync_popup) {
            g_state.sync_stage = SYNC_STAGE_SCAN;
            sync_ui_popup_refresh();
        }
        /* The scan is async now: local_import_scanner() walks the tree
         * on the worker and applies the collected books in bounded
         * main-thread slices; its final slice ends the sync via
         * sync_local_finish(), so no finish_sync() here. */
        local_import_scanner();
        return;
    }
    if (g_state.source == SOURCE_FOLDER) {
        finish_sync();
        return;
    }

    /* Remote: async rounds.  One HTTP fetch per round runs as a worker
     * job; its done_cb applies each response on the main thread and
     * chains the next round, so the event loop keeps serving input
     * during a big first sync.  Cursor-based delta: each round fetches
     * at most SYNC_BATCH books, writes them in one transaction and
     * persists the cursor, so a 100k-book library syncs in bounded-RAM
     * rounds and resumes after a crash. */
    g_sync_cursor = store_get_cursor();
    g_sync_rounds = 0;
    sync_submit_round();
}

/* Abort any in-flight sync: bump the chain generation so rounds
 * submitted before the abort are dropped by sync_round_done's stale
 * guard (it consumes them without applying), clear the sync state and
 * stop the spinner.  Called before settings/source changes that
 * rebuild the endpoint URLs, so an in-flight chain never fetches the
 * next round from the new endpoint with the old cursor — and never
 * applies a stale response on top of the new configuration. */
void
sync_abort(void)
{
    g_sync_gen++;
    g_state.sync_state = 0;
    sync_ui_active(0);
    /* Drop an in-flight local scan chain the same way (its apply
     * slices check their own generation token). */
    local_scan_abort();
}

/* Terminal bookkeeping for the async local scan chain (bs_local.c):
 * the same close-out a remote sync's finish job runs, minus the
 * server state POST. */
void
sync_local_finish(void)
{
    finish_sync();
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

/* Path of the raw extracted cover for a local book (the exact bytes the
 * extractor pulled from the file — PNG or JPEG).  Persisting it makes
 * cover_tick skip the zip parse on every later view. */
void
cover_raw_path(const char *id, char *out, size_t cap)
{
    char safe[MAX_ID_LEN];
    snprintf(safe, sizeof safe, "%s", id);
    for (char *p = safe; *p; p++)
        if (*p == '/')
            *p = '_';
    snprintf(out, cap, "%s/%s.raw", g_covers_dir, safe);
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
/* Decode a cover image scaled to 240x360.  Sniffs PNG vs JPEG; on a
 * colour display the decode stays RGB24 (same choice as
 * load_cover_scaled).  The caller frees the returned bitmap. */
ibitmap *
load_image_scaled(const char *path)
{
    FILE         *f = fopen(path, "rb");
    unsigned char magic[8] = {0};
    if (f != NULL) {
        fread(magic, 1, sizeof magic, f);
        fclose(f);
    }
    int         is_png = magic[0] == 0x89 && magic[1] == 'P' && magic[2] == 'N' && magic[3] == 'G';
    PixelFormat fmt = g_display_color ? kFmtRGB24 : kFmtGrayscale8;
    ibitmap    *full = is_png ? LoadPNGToFormat(path, fmt) : LoadJPEGToFormat(path, fmt);
    if (full == NULL)
        return NULL;
    ibitmap *small = BitmapStretchCopy(full, 0, 0, full->width, full->height, 240, 360);
    free(full);
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

/* Bounded cover-cache sweep.  Every COVER_SWEEP_EVERY saves, if the
 * covers directory holds more than COVER_CACHE_MAX entries (.png and
 * .raw both count), the oldest by mtime are deleted until the cache
 * is back under the cap, so a huge library cannot grow the on-disk
 * cache without bound.  Same bounded single-pass directory pattern as
 * sweep_stale_parts in bs_downloads.c. */
#define COVER_SWEEP_EVERY 64
#define COVER_CACHE_MAX 4096

typedef struct {
    char name[MAX_ID_LEN + 8]; /* id + ".png"/".raw" */
    long mtime;
} CoverEnt;

static int
cover_mtime_cmp(const void *a, const void *b)
{
    const CoverEnt *ca = a;
    const CoverEnt *cb = b;
    return (ca->mtime > cb->mtime) - (ca->mtime < cb->mtime);
}

static int g_cover_saves; /* saves since the last sweep decision */

static void
cover_cache_sweep(void)
{
    DIR *d = opendir(g_covers_dir);
    if (d == NULL)
        return;
    int       cap = 0, n = 0;
    CoverEnt *ents = NULL;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        size_t len = strlen(e->d_name);
        int    is_png = len > 4 && strcmp(e->d_name + len - 4, ".png") == 0;
        int    is_raw = len > 4 && strcmp(e->d_name + len - 4, ".raw") == 0;
        if (!is_png && !is_raw)
            continue;
        if (n >= cap) {
            int       newcap = cap == 0 ? 512 : cap * 2;
            CoverEnt *nn = realloc(ents, (size_t)newcap * sizeof *nn);
            if (nn == NULL)
                break; /* grow failed: keep what we have */
            ents = nn;
            cap = newcap;
        }
        char path[MAX_PATH_LEN];
        snprintf(path, sizeof path, "%s/%s", g_covers_dir, e->d_name);
        struct stat st;
        ents[n].mtime = 0;
        if (iv_stat(path, &st) == 0)
            ents[n].mtime = (long)st.st_mtime;
        snprintf(ents[n].name, sizeof ents[n].name, "%s", e->d_name);
        n++;
    }
    closedir(d);
    if (n <= COVER_CACHE_MAX) {
        free(ents);
        return;
    }
    qsort(ents, (size_t)n, sizeof *ents, cover_mtime_cmp);
    int remove_n = n - COVER_CACHE_MAX;
    for (int i = 0; i < remove_n; i++) {
        char path[MAX_PATH_LEN];
        snprintf(path, sizeof path, "%s/%s", g_covers_dir, ents[i].name);
        unlink(path);
    }
    LOG("[bookshelf] covers: swept %d oldest entries (%d remain)\n",
        remove_n, n - remove_n);
    free(ents);
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
    /* Bounded cache: check the directory size every 64th save. */
    if ((++g_cover_saves % COVER_SWEEP_EVERY) == 0)
        cover_cache_sweep();
}