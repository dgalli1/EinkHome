#ifndef BOOKSHELF_H
#define BOOKSHELF_H

/*
 * bookshelf.h — shared header for the split bookshelf app.
 *
 * Source files:
 *   bs_i18n.c
 *   bs_config.c
 *   bs_model.c
 *   bs_net.c
 *   bs_ui.c
 *   bs_input.c
 *   bs_launcher.c
 *   bs_downloads.c
 *   bs_main.c
 */


#include <inkview.h>

#include <ctype.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <strings.h>
#include <time.h>
#include <unistd.h>

/* libinkview exports iv_update_panel() but the public SDK header omits it.
 * It renders the system status strip (clock / battery / wifi) into the
 * panel region of the framebuffer.  The C++ PBAppFrame framework calls it
 * from the app's CustomDrawPanel() override; a plain-C app must call it
 * itself after DrawPanel() has populated the panel content, otherwise the
 * strip stays blank.  The argument is the reading-mode-enable flag passed
 * through to the panel draw callback (0 for the normal collapsed bar). */
extern void iv_update_panel(int readingModeEnable);

/* ── configuration ───────────────────────────────────────────────────── */

#ifdef PBEMU_API_HOST
#define API_BASE_DEFAULT PBEMU_API_HOST
#else
#define API_BASE_DEFAULT "http://169.254.1.2:8765"
#endif

#define TOKEN_DEFAULT   "pbemu-dev-token"
#define LOCAL_DOWNLOADS "/mnt/ext1/system/bin"
/* Guest-writable fallback for downloads.  The emulator's non-root qemu-arm
 * guest cannot write LOCAL_DOWNLOADS (/mnt/ext1/system/bin), so downloads
 * fall back to /tmp there (see resolve_downloads_dir).  On a real device
 * LOCAL_DOWNLOADS is writable and used directly. */
#define LOCAL_DOWNLOADS_FALLBACK "/tmp"
#define CONFIG_FILENAME          "bookshelf.cfg"
/* Guest-writable fallback config path (used when the app's own directory
 * is not writable, e.g. the emulator's non-root qemu-arm guest). */
#define CONFIG_TMP_PATH "/tmp/" CONFIG_FILENAME
/* Reader apps the settings page can detect and offer.  The standard
 * PocketBook reader lives in the firmware image; KOReader is a common
 * third-party install dropped into /mnt/ext1/applications.  Detection is
 * a plain access(X_OK) probe so the list adapts to whatever is actually
 * installed on the device. */
#define READER_STD_PATH "/ebrmain/bin/eink-reader.app"
#define READER_KO_PATH  "/mnt/ext1/applications/koreader.app"
#define MAX_READERS     4

#define HTTP_TIMEOUT  8
#define MAX_BOOKS     200
#define MAX_TITLE_LEN 96
#define MAX_ID_LEN    48
#define MAX_PATH_LEN  220
#define MAX_URL_LEN   480
#define MAX_TOKEN_LEN 96
#define MAX_QUERY_LEN 80

/* Layout constants — tuned for the 1072x1448 633 Era panel (300 DPI).
 * All sizes are generous for comfortable e-ink touch targets. */
#define TOP_BAR_H    128
#define SEARCH_ROW_H 88
#define TAB_ROW_H    80
#define PAGER_H      96
#define THUMB_BORDER 4
#define COLS         3
#define ROWS         2
#define PAGESIZE     (COLS * ROWS)
#define CELL_MAX_H   600
#define CELL_MAX_W   420
#define CELL_MIN_H   280
#define CELL_MIN_W   280

/* List-view row height.  A list row is a single full-width band holding a
 * small cover + title + author, so it is much shorter than a grid cell and
 * many more fit per page.  150 px keeps the touch target generous on the
 * 300 dpi panel. */
#define LIST_ROW_H 150

/* More-overlay (right drawer) geometry, shared by the draw and tap paths.
 * Items: Sync, 5 sorts, Grid, List, Download all, Settings, System menu. */
#define MORE_Y0           96
#define MORE_ITEM_H       88
#define MORE_N_ITEMS      12
#define MORE_GRID_IDX     6
#define MORE_LIST_IDX     7
#define MORE_DLALL_IDX    8
#define MORE_SETTINGS_IDX 9
#define MORE_SYSTEM_IDX   10
#define MORE_APPS_IDX     11

/* Cover / blurhash rendering.  On a greyscale framebuffer a blurhash
 * decoded to luminance blits as a soft grey placeholder; on a colour
 * framebuffer the same placeholder reads as a neutral grey while real
 * covers (loaded via LoadPNGStretch) render in full colour.  Covers are
 * fetched one per weak-timer tick so the event loop never blocks. */
#define MAX_BLURHASH_LEN 48
#define COVER_TMP        "/tmp/.bcov.png"
#define COVER_FETCH_MS   60
#define BH_W             24
#define BH_H             36
#define TEXT_AREA        52 /* vertical room below the cover for title+author */
#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

/* Long-press detection.  The emulator only injects POINTERDOWN/UP/MOVE
 * (the firmware-synthesised EVT_POINTERLONG never fires under qemu), so
 * a long-press is detected app-side: POINTERDOWN arms a one-shot timer;
 * if it elapses before the finger lifts or moves away, the context menu
 * opens.  A POINTERUP that arrives while the timer is still pending is a
 * normal tap. */
#define LONGPRESS_MS 550
/* Finger travel (px) that cancels a pending long-press (a drag, not a
 * hold). */
#define LONGPRESS_SLOP 24

/* Context (long-press) menu — a centred modal sheet.  A book offers
 * Download + Delete; a series card offers Download all + Delete series. */
#define CTX_ITEM_H    96
#define CTX_TITLE_H   72
#define CTX_PAD       24
#define CTX_MAX_ITEMS 4

/* Active downloads.  Downloads run synchronously on the event loop
 * (QuickDownload blocks), so at most one is ever in flight; the list
 * still models a queue so a multi-book "Download all" can show every
 * pending item and tick them off one per timer tick. */
#define MAX_DOWNLOADS 64
/* Height reserved at the top of the Downloads tab body for the single
 * batch progress bar (one bar covering every open download). */
#define DL_BAR_H 56

typedef struct {
    const char *key;
    const char *en;
    const char *de;
    const char *fr;
    const char *it;
} I18n;
typedef void (*cfg_kv_cb)(const char *key, const char *value, void *user);
struct cfg_out {
    char  *api_url;
    char  *api_token;
    size_t cap;
};
typedef enum {
    FILTER_ALL,
    FILTER_DOWNLOADED,
    FILTER_REMOTE,
} Filter;
typedef enum {
    SORT_TITLE_ASC,
    SORT_TITLE_DESC,
    SORT_AUTHOR,
    SORT_SERIES,
    SORT_RECENT,
} SortMode;
typedef enum {
    GROUP_ALL,
    GROUP_BY_AUTHOR,
    GROUP_BY_SERIES,
    GROUP_BY_RECENT,
} GroupMode;
typedef enum {
    VIEW_GRID,
    VIEW_LIST,
} ViewMode;
typedef enum {
    TAB_LIBRARY,   /* the cover grid / list */
    TAB_DOWNLOADS, /* active + finished downloads */
} MainTab;
typedef struct {
    char  id[MAX_ID_LEN];
    char  title[MAX_TITLE_LEN];
    char  author[80];
    char  series[48];
    char  series_id[MAX_ID_LEN];
    float series_idx; /* volume/chapter number inside series; 0 if N/A */
    char  ext[8];
    int   size;
    int   downloaded;
    int   selected; /* set if this is the currently-highlighted tile */
    char  local_path[MAX_PATH_LEN];
    char  blurhash[MAX_BLURHASH_LEN];
    long  added_at; /* unix epoch from server "addedAt"; 0 if absent */
} Book;
typedef struct {
    int  is_series;
    int  book_idx; /* index into g_state.books[] */
    char series_id[MAX_ID_LEN];
    char series_name[48];
    int  series_count; /* books in the series (badge) */
} ViewTile;
typedef struct {
    Book books[MAX_BOOKS];
    int  total;      /* total available (post-filter, pre-paging) */
    int  count;      /* currently visible (= total after filter) */
    int  selected;   /* index within `books` for keyboard nav */
    int  sync_state; /* 0 idle, 1 syncing, 2 error */
    char status[160];

    int panel_h; /* height of the system status panel; 0 if hidden */

    char query[MAX_QUERY_LEN];

    char api_base[260];
    char api_token[MAX_TOKEN_LEN];
    char url_books[MAX_URL_LEN];
    char url_delta[MAX_URL_LEN];
    char url_state[MAX_URL_LEN];
    char url_libs[MAX_URL_LEN];
    char url_openwith[MAX_URL_LEN];

    SortMode  sort;
    GroupMode group;
    Filter    filter;
    ViewMode  view_mode; /* GRID = cover grid, LIST = one row per book */

    int     menu_open;     /* hamburger overlay */
    int     more_open;     /* right "..." overlay */
    int     search_open;   /* search input is focused */
    int     settings_open; /* full-screen settings overlay */
    MainTab tab;           /* TAB_LIBRARY (grid) or TAB_DOWNLOADS */
    int     launcher_open;
    int     launcher_page;

    /* Context (long-press) menu.  ctx_open shows a centred modal sheet
     * over the tile named by ctx_book_idx (a book) or ctx_series_id (a
     * series card). */
    int  ctx_open;
    int  ctx_is_series;
    int  ctx_book_idx; /* index into g_state.books[] */
    char ctx_series_id[MAX_ID_LEN];

    /* Reader selection.  reader_pref == 0 means "Auto" (honour the
     * server's open-with resolution); otherwise it is a 1-based index
     * into g_readers[] naming the app to launch directly. */
    int reader_pref;

    int page; /* current page (0-based) */

    int libs_count;
    int cur_lib; /* -1 = all */
} State;
typedef struct {
    char     id[MAX_ID_LEN];
    ibitmap *cover_bmp;
    ibitmap *bh_bmp;
    int      state;
} CoverSlot;
typedef struct {
    char id[MAX_ID_LEN];
    char title[MAX_TITLE_LEN];
    int  state;
} DownloadItem;
typedef struct {
    const char *path;
    const char *label;
} ReaderCandidate;
#define SETTINGS_ROW_H 120
#define SETTINGS_BTN_H 96
typedef struct {
    const char *device;
    const char *partner;
    const char *has_audio;
    const char *has_cloud;
    const char *language;
    const char *localization;
} LcProfile;
#define LC_NDIMS ((int)(sizeof lc_dims / sizeof lc_dims[0]))
#define LAUNCHER_MAX_ITEMS  64
#define LAUNCHER_MAX_PARAMS 4
#define LAUNCHER_PARAM_LEN  64
typedef struct {
    int  kind; /* 0 = header, 1 = app */
    char text[48];
    char path[160];
    char icon[64];
    char params[LAUNCHER_MAX_PARAMS][LAUNCHER_PARAM_LEN];
    int  nparams;
    int  page;
    int  x, y, w, h;
} LauncherItem;
#define LAUNCHER_HEADER_H 104
#define LAUNCHER_PAGER_H  96
#define LAUNCHER_COLS     3
#define LAUNCHER_GROUP_H  64
#define LAUNCHER_CELL_H   232
#define LAUNCHER_ICON_SZ  120
#define LAUNCHER_MARGIN   16

/* ── global variables ── */

extern char g_lang[8];
extern const I18n g_i18n[];
extern FILE *g_log;
extern char g_cfg_reader[220];
extern char g_config_path[600];
extern ViewTile g_view[MAX_BOOKS];
extern int      g_view_count;
extern char     g_drilled_series[MAX_ID_LEN];
extern State g_state;
extern Book g_lib[MAX_BOOKS];
extern int  g_lib_count;
extern char g_search_kb_buf[MAX_QUERY_LEN];
extern CoverSlot g_covers[MAX_BOOKS];
extern int       g_cover_armed;
extern DownloadItem g_downloads[MAX_DOWNLOADS];
extern int          g_download_count;
extern int          g_download_armed;
extern char g_downloads_dir[MAX_PATH_LEN];
extern int g_lp_armed;
extern int g_lp_vi;
extern int g_lp_x;
extern int g_lp_y;
extern int g_ctx_suppress_up;
extern char g_argv0[256];
extern ReaderCandidate g_readers[MAX_READERS];
extern int             g_reader_count;
extern const char bh_base83[84];
extern int g_settings_edit;
extern char g_settings_kb_buf[260];
extern const LcProfile g_lcprof;
extern const char *const lc_dims[];
extern LauncherItem g_launcher_items[LAUNCHER_MAX_ITEMS];
extern int          g_launcher_count;
extern int          g_launcher_pages;
extern int          g_launcher_built;

/* ── function prototypes ── */

const char * i18n(const char *key);
void log_open(const char *argv0);
void log_close(void);
void LOG(const char *fmt, ...);
char * trim_ws(char *s);
int read_kv_file(const char *path, cfg_kv_cb cb, void *user);
void cfg_set_kv(const char *key, const char *value, void *user);
void dirname_of(const char *path, char *out, size_t out_cap);
void load_config_file(const char *argv0, struct cfg_out *out);
void resolve_config_path(const char *argv0);
void resolve_downloads_dir(void);
void detect_readers(void);
int reader_pref_from_path(const char *value);
int save_config_file(void);
int http_get(const char *url, int *status_out, char **body_out, int *len_out);
int http_post(const char *url, const char *body, char **resp_out, int *resp_len);
void build_endpoint_urls(void);
char * json_find_key(const char *obj, const char *key, char *out, size_t cap);
int json_find_int(const char *obj, const char *key, int default_val);
float json_find_float(const char *obj, const char *key, float default_val);
const char * json_next_string(const char *arr, char *out, size_t cap);
int cmp_series_index_hint(const Book *b);
char * json_collect_id_list(const char *json, const char *arr_key, size_t *out_len);
int id_in_list(const char *id, const char *list);
int cmp_title_asc(const void *a, const void *b);
int cmp_title_desc(const void *a, const void *b);
int cmp_author(const void *a, const void *b);
int cmp_series(const void *a, const void *b);
int cmp_recent(const void *a, const void *b);
void build_view(void);
void apply_filter_and_sort(void);
int parse_book_obj(const char *obj, Book *b);
int parse_books_array(const char *arr_start);
void apply_books_from_added(const char *json, int known_count, const char known_ids[][MAX_ID_LEN]);
void do_sync(void);
void do_fetch_all(void);
void draw_text_centered(ifont *f, int cx, int cy, const char *text, int color);
void draw_button( int x, int y, int w, int h, int selected, const char *label, int label_size, int label_color);
void draw_top_bar(void);
void draw_search_row(void);
int downloads_pending(void);
void draw_tab_row(void);
int bh_value(char c);
int bh_decode83(const char *s, int n);
float bh_s2l(int v);
int bh_l2s(float v);
float bh_sign_pow(float v, float e);
CoverSlot * cover_slot(const char *id, int create);
int view_cols(void);
int view_rows(void);
int view_pagesize(void);
void grid_geom(int *top, int *bot, int *cell_w, int *cell_h);
int tile_rect_for_index(int idx, int *x, int *y, int *w, int *h);
void cover_rect(int tx, int ty, int tw, int th, int *cx, int *cy, int *cw, int *ch);
void bh_ensure(CoverSlot *s, const Book *b);
void cover_schedule_next(void);
void blit_cover(int cx, int cy, int cw, int ch, const Book *b);
void draw_series_stack(int cx, int cy, int cw, int ch, int count);
void draw_thumbnail(int x, int y, int w, int h, const ViewTile *vt, int vi);
int downloads_rows(void);
int downloads_pagesize(void);
int current_pages(void);
void draw_dl_progress(int x, int y, int w);
void draw_downloads_tab(void);
void redraw_shelf(void);
void draw_grid(void);
void cover_tick(void *ctx);
void draw_pager(void);
void draw_overlay_menu(void);
void draw_overlay_more(void);
void draw_status_line(void);
void settings_keyboard_handler(char *buffer);
const char * settings_reader_label(void);
void settings_draw_row(int y, const char *label, const char *value, int editing);
void settings_draw_button(int y, const char *label, int filled);
void draw_overlay_settings(void);
int hit_top_bar(int x, int y);
int hit_search(int x, int y);
int hit_tab_row(int x, int y);
int hit_thumbnail(int x, int y);
int hit_pager(int x, int y);
void on_tap_overlay_menu(int x, int y);
void on_tap_overlay_more(int x, int y);
void settings_close(void);
void settings_apply(void);
void on_tap_overlay_settings(int x, int y);
const char * js_skip_ws(const char *p);
const char * js_skip_value(const char *p);
void js_copy_string(const char *p, char *out, size_t cap);
const char * js_object_body(const char *p);
const char * js_find_member(const char *p, const char *key);
const char * lc_prof_val(const char *dim);
const char * lc_pick_key(const char *obj_body, const char *want);
void lc_resolve(const char *p, const char *cur_dim, char *out, size_t cap);
int lc_resolve_bool(const char *p);
char * read_text_file(const char *path);
const char * lc_token_en(const char *tok);
void lc_translate(const char *raw, char *out, size_t cap);
void launcher_layout(void);
void launcher_add_app(const char *apps_body, const char *id);
void launcher_build(void);
void draw_launcher_icon(int cx, int cy, const char *icon_name, const char *title);
void draw_overlay_launcher(void);
void launch_app(const LauncherItem *it);
void on_tap_overlay_launcher(int x, int y);
void launcher_open_set(void);
void launcher_close(void);
void drill_back(void);
void on_tap_thumbnail(int vi);
void book_local_path(const Book *b, char *out, size_t cap);
Book * find_lib_book(const char *id);
void refresh_downloaded(Book *b);
DownloadItem * find_download(const char *id);
void enqueue_download(const Book *b);
int download_book_file(Book *b);
void launch_reader(Book *b);
void book_press_action(Book *b);
void delete_book_file(Book *b);
void download_tick(void *ctx);
void download_series(const char *series_id);
void delete_series(const char *series_id);
void context_geom(int *px, int *py, int *pw, int *ph, int n_items);
int context_item_count(void);
void draw_context_menu(void);
void close_context(void);
void open_context_for_tile(int vi);
void longpress_tick(void *ctx);
void on_tap_context(int x, int y);
int on_event(int type, int par1, int par2);
void keyboard_handler(char *buffer);

#endif /* BOOKSHELF_H */
