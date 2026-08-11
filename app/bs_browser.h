#ifndef BS_BROWSER_H
#define BS_BROWSER_H

/* bs_browser.h — Directory browsers (bs_browser.c): the Settings download-folder picker
 * overlay and the Folder-source body browser. */

#include "bookshelf.h"

void browse_start(const char *dir);

void draw_browse(void);

int on_tap_browse(int x, int y);

int browse_up(void);

void browse_page(int dir);

const char *user_path_display(const char *path, char *out, size_t cap);

extern int g_browse_open;

extern char g_browse_path[256];

extern int g_browse_scroll;

/* Drag state shared by the folder-source browser (body mode) and the
 * download-folder picker overlay — see bs_browser.c. */
extern int g_browser_drag;

extern int g_browser_drag_y;

extern int g_browser_moved;

/* Book-file extension test and the djb2 "fld_" id hash, shared with
 * the Local source import (bs_local.c). */
int is_book_ext(const char *ext);

void hash_hex(const char *s, char out[9]);

void folder_open(void);

void folder_close(void);

void draw_overlay_folder(void);

int on_tap_folder(int x, int y);

#endif /* BS_BROWSER_H */
