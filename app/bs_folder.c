/* bs_folder.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

#include <dirent.h>

/* ── download-folder picker ────────────────────────────────────────────
 * Settings → Download folder.  A full-screen overlay that browses the
 * directory tree, confined to /mnt/ext1: the browser starts there and
 * the ".." row disappears at the root, so on-device storage is the
 * only thing choosable.  Selecting a folder stores it in
 * g_settings_dl_dir (pending); the settings Save button persists it to
 * the config file and resolve_downloads_dir() re-applies it. */

int  g_folder_open = 0;
char g_folder_path[256];
int  g_folder_scroll = 0;
int  g_folder_drag = 0;
int  g_folder_drag_y = 0;
int  g_folder_moved = 0;

/* Subdirectories of g_folder_path, sorted; filled by folder_load_list(). */
static char g_folder_dirs[FOLDER_MAX_DIRS][MAX_PATH_LEN];
static int  g_folder_count = 0;

static int
folder_can_go_up(void)
{
    /* No ascent above the /mnt/ext1 root. */
    return strcmp(g_folder_path, "/mnt/ext1") != 0;
}

/* How many list rows the overlay shows (header + bottom buttons take
 * the rest of the content area). */
static int
folder_rows_visible(void)
{
    int avail = content_bottom() - FOLDER_LIST_TOP - FOLDER_BTN_H - FOLDER_BTN_PAD;
    int rows = avail / FOLDER_ROW_H;
    return rows < 1 ? 1 : rows;
}

/* Fill g_folder_dirs[] with the sorted subdirectory names of
 * g_folder_path.  Non-directories and dot-names are skipped. */
static void
folder_load_list(void)
{
    g_folder_count = 0;
    DIR *d = opendir(g_folder_path);
    if (d == NULL) {
        LOG("[bookshelf] folder: opendir %s failed errno=%d\n", g_folder_path, errno);
        return;
    }
    struct dirent *e;
    while ((e = readdir(d)) != NULL && g_folder_count < FOLDER_MAX_DIRS) {
        if (e->d_name[0] == '.')
            continue;
        if (e->d_type != DT_DIR)
            continue;
        /* Bounded copy: d_name can be NAME_MAX (255) while the row
         * buffer is MAX_PATH_LEN; overlong names are truncated. */
        size_t nlen = strlen(e->d_name);
        if (nlen > MAX_PATH_LEN - 1)
            nlen = MAX_PATH_LEN - 1;
        memcpy(g_folder_dirs[g_folder_count], e->d_name, nlen);
        g_folder_dirs[g_folder_count][nlen] = '\0';
        g_folder_count++;
    }
    closedir(d);
    /* Stable alphabetical order (the firmware tree is mostly sorted
     * already; a plain bubble sort keeps this dependency-free). */
    for (int i = 0; i < g_folder_count; i++) {
        for (int j = i + 1; j < g_folder_count; j++) {
            if (strcmp(g_folder_dirs[j], g_folder_dirs[i]) < 0) {
                char tmp[MAX_PATH_LEN];
                memcpy(tmp, g_folder_dirs[i], MAX_PATH_LEN);
                memcpy(g_folder_dirs[i], g_folder_dirs[j], MAX_PATH_LEN);
                memcpy(g_folder_dirs[j], tmp, MAX_PATH_LEN);
            }
        }
    }
    LOG("[bookshelf] folder: %s -> %d subdirs\n", g_folder_path, g_folder_count);
}

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

void
draw_overlay_folder(void)
{
    int w = ScreenWidth();
    int h = content_bottom();
    int rows = folder_rows_visible();
    int up_row = folder_can_go_up() ? 1 : 0;
    /* Clamp the scroll to the laid-out list (".." steals one row when
     * present). */
    int max_scroll = g_folder_count - (rows - up_row);
    if (max_scroll < 0)
        max_scroll = 0;
    if (g_folder_scroll < 0)
        g_folder_scroll = 0;
    if (g_folder_scroll > max_scroll)
        g_folder_scroll = max_scroll;
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
        user_path_display(g_folder_path, trunc, sizeof trunc);
        while (StringWidth(trunc) > w - 64 && strlen(trunc) > 4)
            trunc[strlen(trunc) - 1] = '\0';
        DrawString(32, 76, trunc);
        CloseFont(pf);
    }
    DrawLine(0, FOLDER_LIST_TOP - 12, w, FOLDER_LIST_TOP - 12, BLACK);

    /* Directory rows; a ".." row leads up whenever we are below the
     * /mnt/ext1 root. */
    int shown = 0;
    for (int i = 0; i < rows; i++) {
        int row_y = FOLDER_LIST_TOP + i * FOLDER_ROW_H;
        FillArea(0, row_y, w, FOLDER_ROW_H, WHITE);
        DrawLine(0, row_y + FOLDER_ROW_H, w, row_y + FOLDER_ROW_H, LGRAY);
        const char *name = NULL;
        if (up_row && i == 0) {
            name = "..";
        } else {
            int idx = g_folder_scroll + i - up_row;
            if (idx >= 0 && idx < g_folder_count)
                name = g_folder_dirs[idx];
        }
        if (name != NULL) {
            ifont *f = OpenFont(DEFAULTFONTB, 28, 0);
            if (f != NULL) {
                SetFont(f, BLACK);
                char trunc[MAX_PATH_LEN + 4];
                snprintf(trunc, sizeof trunc, "%s/", name);
                while (StringWidth(trunc) > w - 64 && strlen(trunc) > 4)
                    trunc[strlen(trunc) - 1] = '\0';
                DrawString(32, row_y + (FOLDER_ROW_H - 28) / 2 - 2, trunc);
                CloseFont(f);
            }
            shown++;
        }
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
}

/* Commit the browsed folder as the pending downloads-folder setting. */
static void
folder_commit(void)
{
    snprintf(g_settings_dl_dir, sizeof g_settings_dl_dir, "%s", g_folder_path);
    LOG("[bookshelf] folder: selected %s\n", g_folder_path);
    g_folder_open = 0;
    draw_overlay_settings();
    flush_content();
}

void
folder_close(void)
{
    g_folder_open = 0;
    draw_overlay_settings();
    flush_content();
}

/* Open the picker from the settings page.  Starts at the /mnt/ext1
 * root so the user can only choose on-device storage. */
void
folder_open(void)
{
    snprintf(g_folder_path, sizeof g_folder_path, "/mnt/ext1");
    g_folder_scroll = 0;
    g_folder_drag = 0;
    g_folder_moved = 0;
    folder_load_list();
    g_folder_open = 1;
    draw_overlay_folder();
    flush_content();
}

/* Descend into a listed subdirectory (or ascend via ".."). */
static void
folder_navigate(const char *name)
{
    if (strcmp(name, "..") == 0) {
        char *slash = strrchr(g_folder_path, '/');
        if (slash != NULL && slash != g_folder_path)
            *slash = '\0';
    } else {
        /* Build <path>/<name> with explicit lengths: snprintf of two
         * MAX_PATH_LEN-sized strings into one buffer trips the
         * truncation analysis even though the cap is enforced. */
        char   next[sizeof g_folder_path];
        size_t plen = strlen(g_folder_path);
        size_t nlen = strlen(name);
        if (plen + 1 + nlen >= sizeof next)
            return; /* path already at the cap; cannot descend */
        memcpy(next, g_folder_path, plen);
        next[plen] = '/';
        memcpy(next + plen + 1, name, nlen);
        next[plen + 1 + nlen] = '\0';
        memcpy(g_folder_path, next, sizeof g_folder_path);
    }
    g_folder_scroll = 0;
    folder_load_list();
    draw_overlay_folder();
    flush_content();
}

/* Handle a tap on the picker overlay.  Returns 1 when the tap was
 * consumed (it always is — the picker owns the screen while open). */
int
on_tap_folder(int x, int y)
{
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

    int rows = folder_rows_visible();
    int up_row = folder_can_go_up() ? 1 : 0;
    int idx = (y - FOLDER_LIST_TOP) / FOLDER_ROW_H;
    if (idx < 0 || idx >= rows)
        return 1;
    if (up_row && idx == 0) {
        folder_navigate("..");
        return 1;
    }
    int dir_idx = g_folder_scroll + idx - up_row;
    if (dir_idx >= 0 && dir_idx < g_folder_count)
        folder_navigate(g_folder_dirs[dir_idx]);
    return 1;
}
