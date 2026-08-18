#ifndef EH_BROWSER_H
#define EH_BROWSER_H

/* eh_browser.h — Directory browsers (eh_browser.c): the Settings download-folder picker
 * overlay and the Folder-source body browser. */

#include "eh_core.h"

void eh_browse_start(const char *dir);

void eh_draw_browse(void);

int eh_on_tap_browse(int x, int y);

int eh_browse_up(void);

void eh_browse_page(int dir);

const char *eh_user_path_display(const char *path, char *out, size_t cap);

extern int eh_g_browse_open;

extern char eh_g_browse_path[256];

extern int eh_g_browse_scroll;

/* Drag state shared by the folder-source browser (body mode) and the
 * download-folder picker overlay — see eh_browser.c. */
extern int eh_g_browser_drag;

extern int eh_g_browser_drag_y;

extern int eh_g_browser_moved;

/* Book-file extension test and the djb2 "fld_" id hash, shared with
 * the Local source import (eh_local.c). */
int eh_is_book_ext(const char *ext);

void eh_hash_hex(const char *s, char out[9]);

void eh_folder_open(void);

void eh_folder_close(void);

void eh_draw_overlay_folder(void);

int eh_on_tap_folder(int x, int y);

#endif /* EH_BROWSER_H */
