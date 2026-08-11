/* bs_local.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
#include "bs_browser.h"
#include "bs_extract.h"
#include "bs_local.h"
#include "bs_store.h"
#include "bs_ui.h"

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

static int g_folder_scan_count;

static void
folder_scan_dir(const char *dir, int depth, const char *src_label)
{
    if (depth > 8 || g_folder_scan_count >= 20000)
        return;
    DIR *d = opendir(dir);
    if (d == NULL)
        return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL && g_folder_scan_count < 20000) {
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
        if (e->d_type == DT_DIR) {
            folder_scan_dir(path, depth + 1, src_label);
            continue;
        }
        if (e->d_type != DT_REG)
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

        Book b;
        memset(&b, 0, sizeof b);
        char h[9];
        hash_hex(path, h);
        snprintf(b.id, sizeof b.id, "fld_%s", h);
        /* Title = filename without extension, truncated to the field. */
        size_t stem_len = nlen > xlen + 1 ? nlen - (xlen + 1) : 0;
        if (stem_len > MAX_TITLE_LEN - 1)
            stem_len = MAX_TITLE_LEN - 1;
        memcpy(b.title, e->d_name, stem_len);
        b.title[stem_len] = '\0';
        snprintf(b.ext, sizeof b.ext, "%s", ext);
        /* The firmware libc exports __xstat, not stat. */
        struct stat stbuf;
        if (__xstat(0, path, &stbuf) == 0)
            b.size = (int)stbuf.st_size;
        b.downloaded = 1;
        /* Copy only the path bytes actually written (plus NUL). */
        memcpy(b.local_path, path, dlen + 1 + nlen + 1);
        size_t fname_len = nlen;
        if (fname_len >= sizeof b.filename)
            fname_len = sizeof b.filename - 1;
        memcpy(b.filename, e->d_name, fname_len);
        b.filename[fname_len] = '\0';
        /* Metadata: the extraction cache spares the file parse on
         * re-imports — only unknown books get parsed. */
        char mtitle[MAX_TITLE_LEN], mauthor[80];
        if (store_local_meta_get(b.id, mtitle, sizeof mtitle, mauthor, sizeof mauthor)) {
            if (mtitle[0] != '\0')
                snprintf(b.title, sizeof b.title, "%s", mtitle);
            if (mauthor[0] != '\0')
                snprintf(b.author, sizeof b.author, "%s", mauthor);
        } else if (extract_book_meta(path, ext, mtitle, sizeof mtitle, mauthor, sizeof mauthor) ==
                   0) {
            if (mtitle[0] != '\0')
                snprintf(b.title, sizeof b.title, "%s", mtitle);
            if (mauthor[0] != '\0')
                snprintf(b.author, sizeof b.author, "%s", mauthor);
            store_local_meta_put(b.id, mtitle, mauthor);
        }
        snprintf(b.source, sizeof b.source, "%s", src_label);
        store_upsert_book(&b);
        g_folder_scan_count++;
        /* Live progress for the sync popup: repaint the counter every
         * 32 files — a full repaint per book would dominate the scan
         * on a large library. */
        if (g_state.sync_popup && g_state.sync_stage == SYNC_STAGE_SCAN &&
            (g_folder_scan_count & 31) == 0) {
            g_state.sync_scan = g_folder_scan_count;
            sync_popup_refresh();
        }
    }
    closedir(d);
}

/* Mirror every book file under `dir` (recursive) into the store with
 * ids "fld_<hash>"; the previous import of the same source is replaced
 * wholesale. */
static void
local_import_dir(const char *dir, const char *src_label)
{
    store_begin();
    store_delete_source(src_label);
    g_folder_scan_count = 0;
    folder_scan_dir(dir, 0, src_label);
    store_commit();
    LOG("[bookshelf] local: imported %d books (%s) from %s\n", g_folder_scan_count, src_label, dir);
}

/* The Local source: every folder under /mnt/ext1. */
void
local_import_scanner(void)
{
    local_import_dir(BROWSE_ROOT, "local");
}
