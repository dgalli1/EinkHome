#ifndef BOOKSHELF_H
#define BOOKSHELF_H
/*
 * bookshelf.h — shared core of the split bookshelf app.
 *
 * This header carries ONLY what every translation unit needs: the SDK
 * include block, the shared constants and types, and the few
 * cross-cutting declarations used across nearly all modules (g_state,
 * g_lang, g_argv0, i18n(), LOG).  Per-module declarations live in the
 * bs_*.h headers, one per translation unit; each includes only this
 * header, so no include cycles are possible.  Every translation unit
 * includes "bookshelf.h" plus the headers of the modules whose
 * functions/globals it calls or references.
 *
 * Translation units: see the SOURCES list in bookshelf/Makefile.
 */

#include <hwconfig.h>
#include <inkview.h>

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
#define DEFAULT_DOWNLOADS_DIR "/mnt/ext1/Downloads"
#define LOCAL_DOWNLOADS_FALLBACK "/tmp"
#define CONFIG_FILENAME "bookshelf.cfg"
/* Guest-writable fallback config path (used when the app's own directory
 * is not writable, e.g. the emulator's non-root qemu-arm guest). */
#define CONFIG_TMP_PATH "/tmp/" CONFIG_FILENAME
/* Reader apps the settings page can detect and offer.  The standard
 * PocketBook reader lives in the firmware image; KOReader is a common
 * third-party install dropped into /mnt/ext1/applications.  Detection is
 * a plain access(X_OK) probe so the list adapts to whatever is actually
 * installed on the device. */
#define READER_STD_PATH "/ebrmain/bin/eink-reader.app"
#define READER_KO_PATH "/mnt/ext1/applications/koreader.app"
#define MAX_READERS 4

#define HTTP_TIMEOUT 8
/* Books per /sync/delta round-trip.  The library itself is unbounded
 * (SQLite-backed); only one batch of parsed books lives in RAM at a
 * time, so 100k books never materialise as one array. */
/* Delta batch size: 1000 books per round.  Bigger than the original
 * 500 cuts the per-round fixed costs (WiFi round trip, worker handoff,
 * transaction fsync) in half for a 100k first sync; the server clamps
 * limits to 2000, and the 400-round ceiling still covers 400k. */
#define SYNC_BATCH 1000
/* Upper bound on grid/list rows per page.  Grid pages hold ROWS rows;
 * list mode computes its row count from the screen geometry and stays
 * below this. */
#define MAX_ROWS 24
/* LRU cover bitmap slots.  Decoded covers are the only per-tile RAM
 * cost; a handful of slots bounds it regardless of library size. */
#define NCOVER_SLOTS 8
#define MAX_TITLE_LEN 96
#define MAX_ID_LEN 48
#define MAX_PATH_LEN 220
#define MAX_URL_LEN 480
#define MAX_TOKEN_LEN 96
#define MAX_QUERY_LEN 80

/* Search suggestion index: per-book term edges in the `suggest`
 * table, queried by prefix (store_suggest_list).  SUGGEST_MAX_TERMS
 * mirrors the server cap (api/storage/suggest.py); SUGGEST_TERM_MAX
 * equals the query buffer so a tapped term always fits g_state.query. */
#define SUGGEST_MAX_TERMS 96
#define SUGGEST_TERM_MAX MAX_QUERY_LEN
#define SUGGEST_MAX_HITS 10

/* Firmware keyboard exports absent from this SDK vintage's headers
 * (same weak pattern as IvSetAppCapability in bs_main.c).  NULL-check
 * before every call; a missing symbol can never crash the app. */
extern void CloseKeyboard(void) __attribute__((weak));
extern void GetKeyboardRect(irect *rect) __attribute__((weak));

/* Layout constants — tuned for the 1072x1448 633 Era panel (300 DPI).
 * All sizes are generous for comfortable e-ink touch targets. */
#define TOP_BAR_H 96
/* White gap between the top bar's bottom border and the shelf body. */
#define TOP_BAR_PAD 12
/* Search input row height — used only on the Search sub-page; the main
 * shelf no longer carries a search row (the magnifier icon lives in the
 * top bar). */
#define SEARCH_ROW_H 88
#define PAGER_H 96
/* Top-bar icon buttons: 96×96 tap boxes padded 8 px from the screen
 * edges, each holding a line-art glyph in a centred 52×52 icon box.
 * Shared by the draw path (bs_ui.c) and the tap hit-test (bs_input.c)
 * so the tappable region always matches the painted button. */
#define TOP_BTN_SIZE 96
#define TOP_BTN_PAD 8
#define TOP_ICON_SIZE 52
#define TOP_ICON_HALF (TOP_ICON_SIZE / 2)
/* History-term rows on the Search sub-page.  SEARCH_HISTORY_MAX caps
 * how many previously committed queries are persisted (and shown). */
#define SEARCH_HISTORY_ROW_H 96
#define SEARCH_HISTORY_MAX 20
#define THUMB_BORDER 4
#define COLS 3
#define ROWS 2
#define PAGESIZE (COLS * ROWS)
#define CELL_MAX_H 600
#define CELL_MAX_W 420
#define CELL_MIN_H 280
#define CELL_MIN_W 280

/* List-view row height.  A list row is a single full-width band holding a
 * small cover + title + author, so it is much shorter than a grid cell and
 * many more fit per page.  150 px keeps the touch target generous on the
 * 300 dpi panel. */
#define LIST_ROW_H 150

/* More-overlay (right drawer) geometry, shared by the draw and tap paths.
 * Items: Sync, 4 sorts, Grid, List, Download all, Settings, System menu,
 * Applications.  (Title Z-A was removed per user request.) */
#define MORE_Y0 96
#define MORE_ITEM_H 88
#define MORE_N_ITEMS 10
#define MORE_GRID_IDX 5
#define MORE_LIST_IDX 6
#define MORE_DLALL_IDX 7
#define MORE_SETTINGS_IDX 8
#define MORE_APPS_IDX 9

/* Cover rendering.  Real covers (loaded via LoadPNGStretch) are fetched
 * one per weak-timer tick so the event loop never blocks; until then a
 * hatch placeholder is drawn.  (Blurhash placeholders were removed — the
 * device is too slow to usefully display them.) */
#define COVER_TMP "/tmp/.bcov.png"
#define COVER_FETCH_MS 60
#define LIB_DB_FILENAME "bookshelf_lib.db"
#define LIB_LEGACY_FILENAME "bookshelf_lib.json" /* pre-sqlite store */
#define COVERS_SUBDIR "covers"
/* Capacity of g_covers_dir: the directory plus '/' + id + ".png" must
 * fit a MAX_PATH_LEN path, so the dir array is bounded by the remainder
 * (MAX_ID_LEN counts its NUL). */
#define COVERS_DIR_CAP (MAX_PATH_LEN - MAX_ID_LEN - 4)
#define TEXT_AREA 52 /* vertical room below the cover for title+author */
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
#define CTX_ITEM_H 96
#define CTX_TITLE_H 72
#define CTX_PAD 24
#define CTX_MAX_ITEMS 4

/* Active downloads.  Each file fetch runs on the shared background
 * worker (bs_worker.c), one download at a time; the list models a
 * queue so a multi-book "Download all" can show every pending item
 * and tick them off as their jobs complete. */
#define MAX_DOWNLOADS 64
/* Height reserved inside the download popup for the batch progress bar
 * (one bar covering every open download). */
#define DL_BAR_H 56
/* Cancel button inside the download popup: a 64x64 square directly
 * right of the batch progress bar (comfortable touch target on 300
 * DPI), so it reads as "abort the downloads" rather than a popup
 * close button. */
#define DL_CANCEL_SIZE 64
#define DL_CANCEL_GAP 16

/* Sync-progress popup stages (g_state.sync_stage). */
#define SYNC_STAGE_META 1   /* fetching metadata batches */
#define SYNC_STAGE_SCAN 2   /* local library scan */
#define SYNC_STAGE_COVERS 3 /* cover thumbnails after the sync */
#define SYNC_STAGE_DONE 4   /* finished */
#define SYNC_STAGE_FAIL 5   /* the sync failed */

/* Log viewer (Settings → Show logs) geometry. */
#define LOG_BACK_X 8
#define LOG_BACK_Y 10
#define LOG_BACK_W 128
#define LOG_BACK_H 72
#define LOG_ROW_H 26
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
  char *api_url;
  size_t url_cap;
  char *api_token;
  size_t token_cap;
};
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
/* One modal overlay at a time.  Stackable popups (dl_popup,
 * sync_popup) and the search keyboard (search_kb) are NOT part of
 * this enum — they stay flags and can coexist with any overlay. */
typedef enum {
  OV_NONE,
  OV_SOURCE,   /* source chooser sheet (top priority) */
  OV_MENU,     /* hamburger overlay */
  OV_MORE,     /* right "..." overlay */
  OV_SETTINGS, /* full-screen settings */
  OV_LOG,      /* full-screen log viewer */
  OV_LAUNCHER, /* full-screen launcher */
  OV_FOLDER,   /* download-folder picker (opens ON TOP of settings) */
  OV_CTX,      /* context (long-press) menu */
} Overlay;
typedef struct {
  char id[MAX_ID_LEN];
  char title[MAX_TITLE_LEN];
  char author[80];
  char series[48];
  char series_id[MAX_ID_LEN];
  float series_idx; /* volume/chapter number inside series; 0 if N/A */
  char ext[8];
  int size;
  int downloaded;
  char local_path[MAX_PATH_LEN];
  /* Original filename on the provider (saved downloads use it instead
   * of the opaque id); empty → id-based name. */
  char filename[MAX_PATH_LEN];
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
} Book;
typedef struct {
  int is_series;
  Book book; /* embedded record (book, or series-card representative) */
  char series_id[MAX_ID_LEN];
  char series_name[48];
  int series_count; /* books in the series (badge) */
} TileRow;
typedef struct {
  int sync_state; /* 0 idle, 1 syncing, 2 error */
  int sync_angle; /* rotation (deg) of the top-bar sync arc */

  int panel_h; /* height of the system status panel at the BOTTOM of the screen
                */

  char query[MAX_QUERY_LEN];

  char api_base[260];
  char api_token[MAX_TOKEN_LEN];
  char url_delta[MAX_URL_LEN];
  char url_state[MAX_URL_LEN];
  char url_openwith[MAX_URL_LEN];

  SortMode sort;
  GroupMode group;
  ViewMode view_mode; /* GRID = cover grid, LIST = one row per book */
  /* One modal overlay at a time.  Stackable popups (dl_popup,
   * sync_popup) and the search keyboard (search_kb) are NOT part of
   * this enum — they stay flags and can coexist with any overlay. */
  Overlay overlay;
  int search_kb;      /* on-screen keyboard is editing the search input */
  MainTab tab;        /* TAB_LIBRARY / TAB_SEARCH */
  int launcher_scroll; /* vertical scroll offset (px) of the launcher body */
  int launcher_drag_y; /* last POINTERMOVE y while dragging the launcher */
  int launcher_drag;   /* a drag is in progress (suppress tap on lift) */
  int launcher_moved;  /* finger travelled far enough to count as drag */

  /* Context (long-press) menu.  The centred modal sheet sits over the
   * tile named by ctx_book_id (a book) or ctx_series_id (a series
   * card); its open state lives in g_state.overlay (OV_CTX). */
  int ctx_is_series;
  char ctx_book_id[MAX_ID_LEN]; /* book the context menu is open on */
  char ctx_series_id[MAX_ID_LEN];

  /* Download-progress popup.  dl_popup shows a centred modal sheet
   * with the queue/batch progress bar whenever downloads are running
   * (book tap, context-menu Download, Download all).  dl_popup_auto_open
   * is set when the popup was opened by pressing a single book: when
   * the queue drains, the reader launches for dl_popup_book_id. */
  int dl_popup;
  int dl_popup_auto_open;
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

} State;
typedef struct {
  char id[MAX_ID_LEN];
  ibitmap *cover_bmp;
  int state;
  long last_use; /* LRU counter for eviction */
} CoverSlot;
typedef struct {
  char id[MAX_ID_LEN];
  char title[MAX_TITLE_LEN];
  int state;
  unsigned int gen; /* generation token: separates a re-enqueued book from
                       a stale in-flight job's settle (see bs_downloads.c) */
} DownloadItem;
typedef struct {
  const char *path;
  const char *label;
} ReaderCandidate;
#define SETTINGS_ROW_H 120
#define SETTINGS_BTN_H 96
/* Download-folder picker overlay (bs_browser.c): header with the current
 * path, a scrollable list of subdirectories, and Select/Back buttons.
 * Browsing is confined to /mnt/ext1 — the list has no ".." above the
 * root, so on-device storage is the only thing choosable. */
#define FOLDER_ROW_H 96
#define FOLDER_LIST_TOP 120
#define FOLDER_BTN_H 96
#define FOLDER_BTN_PAD 24
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
#define LAUNCHER_MAX_ITEMS 64
#define LAUNCHER_MAX_PARAMS 4
#define LAUNCHER_PARAM_LEN 64
typedef struct {
  int kind; /* 0 = header, 1 = app */
  char text[48];
  char path[MAX_PATH_LEN]; /* full app path; MAX_PATH_LEN so long .app names survive */
  char icon[64];
  char params[LAUNCHER_MAX_PARAMS][LAUNCHER_PARAM_LEN];
  int nparams;
  int x, y, w, h;
} LauncherItem;
#define LAUNCHER_HEADER_H 104
#define LAUNCHER_COLS 3
#define LAUNCHER_GROUP_H 64
#define LAUNCHER_CELL_H 232
#define LAUNCHER_ICON_SZ 120
#define LAUNCHER_MARGIN 16
#define LAUNCHER_DRAG_SLOP 24 /* px of travel before a launcher drag counts */

/* Sync-engine → UI hooks (registered once at startup so the sync
 * engine never calls drawing code directly). */
typedef struct {
  void (*set_active)(int on);      /* sync_set_active: spinner state */
  void (*popup_refresh)(void);     /* sync_popup_refresh */
  void (*popup_finish)(void);      /* sync_popup_finish */
  void (*popup_fail)(void);        /* sync_popup_fail */
  void (*repaint)(void);           /* redraw_shelf */
} SyncUiHooks;

/* ── global variables ── */

extern State g_state;
extern char g_lang[8];
extern char g_argv0[256];

/* ── function prototypes ── */

const char *i18n(const char *key);
void LOG(const char *fmt, ...);

/* bs_main.c's event loop and search-keyboard callback are the app's
 * entry points; bs_main.c has no header of its own, so they stay here. */
int on_event(int type, int par1, int par2);
void keyboard_handler(char *buffer);

/* Run the bounded cover-cache sweep if the worker flagged it due.  Must
 * be called on the main thread (from the periodic cover_tick) so all
 * covers-directory mutation stays off the worker thread. */
void cover_cache_sweep_if_pending(void);

#endif /* BOOKSHELF_H */
