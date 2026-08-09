/* bs_browse.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

#include <dirent.h>

/* ── folder-source file browser ────────────────────────────────────────
 * The Folder library source is a live file browser, not an imported
 * list: it shows the subdirectories and book files of the picked root
 * (default /mnt/ext1), descending on tap and opening a book file
 * through the same launch_reader()/OpenBook flow the Kavita library
 * uses.  Nothing is imported into the store — the shelf is replaced by
 * the browser while this source is active. */

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

#define BROWSE_MAX_ENTRIES 512

int  g_browse_open = 0;
char g_browse_path[256];
int  g_browse_scroll = 0;
int  g_browse_drag = 0;
int  g_browse_drag_y = 0;
int  g_browse_moved = 0;

static char g_browse_names[BROWSE_MAX_ENTRIES][MAX_PATH_LEN];
static char g_browse_is_dir[BROWSE_MAX_ENTRIES];
static int  g_browse_count = 0;

void draw_browse(void);

static int
browse_is_book_ext(const char *ext)
{
    static const char *const exts[] = {
        "epub", "pdf", "mobi", "azw", "azw3", "fb2", "djvu", "txt", "cbr", "cbz"};
    for (size_t i = 0; i < sizeof exts / sizeof exts[0]; i++)
        if (strcmp(ext, exts[i]) == 0)
            return 1;
    return 0;
}

/* Fill g_browse_* with the current directory's entries: ".." (when not
 * at the /mnt/ext1 root), then subdirectories, then book files — each
 * group sorted alphabetically. */
static void
browse_load(void)
{
    g_browse_count = 0;
    int can_up = strcmp(g_browse_path, "/mnt/ext1") != 0;
    if (can_up) {
        snprintf(g_browse_names[0], MAX_PATH_LEN, "..");
        g_browse_is_dir[0] = 1;
        g_browse_count = 1;
    }

    DIR *d = opendir(g_browse_path);
    if (d == NULL) {
        LOG("[bookshelf] browse: opendir %s failed errno=%d\n", g_browse_path, errno);
        return;
    }
    struct dirent *e;
    while ((e = readdir(d)) != NULL && g_browse_count < BROWSE_MAX_ENTRIES) {
        if (e->d_name[0] == '.')
            continue;
        int is_dir = e->d_type == DT_DIR;
        if (!is_dir && e->d_type != DT_REG)
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
            if (!browse_is_book_ext(ext))
                continue;
        }
        size_t nlen = strlen(e->d_name);
        if (nlen >= MAX_PATH_LEN)
            nlen = MAX_PATH_LEN - 1;
        memcpy(g_browse_names[g_browse_count], e->d_name, nlen);
        g_browse_names[g_browse_count][nlen] = '\0';
        g_browse_is_dir[g_browse_count] = (char)is_dir;
        g_browse_count++;
    }
    closedir(d);

    /* Insertion sort: dirs first, then books, alphabetical within. */
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
    LOG("[bookshelf] browse: %s -> %d entries\n", g_browse_path, g_browse_count);
}

/* Rows fit below the top bar, above the system panel. */
static int
browse_rows_visible(void)
{
    int avail = content_bottom() - (TOP_BAR_H + TOP_BAR_PAD) - 8;
    int rows = avail / FOLDER_ROW_H;
    return rows < 1 ? 1 : rows;
}

/* Open a book file with the same flow the Kavita library uses (reader
 * preference → OpenBook or direct launch). */
static void
browse_open_book(const char *path)
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
    /* Stable id so the reader book-open handshake round-trips. */
    unsigned long h = 5381;
    for (const unsigned char *p = (const unsigned char *)path; *p; p++)
        h = h * 33 + *p;
    snprintf(b.id, sizeof b.id, "fld_%08lx", h & 0xfffffffful);
    b.downloaded = 1;
    snprintf(b.local_path, sizeof b.local_path, "%s", path);
    snprintf(b.filename, sizeof b.filename, "%s", base);
    snprintf(b.source, sizeof b.source, "folder");
    LOG("[bookshelf] browse: opening %s\n", path);
    launch_reader(&b);
}

void
browse_start(const char *dir)
{
    snprintf(g_browse_path, sizeof g_browse_path, "%s", dir);
    g_browse_scroll = 0;
    g_browse_drag = 0;
    g_browse_moved = 0;
    browse_load();
    g_browse_open = 1;
    /* The browser is the shelf body; a full shelf redraw shows the top
     * bar (with the path as title) plus the list. */
    redraw_shelf();
}

static void
browse_navigate(const char *name)
{
    if (strcmp(name, "..") == 0) {
        browse_up();
        return;
    }
    size_t plen = strlen(g_browse_path);
    size_t nlen = strlen(name);
    if (plen + 1 + nlen >= sizeof g_browse_path)
        return;
    g_browse_path[plen] = '/';
    memcpy(g_browse_path + plen + 1, name, nlen);
    g_browse_path[plen + 1 + nlen] = '\0';
    g_browse_scroll = 0;
    browse_load();
    draw_browse();
    flush_content();
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
    browse_load();
    draw_browse();
    flush_content();
    return 1;
}

/* Page the list one screen; dir > 0 = forward.  draw_browse clamps. */
void
browse_page(int dir)
{
    g_browse_scroll += dir * browse_rows_visible();
    draw_browse();
    flush_content();
}

void
draw_browse(void)
{
    int w = ScreenWidth();
    int top = TOP_BAR_H + TOP_BAR_PAD;
    int bottom = content_bottom();
    int rows = browse_rows_visible();

    int max_scroll = g_browse_count - rows;
    if (max_scroll < 0)
        max_scroll = 0;
    if (g_browse_scroll < 0)
        g_browse_scroll = 0;
    if (g_browse_scroll > max_scroll)
        g_browse_scroll = max_scroll;

    /* Body only — the top bar (with the path as its title) is drawn by
     * the caller.  The fill starts below the top bar's bottom border
     * (TOP_BAR_PAD gap), so it leaves the border intact. */
    FillArea(0, top, w, bottom - top, WHITE);

    for (int i = 0; i < rows; i++) {
        int idx = g_browse_scroll + i;
        int row_y = top + 8 + i * FOLDER_ROW_H;
        FillArea(0, row_y, w, FOLDER_ROW_H, WHITE);
        DrawLine(0, row_y + FOLDER_ROW_H, w, row_y + FOLDER_ROW_H, LGRAY);
        if (idx >= g_browse_count)
            continue;
        const char *name = g_browse_names[idx];
        int         is_dir = g_browse_is_dir[idx];
        ifont      *f = OpenFont(DEFAULTFONTB, 28, 0);
        if (f != NULL) {
            SetFont(f, BLACK);
            char trunc[MAX_PATH_LEN + 4];
            snprintf(trunc, sizeof trunc, "%s%s", name, is_dir ? "/" : "");
            while (StringWidth(trunc) > w - 64 && strlen(trunc) > 4)
                trunc[strlen(trunc) - 1] = '\0';
            DrawString(32, row_y + (FOLDER_ROW_H - 28) / 2 - 2, trunc);
            CloseFont(f);
        }
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
    if (g_browse_is_dir[idx])
        browse_navigate(g_browse_names[idx]);
    else {
        char   path[MAX_PATH_LEN];
        size_t plen = strlen(g_browse_path);
        size_t nlen = strlen(g_browse_names[idx]);
        if (plen + 1 + nlen < sizeof path) {
            memcpy(path, g_browse_path, plen);
            path[plen] = '/';
            memcpy(path + plen + 1, g_browse_names[idx], nlen);
            path[plen + 1 + nlen] = '\0';
            browse_open_book(path);
        }
    }
    return 1;
}
