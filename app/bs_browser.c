/* bs_browser.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
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
} BrowserMode;

static BrowserMode s_bmode = BR_MODE_BROWSER;

#define BROWSE_MAX_ENTRIES 512

int  g_browse_open = 0;
char g_browse_path[256];
int  g_browse_scroll = 0;
int  g_browser_drag = 0;
int  g_browser_drag_y = 0;
int  g_browser_moved = 0;

/* Entries of g_browse_path, sorted; filled by browser_load().  The
 * browser mode carries ".." as entry 0 when below the root; the picker
 * draws its ".." row separately (see browser_can_go_up). */
static char g_browse_names[BROWSE_MAX_ENTRIES][MAX_PATH_LEN];
static char g_browse_is_dir[BROWSE_MAX_ENTRIES];
static int  g_browse_count = 0;

/* Extensions the shelf treats as book files.  Shared with the Local
 * source import (bs_local.c) — keep both callers in lockstep. */
static const char *const BOOK_EXTS[] = {
    "epub", "pdf", "mobi", "azw", "azw3", "fb2", "djvu", "txt", "cbr", "cbz"};

int
is_book_ext(const char *ext)
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
hash_hex(const char *s, char out[9])
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
user_path_display(const char *path, char *out, size_t cap)
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
    return strcmp(g_browse_path, "/mnt/ext1") != 0;
}

/* How many list rows the mode shows.  The picker overlay reserves the
 * header and the Select/Back band; the browser body runs from below
 * the top bar to the system panel. */
static int
browser_rows_visible(void)
{
    int avail;
    if (s_bmode == BR_MODE_PICKER)
        avail = content_bottom() - FOLDER_LIST_TOP - FOLDER_BTN_H - FOLDER_BTN_PAD;
    else
        avail = content_bottom() - (TOP_BAR_H + TOP_BAR_PAD) - 8;
    int rows = avail / FOLDER_ROW_H;
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
    if (g_browse_scroll < 0)
        g_browse_scroll = 0;
    if (g_browse_scroll > max_scroll)
        g_browse_scroll = max_scroll;
    return max_scroll;
}

/* Fill g_browse_* with the current directory's entries.  The picker
 * lists subdirectories only; the browser lists ".." (when below the
 * root), then subdirectories, then book files. */
static void
browser_load(void)
{
    g_browse_count = 0;
    if (s_bmode == BR_MODE_BROWSER && browser_can_go_up()) {
        snprintf(g_browse_names[0], MAX_PATH_LEN, "..");
        g_browse_is_dir[0] = 1;
        g_browse_count = 1;
    }

    int max_entries =
        s_bmode == BR_MODE_PICKER ? FOLDER_MAX_DIRS : BROWSE_MAX_ENTRIES;
    DIR *d = opendir(g_browse_path);
    if (d == NULL) {
        LOG("[bookshelf] browser: opendir %s failed errno=%d\n", g_browse_path, errno);
        return;
    }
    struct dirent *e;
    while ((e = readdir(d)) != NULL && g_browse_count < max_entries) {
        if (e->d_name[0] == '.')
            continue;
        /* d_type is a hint: DT_UNKNOWN filesystems (FAT, some FUSE)
         * report nothing and symlinks need following, so resolve the
         * real type by stat() whenever the dirent type is
         * inconclusive.  The stat target is <path>/<name> where name
         * comes from readdir (never ".." or "/"), so the browser-root
         * confinement is untouched. */
        int is_dir = e->d_type == DT_DIR;
        int is_reg = e->d_type == DT_REG;
        if (e->d_type == DT_UNKNOWN || e->d_type == DT_LNK) {
            char   path[MAX_PATH_LEN];
            size_t plen = strlen(g_browse_path);
            size_t nlen = strlen(e->d_name);
            if (plen + 1 + nlen < sizeof path) {
                memcpy(path, g_browse_path, plen);
                path[plen] = '/';
                memcpy(path + plen + 1, e->d_name, nlen);
                path[plen + 1 + nlen] = '\0';
                struct stat st;
                if (iv_stat(path, &st) == 0) {
                    is_dir = S_ISDIR(st.st_mode);
                    is_reg = S_ISREG(st.st_mode);
                }
            }
        }
        if (s_bmode == BR_MODE_PICKER) {
            if (!is_dir)
                continue;
        } else {
            if (!is_dir && !is_reg)
                continue;
            if (!is_dir) {
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
            }
        }
        /* Bounded copy: d_name can be NAME_MAX (255) while the row
         * buffer is MAX_PATH_LEN; overlong names are truncated. */
        size_t nlen = strlen(e->d_name);
        if (nlen >= MAX_PATH_LEN)
            nlen = MAX_PATH_LEN - 1;
        memcpy(g_browse_names[g_browse_count], e->d_name, nlen);
        g_browse_names[g_browse_count][nlen] = '\0';
        g_browse_is_dir[g_browse_count] = (char)is_dir;
        g_browse_count++;
    }
    closedir(d);

    /* Stable insertion sort on the shared key: directories before
     * files, alphabetical within each group.  The picker's list is all
     * directories, so the dirs-first key reduces to the plain
     * alphabetical order the old picker produced. */
    for (int i = 1; i < g_browse_count; i++) {
        char name[MAX_PATH_LEN];
        memcpy(name, g_browse_names[i], MAX_PATH_LEN);
        int is_dir = g_browse_is_dir[i];
        int j = i - 1;
        while (j >= 0) {
            int jd = g_browse_is_dir[j];
            int j_before = (is_dir != jd) ? is_dir : (strcmp(name, g_browse_names[j]) < 0);
            if (!j_before)
                break;
            memcpy(g_browse_names[j + 1], g_browse_names[j], MAX_PATH_LEN);
            g_browse_is_dir[j + 1] = g_browse_is_dir[j];
            j--;
        }
        memcpy(g_browse_names[j + 1], name, MAX_PATH_LEN);
        g_browse_is_dir[j + 1] = (char)is_dir;
    }
    LOG("[bookshelf] browser: %s -> %d entries\n", g_browse_path, g_browse_count);
}

/* Drop whole characters from the end of `s` until it fits `max_w`
 * pixels.  Never splits a multibyte UTF-8 sequence: the last char is
 * backed up to over its continuation bytes, so a sequence is either
 * kept intact or removed entirely.  Stops once the string is down to
 * `min_len` bytes — a lone over-wide glyph then overflows the band
 * instead of looping forever.  Returns `s`. */
static char *
utf8_fit_width(char *s, int max_w, size_t min_len)
{
    while (StringWidth(s) > max_w) {
        size_t len = strlen(s);
        if (len <= min_len)
            break;
        size_t i = len - 1;
        while (i > 0 && ((unsigned char)s[i] & 0xC0) == 0x80)
            i--;
        s[i] = '\0';
    }
    return s;
}

/* Paint one list row across the full width: white fill, a separator
 * line under it, and the entry name in bold with a trailing "/" for
 * directories.  name == NULL paints the bare row (off-list slot). */
static void
browser_draw_row(int row_y, const char *name, int is_dir)
{
    int w = ScreenWidth();
    FillArea(0, row_y, w, FOLDER_ROW_H, WHITE);
    DrawLine(0, row_y + FOLDER_ROW_H, w, row_y + FOLDER_ROW_H, LGRAY);
    if (name == NULL)
        return;
    ifont *f = OpenFont(DEFAULTFONTB, 28, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        char trunc[MAX_PATH_LEN + 4];
        snprintf(trunc, sizeof trunc, "%s%s", name, is_dir ? "/" : "");
        utf8_fit_width(trunc, w - 64, 4);
        DrawString(32, row_y + (FOLDER_ROW_H - 28) / 2 - 2, trunc);
        CloseFont(f);
    }
}

/* Descend into a listed subdirectory (or ascend via ".."). */
static void
browser_navigate(const char *name)
{
    if (strcmp(name, "..") == 0) {
        char *slash = strrchr(g_browse_path, '/');
        if (slash != NULL && slash != g_browse_path)
            *slash = '\0';
    } else {
        /* Build <path>/<name> with explicit lengths: snprintf of two
         * MAX_PATH_LEN-sized strings into one buffer trips the
         * truncation analysis even though the cap is enforced. */
        char   next[sizeof g_browse_path];
        size_t plen = strlen(g_browse_path);
        size_t nlen = strlen(name);
        if (plen + 1 + nlen >= sizeof next)
            return; /* path already at the cap; cannot descend */
        memcpy(next, g_browse_path, plen);
        next[plen] = '/';
        memcpy(next + plen + 1, name, nlen);
        next[plen + 1 + nlen] = '\0';
        memcpy(g_browse_path, next, sizeof g_browse_path);
    }
    g_browse_scroll = 0;
    browser_load();
    if (s_bmode == BR_MODE_PICKER)
        draw_overlay_folder();
    else
        draw_browse();
    flush_content();
}

/* Open a book file with the same flow the Kavita library uses (reader
 * preference → OpenBook or direct launch). */
static void
browser_open_book(const char *path)
{
    Book b;
    memset(&b, 0, sizeof b);
    const char *base = strrchr(path, '/');
    base = base ? base + 1 : path;
    const char *dot = strrchr(base, '.');
    size_t      tlen = dot ? (size_t)(dot - base) : strlen(base);
    if (tlen > MAX_TITLE_LEN - 1)
        tlen = MAX_TITLE_LEN - 1;
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
    hash_hex(path, h);
    snprintf(b.id, sizeof b.id, "fld_%s", h);
    b.downloaded = 1;
    snprintf(b.local_path, sizeof b.local_path, "%s", path);
    snprintf(b.filename, sizeof b.filename, "%s", base);
    snprintf(b.source, sizeof b.source, "folder");
    LOG("[bookshelf] browse: opening %s\n", path);
    launch_reader(&b);
}

/* ── BROWSER mode (folder-source shelf body) ───────────────────────── */

void
browse_start(const char *dir)
{
    s_bmode = BR_MODE_BROWSER;
    snprintf(g_browse_path, sizeof g_browse_path, "%s", dir);
    g_browse_scroll = 0;
    g_browser_drag = 0;
    g_browser_moved = 0;
    browser_load();
    g_browse_open = 1;
    /* The browser is the shelf body; a full shelf redraw shows the top
     * bar (with the path as title) plus the list. */
    redraw_shelf();
}

/* Ascend one level; returns 0 when already at the browser root (the
 * caller then decides what "back" means). */
int
browse_up(void)
{
    if (strcmp(g_browse_path, BROWSE_ROOT) == 0)
        return 0;
    char *slash = strrchr(g_browse_path, '/');
    if (slash != NULL && slash != g_browse_path)
        *slash = '\0';
    g_browse_scroll = 0;
    browser_load();
    draw_browse();
    flush_content();
    return 1;
}

/* Page the list one screen; dir > 0 = forward.  draw_browse clamps. */
void
browse_page(int dir)
{
    g_browse_scroll += dir * browser_rows_visible();
    draw_browse();
    flush_content();
}

void
draw_browse(void)
{
    int w = ScreenWidth();
    int top = TOP_BAR_H + TOP_BAR_PAD;
    int bottom = content_bottom();
    int rows = browser_rows_visible();
    int max_scroll = browser_clamp_scroll();

    /* Body only — the top bar (with the path as its title) is drawn by
     * the caller.  The fill starts below the top bar's bottom border
     * (TOP_BAR_PAD gap), so it leaves the border intact. */
    FillArea(0, top, w, bottom - top, WHITE);

    for (int i = 0; i < rows; i++) {
        int idx = g_browse_scroll + i;
        int row_y = top + 8 + i * FOLDER_ROW_H;
        const char *name = (idx >= 0 && idx < g_browse_count) ? g_browse_names[idx] : NULL;
        browser_draw_row(row_y, name, name ? g_browse_is_dir[idx] : 0);
    }
    if (g_browse_count == 0) {
        ifont *f = OpenFont(DEFAULTFONT, 26, 0);
        if (f != NULL) {
            SetFont(f, DGRAY);
            DrawString(32, top + 32, i18n("folder.empty"));
            CloseFont(f);
        }
    }
    draw_scroll_buttons(g_browse_scroll > 0, g_browse_scroll < max_scroll);
}

int
on_tap_browse(int x, int y)
{
    /* Corner scroll buttons page the listing. */
    int dir = hit_scroll_button(x, y);
    if (dir != 0) {
        browse_page(dir);
        return 1;
    }
    (void)x; /* rows span the full width; only y matters */
    if (y < TOP_BAR_H + TOP_BAR_PAD)
        return 1;
    int idx = (y - (TOP_BAR_H + TOP_BAR_PAD + 8)) / FOLDER_ROW_H + g_browse_scroll;
    if (idx < 0 || idx >= g_browse_count)
        return 1;
    if (g_browse_is_dir[idx]) {
        browser_navigate(g_browse_names[idx]);
    } else {
        char   path[MAX_PATH_LEN];
        size_t plen = strlen(g_browse_path);
        size_t nlen = strlen(g_browse_names[idx]);
        if (plen + 1 + nlen < sizeof path) {
            memcpy(path, g_browse_path, plen);
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
    *sel_h = FOLDER_BTN_H;
    *sel_x = 32;
    *sel_y = content_bottom() - FOLDER_BTN_H - FOLDER_BTN_PAD;
}

/* Commit the browsed folder as the pending downloads-folder setting. */
static void
folder_commit(void)
{
    snprintf(g_settings_dl_dir, sizeof g_settings_dl_dir, "%s", g_browse_path);
    LOG("[bookshelf] folder: selected %s\n", g_browse_path);
    /* The picker opened on top of the settings page; closing it
     * restores settings as the top overlay. */
    g_state.overlay = OV_SETTINGS;
    draw_overlay_settings();
    flush_content();
}

void
folder_close(void)
{
    /* The picker opened on top of the settings page; closing it
     * restores settings as the top overlay. */
    g_state.overlay = OV_SETTINGS;
    draw_overlay_settings();
    flush_content();
}

/* Open the picker from the settings page.  Starts at the /mnt/ext1
 * root so the user can only choose on-device storage. */
void
folder_open(void)
{
    s_bmode = BR_MODE_PICKER;
    snprintf(g_browse_path, sizeof g_browse_path, "/mnt/ext1");
    g_browse_scroll = 0;
    g_browser_drag = 0;
    g_browser_moved = 0;
    browser_load();
    /* The picker opens ON TOP of the settings page (settings stays
     * conceptually underneath and is restored on close). */
    g_state.overlay = OV_FOLDER;
    draw_overlay_folder();
    flush_content();
}

void
draw_overlay_folder(void)
{
    int w = ScreenWidth();
    int h = content_bottom();
    int rows = browser_rows_visible();
    int up_row = browser_can_go_up() ? 1 : 0;
    int max_scroll = browser_clamp_scroll();
    FillArea(0, 0, w, h, WHITE);

    /* Header: title + current path. */
    ifont *tf = OpenFont(DEFAULTFONTB, 32, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(32, 24, i18n("settings.dl_dir"));
        CloseFont(tf);
    }
    ifont *pf = OpenFont(DEFAULTFONT, 22, 0);
    if (pf != NULL) {
        SetFont(pf, DGRAY);
        char trunc[256];
        /* Show the path relative to /mnt/ext1 — the mount point is
         * hidden from the user. */
        user_path_display(g_browse_path, trunc, sizeof trunc);
        utf8_fit_width(trunc, w - 64, 4);
        DrawString(32, 76, trunc);
        CloseFont(pf);
    }
    DrawLine(0, FOLDER_LIST_TOP - 12, w, FOLDER_LIST_TOP - 12, BLACK);

    /* Directory rows; a ".." row leads up whenever we are below the
     * /mnt/ext1 root. */
    int shown = 0;
    for (int i = 0; i < rows; i++) {
        int row_y = FOLDER_LIST_TOP + i * FOLDER_ROW_H;
        const char *name = NULL;
        if (up_row && i == 0) {
            name = "..";
        } else {
            int idx = g_browse_scroll + i - up_row;
            if (idx >= 0 && idx < g_browse_count)
                name = g_browse_names[idx];
        }
        browser_draw_row(row_y, name, 1); /* picker rows are all dirs */
        if (name != NULL)
            shown++;
    }
    if (shown == 0) {
        ifont *f = OpenFont(DEFAULTFONT, 26, 0);
        if (f != NULL) {
            SetFont(f, DGRAY);
            DrawString(32, FOLDER_LIST_TOP + 24, i18n("folder.empty"));
            CloseFont(f);
        }
    }

    /* Select / Back. */
    int sx, sy, sw, sh;
    folder_buttons(&sx, &sy, &sw, &sh);
    FillArea(sx, sy, sw, sh - 12, BLACK);
    DrawRect(sx, sy, sw, sh - 12, BLACK);
    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, WHITE);
        int tw = StringWidth(i18n("folder.select"));
        DrawString(sx + (sw - tw) / 2, sy + (sh - 12 - 32) / 2, i18n("folder.select"));
        CloseFont(f);
    }
    int bx = sx + sw + 16;
    FillArea(bx, sy, sw, sh - 12, WHITE);
    DrawRect(bx, sy, sw, sh - 12, BLACK);
    f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        int tw = StringWidth(i18n("settings.back"));
        DrawString(bx + (sw - tw) / 2, sy + (sh - 12 - 32) / 2, i18n("settings.back"));
        CloseFont(f);
    }

    /* Corner scroll buttons, raised above the Select/Back band. */
    int sy0 = content_bottom() - FOLDER_BTN_H - FOLDER_BTN_PAD - SCROLL_BTN_H;
    draw_scroll_buttons_at(g_browse_scroll > 0, g_browse_scroll < max_scroll, sy0);
}

/* Handle a tap on the picker overlay.  Returns 1 when the tap was
 * consumed (it always is — the picker owns the screen while open). */
int
on_tap_folder(int x, int y)
{
    /* Corner scroll buttons (raised above the Select/Back band). */
    int sy0 = content_bottom() - FOLDER_BTN_H - FOLDER_BTN_PAD - SCROLL_BTN_H;
    int dir = hit_scroll_button_at(x, y, sy0);
    if (dir != 0) {
        int rows = browser_rows_visible();
        g_browse_scroll += dir * rows;
        if (g_browse_scroll < 0)
            g_browse_scroll = 0;
        draw_overlay_folder();
        flush_content();
        return 1;
    }
    int sx, sy, sw, sh;
    folder_buttons(&sx, &sy, &sw, &sh);
    if (y >= sy && y < sy + sh - 12) {
        if (x >= sx && x < sx + sw) {
            folder_commit();
        } else {
            folder_close();
        }
        return 1;
    }
    if (y < FOLDER_LIST_TOP)
        return 1;

    int rows = browser_rows_visible();
    int up_row = browser_can_go_up() ? 1 : 0;
    int idx = (y - FOLDER_LIST_TOP) / FOLDER_ROW_H;
    if (idx < 0 || idx >= rows)
        return 1;
    if (up_row && idx == 0) {
        browser_navigate("..");
        return 1;
    }
    int dir_idx = g_browse_scroll + idx - up_row;
    if (dir_idx >= 0 && dir_idx < g_browse_count)
        browser_navigate(g_browse_names[dir_idx]);
    return 1;
}
