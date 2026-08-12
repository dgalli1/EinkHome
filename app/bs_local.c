/* bs_local.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
#include "bs_browser.h"
#include "bs_extract.h"
#include "bs_local.h"
#include "bs_model.h"
#include "bs_store.h"
#include "bs_ui.h"
#include "bs_worker.h"

#include <dirent.h>

/* ── local book sources ────────────────────────────────────────────────
 * One filesystem-backed source next to the remote Kavita library:
 *
 *  - SOURCE_LOCAL: every folder under /mnt/ext1 is walked for book
 *    files (the firmware's own library lives there).  The Folder
 *    source is a live file browser (bs_browser.c), not an import.
 *
 * The import replaces the source's rows wholesale (store_delete_source)
 * and marks every book downloaded=1 — the files ARE the books. */

/* The book-extension table (is_book_ext) and the djb2 "fld_" id hash
 * (hash_hex) live in bs_browser.c, shared with the folder-source
 * browser so both sources derive identical ids. */

/* The firmware libc exports __xstat (no plain `stat` alias) and the
 * cross headers hide it; ARM glibc uses kernel stat version 0. */
extern int __xstat(int ver, const char *path, struct stat *buf);

/* Scan caps (unchanged from the old synchronous walk): directory
 * depth and total books per import. */
#define LOCAL_SCAN_DEPTH 8
#define LOCAL_SCAN_CAP 20000

/* The directory walk now runs on the shared background worker
 * (bs_worker.c) so a big /mnt/ext1 tree never blocks the event loop;
 * the collected records are applied to the store on the main thread in
 * bounded slices (one SQLite transaction per slice of SYNC_BATCH),
 * mirroring the remote sync's round/apply structure.  The worker
 * touches no SQLite, no UI and no g_state. */

/* One collected file record — the lean subset of Book the walk can
 * fill without the SQLite metadata cache (author/title come from
 * store_local_meta_get / extract_book_meta during the apply). */
typedef struct {
    char id[MAX_ID_LEN];
    char title[MAX_TITLE_LEN];
    char filename[MAX_PATH_LEN];
    char local_path[MAX_PATH_LEN];
    char ext[8];
    char source[16];
    int  size;
} LocalFile;

/* The whole collected walk result (worker-allocated; the last
 * main-thread slice frees it). */
typedef struct {
    LocalFile *books;
    int        count;
    int        cap;
    int        truncated; /* LOCAL_SCAN_CAP hit (or grow failure) */
    char       src[16];
} LocalScanResult;

/* Chain generation: bumped on every local_import_scanner() kick and
 * by local_scan_abort() (called from sync_abort on settings/source
 * changes), so a stale in-flight chain — walk or apply — drops its
 * results and never calls the model's finish hook under a newer
 * chain.  Main thread only. */
static int g_local_scan_gen;

/* Append one record slot; NULL when the cap is hit (or the grow
 * failed), which stops the walk. */
static LocalFile *
local_result_append(LocalScanResult *res)
{
    if (res->count >= LOCAL_SCAN_CAP) {
        res->truncated = 1;
        return NULL;
    }
    if (res->count >= res->cap) {
        int        newcap = res->cap == 0 ? 256 : res->cap * 2;
        LocalFile *nb = realloc(res->books, (size_t)newcap * sizeof *nb);
        if (nb == NULL) {
            res->truncated = 1;
            return NULL;
        }
        res->books = nb;
        res->cap = newcap;
    }
    return &res->books[res->count++];
}

/* Worker: walk the tree and collect book records (blocking I/O only —
 * no SQLite, no UI, no g_state). */
static void
folder_scan_collect(const char *dir, int depth, LocalScanResult *res)
{
    if (depth > LOCAL_SCAN_DEPTH || res->count >= LOCAL_SCAN_CAP)
        return;
    DIR *d = opendir(dir);
    if (d == NULL)
        return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL && res->count < LOCAL_SCAN_CAP) {
        if (e->d_name[0] == '.')
            continue;
        char   path[MAX_PATH_LEN];
        size_t dlen = strlen(dir);
        size_t nlen = strlen(e->d_name);
        if (dlen + 1 + nlen >= sizeof path)
            continue; /* path too deep to represent */
        memcpy(path, dir, dlen);
        path[dlen] = '/';
        memcpy(path + dlen + 1, e->d_name, nlen);
        path[dlen + 1 + nlen] = '\0';
        /* d_type is a hint: DT_UNKNOWN filesystems (FAT, some FUSE)
         * report nothing and symlinks need following — resolve the
         * real type by stat() whenever the dirent type is
         * inconclusive.  The recursion depth cap below bounds any
         * symlink cycle. */
        int is_dir = e->d_type == DT_DIR;
        int is_reg = e->d_type == DT_REG;
        if (e->d_type == DT_UNKNOWN || e->d_type == DT_LNK) {
            struct stat stbuf;
            if (__xstat(0, path, &stbuf) == 0) {
                is_dir = S_ISDIR(stbuf.st_mode);
                is_reg = S_ISREG(stbuf.st_mode);
            }
        }
        if (is_dir) {
            folder_scan_collect(path, depth + 1, res);
            continue;
        }
        if (!is_reg)
            continue;
        const char *dot = strrchr(e->d_name, '.');
        if (dot == NULL || dot[1] == '\0')
            continue;
        char   ext[8];
        size_t xlen = strlen(dot + 1);
        if (xlen >= sizeof ext)
            xlen = sizeof ext - 1;
        memcpy(ext, dot + 1, xlen);
        ext[xlen] = '\0';
        for (char *p = ext; *p; p++)
            *p = (char)((*p >= 'A' && *p <= 'Z') ? *p + 32 : *p);
        if (!is_book_ext(ext))
            continue;

        LocalFile *f = local_result_append(res);
        if (f == NULL)
            break; /* cap reached (or grow failed): stop scanning */
        char h[9];
        hash_hex(path, h);
        snprintf(f->id, sizeof f->id, "fld_%s", h);
        /* Title = filename without extension, truncated to the field. */
        size_t stem_len = nlen > xlen + 1 ? nlen - (xlen + 1) : 0;
        if (stem_len > MAX_TITLE_LEN - 1)
            stem_len = MAX_TITLE_LEN - 1;
        memcpy(f->title, e->d_name, stem_len);
        f->title[stem_len] = '\0';
        snprintf(f->ext, sizeof f->ext, "%s", ext);
        /* The firmware libc exports __xstat, not stat. */
        struct stat stbuf;
        if (__xstat(0, path, &stbuf) == 0)
            f->size = (int)stbuf.st_size;
        /* Copy only the path bytes actually written (plus NUL). */
        memcpy(f->local_path, path, dlen + 1 + nlen + 1);
        size_t fname_len = nlen;
        if (fname_len >= sizeof f->filename)
            fname_len = sizeof f->filename - 1;
        memcpy(f->filename, e->d_name, fname_len);
        f->filename[fname_len] = '\0';
        snprintf(f->source, sizeof f->source, "%s", res->src);
    }
    closedir(d);
}

/* Worker entry: collect every book file under /mnt/ext1 into the job
 * result. */
static void
local_scan_walk(BsJob *job)
{
    LocalScanResult *res = calloc(1, sizeof *res);
    if (res == NULL) {
        job->rc = -1;
        __atomic_store_n(&job->done, 1, __ATOMIC_RELEASE);
        return;
    }
    snprintf(res->src, sizeof res->src, "local");
    folder_scan_collect(BROWSE_ROOT, 0, res);
    if (res->truncated)
        LOG("[bookshelf] local: scan cap %d reached, import truncated\n",
            LOCAL_SCAN_CAP);
    job->result = res;
    job->rc = 0;
    __atomic_store_n(&job->done, 1, __ATOMIC_RELEASE);
}

/* Apply-chain argument: the shared result plus the next index to
 * apply.  Caller-allocated, freed by the last slice's done_cb. */
typedef struct {
    LocalScanResult *res;
    int              offset; /* next book index to apply */
    int              gen;
} LocalApplyArg;

/* Build the full Book record on the main thread (SQLite metadata
 * cache + file extraction) — the worker only collected the file
 * facts. */
static void
local_file_to_book(const LocalFile *f, Book *b)
{
    memset(b, 0, sizeof *b);
    snprintf(b->id, sizeof b->id, "%s", f->id);
    snprintf(b->title, sizeof b->title, "%s", f->title);
    snprintf(b->ext, sizeof b->ext, "%s", f->ext);
    b->size = f->size;
    b->downloaded = 1;
    snprintf(b->local_path, sizeof b->local_path, "%s", f->local_path);
    snprintf(b->filename, sizeof b->filename, "%s", f->filename);
    snprintf(b->source, sizeof b->source, "%s", f->source);
    /* Metadata: the extraction cache spares the file parse on
     * re-imports — only unknown books get parsed. */
    char mtitle[MAX_TITLE_LEN], mauthor[80];
    if (store_local_meta_get(f->id, mtitle, sizeof mtitle, mauthor,
                             sizeof mauthor)) {
        if (mtitle[0] != '\0')
            snprintf(b->title, sizeof b->title, "%s", mtitle);
        if (mauthor[0] != '\0')
            snprintf(b->author, sizeof b->author, "%s", mauthor);
    } else if (extract_book_meta(f->local_path, f->ext, mtitle,
                                 sizeof mtitle, mauthor,
                                 sizeof mauthor) == 0) {
        if (mtitle[0] != '\0')
            snprintf(b->title, sizeof b->title, "%s", mtitle);
        if (mauthor[0] != '\0')
            snprintf(b->author, sizeof b->author, "%s", mauthor);
        store_local_meta_put(f->id, mtitle, mauthor);
    }
}

/* No-op worker hop: the slice's real work (SQLite) runs in the done_cb
 * on the main thread; the hop just re-enters it through the worker
 * queue, mirroring dl_kick in bs_downloads.c. */
static void
local_apply_nop(BsJob *job)
{
    (void)job;
    __atomic_store_n(&job->done, 1, __ATOMIC_RELEASE);
}

static void local_apply_slice(BsJob *job);

/* Abort a failed local import the way the remote path aborts a failed
 * round: free the apply chain and surface the error to the UI
 * (sync_state=2, spinner off, popup fail) — the import is not "done",
 * so there is no success finish.  Main thread only. */
static void
local_apply_fail(LocalApplyArg *a)
{
    LocalScanResult *res = a->res;
    free(res->books);
    free(res);
    free(a);
    g_state.sync_state = 2;
    sync_set_active(0);
    sync_popup_fail();
}

/* done_cb of the walk: hand the collected result to the apply chain. */
static void
local_scan_start_apply(BsJob *job)
{
    LocalApplyArg  *a = job->arg;
    LocalScanResult *res = job->result;
    if (a->gen != g_local_scan_gen) {
        /* Stale chain (aborted / re-kicked while walking): drop. */
        if (res != NULL) {
            free(res->books);
            free(res);
        }
        free(a);
        return;
    }
    if (res == NULL || res->count == 0) {
        /* Walk failure or empty library: nothing to import; still end
         * the sync (popup close, view rebuild, spinner off). */
        if (res != NULL) {
            free(res->books);
            free(res);
        }
        free(a);
        sync_local_finish();
        return;
    }
    a->res = res;
    a->offset = 0;
    if (bs_worker_submit(local_apply_nop, local_apply_slice, a) == NULL) {
        free(res->books);
        free(res);
        free(a);
        sync_local_finish();
    }
}

/* done_cb for one apply hop: write one bounded slice (SQLite, main
 * thread) and chain the next, or finish. */
static void
local_apply_slice(BsJob *job)
{
    (void)job;
    LocalApplyArg   *a = job->arg;
    LocalScanResult *res = a->res;
    if (a->gen != g_local_scan_gen) {
        /* Aborted mid-apply (source switch / settings save): drop. */
        free(res->books);
        free(res);
        free(a);
        return;
    }
    int end = a->offset + SYNC_BATCH;
    if (end > res->count)
        end = res->count;
    if (store_begin() != 0) {
        /* Store failure: do not apply the slice; abort the import as
         * a store failure (the transaction never opened, so there is
         * nothing to roll back). */
        LOG("[bookshelf] local: store_begin failed; aborting import\n");
        local_apply_fail(a);
        return;
    }
    if (a->offset == 0)
        store_delete_source(res->src);
    for (int i = a->offset; i < end; i++) {
        Book b;
        local_file_to_book(&res->books[i], &b);
        if (store_upsert_book(&b) != 0) {
            /* Store write failed (e.g. disk full): roll back the slice
             * so the partially-applied batch is never committed, and
             * abort the import instead of silently truncating it. */
            LOG("[bookshelf] local: upsert failed id=%s; "
                "rolling back slice and aborting import\n", b.id);
            store_rollback();
            local_apply_fail(a);
            return;
        }
    }
    store_commit();
    /* A full local import is also a long main-thread job: keep the
     * device awake across the slices (auto-expires after the last one). */
    sync_keep_awake();
    /* Rows were applied (every slice writes at least one book): the
     * finish path must rebuild the view. */
    g_sync_changed = 1;
    a->offset = end;
    /* Live progress for the sync popup: repaint the counter once per
     * applied slice — a full repaint per book would dominate the
     * apply on a large library. */
    if (g_state.sync_popup && g_state.sync_stage == SYNC_STAGE_SCAN) {
        g_state.sync_scan = a->offset;
        sync_popup_refresh();
    }
    if (a->offset < res->count) {
        if (bs_worker_submit(local_apply_nop, local_apply_slice, a) == NULL) {
            free(res->books);
            free(res);
            free(a);
            sync_local_finish();
        }
        return;
    }
    LOG("[bookshelf] local: imported %d books (%s) from %s\n", res->count,
        res->src, BROWSE_ROOT);
    free(res->books);
    free(res);
    free(a);
    sync_local_finish();
}

/* The Local source: kick the async walk+apply chain for /mnt/ext1.
 * Safe to call from the boot path (bs_main.c EVT_INIT) and from
 * do_sync; a new kick invalidates any in-flight chain. */
void
local_import_scanner(void)
{
    g_local_scan_gen++;
    LocalApplyArg *a = malloc(sizeof *a);
    if (a == NULL) {
        LOG("[bookshelf] local: import start alloc failed\n");
        sync_local_finish();
        return;
    }
    a->res = NULL;
    a->offset = 0;
    a->gen = g_local_scan_gen;
    if (bs_worker_submit(local_scan_walk, local_scan_start_apply, a) == NULL) {
        free(a);
        LOG("[bookshelf] local: worker submit failed\n");
        sync_local_finish();
        return;
    }
    LOG("[bookshelf] local: import scan started\n");
}

/* Abort any in-flight local scan chain (called from sync_abort on
 * settings/source changes): the generation bump makes every queued
 * apply drop its slice. */
void
local_scan_abort(void)
{
    g_local_scan_gen++;
}
