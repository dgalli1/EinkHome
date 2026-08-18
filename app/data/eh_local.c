/* eh_local.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_browser.h"
#include "eh_extract.h"
#include "eh_local.h"
#include "eh_model.h"
#include "eh_store.h"
#include "eh_ui.h"
#include "eh_worker.h"

#include <dirent.h>

/* ── local book sources ────────────────────────────────────────────────
 * One filesystem-backed source next to the remote Kavita library:
 *
 *  - SOURCE_LOCAL: every folder under /mnt/ext1 is walked for book
 *    files (the firmware's own library lives there).  The Folder
 *    source is a live file browser (eh_browser.c), not an import.
 *
 * The import replaces the source's rows wholesale (store_delete_source)
 * and marks every book downloaded=1 — the files ARE the books. */

/* The book-extension table (is_book_ext) and the djb2 "fld_" id hash
 * (hash_hex) live in eh_browser.c, shared with the folder-source
 * browser so both sources derive identical ids. */

/* The firmware libc exports __xstat (no plain `stat` alias) and the
 * cross headers hide it; ARM glibc uses kernel stat version 0. */
extern int __xstat(int ver, const char *path, struct stat *buf);

/* Scan caps (unchanged from the old synchronous walk): directory
 * depth and total books per import. */
#define EH_LOCAL_SCAN_DEPTH 8
#define EH_LOCAL_SCAN_CAP 20000

/* The directory walk now runs on the shared background worker
 * (eh_worker.c) so a big /mnt/ext1 tree never blocks the event loop;
 * the collected records are applied to the store on the main thread in
 * bounded slices (one SQLite transaction per slice of SYNC_BATCH),
 * mirroring the remote sync's round/apply structure.  The worker
 * touches no SQLite, no UI and no g_state. */

/* One collected file record — the lean subset of Book the walk can
 * fill without the SQLite metadata cache (author/title come from
 * store_local_meta_get / extract_book_meta during the apply). */
typedef struct {
    char id[EH_MAX_ID_LEN];
    char title[EH_MAX_TITLE_LEN];
    char filename[EH_MAX_PATH_LEN];
    char local_path[EH_MAX_PATH_LEN];
    char ext[8];
    char source[16];
    int  size;
} BsLocalFile;

/* The whole collected walk result (worker-allocated; the last
 * main-thread slice frees it). */
typedef struct {
    BsLocalFile *books;
    int        count;
    int        cap;
    int        truncated; /* LOCAL_SCAN_CAP hit (or grow failure) */
    char       src[16];
} BsLocalScanResult;

/* Chain generation: bumped on every local_import_scanner() kick and
 * by local_scan_abort() (called from sync_abort on settings/source
 * changes), so a stale in-flight chain — walk or apply — drops its
 * results and never calls the model's finish hook under a newer
 * chain.  Main thread only. */
static int g_local_scan_gen;

/* Append one record slot; NULL when the cap is hit (or the grow
 * failed), which stops the walk. */
static BsLocalFile *
local_result_append(BsLocalScanResult *res)
{
    if (res->count >= EH_LOCAL_SCAN_CAP) {
        res->truncated = 1;
        return NULL;
    }
    if (res->count >= res->cap) {
        int        newcap = res->cap == 0 ? 256 : res->cap * 2;
        BsLocalFile *nb = realloc(res->books, (size_t)newcap * sizeof *nb);
        if (nb == NULL) {
            res->truncated = 1;
            return NULL;
        }
        res->books = nb;
        res->cap = newcap;
    }
    return &res->books[res->count++];
}

/* Join "dir/name" into path (cap bytes); returns 0 on success, -1 when
 * the path cannot be represented. */
static int
local_scan_path(const char *dir, const char *name, char *path, size_t cap)
{
    size_t dlen = strlen(dir);
    size_t nlen = strlen(name);
    if (dlen + 1 + nlen >= cap)
        return -1; /* path too deep to represent */
    memcpy(path, dir, dlen);
    path[dlen] = '/';
    memcpy(path + dlen + 1, name, nlen);
    path[dlen + 1 + nlen] = '\0';
    return 0;
}

/* Classify a dirent as directory or regular file.  d_type is a hint:
 * DT_UNKNOWN filesystems (FAT, some FUSE) report nothing and symlinks
 * need following — resolve the real type by stat() whenever the
 * dirent type is inconclusive.  Hidden entries yield neither.  The
 * recursion depth cap bounds any symlink cycle. */
static void
local_scan_classify(const char *name, const char *path, int d_type,
                    int *is_dir, int *is_reg)
{
    if (name[0] == '.') {
        *is_dir = 0;
        *is_reg = 0;
        return;
    }
    if (d_type == DT_DIR) {
        *is_dir = 1;
        *is_reg = 0;
        return;
    }
    if (d_type == DT_REG) {
        *is_dir = 0;
        *is_reg = 1;
        return;
    }
    /* DT_UNKNOWN / DT_LNK: resolve by stat() (__xstat on the
     * firmware libc). */
    *is_dir = 0;
    *is_reg = 0;
    struct stat stbuf;
    if (__xstat(0, path, &stbuf) == 0) {
        *is_dir = S_ISDIR(stbuf.st_mode);
        *is_reg = S_ISREG(stbuf.st_mode);
    }
}

/* Normalize (lowercase) the filename's extension into ext and report
 * whether it is a book extension. */
static int
local_scan_is_book(const char *name, char *ext, size_t extcap)
{
    const char *dot = strrchr(name, '.');
    if (dot == NULL || dot[1] == '\0')
        return 0;
    size_t xlen = strlen(dot + 1);
    if (xlen >= extcap)
        xlen = extcap - 1;
    memcpy(ext, dot + 1, xlen);
    ext[xlen] = '\0';
    for (char *p = ext; *p; p++)
        *p = (char)((*p >= 'A' && *p <= 'Z') ? *p + 32 : *p);
    return eh_is_book_ext(ext);
}

/* Fill the leaf fields of a collected record (title, ext, size, path,
 * filename) from the walked entry.  Behavior mirrors the original
 * inline fill. */
static void
local_scan_fill(const char *dir, const char *name, const char *path,
                const char *ext, BsLocalFile *f)
{
    size_t dlen = strlen(dir);
    size_t nlen = strlen(name);
    size_t xlen = strlen(ext);
    /* Title = filename without extension, truncated to the field. */
    size_t stem_len = nlen > xlen + 1 ? nlen - (xlen + 1) : 0;
    if (stem_len > EH_MAX_TITLE_LEN - 1)
        stem_len = EH_MAX_TITLE_LEN - 1;
    memcpy(f->title, name, stem_len);
    f->title[stem_len] = '\0';
    snprintf(f->ext, sizeof f->ext, "%s", ext);
    struct stat stbuf;
    if (__xstat(0, path, &stbuf) == 0)
        f->size = (int)stbuf.st_size;
    /* Copy only the path bytes actually written (plus NUL). */
    memcpy(f->local_path, path, dlen + 1 + nlen + 1);
    size_t fname_len = nlen;
    if (fname_len >= sizeof f->filename)
        fname_len = sizeof f->filename - 1;
    memcpy(f->filename, name, fname_len);
    f->filename[fname_len] = '\0';
}

/* Worker: walk the tree and collect book records (blocking I/O only —
 * no SQLite, no UI, no g_state). */
static void
folder_scan_collect(const char *dir, int depth, BsLocalScanResult *res)
{
    if (depth > EH_LOCAL_SCAN_DEPTH || res->count >= EH_LOCAL_SCAN_CAP)
        return;
    DIR *d = opendir(dir);
    if (d == NULL)
        return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL && res->count < EH_LOCAL_SCAN_CAP) {
        char path[EH_MAX_PATH_LEN];
        if (local_scan_path(dir, e->d_name, path, sizeof path) != 0)
            continue; /* path too deep to represent */
        int is_dir, is_reg;
        local_scan_classify(e->d_name, path, e->d_type, &is_dir, &is_reg);
        if (is_dir) {
            folder_scan_collect(path, depth + 1, res);
            continue;
        }
        if (!is_reg)
            continue;
        char ext[8];
        if (!local_scan_is_book(e->d_name, ext, sizeof ext))
            continue;

        BsLocalFile *f = local_result_append(res);
        if (f == NULL)
            break; /* cap reached (or grow failed): stop scanning */
        char h[9];
        eh_hash_hex(path, h);
        snprintf(f->id, sizeof f->id, "fld_%s", h);
        local_scan_fill(dir, e->d_name, path, ext, f);
        snprintf(f->source, sizeof f->source, "%s", res->src);
    }
    closedir(d);
}

/* Worker entry: collect every book file under /mnt/ext1 into the job
 * result. */
static void
local_scan_walk(BsJob *job)
{
    BsLocalScanResult *res = calloc(1, sizeof *res);
    if (res == NULL) {
        job->rc = -1;
        atomic_store_explicit(&job->done, 1, memory_order_release);
        return;
    }
    snprintf(res->src, sizeof res->src, "local");
    folder_scan_collect(EH_BROWSE_ROOT, 0, res);
    if (res->truncated)
        eh_LOG("[bookshelf] local: scan cap %d reached, import truncated\n",
            EH_LOCAL_SCAN_CAP);
    job->result = res;
    job->rc = 0;
    atomic_store_explicit(&job->done, 1, memory_order_release);
}

/* Apply-chain argument: the shared result plus the next index to
 * apply.  Caller-allocated, freed by the last slice's done_cb. */
typedef struct {
    BsLocalScanResult *res;
    int              offset; /* next book index to apply */
    int              gen;
} BsLocalApplyArg;

/* Build the full Book record on the main thread (SQLite metadata
 * cache + file extraction) — the worker only collected the file
 * facts. */
static void
local_file_to_book(const BsLocalFile *f, BsBook *b)
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
    char mtitle[EH_MAX_TITLE_LEN], mauthor[80];
    if (eh_store_local_meta_get(f->id, mtitle, sizeof mtitle, mauthor,
                             sizeof mauthor)) {
        if (mtitle[0] != '\0')
            snprintf(b->title, sizeof b->title, "%s", mtitle);
        if (mauthor[0] != '\0')
            snprintf(b->author, sizeof b->author, "%s", mauthor);
    } else if (eh_extract_book_meta(f->local_path, f->ext, mtitle,
                                 sizeof mtitle, mauthor,
                                 sizeof mauthor) == 0) {
        if (mtitle[0] != '\0')
            snprintf(b->title, sizeof b->title, "%s", mtitle);
        if (mauthor[0] != '\0')
            snprintf(b->author, sizeof b->author, "%s", mauthor);
        eh_store_local_meta_put(f->id, mtitle, mauthor);
    }
}

/* No-op worker hop: the slice's real work (SQLite) runs in the done_cb
 * on the main thread; the hop just re-enters it through the worker
 * queue, mirroring dl_kick in eh_downloads.c. */
static void
local_apply_nop(BsJob *job)
{
    (void)job;
    atomic_store_explicit(&job->done, 1, memory_order_release);
}

static void local_apply_slice(BsJob *job);

/* Abort a failed local import the way the remote path aborts a failed
 * round: free the apply chain and surface the error to the UI
 * (sync_state=2, spinner off, popup fail) — the import is not "done",
 * so there is no success finish.  Main thread only. */
static void
local_apply_fail(BsLocalApplyArg *a)
{
    BsLocalScanResult *res = a->res;
    free(res->books);
    free(res);
    free(a);
    eh_g_state.sync_state = 2;
    eh_sync_set_active(0);
    eh_sync_popup_fail();
}

/* done_cb of the walk: hand the collected result to the apply chain. */
static void
local_scan_start_apply(BsJob *job)
{
    BsLocalApplyArg  *a = job->arg;
    BsLocalScanResult *res = job->result;
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
        eh_sync_local_finish();
        return;
    }
    a->res = res;
    a->offset = 0;
    if (eh_worker_submit(local_apply_nop, local_apply_slice, a) == NULL) {
        free(res->books);
        free(res);
        free(a);
        eh_sync_local_finish();
    }
}

/* done_cb for one apply hop: write one bounded slice (SQLite, main
 * thread) and chain the next, or finish. */
static void
local_apply_slice(BsJob *job)
{
    (void)job;
    BsLocalApplyArg   *a = job->arg;
    BsLocalScanResult *res = a->res;
    if (a->gen != g_local_scan_gen) {
        /* Aborted mid-apply (source switch / settings save): drop. */
        free(res->books);
        free(res);
        free(a);
        return;
    }
    int end = a->offset + EH_SYNC_BATCH;
    if (end > res->count)
        end = res->count;
    if (eh_store_begin() != 0) {
        /* Store failure: do not apply the slice; abort the import as
         * a store failure (the transaction never opened, so there is
         * nothing to roll back). */
        eh_LOG("[bookshelf] local: store_begin failed; aborting import\n");
        local_apply_fail(a);
        return;
    }
    if (a->offset == 0)
        eh_store_delete_source(res->src);
    for (int i = a->offset; i < end; i++) {
        BsBook b;
        local_file_to_book(&res->books[i], &b);
        if (eh_store_upsert_book(&b) != 0) {
            /* Store write failed (e.g. disk full): roll back the slice
             * so the partially-applied batch is never committed, and
             * abort the import instead of silently truncating it. */
            eh_LOG("[bookshelf] local: upsert failed id=%s; "
                "rolling back slice and aborting import\n", b.id);
            eh_store_rollback();
            local_apply_fail(a);
            return;
        }
    }
    if (eh_store_commit() != 0) {
        /* Commit failed: the transaction was rolled back inside the
         * store, so the slice was not persisted; abort the import
         * instead of silently truncating it. */
        eh_LOG("[bookshelf] local: store_commit failed; aborting import\n");
        local_apply_fail(a);
        return;
    }
    /* A full local import is also a long main-thread job: keep the
     * device awake across the slices (auto-expires after the last one). */
    eh_sync_keep_awake();
    /* Rows were applied (every slice writes at least one book): the
     * finish path must rebuild the view. */
    eh_g_sync_changed = 1;
    a->offset = end;
    /* Live progress for the sync popup: repaint the counter once per
     * applied slice — a full repaint per book would dominate the
     * apply on a large library. */
    if (eh_g_state.sync_popup && eh_g_state.sync_stage == EH_SYNC_STAGE_SCAN) {
        eh_g_state.sync_scan = a->offset;
        eh_sync_popup_refresh();
    }
    if (a->offset < res->count) {
        if (eh_worker_submit(local_apply_nop, local_apply_slice, a) == NULL) {
            free(res->books);
            free(res);
            free(a);
            eh_sync_local_finish();
        }
        return;
    }
    eh_LOG("[bookshelf] local: imported %d books (%s) from %s\n", res->count,
        res->src, EH_BROWSE_ROOT);
    free(res->books);
    free(res);
    free(a);
    eh_sync_local_finish();
}

/* The Local source: kick the async walk+apply chain for /mnt/ext1.
 * Safe to call from the boot path (eh_main.c EVT_INIT) and from
 * do_sync; a new kick invalidates any in-flight chain. */
void
eh_local_import_scanner(void)
{
    g_local_scan_gen++;
    BsLocalApplyArg *a = malloc(sizeof *a);
    if (a == NULL) {
        eh_LOG("[bookshelf] local: import start alloc failed\n");
        eh_sync_local_finish();
        return;
    }
    a->res = NULL;
    a->offset = 0;
    a->gen = g_local_scan_gen;
    if (eh_worker_submit(local_scan_walk, local_scan_start_apply, a) == NULL) {
        free(a);
        eh_LOG("[bookshelf] local: worker submit failed\n");
        eh_sync_local_finish();
        return;
    }
    eh_LOG("[bookshelf] local: import scan started\n");
}

/* Abort any in-flight local scan chain (called from sync_abort on
 * settings/source changes): the generation bump makes every queued
 * apply drop its slice. */
void
eh_local_scan_abort(void)
{
    g_local_scan_gen++;
}
