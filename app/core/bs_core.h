#ifndef BS_CORE_H
#define BS_CORE_H
/*
 * bs_core.h — shared core of the split bookshelf app.
 *
 * This header carries ONLY what every translation unit needs: the SDK
 * include block, the shared constants and types, and the few
 * cross-cutting declarations used across nearly all modules (bs_g_state,
 * bs_g_lang, bs_g_argv0, bs_i18n(), bs_LOG).  Per-module declarations live in the
 * bs_*.h headers, one per translation unit; each includes only this
 * header, so no include cycles are possible.  Every translation unit
 * includes "bs_core.h" plus the headers of the modules whose
 * functions/globals it calls or references.
 *
 * Translation units: see the SOURCES list in Makefile.
 */

#include "bs_plat.h"

#include <ctype.h>
#include <errno.h>
#include <math.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

/* ── configuration ───────────────────────────────────────────────────── */

#ifdef PBEMU_API_HOST
#define BS_API_BASE_DEFAULT PBEMU_API_HOST
#else
#define BS_API_BASE_DEFAULT "http://169.254.1.2:8765"
#endif

#define BS_TOKEN_DEFAULT "pbemu-dev-token"
/* Download folder for books, chosen in Settings → Download folder.
 * The picker only browses inside /mnt/ext1 (on-device storage); the
 * default is the stock PocketBook downloads folder.  In the emulator
 * the non-root qemu-arm guest cannot write /mnt/ext1 at all, so
 * downloads fall back to /tmp there (see resolve_downloads_dir). */
#define BS_DEFAULT_DOWNLOADS_DIR "/mnt/ext1/Downloads"
#define BS_LOCAL_DOWNLOADS_FALLBACK "/tmp"
#define BS_CONFIG_FILENAME "bookshelf.cfg"
/* Guest-writable fallback config path (used when the app's own directory
 * is not writable, e.g. the emulator's non-root qemu-arm guest). */
#define BS_CONFIG_TMP_PATH "/tmp/" BS_CONFIG_FILENAME
/* Reader apps the settings page can detect and offer.  The standard
 * PocketBook reader lives in the firmware image; KOReader is a common
 * third-party install dropped into /mnt/ext1/applications.  Detection is
 * a plain access(X_OK) probe so the list adapts to whatever is actually
 * installed on the device. */
#define BS_READER_STD_PATH "/ebrmain/bin/eink-reader.app"
#define BS_READER_KO_PATH "/mnt/ext1/applications/koreader.app"
#define BS_MAX_READERS 4

#define BS_HTTP_TIMEOUT 8
/* Books per /sync/delta round-trip.  The library itself is unbounded
 * (SQLite-backed); only one batch of parsed books lives in RAM at a
 * time, so 100k books never materialise as one array. */
/* Delta batch size: 1000 books per round.  Bigger than the original
 * 500 cuts the per-round fixed costs (WiFi round trip, worker handoff,
 * transaction fsync) in half for a 100k first sync; the server clamps
 * limits to 2000, and the 400-round ceiling still covers 400k. */
#define BS_SYNC_BATCH 1000
/* Upper bound on grid/list rows per page.  Grid pages hold ROWS rows;
 * list mode computes its row count from the screen geometry and stays
 * below this. */
#define BS_MAX_ROWS 24
/* LRU cover bitmap slots.  Decoded covers are the only per-tile RAM
 * cost; a handful of slots bounds it regardless of library size. */
#define BS_NCOVER_SLOTS 8
#define BS_MAX_TITLE_LEN 96
#define BS_MAX_ID_LEN 48
#define BS_MAX_PATH_LEN 220
#define BS_MAX_URL_LEN 480
#define BS_MAX_TOKEN_LEN 96
#define BS_MAX_QUERY_LEN 80

/* Search suggestion index: per-book term edges in the `suggest`
 * table, queried by prefix (store_suggest_list).  SUGGEST_MAX_TERMS
 * mirrors the server cap (api/storage/suggest.py); SUGGEST_TERM_MAX
 * equals the query buffer so a tapped term always fits g_state.query. */
#define BS_SUGGEST_MAX_TERMS 96
#define BS_SUGGEST_TERM_MAX BS_MAX_QUERY_LEN
#define BS_SUGGEST_MAX_HITS 10


/* Layout constants — tuned for the 1072x1448 633 Era panel (300 DPI).
 * All sizes are generous for comfortable e-ink touch targets. */
#define BS_TOP_BAR_H 96
/* White gap between the top bar's bottom border and the shelf body. */
#define BS_TOP_BAR_PAD 12
/* Search input row height — used only on the Search sub-page; the main
 * shelf no longer carries a search row (the magnifier icon lives in the
 * top bar). */
#define BS_SEARCH_ROW_H 88
#define BS_PAGER_H 96
/* Top-bar icon buttons: 96×96 tap boxes padded 8 px from the screen
 * edges, each holding a line-art glyph in a centred 52×52 icon box.
 * Shared by the draw path (bs_grid.c / bs_screen.c) and the tap hit-test (bs_input.c)
 * so the tappable region always matches the painted button. */
#define BS_TOP_BTN_SIZE 96
#define BS_TOP_BTN_PAD 8
#define BS_TOP_ICON_SIZE 52
#define BS_TOP_ICON_HALF (BS_TOP_ICON_SIZE / 2)
/* History-term rows on the Search sub-page.  SEARCH_HISTORY_MAX caps
 * how many previously committed queries are persisted (and shown). */
#define BS_SEARCH_HISTORY_ROW_H 96
#define BS_SEARCH_HISTORY_MAX 20
#define BS_THUMB_BORDER 4
#define BS_COLS 3
#define BS_ROWS 2
#define BS_PAGESIZE (BS_COLS * BS_ROWS)
#define BS_CELL_MAX_H 780
#define BS_CELL_MAX_W 420
#define BS_CELL_MIN_H 280
#define BS_CELL_MIN_W 280

/* List-view row height.  A list row is a single full-width band holding a
 * small cover + title + author, so it is much shorter than a grid cell and
 * many more fit per page.  150 px keeps the touch target generous on the
 * 300 dpi panel. */
#define BS_LIST_ROW_H 150

/* More-overlay (right drawer) geometry, shared by the draw and tap paths.
 * Items: Sync, 4 sorts, Grid, List, Download all, Settings, System menu,
 * Applications.  (Title Z-A was removed per user request.) */
#define BS_MORE_Y0 96
#define BS_MORE_ITEM_H 88
#define BS_MORE_N_ITEMS 5
#define BS_MORE_GROUP_IDX 0
#define BS_MORE_SORT_IDX 1
#define BS_MORE_DLALL_IDX 2
#define BS_MORE_SETTINGS_IDX 3
#define BS_MORE_APPS_IDX 4

/* Cover rendering.  Real covers (loaded via LoadPNGStretch) are fetched
 * one per weak-timer tick so the event loop never blocks; until then a
 * hatch placeholder is drawn.  (Blurhash placeholders were removed — the
 * device is too slow to usefully display them.) */
#define BS_COVER_TMP "/tmp/.bcov.png"
#define BS_COVER_FETCH_MS 60
#define BS_LIB_DB_FILENAME "bookshelf_lib.db"
#define BS_LIB_LEGACY_FILENAME "bookshelf_lib.json" /* pre-sqlite store */
#define BS_COVERS_SUBDIR "covers"
/* Capacity of g_covers_dir: the directory plus '/' + id + ".png" must
 * fit a MAX_PATH_LEN path, so the dir array is bounded by the remainder
 * (MAX_ID_LEN counts its NUL). */
#define BS_COVERS_DIR_CAP (BS_MAX_PATH_LEN - BS_MAX_ID_LEN - 4)
#define BS_TEXT_AREA 52 /* vertical room below the cover for title+author */
#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif
/* Height of the self-drawn status strip used when the firmware's panel
 * painter never activates (PanelHeight()==0 on the live device).  Matches
 * the stock collapsed bar height the emulator's PanelHeight() reports. */
#define BS_SELF_PANEL_H 106

/* Long-press detection.  The emulator only injects POINTERDOWN/UP/MOVE
 * (the firmware-synthesised EVT_POINTERLONG never fires under qemu), so
 * a long-press is detected app-side: POINTERDOWN arms a one-shot timer;
 * if it elapses before the finger lifts or moves away, the context menu
 * opens.  A POINTERUP that arrives while the timer is still pending is a
 * normal tap. */
#define BS_LONGPRESS_MS 550
/* Finger travel (px) that cancels a pending long-press (a drag, not a
 * hold). */
#define BS_LONGPRESS_SLOP 24
/* Context (long-press) menu — a centred modal sheet.  A book offers
 * Open + Download + Delete; a series card offers Download all + Delete
 * series. */
#define BS_CTX_ITEM_H 96
#define BS_CTX_TITLE_H 72
#define BS_CTX_PAD 24
#define BS_CTX_MAX_ITEMS 4

/* Active downloads.  Each file fetch runs on the shared background
 * worker (bs_worker.c), one download at a time; the list models a
 * queue so a multi-book "Download all" can show every pending item
 * and tick them off as their jobs complete. */
#define BS_MAX_DOWNLOADS 64
/* Height reserved inside the download popup for the batch progress bar
 * (one bar covering every open download). */
#define BS_DL_BAR_H 56
/* Cancel button inside the download popup: a 64x64 square directly
 * right of the batch progress bar (comfortable touch target on 300
 * DPI), so it reads as "abort the downloads" rather than a popup
 * close button. */
#define BS_DL_CANCEL_SIZE 64
#define BS_DL_CANCEL_GAP 16

/* Sync-progress popup stages (g_state.sync_stage). */
#define BS_SYNC_STAGE_META 1   /* fetching metadata batches */
#define BS_SYNC_STAGE_SCAN 2   /* local library scan */
#define BS_SYNC_STAGE_COVERS 3 /* cover thumbnails after the sync */
#define BS_SYNC_STAGE_DONE 4   /* finished */
#define BS_SYNC_STAGE_FAIL 5   /* the sync failed */

/* Full-screen overlay header (launcher, settings, log viewer): a fixed
 * white bar with the Back chevron in the same touch box as the search
 * page's top-bar back button, and the centred title.  Every overlay
 * draws through bs_draw_overlay_header() with these shared values so
 * the geometry can never drift between pages — and the bar height is
 * the top bar's own (BS_TOP_BAR_H), so the overlays sit at exactly
 * the same height as the search and home pages. */
#define BS_OVERLAY_HEADER_H BS_TOP_BAR_H
#define BS_OVERLAY_BACK_X BS_TOP_BTN_PAD
#define BS_OVERLAY_BACK_W BS_TOP_BTN_SIZE
#define BS_OVERLAY_BACK_H 56
#define BS_OVERLAY_BACK_Y ((BS_OVERLAY_HEADER_H - BS_OVERLAY_BACK_H) / 2)

/* Log viewer (Settings → Show logs) geometry.  The header itself is
 * the shared overlay header; the log file path rides in a band just
 * below the header border, then the rows start. */
#define BS_LOG_ROW_H 26
#define BS_LOG_FONT_PX 20
#define BS_LOG_BODY_TOP (BS_OVERLAY_HEADER_H + 42)

/* Stock up/down scroll buttons (the pattern firmware apps use, e.g.
 * the coloring app): an up chevron at the bottom-left corner, a down
 * chevron at the bottom-right, overlaid on the scrollable surface. */
#define BS_SCROLL_BTN_W 150
#define BS_SCROLL_BTN_H 96

typedef struct {
  const char *key;
  const char *en;
  const char *de;
  const char *fr;
  const char *it;
} BsI18n;
typedef void (*bs_cfg_kv_cb)(const char *key, const char *value, void *user);
struct bs_cfg_out {
  char *api_url;
  size_t url_cap;
  char *api_token;
  size_t token_cap;
};
typedef enum {
  BS_SORT_TITLE_ASC,
  BS_SORT_AUTHOR,
  BS_SORT_SERIES,
  BS_SORT_RECENT,
} BsSortMode;
typedef enum {
  BS_GROUP_ALL,       /* no grouping: collapse series into cards (default) */
  BS_GROUP_BY_SERIES, /* dimension groupings: rendered headers + drill-in */
  BS_GROUP_BY_AUTHOR,
  BS_GROUP_BY_YEAR,
  BS_GROUP_BY_GENRE,
} BsGroupDim;
typedef enum {
  BS_VIEW_GRID,
  BS_VIEW_LIST,
} BsViewMode;
typedef enum {
  BS_TAB_LIBRARY, /* the cover grid / list */
  BS_TAB_SEARCH,  /* search sub-page: input row + history terms */
} BsMainTab;
typedef enum {
  BS_SOURCE_KAVITA = 0, /* remote server (Kavita) library */
  BS_SOURCE_LOCAL = 1,  /* books indexed by the firmware's scanner.app */
  BS_SOURCE_FOLDER = 2, /* books scanned from a user-picked folder */
} BsSourceMode;
/* One modal overlay at a time.  Stackable popups (dl_popup,
 * sync_popup) and the search keyboard (search_kb) are NOT part of
 * this enum — they stay flags and can coexist with any overlay. */
typedef enum {
  BS_OV_NONE,
  BS_OV_SOURCE,   /* source chooser sheet (top priority) */
  BS_OV_MORE,     /* right "..." drawer (burger) */
  BS_OV_GROUP,    /* group-by dimension chooser sheet */
  BS_OV_SORT,     /* sort chooser sheet */
  BS_OV_SETTINGS, /* full-screen settings */
  BS_OV_LOG,      /* full-screen log viewer */
  BS_OV_LAUNCHER, /* full-screen launcher */
  BS_OV_FOLDER,   /* download-folder picker (opens ON TOP of settings) */
  BS_OV_CTX,      /* context (long-press) menu */
} BsOverlay;
typedef struct {
  char id[BS_MAX_ID_LEN];
  char title[BS_MAX_TITLE_LEN];
  char author[80];
  char series[48];
  char series_id[BS_MAX_ID_LEN];
  float series_idx; /* volume/chapter number inside series; 0 if N/A */
  char genre[48];   /* grouping dimension; empty when the source doesn't provide it */
  char ext[8];
  int size;
  int downloaded;
  char local_path[BS_MAX_PATH_LEN];
  /* Original filename on the provider (saved downloads use it instead
   * of the opaque id); empty → id-based name. */
  char filename[BS_MAX_PATH_LEN];
  /* Source this book came from: "kavita" (server sync), "local"
   * (scanner.app library), "folder" (user-picked folder scan). */
  char source[16];
  /* Server-folded search blob (folded title + authors + series).
   * Suggestions are folded server-side, so searches must match this
   * folded text — a "songgong" suggestion from "sŏnggong" never
   * matches the raw title.  Empty for local imports (raw fields
   * still match). */
  char search_text[512];
  long added_at; /* unix epoch from server "addedAt"; 0 if absent */
} BsBook;
typedef struct {
  int is_series;
  BsBook book; /* embedded record (book, or series-card representative) */
  char series_id[BS_MAX_ID_LEN];
  char series_name[48];
  int series_count; /* books in the series (badge) */
} BsTileRow;
typedef struct {
  int sync_state; /* 0 idle, 1 syncing, 2 error */
  int sync_angle; /* rotation (deg) of the top-bar sync arc */

  int panel_h; /* height of the system status panel at the BOTTOM of the screen
                */

  char query[BS_MAX_QUERY_LEN];

  char api_base[260];
  char api_token[BS_MAX_TOKEN_LEN];
  char url_delta[BS_MAX_URL_LEN];
  char url_state[BS_MAX_URL_LEN];
  char url_openwith[BS_MAX_URL_LEN];

  BsSortMode sort;
  BsViewMode view_mode; /* GRID = cover grid, LIST = one row per book */
  /* One modal overlay at a time.  Stackable popups (dl_popup,
   * sync_popup) and the search keyboard (search_kb) are NOT part of
   * this enum — they stay flags and can coexist with any overlay. */
  BsOverlay overlay;
  int search_kb;      /* on-screen keyboard is editing the search input */
  BsMainTab tab;        /* TAB_LIBRARY / TAB_SEARCH */
  int launcher_scroll; /* vertical scroll offset (px) of the launcher body */
  int launcher_drag_y; /* last POINTERMOVE y while dragging the launcher */
  int launcher_drag;   /* a drag is in progress (suppress tap on lift) */
  int launcher_moved;  /* finger travelled far enough to count as drag */

  /* Context (long-press) menu.  The centred modal sheet sits over the
   * tile named by ctx_book_id (a book) or ctx_series_id (a series
   * card); its open state lives in g_state.overlay (OV_CTX). */
  int ctx_is_series;
  char ctx_book_id[BS_MAX_ID_LEN]; /* book the context menu is open on */
  char ctx_series_id[BS_MAX_ID_LEN];

  /* Download-progress popup.  dl_popup shows a centred modal sheet
   * with the queue/batch progress bar whenever downloads are running
   * (book tap, context-menu Download, Download all).  dl_popup_auto_open
   * is set when the popup was opened by pressing a single book: when
   * the queue drains, the reader launches for dl_popup_book_id. */
  int dl_popup;
  int dl_popup_auto_open;
  char dl_popup_book_id[BS_MAX_ID_LEN];

  /* Sync-progress popup (sync button tap).  sync_popup shows a small
   * centred sheet describing what the in-flight sync is doing;
   * sync_stage picks the status line (metadata batch / local scan /
   * covers / done / failed).  Only manual syncs (button, settings
   * apply) open it; boot and timer syncs stay silent. */
  int sync_popup;
  int sync_stage; /* SYNC_STAGE_* */
  int sync_round; /* metadata batch counter */
  int sync_scan;  /* local scan file counter */

  /* Full-screen log viewer (settings → Show logs; its open state
   * lives in g_state.overlay as OV_LOG).  log_scroll < 0 means
   * "tail", otherwise the index of the first visible line (0 =
   * oldest). */
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

} BsState;
typedef struct {
  char id[BS_MAX_ID_LEN];
  ibitmap *cover_bmp;
  int state;
  long last_use; /* LRU counter for eviction */
} BsCoverSlot;
typedef struct {
  char id[BS_MAX_ID_LEN];
  char title[BS_MAX_TITLE_LEN];
  int state;
  unsigned int gen; /* generation token: separates a re-enqueued book from
                       a stale in-flight job's settle (see bs_downloads.c) */
} BsDownloadItem;
typedef struct {
  const char *path;
  const char *label;
} BsReaderCandidate;
#define BS_SETTINGS_ROW_H 120
#define BS_SETTINGS_BTN_H 96
/* Download-folder picker overlay (bs_browser.c): header with the current
 * path, a scrollable list of subdirectories, and Select/Back buttons.
 * Browsing is confined to /mnt/ext1 — the list has no ".." above the
 * root, so on-device storage is the only thing choosable. */
#define BS_FOLDER_ROW_H 96
#define BS_FOLDER_LIST_TOP 120
#define BS_FOLDER_BTN_H 96
#define BS_FOLDER_BTN_PAD 24
#define BS_FOLDER_MAX_DIRS 128
/* Root of the folder-source file browser and the Local source scan. */
#define BS_BROWSE_ROOT "/mnt/ext1"
/* Source button (right of the house): the active library source as a
 * small icon + label (globe = Kavita, book = Local, folder = Folder).
 * Wider than the old bare-icon button because it carries text. */
#define BS_SOURCE_BTN_X 112
#define BS_SOURCE_BTN_W 176
typedef struct BsLcProfile {
  char device[16];        /* view.json "device" capability: "all"/"notouch"/"1030" */
  char partner[24];       /* "pocketbook" */
  char has_audio[8];      /* "true"/"false" */
  char has_cloud[8];      /* "false" */
  char language[8];       /* bs_g_lang at init */
  char localization[8];   /* "WW" */
} BsLcProfile;
#define BS_LAUNCHER_MAX_ITEMS 64
#define BS_LAUNCHER_MAX_PARAMS 4
#define BS_LAUNCHER_PARAM_LEN 64
typedef struct BsLauncherItem {
  int kind; /* 0 = header, 1 = app */
  char text[48];
  char path[BS_MAX_PATH_LEN]; /* full app path; MAX_PATH_LEN so long .app names survive */
  char icon[64];
  char params[BS_LAUNCHER_MAX_PARAMS][BS_LAUNCHER_PARAM_LEN];
  int nparams;
  int x, y, w, h;
} BsLauncherItem;
#define BS_LAUNCHER_COLS 3
#define BS_LAUNCHER_GROUP_H 64
#define BS_LAUNCHER_CELL_H 232
#define BS_LAUNCHER_ICON_SZ 120
#define BS_LAUNCHER_MARGIN 16
#define BS_LAUNCHER_DRAG_SLOP 24 /* px of travel before a launcher drag counts */

/* Sync-engine → UI hooks (registered once at startup so the sync
 * engine never calls drawing code directly). */
typedef struct {
  void (*set_active)(int on);      /* sync_set_active: spinner state */
  void (*popup_refresh)(void);     /* sync_popup_refresh */
  void (*popup_finish)(void);      /* sync_popup_finish */
  void (*popup_fail)(void);        /* sync_popup_fail */
  void (*repaint)(void);           /* redraw_shelf */
} BsSyncUiHooks;

/* ── global variables ── */

extern BsState bs_g_state;
extern char bs_g_lang[8];
extern char bs_g_argv0[256];

/* ── function prototypes ── */

const char *bs_i18n(const char *key);
void bs_LOG(const char *fmt, ...);

/* bs_main.c's event loop and search-keyboard callback are the app's
 * entry points; bs_main.c has no header of its own, so they stay here. */
int bs_on_event(int type, int par1, int par2);
void bs_keyboard_handler(char *buffer);

/* Run the bounded cover-cache sweep if the worker flagged it due.  Must
 * be called on the main thread (from the periodic cover_tick) so all
 * covers-directory mutation stays off the worker thread. */
void bs_cover_cache_sweep_if_pending(void);

#endif /* BOOKSHELF_H */
