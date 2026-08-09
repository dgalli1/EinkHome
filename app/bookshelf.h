#ifndef BOOKSHELF_H
#define BOOKSHELF_H

/*
 * bookshelf.h — shared header for the split bookshelf app.
 *
 * Translation units: see the SOURCES list in bookshelf/Makefile.
 */

#include <inkview.h>
#include <hwconfig.h>

#include <ctype.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <strings.h>
#include <time.h>
#include <unistd.h>
#include <sys/stat.h>
#include <errno.h>

/* libinkview exports iv_update_panel() but the public SDK header omits it.
 * It renders the system status strip (clock / battery / wifi) into the
 * panel region of the framebuffer.  The C++ PBAppFrame framework calls it
 * from the app's CustomDrawPanel() override; a plain-C app must call it
 * itself after DrawPanel() has populated the panel content, otherwise the
 * strip stays blank.  The argument is the reading-mode-enable flag passed
 * through to the panel draw callback (0 for the normal collapsed bar). */
extern void iv_update_panel(int readingModeEnable);

/* libinkview also exports the canvas lock API but the public SDK header
 * omits it.  GetCanvas() (declared in inkview.h) returns the active draw
 * canvas; the QPA bridge (eink-reader) writes RGB24 pixels straight into
 * the canvas to bypass libinkview's 8-bit draw pipeline — the only way
 * an app gets colour on the Kaleido panel. */
extern void lockCanvasDrawing(void);
extern void unlockCanvasDrawing(void);

/* ── configuration ───────────────────────────────────────────────────── */

#ifdef PBEMU_API_HOST
#define API_BASE_DEFAULT PBEMU_API_HOST
#else
#define API_BASE_DEFAULT "http://169.254.1.2:8765"
#endif

#define TOKEN_DEFAULT "pbemu-dev-token"
/* Download folder for books, chosen in Settings → Download folder.
 * The picker only browses inside /mnt/ext1 (on-device storage); the
 * default is the stock PocketBook downloads folder.  In the emulator
 * the non-root qemu-arm guest cannot write /mnt/ext1 at all, so
 * downloads fall back to /tmp there (see resolve_downloads_dir). */
#define DEFAULT_DOWNLOADS_DIR    "/mnt/ext1/Downloads"
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

#define HTTP_TIMEOUT 8
/* Books per /sync/delta round-trip.  The library itself is unbounded
 * (SQLite-backed); only one batch of parsed books lives in RAM at a
 * time, so 100k books never materialise as one array. */
#define SYNC_BATCH 500
/* Upper bound on grid/list rows per page.  Grid pages hold ROWS rows;
 * list mode computes its row count from the screen geometry and stays
 * below this. */
#define MAX_ROWS 24
/* LRU cover bitmap slots.  Decoded covers are the only per-tile RAM
 * cost; a handful of slots bounds it regardless of library size. */
#define NCOVER_SLOTS  8
#define MAX_TITLE_LEN 96
#define MAX_ID_LEN    48
#define MAX_PATH_LEN  220
#define MAX_URL_LEN   480
#define MAX_TOKEN_LEN 96
#define MAX_QUERY_LEN 80

/* Layout constants — tuned for the 1072x1448 633 Era panel (300 DPI).
 * All sizes are generous for comfortable e-ink touch targets. */
#define TOP_BAR_H 96
/* White gap between the top bar's bottom border and the shelf body. */
#define TOP_BAR_PAD 12
/* Search input row height — used only on the Search sub-page; the main
 * shelf no longer carries a search row (the magnifier icon lives in the
 * top bar). */
#define SEARCH_ROW_H 88
#define TAB_ROW_H    0 /* tab row removed; downloads via top-bar icon */
#define PAGER_H      96
/* History-term rows on the Search sub-page.  SEARCH_HISTORY_MAX caps
 * how many previously committed queries are persisted (and shown). */
#define SEARCH_HISTORY_ROW_H 96
#define SEARCH_HISTORY_MAX   20
#define THUMB_BORDER         4
#define COLS                 3
#define ROWS                 2
#define PAGESIZE             (COLS * ROWS)
#define CELL_MAX_H           600
#define CELL_MAX_W           420
#define CELL_MIN_H           280
#define CELL_MIN_W           280

/* List-view row height.  A list row is a single full-width band holding a
 * small cover + title + author, so it is much shorter than a grid cell and
 * many more fit per page.  150 px keeps the touch target generous on the
 * 300 dpi panel. */
#define LIST_ROW_H 150

/* More-overlay (right drawer) geometry, shared by the draw and tap paths.
 * Items: Sync, 4 sorts, Grid, List, Download all, Settings, System menu,
 * Applications.  (Title Z-A was removed per user request.) */
#define MORE_Y0           96
#define MORE_ITEM_H       88
#define MORE_N_ITEMS      10
#define MORE_GRID_IDX     5
#define MORE_LIST_IDX     6
#define MORE_DLALL_IDX    7
#define MORE_SETTINGS_IDX 8
#define MORE_APPS_IDX     9

/* Cover rendering.  Real covers (loaded via LoadPNGStretch) are fetched
 * one per weak-timer tick so the event loop never blocks; until then a
 * hatch placeholder is drawn.  (Blurhash placeholders were removed — the
 * device is too slow to usefully display them.) */
#define COVER_TMP           "/tmp/.bcov.png"
#define COVER_FETCH_MS      60
#define LIB_DB_FILENAME     "bookshelf_lib.db"
#define LIB_LEGACY_FILENAME "bookshelf_lib.json" /* pre-sqlite store */
#define COVERS_SUBDIR       "covers"
/* Capacity of g_covers_dir: the directory plus '/' + id + ".png" must
 * fit a MAX_PATH_LEN path, so the dir array is bounded by the remainder
 * (MAX_ID_LEN counts its NUL). */
#define COVERS_DIR_CAP (MAX_PATH_LEN - MAX_ID_LEN - 4)
#define TEXT_AREA      52 /* vertical room below the cover for title+author */
#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif
/* Height of the self-drawn status strip used when the firmware's panel
 * painter never activates (PanelHeight()==0 on the live device).  Matches
 * the stock collapsed bar height the emulator's PanelHeight() reports. */
#define SELF_PANEL_H 106

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
 * Open + Download + Delete; a series card offers Download all + Delete
 * series. */
#define CTX_ITEM_H    96
#define CTX_TITLE_H   72
#define CTX_PAD       24
#define CTX_MAX_ITEMS 4

/* Active downloads.  Downloads run synchronously on the event loop
 * (QuickDownload blocks), so at most one is ever in flight; the list
 * still models a queue so a multi-book "Download all" can show every
 * pending item and tick them off one per timer tick. */
#define MAX_DOWNLOADS 64
/* Height reserved inside the download popup for the batch progress bar
 * (one bar covering every open download). */
#define DL_BAR_H 56
/* Cancel button inside the download popup: a 64x64 square directly
 * right of the batch progress bar (comfortable touch target on 300
 * DPI), so it reads as "abort the downloads" rather than a popup
 * close button. */
#define DL_CANCEL_SIZE 64
#define DL_CANCEL_GAP  16

/* Sync-progress popup stages (g_state.sync_stage). */
#define SYNC_STAGE_META   1 /* fetching metadata batches */
#define SYNC_STAGE_SCAN   2 /* local library scan */
#define SYNC_STAGE_COVERS 3 /* cover thumbnails after the sync */
#define SYNC_STAGE_DONE   4 /* finished */
#define SYNC_STAGE_FAIL   5 /* the sync failed */

/* Log viewer (Settings → Show logs) geometry. */
#define LOG_BACK_X  8
#define LOG_BACK_Y  10
#define LOG_BACK_W  128
#define LOG_BACK_H  72
#define LOG_ROW_H   26
#define LOG_FONT_PX 20

/* Stock up/down scroll buttons (the pattern firmware apps use, e.g.
 * the coloring app): an up chevron at the bottom-left corner, a down
 * chevron at the bottom-right, overlaid on the scrollable surface. */
#define SCROLL_BTN_W 150
#define SCROLL_BTN_H 96

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
    TAB_LIBRARY, /* the cover grid / list */
    TAB_SEARCH,  /* search sub-page: input row + history terms */
} MainTab;
typedef enum {
    SOURCE_KAVITA = 0, /* remote server (Kavita) library */
    SOURCE_LOCAL = 1,  /* books indexed by the firmware's scanner.app */
    SOURCE_FOLDER = 2, /* books scanned from a user-picked folder */
} SourceMode;
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
    char  local_path[MAX_PATH_LEN];
    /* Original filename on the provider (saved downloads use it instead
     * of the opaque id); empty → id-based name. */
    char filename[MAX_PATH_LEN];
    /* Source this book came from: "kavita" (server sync), "local"
     * (scanner.app library), "folder" (user-picked folder scan). */
    char source[16];
    long added_at; /* unix epoch from server "addedAt"; 0 if absent */
} Book;
typedef struct {
    int  is_series;
    Book book; /* embedded record (book, or series-card representative) */
    char series_id[MAX_ID_LEN];
    char series_name[48];
    int  series_count; /* books in the series (badge) */
} TileRow;
typedef struct {
    int  sync_state; /* 0 idle, 1 syncing, 2 error */
    int  sync_angle; /* rotation (deg) of the top-bar sync arc */
    char status[160];

    int panel_h; /* height of the system status panel at the BOTTOM of the screen */

    char query[MAX_QUERY_LEN];

    char api_base[260];
    char api_token[MAX_TOKEN_LEN];
    char url_delta[MAX_URL_LEN];
    char url_state[MAX_URL_LEN];
    char url_openwith[MAX_URL_LEN];

    SortMode  sort;
    GroupMode group;
    Filter    filter;
    ViewMode  view_mode;     /* GRID = cover grid, LIST = one row per book */
    int       menu_open;     /* hamburger overlay */
    int       more_open;     /* right "..." overlay */
    int       search_kb;     /* on-screen keyboard is editing the search input */
    int       settings_open; /* full-screen settings overlay */
    MainTab   tab;           /* TAB_LIBRARY / TAB_SEARCH */
    int       launcher_open;
    int       launcher_scroll; /* vertical scroll offset (px) of the launcher body */
    int       launcher_drag_y; /* last POINTERMOVE y while dragging the launcher */
    int       launcher_drag;   /* a drag is in progress (suppress tap on lift) */
    int       launcher_moved;  /* finger travelled far enough to count as drag */

    /* Context (long-press) menu.  ctx_open shows a centred modal sheet
     * over the tile named by ctx_book_id (a book) or ctx_series_id (a
     * series card). */
    int  ctx_open;
    int  ctx_is_series;
    char ctx_book_id[MAX_ID_LEN]; /* book the context menu is open on */
    char ctx_series_id[MAX_ID_LEN];

    /* Download-progress popup.  dl_popup shows a centred modal sheet
     * with the queue/batch progress bar whenever downloads are running
     * (book tap, context-menu Download, Download all).  dl_popup_auto_open
     * is set when the popup was opened by pressing a single book: when
     * the queue drains, the reader launches for dl_popup_book_id. */
    int  dl_popup;
    int  dl_popup_auto_open;
    char dl_popup_book_id[MAX_ID_LEN];

    /* Sync-progress popup (sync button tap).  sync_popup shows a small
     * centred sheet describing what the in-flight sync is doing;
     * sync_stage picks the status line (metadata batch / local scan /
     * covers / done / failed).  Only manual syncs (button, settings
     * apply) open it; boot and timer syncs stay silent. */
    int sync_popup;
    int sync_stage; /* SYNC_STAGE_* */
    int sync_round; /* metadata batch counter */
    int sync_scan;  /* local scan file counter */

    /* Full-screen log viewer (settings → Show logs).  log_open shows
     * the app log tail; log_scroll < 0 means "tail", otherwise the
     * index of the first visible line (0 = oldest). */
    int log_open;
    int log_scroll;

    /* Reader selection.  reader_pref == 0 means "Auto" (honour the
     * server's open-with resolution); otherwise it is a 1-based index
     * into g_readers[] naming the app to launch directly. */
    int reader_pref;

    /* Library source (top-bar button right of home): which books the
     * shelf shows and where downloads come from. */
    int source; /* SOURCE_KAVITA / SOURCE_LOCAL / SOURCE_FOLDER */

    int page;       /* current page (0-based) */
    int saved_page; /* library page to restore on drill-back */

} State;
typedef struct {
    char     id[MAX_ID_LEN];
    ibitmap *cover_bmp;
    int      state;
    long     last_use; /* LRU counter for eviction */
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
/* Download-folder picker overlay (bs_folder.c): header with the current
 * path, a scrollable list of subdirectories, and Select/Back buttons.
 * Browsing is confined to /mnt/ext1 — the list has no ".." above the
 * root, so on-device storage is the only thing choosable. */
#define FOLDER_ROW_H    96
#define FOLDER_LIST_TOP 120
#define FOLDER_BTN_H    96
#define FOLDER_BTN_PAD  24
#define FOLDER_MAX_DIRS 128
/* Root of the folder-source file browser and the Local source scan. */
#define BROWSE_ROOT "/mnt/ext1"
/* Source button (right of the house): the active library source as a
 * small icon + label (globe = Kavita, book = Local, folder = Folder).
 * Wider than the old bare-icon button because it carries text. */
#define SOURCE_BTN_X 112
#define SOURCE_BTN_W 176
typedef struct {
    const char *device;
    const char *partner;
    const char *has_audio;
    const char *has_cloud;
    const char *language;
    const char *localization;
} LcProfile;
#define LC_NDIMS            ((int)(sizeof lc_dims / sizeof lc_dims[0]))
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
    int  x, y, w, h;
} LauncherItem;
#define LAUNCHER_HEADER_H  104
#define LAUNCHER_COLS      3
#define LAUNCHER_GROUP_H   64
#define LAUNCHER_CELL_H    232
#define LAUNCHER_ICON_SZ   120
#define LAUNCHER_MARGIN    16
#define LAUNCHER_DRAG_SLOP 24 /* px of travel before a launcher drag counts */

/* ── global variables ── */

extern char         g_lang[8];
extern const I18n   g_i18n[];
extern FILE        *g_log;
extern char         g_cfg_reader[220];
extern char         g_config_path[600];
extern char         g_drilled_series[MAX_ID_LEN];
extern State        g_state;
extern char         g_search_kb_buf[MAX_QUERY_LEN];
extern CoverSlot    g_covers[NCOVER_SLOTS];
extern TileRow      g_rows[MAX_ROWS * COLS]; /* current page rows */
extern int          g_row_count;             /* rows on the page */
extern int          g_view_total;            /* tiles in the view */
extern int          g_dl_batch_active;       /* download-all batch mode */
extern int          g_dl_batch_total;
extern int          g_dl_batch_done;
extern int          g_dl_batch_failed;
extern char         g_dl_batch_failed_ids[MAX_DOWNLOADS * 4][MAX_ID_LEN];
extern int          g_dl_batch_failed_count;
void                download_all_start(void);
extern int          g_cover_armed;
extern DownloadItem g_downloads[MAX_DOWNLOADS];
extern int          g_download_count;
extern int          g_download_armed;
extern char         g_downloads_dir[128];
/* Raw `downloads_dir=` from the config file (validated against /mnt/ext1
 * by resolve_downloads_dir). */
extern char g_cfg_downloads_dir[256];
/* Folder picked in Settings → Download folder, pending the Save tap. */
extern char              g_settings_dl_dir[256];
extern char              g_covers_dir[COVERS_DIR_CAP];
extern int               g_lp_armed;
extern int               g_lp_vi;
extern int               g_lp_x;
extern int               g_lp_y;
extern int               g_ctx_suppress_up;
extern char              g_argv0[256];
extern ReaderCandidate   g_readers[MAX_READERS];
extern int               g_reader_count;
extern int               g_self_panel;    /* 1 = we draw the status strip ourselves */
extern int               g_display_color; /* 1 = colour display (device_display_colormask) */
extern int               g_settings_edit;
extern char              g_settings_kb_buf[260];
extern const LcProfile   g_lcprof;
extern const char *const lc_dims[];
extern LauncherItem      g_launcher_items[LAUNCHER_MAX_ITEMS];
extern int               g_launcher_count;
extern int               g_launcher_body_h; /* total laid-out body height */
extern int               g_launcher_built;

/* ── function prototypes ── */

const char *i18n(const char *key);
void        log_open(const char *argv0);
void        log_close(void);
void        LOG(const char *fmt, ...);
char       *trim_ws(char *s);
int         read_kv_file(const char *path, cfg_kv_cb cb, void *user);
void        cfg_set_kv(const char *key, const char *value, void *user);
void        dirname_of(const char *path, char *out, size_t out_cap);
void        load_config_file(const char *argv0, struct cfg_out *out);
void        resolve_config_path(const char *argv0);
void        resolve_covers_dir(void);
void        resolve_downloads_dir(void);
void        detect_readers(void);
int         reader_pref_from_path(const char *value);
int         save_config_file(void);
void        local_import_scanner(void);
int         extract_book_meta(const char *path,
                              const char *ext,
                              char       *title,
                              size_t      title_cap,
                              char       *author,
                              size_t      author_cap);
int         extract_book_cover(const char *path, const char *ext, char *out_path, size_t out_cap);
ibitmap    *load_image_scaled(const char *path);
void        progress_reload(void);
int         progress_percent(const char *path);
void        draw_overlay_source(void);
int         on_tap_source(int x, int y);
void        source_geom(int *px, int *py, int *pw, int *ph);
extern int  g_source_open;
void        browse_start(const char *dir);
void        draw_browse(void);
int         on_tap_browse(int x, int y);
int         browse_up(void);
void        browse_page(int dir);
const char *user_path_display(const char *path, char *out, size_t cap);
extern int  g_browse_open;
extern char g_browse_path[256];
extern int  g_browse_scroll;
extern int  g_browse_drag;
extern int  g_browse_drag_y;
extern int  g_browse_moved;
int         http_get(const char *url, int *status_out, char **body_out, int *len_out);
int         http_post(const char *url, const char *body, char **resp_out, int *resp_len);
int
http_post_timeout(const char *url, const char *body, int timeout, char **resp_out, int *resp_len);
void        build_endpoint_urls(void);
char       *json_find_key(const char *obj, const char *key, char *out, size_t cap);
int         json_find_int(const char *obj, const char *key, int default_val);
float       json_find_float(const char *obj, const char *key, float default_val);
int         json_find_bool(const char *obj, const char *key, int default_val);
const char *json_next_string(const char *arr, char *out, size_t cap);
const char *json_next_object(const char *p, const char **end_out);
int         parse_book_obj(const char *obj, Book *b);
void        do_sync(void);
void        store_open(void);
void        store_close(void);
int         store_count(void);
long long   store_get_cursor(void);
void        store_set_cursor(long long cursor);
int         store_upsert_book(const Book *b);
void        store_delete_book(const char *id);
void        store_delete_source(const char *source);
int         store_local_meta_get(
            const char *id, char *title, size_t title_cap, char *author, size_t author_cap);
void     store_local_meta_put(const char *id, const char *title, const char *author);
void     store_set_downloaded(const char *id, int downloaded, const char *local_path);
int      store_get_book(const char *id, Book *out);
void     store_begin(void);
void     store_set_meta(const char *key, const char *value);
int      store_meta_value(const char *key, char *out, size_t cap);
void     store_commit(void);
void     store_series_name(const char *series_id, char *out, size_t cap);
int      store_series_members(const char *series_id, Book *out, int cap);
int      store_count_undownloaded(void);
int      store_next_undownloaded(char ids[][MAX_ID_LEN], int cap);
int      store_next_ids(char ids[][MAX_ID_LEN], int cap, int offset);
void     store_delete_book_file(const char *id);
int      store_series_ids(const char *series_id, char ids[][MAX_ID_LEN], int cap, int offset);
void     store_search_add(const char *term);
int      store_search_count(void);
int      store_search_list(char terms[][MAX_QUERY_LEN], int cap, int offset);
void     view_rebuild(void);
int      view_fetch_page(int page, TileRow *rows, int cap);
int      view_fetch_row(int idx, TileRow *out);
int      view_total(void);
void     cover_cache_path(const char *id, char *out, size_t cap);
void     cover_raw_path(const char *id, char *out, size_t cap);
int      cover_cache_load(const char *id, ibitmap **out_bmp);
void     cover_cache_save(const char *id, const char *png_data, int len);
ibitmap *load_cover_scaled(const char *path);
void     draw_text_centered(ifont *f, int cx, int cy, const char *text, int color);
void     draw_button(
        int x, int y, int w, int h, int selected, const char *label, int label_size, int label_color);
void draw_top_bar(void);
void draw_search_icon(void);
void draw_search_tab(void);
int  downloads_pending(void);
/* draw_tab_row removed — tab row no longer drawn */
CoverSlot *cover_slot(const char *id, int create);
int        view_cols(void);
void       stamp_panel(void);
int        content_bottom(void);
int        view_rows(void);
int        view_pagesize(void);
void       grid_geom(int *top, int *bot, int *cell_w, int *cell_h);
int        tile_rect_for_index(int idx, int *x, int *y, int *w, int *h);
void       cover_rect(int tx, int ty, int tw, int th, int *cx, int *cy, int *cw, int *ch);
void       draw_system_strip(void);
void       cover_schedule_next(void);
void       blit_cover(int cx, int cy, int cw, int ch, const Book *b);
void       draw_series_stack_back(int cx, int cy, int cw, int ch);
void       draw_series_stack_badge(int cx, int cy, int cw, int ch, int count);
void       draw_thumbnail(int x, int y, int w, int h, const TileRow *tr, int vi);
int        history_pagesize(void);
int        current_pages(void);
/* draw_downloads_tab removed — the Downloads page is gone; the progress
 * bar lives in the download popup (draw_dl_popup). */
void          draw_dl_popup(void);
void          dl_popup_geom(int *px, int *py, int *pw, int *ph);
void          refresh_dl_popup(void);
void          dl_cancel_rect(int *x, int *y);
void          cancel_downloads(void);
void          dl_progress_metrics(int *total, int *done, int *failed, int *active);
void          draw_sync_popup(void);
void          sync_popup_geom(int *px, int *py, int *pw, int *ph);
void          sync_popup_open(void);
void          sync_popup_close(void);
void          sync_popup_refresh(void);
void          sync_popup_finish(void); /* sync ended: covers/done stage + auto-close */
void          sync_popup_fail(void);   /* sync failed: show the error, then close */
void          draw_log_view(void);
void          on_tap_log_view(int x, int y);
const char   *log_path(void);
void          draw_scroll_buttons(int up_ok, int down_ok);
void          draw_scroll_buttons_at(int up_ok, int down_ok, int y0);
int           hit_scroll_button(int x, int y);
int           hit_scroll_button_at(int x, int y, int y0);
void          redraw_shelf(void);
void          flush_content(void);
void          draw_grid(void);
void          cover_tick(void *ctx);
void          draw_pager(void);
void          draw_overlay_menu(void);
void          draw_overlay_more(void);
void          draw_status_line(void);
void          settings_keyboard_handler(char *buffer);
const char   *settings_reader_label(void);
void          settings_draw_row(int y, const char *label, const char *value, int editing);
void          settings_draw_button(int y, const char *label, int filled);
void          draw_overlay_settings(void);
void          folder_open(void);
void          folder_close(void);
void          draw_overlay_folder(void);
int           on_tap_folder(int x, int y);
extern int    g_folder_open;
extern char   g_folder_path[256];
extern int    g_folder_scroll;
extern int    g_folder_drag;
extern int    g_folder_drag_y;
extern int    g_folder_moved;
int           hit_top_bar(int x, int y);
void          draw_sync_icon(void);
void          sync_set_active(int on);
int           hit_search_icon(int x, int y);
int           hit_search_input(int x, int y);
int           hit_history(int x, int y);
int           hit_thumbnail(int x, int y);
int           hit_pager(int x, int y);
void          on_tap_overlay_menu(int x, int y);
int           on_tap_overlay_more(int x, int y);
void          settings_close(void);
void          settings_apply(void);
void          on_tap_overlay_settings(int x, int y);
const char   *js_skip_ws(const char *p);
const char   *js_skip_value(const char *p);
void          js_copy_string(const char *p, char *out, size_t cap);
const char   *js_object_body(const char *p);
const char   *js_find_member(const char *p, const char *key);
const char   *lc_prof_val(const char *dim);
const char   *lc_pick_key(const char *obj_body, const char *want);
void          lc_resolve(const char *p, const char *cur_dim, char *out, size_t cap);
int           lc_resolve_bool(const char *p);
char         *read_text_file(const char *path);
const char   *lc_token_en(const char *tok);
void          lc_translate(const char *raw, char *out, size_t cap);
void          launcher_layout(void);
void          launcher_add_app(const char *apps_body, const char *id);
void          launcher_build(void);
void          launcher_scan_ext1_apps(void);
void          draw_launcher_icon(int cx, int cy, const char *icon_name, const char *title);
void          draw_overlay_launcher(void);
void          launch_app(const LauncherItem *it);
void          on_tap_overlay_launcher(int x, int y);
void          launcher_open_set(void);
void          launcher_close(void);
void          drill_back(void);
void          on_tap_thumbnail(int vi);
void          book_local_path(const Book *b, char *out, size_t cap);
void          book_existing_path(const Book *b, char *out, size_t cap);
void          refresh_downloaded(Book *b);
void          refresh_downloaded_flags(void);
DownloadItem *find_download(const char *id);
void          enqueue_download(const Book *b);
void          launch_reader(Book *b);
void          book_press_action(Book *b);
void          delete_book_file(Book *b);
void          download_tick(void *ctx);
void          download_series(const char *series_id);
void          delete_series(const char *series_id);
void          context_geom(int *px, int *py, int *pw, int *ph, int n_items);
int           context_item_count(void);
void          draw_context_menu(void);
void          close_context(void);
void          open_context_for_tile(int vi);
void          longpress_tick(void *ctx);
void          on_tap_context(int x, int y);
int           on_event(int type, int par1, int par2);
void          keyboard_handler(char *buffer);

#endif /* BOOKSHELF_H */
