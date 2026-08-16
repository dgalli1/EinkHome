/* bs_browser.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_browser.h"
#include "bs_downloads.h"
#include "bs_model.h"
#include "bs_ui.h"

#include <dirent.h>

/* ── directory browsers (folder picker + folder source) ───────────────
 * One module hosts the two near-identical directory UIs, driven by a
 * mode set when each opens:
 *
 *  - PICKER: the Settings → Download folder overlay.  A full-screen
 *    sheet over the settings page that browses the tree confined to
 *    /mnt/ext1 (the list has no ".." above the root, so on-device
 *    storage is the only thing choosable).  Select commits the folder
 *    to g_settings_dl_dir (pending) and Back/Select return to
 *    OV_SETTINGS.
 *
 *  - BROWSER: the Folder library source.  A live file browser as the
 *    shelf body, rooted at BROWSE_ROOT; it drills into subdirectories
 *    and opens book files through the same launch_reader() flow the
 *    Kavita library uses.  Nothing is imported into the store while
 *    this source is active.
 *
 * The two modes share one set of state globals (path, scroll, drag,
 * entry list) — the picker only ever runs over OV_SETTINGS and the
 * browser only as the body mode, so they never both own the screen. */

typedef enum {
    BR_MODE_BROWSER, /* folder-source body browser */
    BR_MODE_PICKER,  /* settings download-folder overlay */
} BsBrowserMode;

static BsBrowserMode s_bmode = BR_MODE_BROWSER;

#define BS_BROWSE_MAX_ENTRIES 512

int  bs_g_browse_open = 0;
char bs_g_browse_path[256];
int  bs_g_browse_scroll = 0;
int  bs_g_browser_drag = 0;
int  bs_g_browser_drag_y = 0;
int  bs_g_browser_moved = 0;

/* Entries of g_browse_path, sorted; filled by browser_load().  The
 * browser mode carries ".." as entry 0 when below the root; the picker
 * draws its ".." row separately (see browser_can_go_up). */
static char g_browse_names[BS_BROWSE_MAX_ENTRIES][BS_MAX_PATH_LEN];
static char g_browse_is_dir[BS_BROWSE_MAX_ENTRIES];
static int  g_browse_count = 0;

/* Extensions the shelf treats as book files.  Shared with the Local
 * source import (bs_local.c) — keep both callers in lockstep. */
static const char *const BOOK_EXTS[] = {
    "epub", "pdf", "mobi", "azw", "azw3", "fb2", "djvu", "txt", "cbr", "cbz"};

int
bs_is_book_ext(const char *ext)
{
    if (ext == NULL)
        return 0;
    for (size_t i = 0; i < sizeof BOOK_EXTS / sizeof BOOK_EXTS[0]; i++)
        if (strcmp(ext, BOOK_EXTS[i]) == 0)
            return 1;
    return 0;
}

/* djb2 hash → 8 hex chars, for the stable opaque "fld_" ids both the
 * folder-source browser and the Local import derive from file paths.
 * Single home of the algorithm; bs_local.c calls this. */
void
bs_hash_hex(const char *s, char out[9])
{
    unsigned long h = 5381;
    for (const unsigned char *p = (const unsigned char *)s; *p; p++)
        h = h * 33 + *p;
    snprintf(out, 9, "%08lx", h & 0xfffffffful);
}

/* Display form of an absolute path: everything under /mnt/ext1 shows
 * relative to it (the user only has this partition — the mount point
 * is noise).  The root itself shows as "/"; paths outside the mount
 * are shown verbatim. */
const char *
bs_user_path_display(const char *path, char *out, size_t cap)
{
    static const char prefix[] = "/mnt/ext1";
    if (strncmp(path, prefix, sizeof prefix - 1) == 0) {
        if (path[sizeof prefix - 1] == '/')
            snprintf(out, cap, "%s", path + sizeof prefix - 1);
        else if (path[sizeof prefix - 1] == '\0')
            snprintf(out, cap, "/");
        else
            snprintf(out, cap, "%s", path); /* /mnt/ext1x — not ours */
    } else {
        snprintf(out, cap, "%s", path);
    }
    return out;
}

/* No ascent above the /mnt/ext1 root. */
static int
browser_can_go_up(void)
{
    return strcmp(bs_g_browse_path, "/mnt/ext1") != 0;
}

/* How many list rows the mode shows.  The picker overlay reserves the
 * header and the Select/Back band; the browser body runs from below
 * the top bar to the system panel. */
static int
browser_rows_visible(void)
{
    int avail;
    if (s_bmode == BR_MODE_PICKER)
        avail = bs_content_bottom() - BS_FOLDER_LIST_TOP - BS_FOLDER_BTN_H - BS_FOLDER_BTN_PAD;
    else
        avail = bs_content_bottom() - (BS_TOP_BAR_H + BS_TOP_BAR_PAD) - 8;
    int rows = avail / BS_FOLDER_ROW_H;
    return rows < 1 ? 1 : rows;
}

/* Clamp g_browse_scroll to the laid-out list and return the max.  In
 * the picker the ".." row steals one of the visible rows; in the
 * browser ".." is a list entry and is already counted in
 * g_browse_count. */
static int
browser_clamp_scroll(void)
{
    int rows = browser_rows_visible();
    int up_row = (s_bmode == BR_MODE_PICKER && browser_can_go_up()) ? 1 : 0;
    int max_scroll = g_browse_count - (rows - up_row);
    if (max_scroll < 0)
        max_scroll = 0;
    if (bs_g_browse_scroll < 0)
        bs_g_browse_scroll = 0;
    if (bs_g_browse_scroll > max_scroll)
        bs_g_browse_scroll = max_scroll;
    return max_scroll;
}

/* Fill g_browse_* with the current directory's entries.  The picker
 * lists subdirectories only; the browser lists ".." (when below the
* the root), then subdirectories, then book files. */

/* qsort comparator over row indices: directories before files, then
 * alphabetical; ties broken by original index so the sort is stable
 * (preserving the order the previous insertion sort produced for
 * equal names).  Index comparisons keep qsort cheap — no 220-byte
 * memcpy per compare. */
static int
browser_row_cmp(const void *a, const void *b)
{
    int ia = *(const int *)a;
    int ib = *(const int *)b;
    int da = g_browse_is_dir[ia];
    int db = g_browse_is_dir[ib];
    if (da != db)
        return db - da; /* directories first (is_dir is 0 or 1) */
    int c = strcmp(g_browse_names[ia], g_browse_names[ib]);
    if (c != 0)
        return c;
    return ia - ib; /* stable tie-break */
}

/* Resolve the real type of a dirent.  d_type is only a hint: DT_UNKNOWN
 * filesystems (FAT, some FUSE) report nothing and symlinks need
 * following, so stat() whenever the dirent type is inconclusive.  The
 * stat target is <path>/<name> where name comes from readdir (never
 * ".." or "/"), so the browser-root confinement is untouched. */
static int
browser_entry_is_dir(const struct dirent *e, int *is_reg)
{
    int is_dir = e->d_type == DT_DIR;
    int rreg = e->d_type == DT_REG;
    if (e->d_type == DT_UNKNOWN || e->d_type == DT_LNK) {
        char   path[BS_MAX_PATH_LEN];
        size_t plen = strlen(bs_g_browse_path);
        size_t nlen = strlen(e->d_name);
        if (plen + 1 + nlen < sizeof path) {
            memcpy(path, bs_g_browse_path, plen);
            path[plen] = '/';
            memcpy(path + plen + 1, e->d_name, nlen);
            path[plen + 1 + nlen] = '\0';
            struct stat st;
            if (iv_stat(path, &st) == 0) {
                is_dir = S_ISDIR(st.st_mode);
                rreg = S_ISREG(st.st_mode);
            }
        }
    }
    *is_reg = rreg;
    return is_dir;
}

/* True when name is a browser-mode book file: it ends in one of
 * BOOK_EXTS (case-insensitive).  Subdirectories are handled by the
 * caller. */
static int
browser_should_include_book(const char *name)
{
    const char *dot = strrchr(name, '.');
    if (dot == NULL || dot[1] == '\0')
        return 0;
    char   ext[8];
    size_t xlen = strlen(dot + 1);
    if (xlen >= sizeof ext)
        xlen = sizeof ext - 1;
    memcpy(ext, dot + 1, xlen);
    ext[xlen] = '\0';
    for (char *p = ext; *p; p++)
        *p = (char)((*p >= 'A' && *p <= 'Z') ? *p + 32 : *p);
    return bs_is_book_ext(ext);
}

/* True when a dirent row should be listed in the current mode: the
 * picker lists subdirectories only; the browser lists subdirectories
 * and book files. */
static int
browser_include_entry(int is_dir, int is_reg, const char *name)
{
    if (s_bmode == BR_MODE_PICKER)
        return is_dir;
    return is_dir || (is_reg && browser_should_include_book(name));
}

/* Write the rows into place once, following each sort cycle in place:
 * every row is moved exactly once (O(n) memcpys total). */
static void
browser_apply_order(int *order)
{
    for (int i = 0; i < g_browse_count; i++) {
        if (order[i] == i)
            continue;
        char name[BS_MAX_PATH_LEN];
        memcpy(name, g_browse_names[i], BS_MAX_PATH_LEN);
        int is_dir = g_browse_is_dir[i];
        int src = i;
        for (;;) {
            int dst = order[src];
            if (dst == i) {
                memcpy(g_browse_names[src], name, BS_MAX_PATH_LEN);
                g_browse_is_dir[src] = (char)is_dir;
            } else {
                memcpy(g_browse_names[src], g_browse_names[dst], BS_MAX_PATH_LEN);
                g_browse_is_dir[src] = g_browse_is_dir[dst];
            }
            order[src] = src; /* this slot is now placed */
            if (dst == i)
                break;
            src = dst;
        }
    }
}

static void
browser_load(void)
{
    g_browse_count = 0;
    if (s_bmode == BR_MODE_BROWSER && browser_can_go_up()) {
        snprintf(g_browse_names[0], BS_MAX_PATH_LEN, "..");
        g_browse_is_dir[0] = 1;
        g_browse_count = 1;
    }

    int max_entries =
        s_bmode == BR_MODE_PICKER ? BS_FOLDER_MAX_DIRS : BS_BROWSE_MAX_ENTRIES;
    DIR *d = opendir(bs_g_browse_path);
    if (d == NULL) {
        bs_LOG("[bookshelf] browser: opendir %s failed errno=%d\n", bs_g_browse_path, errno);
        return;
    }
    struct dirent *e;
    while ((e = readdir(d)) != NULL && g_browse_count < max_entries) {
        if (e->d_name[0] == '.')
            continue;
        int is_reg = 0;
        int is_dir = browser_entry_is_dir(e, &is_reg);
        if (!browser_include_entry(is_dir, is_reg, e->d_name))
            continue;
        /* Bounded copy: d_name can be NAME_MAX (255) while the row
         * buffer is MAX_PATH_LEN; overlong names are truncated. */
        size_t nlen = strlen(e->d_name);
        if (nlen >= BS_MAX_PATH_LEN)
            nlen = BS_MAX_PATH_LEN - 1;
        memcpy(g_browse_names[g_browse_count], e->d_name, nlen);
        g_browse_names[g_browse_count][nlen] = '\0';
        g_browse_is_dir[g_browse_count] = (char)is_dir;
        g_browse_count++;
    }
    closedir(d);

    /* Stable sort on the shared key: directories before files,
     * alphabetical within each group.  qsort over an index array
     * keeps comparisons cheap (no 220-byte memcpy per compare) and
     * the (name, original_index) tie-break preserves the insertion
     * sort's stability for equal names.  The picker's list is all
     * directories, so the dirs-first key reduces to the plain
     * alphabetical order the old picker produced. */
    int order[BS_BROWSE_MAX_ENTRIES];
    for (int i = 0; i < g_browse_count; i++)
        order[i] = i;
    // cppcheck-suppress uninitvar -- order[0..g_browse_count) fully initialised above.
    qsort(order, (size_t)g_browse_count, sizeof order[0], browser_row_cmp);
    browser_apply_order(order);
    bs_LOG("[bookshelf] browser: %s -> %d entries\n", bs_g_browse_path, g_browse_count);
}

/* Paint one list row across the full width: white fill, a separator
 * line under it, and the entry name in bold with a trailing "/" for
 * directories.  name == NULL paints the bare row (off-list slot). */
static void
browser_draw_row(int row_y, const char *name, int is_dir, ifont *f)
{
    int w = ScreenWidth();
    FillArea(0, row_y, w, BS_FOLDER_ROW_H, WHITE);
    DrawLine(0, row_y + BS_FOLDER_ROW_H, w, row_y + BS_FOLDER_ROW_H, LGRAY);
    if (name == NULL)
        return;
    if (f != NULL) {
        SetFont(f, BLACK);
        char trunc[BS_MAX_PATH_LEN + 4];
        snprintf(trunc, sizeof trunc, "%s%s", name, is_dir ? "/" : "");
        bs_utf8_fit_width(trunc, sizeof trunc, w - 64);
        DrawString(32, row_y + (BS_FOLDER_ROW_H - 28) / 2 - 2, trunc);
    }
}

/* Descend into a listed subdirectory (or ascend via ".."). */
static void
browser_navigate(const char *name)
{
    if (strcmp(name, "..") == 0) {
        char *slash = strrchr(bs_g_browse_path, '/');
        if (slash != NULL && slash != bs_g_browse_path)
            *slash = '\0';
    } else {
        /* Build <path>/<name> with explicit lengths: snprintf of two
         * MAX_PATH_LEN-sized strings into one buffer trips the
         * truncation analysis even though the cap is enforced. */
        char   next[sizeof bs_g_browse_path];
        size_t plen = strlen(bs_g_browse_path);
        size_t nlen = strlen(name);
        if (plen + 1 + nlen >= sizeof next)
            return; /* path already at the cap; cannot descend */
        memcpy(next, bs_g_browse_path, plen);
        next[plen] = '/';
        memcpy(next + plen + 1, name, nlen);
        next[plen + 1 + nlen] = '\0';
        memcpy(bs_g_browse_path, next, sizeof bs_g_browse_path);
    }
    bs_g_browse_scroll = 0;
    browser_load();
    if (s_bmode == BR_MODE_PICKER) {
        /* The picker is a full-screen overlay with its own header; the
         * top bar is not on screen. */
        bs_draw_overlay_folder();
    } else {
        /* The top bar carries the current directory as its title;
         * redraw it and flush just its band alongside the body. */
        bs_draw_top_bar();
        PartialUpdate(0, 0, ScreenWidth(), BS_TOP_BAR_H);
        bs_draw_browse();
    }
    bs_flush_content();
}

/* Open a book file with the same flow the Kavita library uses (reader
 * preference → OpenBook or direct launch). */
static void
browser_open_book(const char *path)
{
    BsBook b;
    memset(&b, 0, sizeof b);
    const char *base = strrchr(path, '/');
    base = base ? base + 1 : path;
    const char *dot = strrchr(base, '.');
    size_t      tlen = dot ? (size_t)(dot - base) : strlen(base);
    if (tlen > BS_MAX_TITLE_LEN - 1)
        tlen = BS_MAX_TITLE_LEN - 1;
    memcpy(b.title, base, tlen);
    b.title[tlen] = '\0';
    if (dot != NULL && dot[1] != '\0') {
        snprintf(b.ext, sizeof b.ext, "%s", dot + 1);
        for (char *p = b.ext; *p; p++)
            *p = (char)((*p >= 'A' && *p <= 'Z') ? *p + 32 : *p);
    }
    /* Stable id so the reader book-open handshake round-trips; the
     * Local import derives its fld_ ids from the same hash. */
    char h[9];
    bs_hash_hex(path, h);
    snprintf(b.id, sizeof b.id, "fld_%s", h);
    b.downloaded = 1;
    snprintf(b.local_path, sizeof b.local_path, "%s", path);
    snprintf(b.filename, sizeof b.filename, "%s", base);
    snprintf(b.source, sizeof b.source, "folder");
    bs_LOG("[bookshelf] browse: opening %s\n", path);
    bs_launch_reader(&b);
}

/* ── BROWSER mode (folder-source shelf body) ───────────────────────── */

void
bs_browse_start(const char *dir)
{
    s_bmode = BR_MODE_BROWSER;
    snprintf(bs_g_browse_path, sizeof bs_g_browse_path, "%s", dir);
    bs_g_browse_scroll = 0;
    bs_g_browser_drag = 0;
    bs_g_browser_moved = 0;
    browser_load();
    bs_g_browse_open = 1;
    /* The browser is the shelf body; a full shelf redraw shows the top
     * bar (with the path as title) plus the list. */
    bs_redraw_shelf();
}

/* Ascend one level; returns 0 when already at the browser root (the
 * caller then decides what "back" means). */
int
bs_browse_up(void)
{
    if (strcmp(bs_g_browse_path, BS_BROWSE_ROOT) == 0)
        return 0;
    char *slash = strrchr(bs_g_browse_path, '/');
    if (slash != NULL && slash != bs_g_browse_path)
        *slash = '\0';
    bs_g_browse_scroll = 0;
    browser_load();
    /* The top bar shows the current directory as its title; the body
     * alone is not enough on an ascend. */
    bs_draw_top_bar();
    PartialUpdate(0, 0, ScreenWidth(), BS_TOP_BAR_H);
    bs_draw_browse();
    bs_flush_content();
    return 1;
}

/* Page the list one screen; dir > 0 = forward.  draw_browse clamps. */
void
bs_browse_page(int dir)
{
    bs_g_browse_scroll += dir * browser_rows_visible();
    bs_draw_browse();
    bs_flush_content();
}

void
bs_draw_browse(void)
{
    int w = ScreenWidth();
    int top = BS_TOP_BAR_H + BS_TOP_BAR_PAD;
    int bottom = bs_content_bottom();
    int rows = browser_rows_visible();
    int max_scroll = browser_clamp_scroll();

    /* Body only — the top bar (with the path as its title) is drawn by
     * the caller.  The fill starts below the top bar's bottom border
     * (TOP_BAR_PAD gap), so it leaves the border intact. */
    FillArea(0, top, w, bottom - top, WHITE);

    /* Row font opened once for the whole listing pass instead of once
     * per row. */
    ifont *rf = OpenFont(DEFAULTFONTB, 28, 0);
    for (int i = 0; i < rows; i++) {
        int idx = bs_g_browse_scroll + i;
        int row_y = top + 8 + i * BS_FOLDER_ROW_H;
        const char *name = (idx >= 0 && idx < g_browse_count) ? g_browse_names[idx] : NULL;
        browser_draw_row(row_y, name, name ? g_browse_is_dir[idx] : 0, rf);
    }
    if (rf != NULL)
        CloseFont(rf);
    if (g_browse_count == 0) {
        ifont *f = OpenFont(DEFAULTFONT, 26, 0);
        if (f != NULL) {
            SetFont(f, DGRAY);
            DrawString(32, top + 32, bs_i18n("folder.empty"));
            CloseFont(f);
        }
    }
    bs_draw_scroll_buttons(bs_g_browse_scroll > 0, bs_g_browse_scroll < max_scroll);
}

int
bs_on_tap_browse(int x, int y)
{
    /* Corner scroll buttons page the listing. */
    int dir = bs_hit_scroll_button(x, y);
    if (dir != 0) {
        bs_browse_page(dir);
        return 1;
    }
    (void)x; /* rows span the full width; only y matters */
    if (y < BS_TOP_BAR_H + BS_TOP_BAR_PAD)
        return 1;
    int idx = (y - (BS_TOP_BAR_H + BS_TOP_BAR_PAD + 8)) / BS_FOLDER_ROW_H + bs_g_browse_scroll;
    if (idx < 0 || idx >= g_browse_count)
        return 1;
    if (g_browse_is_dir[idx]) {
        browser_navigate(g_browse_names[idx]);
    } else {
        char   path[BS_MAX_PATH_LEN];
        size_t plen = strlen(bs_g_browse_path);
        size_t nlen = strlen(g_browse_names[idx]);
        if (plen + 1 + nlen < sizeof path) {
            memcpy(path, bs_g_browse_path, plen);
            path[plen] = '/';
            memcpy(path + plen + 1, g_browse_names[idx], nlen);
            path[plen + 1 + nlen] = '\0';
            browser_open_book(path);
        }
    }
    return 1;
}

/* ── PICKER mode (settings download-folder overlay) ────────────────── */

/* Bottom button row: Select (filled, left) and Back (right). */
static void
folder_buttons(int *sel_x, int *sel_y, int *sel_w, int *sel_h)
{
    int w = ScreenWidth();
    *sel_w = (w - 64 - 16) / 2;
    *sel_h = BS_FOLDER_BTN_H;
    *sel_x = 32;
    *sel_y = bs_content_bottom() - BS_FOLDER_BTN_H - BS_FOLDER_BTN_PAD;
}

/* Commit the browsed folder as the pending downloads-folder setting. */
static void
folder_commit(void)
{
    snprintf(bs_g_settings_dl_dir, sizeof bs_g_settings_dl_dir, "%s", bs_g_browse_path);
    bs_LOG("[bookshelf] folder: selected %s\n", bs_g_browse_path);
    /* The picker opened on top of the settings page; closing it
     * restores settings as the top overlay. */
    bs_g_state.overlay = BS_OV_SETTINGS;
    bs_draw_overlay_settings();
    bs_flush_content();
}

void
bs_folder_close(void)
{
    /* The picker opened on top of the settings page; closing it
     * restores settings as the top overlay. */
    bs_g_state.overlay = BS_OV_SETTINGS;
    bs_draw_overlay_settings();
    bs_flush_content();
}

/* Open the picker from the settings page.  Starts at the /mnt/ext1
 * root so the user can only choose on-device storage. */
void
bs_folder_open(void)
{
    s_bmode = BR_MODE_PICKER;
    snprintf(bs_g_browse_path, sizeof bs_g_browse_path, "/mnt/ext1");
    bs_g_browse_scroll = 0;
    bs_g_browser_drag = 0;
    bs_g_browser_moved = 0;
    browser_load();
    /* The picker opens ON TOP of the settings page (settings stays
     * conceptually underneath and is restored on close). */
    bs_g_state.overlay = BS_OV_FOLDER;
    bs_draw_overlay_folder();
    bs_flush_content();
}

/* Overlay header: title + current path (shown relative to /mnt/ext1 —
 * the mount point is hidden from the user). */
static void
overlay_header(int w)
{
    ifont *tf = OpenFont(DEFAULTFONTB, 32, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(32, 24, bs_i18n("settings.dl_dir"));
        CloseFont(tf);
    }
    ifont *pf = OpenFont(DEFAULTFONT, 22, 0);
    if (pf != NULL) {
        SetFont(pf, DGRAY);
        char trunc[256];
        bs_user_path_display(bs_g_browse_path, trunc, sizeof trunc);
        bs_utf8_fit_width(trunc, sizeof trunc, w - 64);
        DrawString(32, 76, trunc);
        CloseFont(pf);
    }
}

/* Overlay list: directory rows (a ".." row leads up whenever below the
 * /mnt/ext1 root) plus the empty-list message.  Row font opened once
 * for the pass. */
static void
overlay_rows(void)
{
    int rows = browser_rows_visible();
    int up_row = browser_can_go_up() ? 1 : 0;
    ifont *rf = OpenFont(DEFAULTFONTB, 28, 0);
    int shown = 0;
    for (int i = 0; i < rows; i++) {
        int row_y = BS_FOLDER_LIST_TOP + i * BS_FOLDER_ROW_H;
        const char *name = NULL;
        if (up_row && i == 0) {
            name = "..";
        } else {
            int idx = bs_g_browse_scroll + i - up_row;
            if (idx >= 0 && idx < g_browse_count)
                name = g_browse_names[idx];
        }
        browser_draw_row(row_y, name, 1, rf); /* picker rows are all dirs */
        if (name != NULL)
            shown++;
    }
    if (rf != NULL)
        CloseFont(rf);
    if (shown == 0) {
        ifont *f = OpenFont(DEFAULTFONT, 26, 0);
        if (f != NULL) {
            SetFont(f, DGRAY);
            DrawString(32, BS_FOLDER_LIST_TOP + 24, bs_i18n("folder.empty"));
            CloseFont(f);
        }
    }
}

/* Handle a tap on the corner scroll buttons (raised above the
 * Select/Back band).  Returns 1 when consumed. */
static int
overlay_tap_scroll(int x, int y)
{
    int sy0 = bs_content_bottom() - BS_FOLDER_BTN_H - BS_FOLDER_BTN_PAD - BS_SCROLL_BTN_H;
    int dir = bs_hit_scroll_button_at(x, y, sy0);
    if (dir != 0) {
        int rows = browser_rows_visible();
        bs_g_browse_scroll += dir * rows;
        if (bs_g_browse_scroll < 0)
            bs_g_browse_scroll = 0;
        bs_draw_overlay_folder();
        bs_flush_content();
        return 1;
    }
    return 0;
}

/* Handle a tap inside the list rows (including the ".." up row).
 * Returns 1 when the tap reached the list area. */
static int
overlay_tap_row(int y)
{
    int rows = browser_rows_visible();
    int up_row = browser_can_go_up() ? 1 : 0;
    int idx = (y - BS_FOLDER_LIST_TOP) / BS_FOLDER_ROW_H;
    if (idx < 0 || idx >= rows)
        return 0;
    if (up_row && idx == 0) {
        browser_navigate("..");
        return 1;
    }
    int dir_idx = bs_g_browse_scroll + idx - up_row;
    if (dir_idx >= 0 && dir_idx < g_browse_count)
        browser_navigate(g_browse_names[dir_idx]);
    return 1;
}

/* Handle a tap on the picker overlay.  Returns 1 when the tap was
 * consumed (it always is — the picker owns the screen while open). */
int
bs_on_tap_folder(int x, int y)
{
    if (overlay_tap_scroll(x, y))
        return 1;
    int sx, sy, sw, sh;
    folder_buttons(&sx, &sy, &sw, &sh);
    if (y >= sy && y < sy + sh - 12) {
        if (x >= sx && x < sx + sw) {
            folder_commit();
        } else {
            bs_folder_close();
        }
        return 1;
    }
    if (y < BS_FOLDER_LIST_TOP)
        return 1;
    overlay_tap_row(y);
    return 1;
}

void
bs_draw_overlay_folder(void)
{
    int w = ScreenWidth();
    int h = bs_content_bottom();
    int max_scroll = browser_clamp_scroll();
    FillArea(0, 0, w, h, WHITE);

    /* Header: title + current path. */
    overlay_header(w);
    DrawLine(0, BS_FOLDER_LIST_TOP - 12, w, BS_FOLDER_LIST_TOP - 12, BLACK);

    /* Directory rows. */
    overlay_rows();

    /* Select / Back.  Button font opened once for both. */
    int sx, sy, sw, sh;
    folder_buttons(&sx, &sy, &sw, &sh);
    FillArea(sx, sy, sw, sh - 12, BLACK);
    DrawRect(sx, sy, sw, sh - 12, BLACK);
    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, WHITE);
        int tw = StringWidth(bs_i18n("folder.select"));
        DrawString(sx + (sw - tw) / 2, sy + (sh - 12 - 32) / 2, bs_i18n("folder.select"));
    }
    int bx = sx + sw + 16;
    FillArea(bx, sy, sw, sh - 12, WHITE);
    DrawRect(bx, sy, sw, sh - 12, BLACK);
    if (f != NULL) {
        SetFont(f, BLACK);
        int tw = StringWidth(bs_i18n("settings.back"));
        DrawString(bx + (sw - tw) / 2, sy + (sh - 12 - 32) / 2, bs_i18n("settings.back"));
        CloseFont(f);
    }

    /* Corner scroll buttons, raised above the Select/Back band. */
    int sy0 = bs_content_bottom() - BS_FOLDER_BTN_H - BS_FOLDER_BTN_PAD - BS_SCROLL_BTN_H;
    bs_draw_scroll_buttons_at(bs_g_browse_scroll > 0, bs_g_browse_scroll < max_scroll, sy0);
}
