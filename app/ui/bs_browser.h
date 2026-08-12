#ifndef BS_BROWSER_H
#define BS_BROWSER_H

/* bs_browser.h — Directory browsers (bs_browser.c): the Settings download-folder picker
 * overlay and the Folder-source body browser. */

#include "bs_core.h"

void bs_browse_start(const char *dir);

void bs_draw_browse(void);

int bs_on_tap_browse(int x, int y);

int bs_browse_up(void);

void bs_browse_page(int dir);

const char *bs_user_path_display(const char *path, char *out, size_t cap);

extern int bs_g_browse_open;

extern char bs_g_browse_path[256];

extern int bs_g_browse_scroll;

/* Drag state shared by the folder-source browser (body mode) and the
 * download-folder picker overlay — see bs_browser.c. */
extern int bs_g_browser_drag;

extern int bs_g_browser_drag_y;

extern int bs_g_browser_moved;

/* Book-file extension test and the djb2 "fld_" id hash, shared with
 * the Local source import (bs_local.c). */
int bs_is_book_ext(const char *ext);

void bs_hash_hex(const char *s, char out[9]);

void bs_folder_open(void);

void bs_folder_close(void);

void bs_draw_overlay_folder(void);

int bs_on_tap_folder(int x, int y);

#endif /* BS_BROWSER_H */
