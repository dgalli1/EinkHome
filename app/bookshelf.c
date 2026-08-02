/*
 * bookshelf.c — pbemu bookshelf replacement, step 3.
 *
 * A libinkview-based replacement for the firmware's
 * `/mnt/ext1/applications/bookshelf.app` that talks to the new clean
 * API server (`api/api/server.py`, see `api/README.md`).
 *
 * NOTE: name this binary `books.app` on the device.  PocketBook's
 * launcher dispatches by basename — calling it `bookshelf.app` will
 * instead launch the firmware's original.  `build/bookshelf.cfg` is
 * the matching sample config (edit `api_url` before deploying).
 *
 * UI layout (top to bottom):
 *
 *   ┌─────────────────────────────────────────────────────────┐
 *   │ [≡]  pbemu bookshelf                  [⟳]  [⋮]         │  top bar  (62 px)
 *   ├─────────────────────────────────────────────────────────┤
 *   │ search: [_______________________________]               │  search row (52 px)
 *   ├─────────────────────────────────────────────────────────┤
 *   │                                                         │
 *   │   ┌────┐   ┌────┐   ┌────┐                              │
 *   │   │cover│ │cover│ │cover│   <- 3 col × 2 row grid       │  grid
 *   │   └────┘   └────┘   └────┘                              │
 *   │   title    title    title                              │
 *   │   author   author   author                             │
 *   │                                                         │
 *   │   ┌────┐   ┌────┐   ┌────┐                              │
 *   │   │cover│ │cover│ │cover│                              │
 *   │   └────┘   └────┘   └────┘                              │
 *   │   title    title    title                              │
 *   │   author   author   author                             │
 *   │                                                         │
 *   │              ‹  1 / 3  ›                                │  pager
 *   └─────────────────────────────────────────────────────────┘
 *
 * Left overlay (≡): group-by chooser (All / Authors / Series / Recent).
 * Right overlay (⋮): sort + view options.
 *
 * Build:
 *   sdk/build_armel.sh bookshelf/bookshelf.c --output build/bookshelf.app
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

/* ── i18n ────────────────────────────────────────────────────────────── */

static char g_lang[8] = "en";

/* Trivial i18n table.  Key = English string; value = translation.
 * Add rows here for new languages.  Falls back to English on miss.
 */
typedef struct {
    const char *key;
    const char *en;
    const char *de;
    const char *fr;
    const char *it;
} I18n;

static const I18n g_i18n[] = {
    {"app.title", "Bookshelf", "B\u00fccherregal", "\u00c9tag\u00e8re", "pbemu libreria"},
    {"action.sync", "Sync", "Sync", "Sync", "Sync"},
    {"action.more", "More", "Mehr", "Plus", "Altro"},
    {"action.menu", "Menu", "Men\u00fc", "Menu", "Menu"},
    {"search.ph", "search\u2026", "suchen\u2026", "rechercher\u2026", "cerca\u2026"},
    {"status.idle",
     "Tap \u21bb to sync",
     "Tippe \u21bb zum Sync",
     "Touchez \u21bb",
     "Tocca \u21bb"},
    {"status.syncing",
     "Syncing\u2026",
     "Sync l\u00e4uft\u2026",
     "Sync\u2026",
     "Sincronizzando\u2026"},
    {"status.done", "%d book(s)", "%d Buch/B\u00fccher", "%d livre(s)", "%d libro/i"},
    {"status.fail", "Sync failed", "Sync fehlgeschlagen", "\u00c9chec du sync", "Sync fallito"},
    {"status.no_books", "No books yet", "Noch keine B\u00fccher", "Pas de livres", "Nessun libro"},
    {"status.search_no", "No matches", "Keine Treffer", "Aucun r\u00e9sultat", "Nessun risultato"},
    {"group.all", "All books", "Alle B\u00fccher", "Tous les livres", "Tutti i libri"},
    {"group.author", "By author", "Nach Autor", "Par auteur", "Per autore"},
    {"group.series", "By series", "Nach Reihe", "Par s\u00e9rie", "Per serie"},
    {"group.recent", "By recent", "Nach Neuheit", "Par date", "Per data"},
    {"sort.title_az", "Title A\u2013Z", "Titel A\u2013Z", "Titre A\u2013Z", "Titolo A\u2013Z"},
    {"sort.title_za", "Title Z\u2013A", "Titel Z\u2013A", "Titre Z\u2013A", "Titolo Z\u2013A"},
    {"sort.author", "By author", "Nach Autor", "Par auteur", "Per autore"},
    {"sort.series", "By series", "Nach Reihe", "Par s\u00e9rie", "Per serie"},
    {"sort.recent", "Recent", "Neuheiten", "R\u00e9cent", "Recenti"},
    {"view.grid", "Grid", "Raster", "Grille", "Griglia"},
    {"view.list", "List", "Liste", "Liste", "Elenco"},
    {"pager.info", "%d / %d", "%d / %d", "%d / %d", "%d / %d"},
    {"pager.prev", "<", "<", "<", "<"},
    {"pager.next", ">", ">", ">", ">"},
    {"filter.all", "All", "Alle", "Tous", "Tutti"},
    {"filter.dl", "Downloaded", "Heruntergeladen", "T\u00e9l\u00e9charg\u00e9s", "Scaricati"},
    {"filter.rd", "Remote only", "Nur Remote", "Distant seulement", "Solo remoti"},
    {"action.settings", "Settings", "Einstellungen", "Param\u00e8tres", "Impostazioni"},
    {"settings.title", "Settings", "Einstellungen", "Param\u00e8tres", "Impostazioni"},
    {"settings.api_host", "API host", "API-Host", "H\u00f4te API", "Host API"},
    {"settings.api_key", "API key", "API-Schl\u00fcssel", "Cl\u00e9 API", "Chiave API"},
    {"settings.reader", "Reader app", "Lese-App", "Appli lecture", "App lettore"},
    {"settings.reader_auto", "Auto (server)", "Auto (Server)", "Auto (serveur)", "Auto (server)"},
    {"settings.save", "Save & apply", "Speichern", "Enregistrer", "Salva e applica"},
    {"settings.back", "Back", "Zur\u00fcck", "Retour", "Indietro"},
    {"settings.tap_edit", "tap to edit", "tippen", "toucher", "tocca"},
    {"settings.installed", "installed", "installiert", "install\u00e9e", "installata"},
    {"settings.not_installed", "not found", "nicht da", "absente", "assente"},
    {"action.system", "System menu", "Systemmen\u00fc", "Menu syst\u00e8me", "Menu sistema"},
    {"tab.library", "Library", "Bibliothek", "Biblioth\u00e8que", "Libreria"},
    {"tab.downloads", "Downloads", "Downloads", "T\u00e9l\u00e9chargements", "Download"},
    {"action.download_all",
     "Download all",
     "Alle laden",
     "Tout t\u00e9l\u00e9charger",
     "Scarica tutto"},
    {"ctx.download", "Download", "Laden", "T\u00e9l\u00e9charger", "Scarica"},
    {"ctx.download_all",
     "Download all",
     "Alle laden",
     "Tout t\u00e9l\u00e9charger",
     "Scarica tutto"},
    {"ctx.delete", "Delete", "L\u00f6schen", "Supprimer", "Elimina"},
    {"ctx.delete_series",
     "Delete series",
     "Reihe l\u00f6schen",
     "Supprimer la s\u00e9rie",
     "Elimina serie"},
    {"dl.empty",
     "No active downloads",
     "Keine Downloads",
     "Aucun t\u00e9l\u00e9chargement",
     "Nessun download"},
    {"dl.done", "Downloaded", "Geladen", "T\u00e9l\u00e9charg\u00e9", "Scaricato"},
    {"dl.failed", "Failed", "Fehlgeschlagen", "\u00c9chou\u00e9", "Fallito"},
    {"dl.in_progress",
     "Downloading\u2026",
     "L\u00e4dt\u2026",
     "T\u00e9l\u00e9chargement\u2026",
     "Download\u2026"},
    {"dl.queued", "Queued", "In Warteschlange", "En file", "In coda"},
    {"dl.progress",
     "Downloading %d / %d",
     "Lade %d / %d",
     "T\u00e9l\u00e9chargement %d / %d",
     "Download %d / %d"},
    {"dl.complete", "%d downloaded", "%d geladen", "%d t\u00e9l\u00e9charg\u00e9s", "%d scaricati"},
    {"action.apps", "Applications", "Anwendungen", "Applications", "Applicazioni"},
    {"launcher.title", "Applications", "Anwendungen", "Applications", "Applicazioni"},
    {"launcher.empty",
     "No applications",
     "Keine Anwendungen",
     "Aucune application",
     "Nessuna applicazione"},
    {"launcher.back", "Back", "Zurück", "Retour", "Indietro"},
    {NULL, NULL, NULL, NULL, NULL}};

static const char *
i18n(const char *key)
{
    for (const I18n *e = g_i18n; e->key != NULL; e++) {
        if (strcmp(e->key, key) == 0) {
            if (strcmp(g_lang, "de") == 0 && e->de)
                return e->de;
            if (strcmp(g_lang, "fr") == 0 && e->fr)
                return e->fr;
            if (strcmp(g_lang, "it") == 0 && e->it)
                return e->it;
            return e->en;
        }
    }
    return key;
}

/* ── log file ────────────────────────────────────────────────────────── */

static FILE *g_log = NULL;

static void
log_open(const char *argv0)
{
    char        path[300];
    const char *home = getenv("PBEMU_LOG_DIR");
    if (home != NULL && home[0] != '\0') {
        snprintf(path, sizeof path, "%s/bookshelf.log", home);
    } else if (argv0 != NULL && strchr(argv0, '/') != NULL) {
        char dir[260];
        snprintf(dir, sizeof dir, "%s", argv0);
        char *slash = strrchr(dir, '/');
        if (slash != NULL)
            *slash = '\0';
        snprintf(path, sizeof path, "%s/bookshelf.log", dir);
    } else {
        snprintf(path, sizeof path, "/tmp/bookshelf.log");
    }
    g_log = fopen(path, "a");
    if (g_log == NULL)
        g_log = fopen("/tmp/bookshelf.log", "a");
    if (g_log != NULL) {
        setvbuf(g_log, NULL, _IOLBF, 0);
        fprintf(g_log, "--- bookshelf.app log opened (argv0=%s) ---\n", argv0 ? argv0 : "(null)");
    }
}

static void
log_close(void)
{
    if (g_log != NULL) {
        fclose(g_log);
        g_log = NULL;
    }
}

static void
LOG(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    if (g_log != NULL) {
        va_start(ap, fmt);
        vfprintf(g_log, fmt, ap);
        va_end(ap);
    }
}

/* ── config file reader ──────────────────────────────────────────────── */

static char *
trim_ws(char *s)
{
    if (s == NULL)
        return s;
    while (*s == ' ' || *s == '\t' || *s == '\r' || *s == '\n')
        s++;
    char *end = s + strlen(s);
    while (end > s && (end[-1] == ' ' || end[-1] == '\t' || end[-1] == '\r' || end[-1] == '\n'))
        end--;
    *end = '\0';
    return s;
}

typedef void (*cfg_kv_cb)(const char *key, const char *value, void *user);

static int
read_kv_file(const char *path, cfg_kv_cb cb, void *user)
{
    FILE *f = fopen(path, "r");
    if (f == NULL)
        return -1;
    char line[512];
    int  lineno = 0;
    while (fgets(line, sizeof line, f) != NULL) {
        lineno++;
        char *p = trim_ws(line);
        if (*p == '\0' || *p == '#' || *p == ';')
            continue;
        char *eq = strchr(p, '=');
        if (eq == NULL) {
            LOG("[bookshelf] %s:%d: ignoring `%s`\n", path, lineno, p);
            continue;
        }
        *eq = '\0';
        char *k = trim_ws(p);
        char *v = trim_ws(eq + 1);
        cb(k, v, user);
    }
    fclose(f);
    return 0;
}

/* Raw `reader=` value from the config file, resolved to reader_pref after
 * detect_readers() runs (the reader table must exist first). */
static char g_cfg_reader[220];

struct cfg_out {
    char  *api_url;
    char  *api_token;
    size_t cap;
};

static void
cfg_set_kv(const char *key, const char *value, void *user)
{
    struct cfg_out *out = user;
    if (strcmp(key, "api_url") == 0 || strcmp(key, "url") == 0) {
        snprintf(out->api_url, out->cap, "%s", value);
    } else if (strcmp(key, "api_token") == 0 || strcmp(key, "token") == 0) {
        snprintf(out->api_token, out->cap, "%s", value);
    } else if (strcmp(key, "language") == 0 || strcmp(key, "lang") == 0) {
        snprintf(g_lang, sizeof g_lang, "%.3s", value);
        for (char *p = g_lang; *p; p++)
            *p = (char)tolower((unsigned char)*p);
    } else if (strcmp(key, "reader") == 0) {
        snprintf(g_cfg_reader, sizeof g_cfg_reader, "%s", value);
    } else {
        LOG("[bookshelf] config: unknown key `%s`\n", key);
    }
}

static void
dirname_of(const char *path, char *out, size_t out_cap)
{
    if (path == NULL || out_cap == 0) {
        if (out_cap > 0)
            out[0] = '\0';
        return;
    }
    snprintf(out, out_cap, "%s", path);
    char *slash = strrchr(out, '/');
    if (slash != NULL)
        *slash = '\0';
    else
        out[0] = '\0';
}

static void
load_config_file(const char *argv0, struct cfg_out *out)
{
    snprintf(out->api_token, out->cap, "%s", TOKEN_DEFAULT);
    char path[512];

    if (argv0 != NULL && strchr(argv0, '/') != NULL) {
        dirname_of(argv0, path, sizeof path);
        if (path[0] != '\0') {
            char candidate[512];
            snprintf(candidate, sizeof candidate, "%s/bookshelf.cfg", path);
            if (read_kv_file(candidate, cfg_set_kv, out) == 0)
                LOG("[bookshelf] config: %s\n", candidate);
        }
    }
    if (read_kv_file("/etc/pbemu/bookshelf.cfg", cfg_set_kv, out) == 0)
        LOG("[bookshelf] config: /etc/pbemu/bookshelf.cfg\n");
    /* A settings save that had to fall back to /tmp (unwritable app dir,
     * e.g. the emulator guest) is re-applied last so it overrides the
     * read-only base config on the next launch. */
    if (read_kv_file(CONFIG_TMP_PATH, cfg_set_kv, out) == 0)
        LOG("[bookshelf] config: %s (override)\n", CONFIG_TMP_PATH);
}

/* Resolved path of the config file actually loaded (or the preferred
 * write location when none existed).  save_config_file() rewrites this
 * file so settings changes survive a restart.
 *
 * On-device the app's own directory (next to the binary) is writable, so
 * settings persist there.  In the emulator the guest runs as a non-root
 * qemu-arm process whose binary dir (/mnt/ext1/system/bin) is NOT
 * writable — the same reason its log falls back to /tmp — so we fall back
 * to /tmp/bookshelf.cfg, which the guest can write and which the loader
 * re-reads as an override on the next launch. */
static char g_config_path[600];

static void
resolve_config_path(const char *argv0)
{
    char primary[512];
    primary[0] = '\0';
    if (argv0 != NULL && strchr(argv0, '/') != NULL) {
        char dir[512];
        dirname_of(argv0, dir, sizeof dir);
        if (dir[0] != '\0')
            snprintf(primary, sizeof primary, "%s/%s", dir, CONFIG_FILENAME);
    }
    if (primary[0] == '\0')
        snprintf(primary, sizeof primary, "/etc/pbemu/%s", CONFIG_FILENAME);

    /* Prefer the primary when it's writable (either the file exists and
     * is writable, or its directory is writable so we can create it);
     * otherwise use the guest-writable /tmp fallback. */
    if (access(primary, W_OK) == 0) {
        snprintf(g_config_path, sizeof g_config_path, "%s", primary);
        return;
    }
    char dir_copy[600];
    snprintf(dir_copy, sizeof dir_copy, "%s", primary);
    char *slash = strrchr(dir_copy, '/');
    if (slash != NULL)
        *slash = '\0';
    if (dir_copy[0] != '\0' && access(dir_copy, W_OK) == 0) {
        snprintf(g_config_path, sizeof g_config_path, "%s", primary);
        return;
    }
    snprintf(g_config_path, sizeof g_config_path, "%s", CONFIG_TMP_PATH);
}

/* ── book record ─────────────────────────────────────────────────────── */

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

/* A tile in the projected grid view.  At the top level (not drilled),
 * series with >1 book collapse into a single card (is_series=1) showing
 * the newest volume's cover + a triple border + count badge.  Standalone
 * books and drilled-in series members are individual tiles (is_series=0).
 */
typedef struct {
    int  is_series;
    int  book_idx; /* index into g_state.books[] */
    char series_id[MAX_ID_LEN];
    char series_name[48];
    int  series_count; /* books in the series (badge) */
} ViewTile;

static ViewTile g_view[MAX_BOOKS];
static int      g_view_count;
static char     g_drilled_series[MAX_ID_LEN]; /* "" = top level */

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

static State g_state;

/* Full, unfiltered library — the single source of truth that parse/sync
 * mutate.  g_state.books[] is a *filtered projection* rebuilt from this
 * master by apply_filter_and_sort(), so filtering is non-destructive: a
 * second search always starts from the complete library instead of
 * re-filtering an already-shrunk set. */
static Book g_lib[MAX_BOOKS];
static int  g_lib_count;
static void book_local_path(const Book *b, char *out, size_t cap);

/* Edit buffer handed to OpenKeyboard() for the search field.  It MUST be
 * separate from g_state.query: the firmware writes the live keystrokes
 * straight into the buffer we pass, and on commit keyboard_handler()
 * receives that same pointer as `buffer`.  snprintf(g_state.query, ...,
 * buffer) with buffer aliasing g_state.query would copy over a string
 * being simultaneously overwritten, wiping the query (the "search never
 * searches" bug).  A dedicated scratch buffer breaks the alias. */
static char g_search_kb_buf[MAX_QUERY_LEN];

/* Forward declarations — defined below grid_geom; needed by
 * apply_filter_and_sort which runs before them in file order. */
static int  view_cols(void);
static int  view_rows(void);
static int  view_pagesize(void);
static void download_tick(void *ctx);
static void longpress_tick(void *ctx);
static void redraw_shelf(void);
static void book_press_action(Book *b);
static void on_tap_context(int x, int y);
static void close_context(void);
static void draw_downloads_tab(void);
static void draw_grid(void);
static void draw_pager(void);
static void enqueue_download(const Book *b);
static void launcher_open_set(void);
static void launcher_close(void);
static void draw_overlay_launcher(void);
static void on_tap_overlay_launcher(int x, int y);

/* Per-book cover cache, keyed by id and kept OUTSIDE the Book struct so
 * the wholesale struct copies in parse_books_array() can never leak or
 * double-free a decoded bitmap.  state: 0 untouched, 1 fetch in flight,
 * 2 cover loaded, 3 fetch failed. */
typedef struct {
    char     id[MAX_ID_LEN];
    ibitmap *cover_bmp;
    ibitmap *bh_bmp;
    int      state;
} CoverSlot;

static CoverSlot g_covers[MAX_BOOKS];
static int       g_cover_armed = 0;

/* One queued/finished download shown on the Downloads tab.  Downloads
 * run synchronously on the event loop, so the queue is drained one item
 * per timer tick; `state` records the outcome so the tab can show a
 * running tally of what finished.  state: 0 queued, 1 in flight,
 * 2 done, 3 failed. */
typedef struct {
    char id[MAX_ID_LEN];
    char title[MAX_TITLE_LEN];
    int  state;
} DownloadItem;

static DownloadItem g_downloads[MAX_DOWNLOADS];
static int          g_download_count = 0;
static int          g_download_armed = 0;

/* Directory downloads are written to.  Resolved once at startup by
 * resolve_downloads_dir(): LOCAL_DOWNLOADS when the guest can write it
 * (real device), else the /tmp fallback (emulator). */
static char g_downloads_dir[MAX_PATH_LEN];

static void
resolve_downloads_dir(void)
{
    if (access(LOCAL_DOWNLOADS, W_OK) == 0)
        snprintf(g_downloads_dir, sizeof g_downloads_dir, "%s", LOCAL_DOWNLOADS);
    else
        snprintf(g_downloads_dir, sizeof g_downloads_dir, "%s", LOCAL_DOWNLOADS_FALLBACK);
    LOG("[bookshelf] downloads dir = %s\n", g_downloads_dir);
}

/* Long-press detection state.  POINTERDOWN records the tile under the
 * finger and arms a one-shot timer; if it fires before POINTERUP (and
 * the finger hasn't drifted), the context menu opens for that tile. */
static int g_lp_armed = 0;
static int g_lp_vi = -1; /* view-tile index held, or -1 */
static int g_lp_x = 0;
static int g_lp_y = 0;
/* Set when longpress_tick opens the context menu: the finger is still
 * down, so the very next EVT_POINTERUP is the long-press release and must
 * NOT be treated as a tap on the just-opened menu (which would dismiss it
 * immediately).  on_event clears the flag and drops that one UP. */
static int g_ctx_suppress_up = 0;

/* argv[0] from main(). */
static char g_argv0[256];

/* Reader candidates offered by the settings page.  `path` is the absolute
 * on-device binary used both for the installed-probe and for NewTaskEx();
 * `label` is the human-facing name.  Populated once at startup by
 * detect_readers(); g_reader_count is how many entries are valid. */
typedef struct {
    const char *path;
    const char *label;
} ReaderCandidate;

static ReaderCandidate g_readers[MAX_READERS];
static int             g_reader_count = 0;

/* Probe the known reader binaries and fill g_readers[] with the ones that
 * are actually installed (access(X_OK)).  The standard PocketBook reader
 * is always present in the firmware image; KOReader appears only if the
 * user installed it.  Call once at startup before the settings page or
 * reader_pref resolution runs. */
static void
detect_readers(void)
{
    static const ReaderCandidate known[] = {
        {READER_STD_PATH, "Standard"},
        {READER_KO_PATH, "KOReader"},
    };
    g_reader_count = 0;
    for (size_t i = 0; i < sizeof known / sizeof known[0] && g_reader_count < MAX_READERS; i++) {
        if (access(known[i].path, X_OK) == 0) {
            g_readers[g_reader_count++] = known[i];
            LOG("[bookshelf] reader detected: %s (%s)\n", known[i].label, known[i].path);
        } else {
            LOG("[bookshelf] reader not found: %s (%s)\n", known[i].label, known[i].path);
        }
    }
}

/* Map a stored reader= value back to a reader_pref index.  "auto"/""/NULL
 * → 0 (server open-with); a path matching a detected reader → its 1-based
 * index; anything else (e.g. a reader that was uninstalled) → 0. */
static int
reader_pref_from_path(const char *value)
{
    if (value == NULL || value[0] == '\0' || strcmp(value, "auto") == 0)
        return 0;
    for (int i = 0; i < g_reader_count; i++) {
        if (strcmp(g_readers[i].path, value) == 0)
            return i + 1;
    }
    return 0;
}

/* Persist the current api_base / api_token / reader_pref to the config
 * file.  Written as a plain key=value list so the existing reader picks
 * it straight back up on the next launch.  Returns 0 on success. */
static int
save_config_file(void)
{
    FILE *f = fopen(g_config_path, "w");
    if (f == NULL) {
        LOG("[bookshelf] settings: cannot write %s\n", g_config_path);
        return -1;
    }
    fprintf(f, "api_url=%s\n", g_state.api_base);
    fprintf(f, "api_token=%s\n", g_state.api_token);
    if (g_state.reader_pref > 0 && g_state.reader_pref <= g_reader_count)
        fprintf(f, "reader=%s\n", g_readers[g_state.reader_pref - 1].path);
    else
        fprintf(f, "reader=auto\n");
    fclose(f);
    LOG("[bookshelf] settings: saved %s (reader_pref=%d)\n", g_config_path, g_state.reader_pref);
    return 0;
}

/* ── HTTP helpers ────────────────────────────────────────────────────── */

static int
http_get(const char *url, int *status_out, char **body_out, int *len_out)
{
    int   retsize = 0;
    char *body = QuickDownload(url, &retsize, HTTP_TIMEOUT);
    *status_out = 0;
    *body_out = NULL;
    *len_out = 0;
    if (!body || retsize <= 0) {
        if (body)
            free(body);
        return -1;
    }
    *body_out = body;
    *len_out = retsize;
    *status_out = 200;
    return 0;
}

static int
http_post(const char *url, const char *body, char **resp_out, int *resp_len)
{
    int   retsize = 0;
    char *resp = QuickDownloadExt(url, &retsize, HTTP_TIMEOUT, NULL, (char *)body);
    if (resp_out)
        *resp_out = resp;
    if (resp_len)
        *resp_len = retsize;
    if (!resp)
        return -1;
    return 0;
}

static void
build_endpoint_urls(void)
{
    const char *base = g_state.api_base;
    const char *tok = g_state.api_token;
    snprintf(g_state.url_books,
             sizeof g_state.url_books,
             "%s/api/v1/books?limit=200&access_token=%s",
             base,
             tok);
    snprintf(g_state.url_delta,
             sizeof g_state.url_delta,
             "%s/api/v1/sync/delta?access_token=%s",
             base,
             tok);
    snprintf(g_state.url_state,
             sizeof g_state.url_state,
             "%s/api/v1/sync/state?access_token=%s",
             base,
             tok);
    snprintf(g_state.url_libs,
             sizeof g_state.url_libs,
             "%s/api/v1/libraries?access_token=%s",
             base,
             tok);
    snprintf(g_state.url_openwith,
             sizeof g_state.url_openwith,
             "%s/api/v1/open-with?access_token=%s",
             base,
             tok);
}

/* ── JSON helpers (the SDK doesn't ship a parser) ───────────────────── */

static char *
json_find_key(const char *obj, const char *key, char *out, size_t cap)
{
    char pat[80];
    snprintf(pat, sizeof pat, "\"%s\"", key);
    const char *p = strstr(obj, pat);
    if (p == NULL) {
        if (cap > 0)
            out[0] = '\0';
        return NULL;
    }
    p += strlen(pat);
    while (*p == ' ' || *p == ':' || *p == '\t')
        p++;
    if (*p != '"') {
        /* number / bool / null */
        const char *e = p;
        while (*e && *e != ',' && *e != '}' && *e != ' ')
            e++;
        size_t n = (size_t)(e - p);
        if (n + 1 > cap)
            n = cap - 1;
        memcpy(out, p, n);
        out[n] = '\0';
        /* JSON null is not a string value — surface it as empty so callers
         * that test for "no value" (e.g. series_id[0] == '\0') work.  Without
         * this a server-emitted `"seriesId": null` is copied verbatim and
         * every null book collapses into one phantom "null" series card. */
        if (n == 4 && memcmp(out, "null", 4) == 0)
            out[0] = '\0';
        return out;
    }
    p++;
    size_t n = 0;
    while (*p && *p != '"' && n + 1 < cap) {
        if (*p == '\\' && p[1] != '\0') {
            p++;
            if (*p == 'n')
                out[n++] = '\n';
            else if (*p == 't')
                out[n++] = '\t';
            else if (*p == 'r')
                out[n++] = '\r';
            else if (*p == '\\' || *p == '"')
                out[n++] = *p;
            else
                out[n++] = *p;
            p++;
        } else {
            out[n++] = *p++;
        }
    }
    out[n] = '\0';
    return out;
}

static int
json_find_int(const char *obj, const char *key, int default_val)
{
    char buf[32];
    if (json_find_key(obj, key, buf, sizeof buf) != NULL)
        return atoi(buf);
    return default_val;
}

static float
json_find_float(const char *obj, const char *key, float default_val)
{
    char buf[32];
    if (json_find_key(obj, key, buf, sizeof buf) != NULL)
        return (float)atof(buf);
    return default_val;
}

/* Strip a string's first JSON-array element.  Looks for the first
 * `"`-quoted string in `arr` and copies it into `out`.  Returns NULL
 * if the array is empty.
 */
static const char *
json_next_string(const char *arr, char *out, size_t cap)
{
    const char *p = strchr(arr, '"');
    if (p == NULL)
        return NULL;
    p++;
    size_t n = 0;
    while (*p && *p != '"' && n + 1 < cap) {
        if (*p == '\\' && p[1] != '\0') {
            p++;
            if (*p == 'n')
                out[n++] = '\n';
            else if (*p == 't')
                out[n++] = '\t';
            else if (*p == 'r')
                out[n++] = '\r';
            else
                out[n++] = *p;
            p++;
        } else {
            out[n++] = *p++;
        }
    }
    out[n] = '\0';
    /* advance past closing quote + comma */
    const char *q = strchr(p, '"');
    if (q == NULL)
        return NULL;
    q++;
    while (*q == ' ' || *q == ',' || *q == '\t')
        q++;
    return q;
}

static int
cmp_series_index_hint(const Book *b)
{
    int n = 0, seen = 0;
    for (int i = (int)strlen(b->id) - 1; i >= 0; i--) {
        if (b->id[i] >= '0' && b->id[i] <= '9') {
            n = n * 10 + (b->id[i] - '0');
            seen = 1;
        } else if (seen) {
            break;
        }
    }
    return n;
}

/* Build a comma-separated list of string ids in the JSON array
 * found at `*arr_key`, returning a malloc'd buffer.  Caller frees.
 */
static char *
json_collect_id_list(const char *json, const char *arr_key, size_t *out_len)
{
    const char *p = strstr(json, arr_key);
    if (p == NULL) {
        if (out_len)
            *out_len = 0;
        return NULL;
    }
    p = strchr(p, '[');
    if (p == NULL) {
        if (out_len)
            *out_len = 0;
        return NULL;
    }
    /* Collect up to 8KB of ids. */
    char *buf = malloc(8192);
    if (buf == NULL) {
        if (out_len)
            *out_len = 0;
        return NULL;
    }
    buf[0] = '\0';
    size_t      n = 0;
    const char *q = p;
    while (q && n < 8190) {
        char        id[MAX_ID_LEN];
        const char *next = json_next_string(q, id, sizeof id);
        if (id[0] == '\0')
            break;
        int written;
        if (n == 0) {
            written = snprintf(buf + n, 8192 - n, "%s", id);
        } else {
            written = snprintf(buf + n, 8192 - n, ",%s", id);
        }
        if (written < 0 || (size_t)written >= 8192 - n)
            break;
        n += (size_t)written;
        q = next;
        if (q == NULL)
            break;
        /* skip any whitespace */
        while (*q == ' ' || *q == '\t' || *q == '\n')
            q++;
    }
    if (out_len)
        *out_len = n;
    return buf;
}

/* Return 1 if `id` is in the comma-separated list `list`. */
static int
id_in_list(const char *id, const char *list)
{
    if (list == NULL || list[0] == '\0')
        return 0;
    size_t      idlen = strlen(id);
    const char *p = list;
    while (p != NULL && *p != '\0') {
        if (strncmp(p, id, idlen) == 0 && (p[idlen] == '\0' || p[idlen] == ','))
            return 1;
        p = strchr(p, ',');
        if (p != NULL)
            p++;
    }
    return 0;
}

static int
cmp_title_asc(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    return strcasecmp(ba->title, bb->title);
}
static int
cmp_title_desc(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    return strcasecmp(bb->title, ba->title);
}
static int
cmp_author(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    int         r = strcasecmp(ba->author, bb->author);
    if (r != 0)
        return r;
    return strcasecmp(ba->title, bb->title);
}
static int
cmp_series(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    int         r = strcasecmp(ba->series, bb->series);
    if (r != 0)
        return r;
    int ia = cmp_series_index_hint(ba);
    int ib = cmp_series_index_hint(bb);
    return (ia < ib) ? -1 : (ia > ib) ? 1 : 0;
}

/* Most-recently-added first; ties fall back to title so the order is
 * stable.  added_at is 0 when the server omits it, in which case the
 * title tie-break still yields a deterministic, non-empty ordering. */
static int
cmp_recent(const void *a, const void *b)
{
    const Book *ba = a, *bb = b;
    if (ba->added_at != bb->added_at)
        return (ba->added_at > bb->added_at) ? -1 : 1;
    return strcasecmp(ba->title, bb->title);
}

/* Build the projected grid view from the filtered+sorted books array.
 * When not drilled and group is ALL or BY_SERIES, books sharing a
 * series_id with >1 member collapse into a single series card tile.
 * The card's book_idx points to the member with the highest series_idx
 * (newest volume).  When drilled, only that series' members appear flat.
 * For AUTHOR/RECENT groups, everything is flat (no collapse). */
static void
build_view(void)
{
    g_view_count = 0;
    int collapse = (g_drilled_series[0] == '\0') &&
                   (g_state.group == GROUP_ALL || g_state.group == GROUP_BY_SERIES);

    if (g_drilled_series[0] != '\0') {
        /* Drilled: show only members of the drilled series, flat. */
        for (int i = 0; i < g_state.count && g_view_count < MAX_BOOKS; i++) {
            if (strcmp(g_state.books[i].series_id, g_drilled_series) == 0) {
                ViewTile *vt = &g_view[g_view_count++];
                vt->is_series = 0;
                vt->book_idx = i;
                vt->series_id[0] = '\0';
                vt->series_name[0] = '\0';
                vt->series_count = 0;
            }
        }
        return;
    }

    if (!collapse) {
        /* Flat mode: one tile per book. */
        for (int i = 0; i < g_state.count && g_view_count < MAX_BOOKS; i++) {
            ViewTile *vt = &g_view[g_view_count++];
            vt->is_series = 0;
            vt->book_idx = i;
            vt->series_id[0] = '\0';
            vt->series_name[0] = '\0';
            vt->series_count = 0;
        }
        return;
    }

    /* Collapse mode: group by series_id. */
    /* First pass: count members per series_id. */
    typedef struct {
        char sid[MAX_ID_LEN];
        int  count;
        int  best_idx; /* book index with highest series_idx */
    } SerGroup;
    SerGroup groups[MAX_BOOKS];
    int      ngroups = 0;

    for (int i = 0; i < g_state.count; i++) {
        const char *sid = g_state.books[i].series_id;
        if (sid[0] == '\0') {
            /* Standalone book — emit immediately as flat tile. */
            if (g_view_count < MAX_BOOKS) {
                ViewTile *vt = &g_view[g_view_count++];
                vt->is_series = 0;
                vt->book_idx = i;
                vt->series_id[0] = '\0';
                vt->series_name[0] = '\0';
                vt->series_count = 0;
            }
            continue;
        }
        /* Find or create group. */
        int gi = -1;
        for (int g = 0; g < ngroups; g++) {
            if (strcmp(groups[g].sid, sid) == 0) {
                gi = g;
                break;
            }
        }
        if (gi < 0) {
            gi = ngroups++;
            snprintf(groups[gi].sid, sizeof groups[gi].sid, "%s", sid);
            groups[gi].count = 0;
            groups[gi].best_idx = i;
        }
        groups[gi].count++;
        if (g_state.books[i].series_idx > g_state.books[groups[gi].best_idx].series_idx)
            groups[gi].best_idx = i;
    }

    /* Second pass: emit series cards (count>1) or flat tiles (count==1). */
    for (int g = 0; g < ngroups && g_view_count < MAX_BOOKS; g++) {
        ViewTile *vt = &g_view[g_view_count++];
        if (groups[g].count > 1) {
            vt->is_series = 1;
            vt->book_idx = groups[g].best_idx;
            memcpy(vt->series_id, groups[g].sid, MAX_ID_LEN);
            snprintf(vt->series_name,
                     sizeof vt->series_name,
                     "%s",
                     g_state.books[groups[g].best_idx].series);
            vt->series_count = groups[g].count;
        } else {
            /* Single-book series: show as flat tile. */
            vt->is_series = 0;
            vt->book_idx = groups[g].best_idx;
            vt->series_id[0] = '\0';
            vt->series_name[0] = '\0';
            vt->series_count = 0;
        }
    }
}

static void
apply_filter_and_sort(void)
{
    /* Rebuild the filtered projection from the full master library so
     * filtering is non-destructive: every search/sort starts from the
     * complete set, never from an already-shrunk previous result. */
    g_state.count = 0;
    for (int i = 0; i < g_lib_count && i < MAX_BOOKS; i++)
        g_state.books[g_state.count++] = g_lib[i];

    /* Filter: search query, downloaded-only / remote-only. */
    int  n = 0;
    char q[MAX_QUERY_LEN];
    snprintf(q, sizeof q, "%s", g_state.query);
    LOG("[bookshelf] apply_filter: lib=%d query=`%s` filter=%d sort=%d\n",
        g_lib_count,
        q,
        (int)g_state.filter,
        (int)g_state.sort);
    for (char *p = q; *p; p++)
        *p = (char)tolower((unsigned char)*p);
    for (int i = 0; i < g_state.count; i++) {
        Book *b = &g_state.books[i];
        if (g_state.filter == FILTER_DOWNLOADED && !b->downloaded)
            continue;
        if (g_state.filter == FILTER_REMOTE && b->downloaded)
            continue;
        if (q[0] != '\0') {
            char title[MAX_TITLE_LEN], author[80];
            snprintf(title, sizeof title, "%s", b->title);
            snprintf(author, sizeof author, "%s", b->author);
            for (char *p = title; *p; p++)
                *p = (char)tolower((unsigned char)*p);
            for (char *p = author; *p; p++)
                *p = (char)tolower((unsigned char)*p);
            if (!strstr(title, q) && !strstr(author, q))
                continue;
        }
        if (n != i)
            g_state.books[n] = *b;
        n++;
    }
    g_state.total = n;
    g_state.count = n;

    /* Sort. */
    int (*cmp)(const void *, const void *);
    switch (g_state.sort) {
    case SORT_TITLE_ASC:
        cmp = cmp_title_asc;
        break;
    case SORT_TITLE_DESC:
        cmp = cmp_title_desc;
        break;
    case SORT_AUTHOR:
        cmp = cmp_author;
        break;
    case SORT_SERIES:
        cmp = cmp_series;
        break;
    case SORT_RECENT:
        cmp = cmp_recent;
        break;
    default:
        cmp = cmp_title_asc;
        break;
    }
    qsort(g_state.books, g_state.count, sizeof(Book), cmp);

    if (g_state.selected >= g_state.count)
        g_state.selected = -1;

    build_view();

    if (g_state.page >= (g_view_count + view_pagesize() - 1) / view_pagesize())
        g_state.page = 0;
}

/* ── loader (parses /books and /sync/delta JSON) ─────────────────────── */

static int
parse_book_obj(const char *obj, Book *b)
{
    memset(b, 0, sizeof *b);
    json_find_key(obj, "id", b->id, sizeof b->id);
    if (b->id[0] == '\0')
        return -1;
    if (json_find_key(obj, "title", b->title, sizeof b->title) == NULL || b->title[0] == '\0') {
        json_find_key(obj, "summary", b->title, sizeof b->title);
    }
    /* authors is a JSON array; take first. */
    char auth[160];
    if (json_find_key(obj, "authors", auth, sizeof auth))
        json_next_string(auth, b->author, sizeof b->author);
    json_find_key(obj, "series", b->series, sizeof b->series);
    json_find_key(obj, "seriesId", b->series_id, sizeof b->series_id);
    b->series_idx = json_find_float(obj, "seriesIdx", 0.0f);
    json_find_key(obj, "format", b->ext, sizeof b->ext);
    /* Strip format string past first non-alnum. */
    for (char *q = b->ext; *q; q++) {
        if (*q >= 'A' && *q <= 'Z')
            *q = (char)(*q + 32);
        if (*q == '/' || *q == '+' || *q == '.') {
            *q = '\0';
            break;
        }
    }
    b->size = json_find_int(obj, "size", 0);
    json_find_key(obj, "blurhash", b->blurhash, sizeof b->blurhash);
    b->added_at = json_find_int(obj, "addedAt", 0);

    /* Check if the file exists on local storage (resolved downloads dir). */
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    FILE *f = fopen(path, "rb");
    if (f) {
        b->downloaded = 1;
        snprintf(b->local_path, sizeof b->local_path, "%s", path);
        fclose(f);
    }
    return 0;
}

/* Parse a JSON array of book objects starting at `arr_start` ('[').
 * For each book, either update an existing in-memory entry (matched by id)
 * or append a new one (up to MAX_BOOKS).  Returns the new count.
 */
static int
parse_books_array(const char *arr_start)
{
    int         n = g_lib_count;
    const char *p = strchr(arr_start, '[');
    if (!p)
        return n;
    while (n < MAX_BOOKS) {
        const char *obj = strchr(p, '{');
        if (!obj)
            break;
        const char *end = strchr(obj, '}');
        if (!end)
            break;
        Book b;
        if (parse_book_obj(obj, &b) == 0) {
            /* Update an existing master entry in place (matched by id) so
             * re-syncs refresh metadata without duplicating the book. */
            int found = -1;
            for (int i = 0; i < g_lib_count; i++) {
                if (strcmp(g_lib[i].id, b.id) == 0) {
                    found = i;
                    break;
                }
            }
            if (found >= 0)
                g_lib[found] = b;
            else
                g_lib[g_lib_count++] = b;
            n = g_lib_count;
        }
        p = end + 1;
    }
    return n;
}

static void
apply_books_from_added(const char *json, int known_count, const char known_ids[][MAX_ID_LEN])
{
    (void)known_count;
    (void)known_ids;
    /* Parse the "added" array (and "items" for /books).  Existing
     * entries matched by id are updated in place; new entries are
     * appended.  We also honour "removed" by dropping entries.
     */
    const char *p = strstr(json, "\"items\"");
    if (p)
        p = strchr(p, '[');
    if (!p) {
        p = strstr(json, "\"added\"");
        if (p)
            p = strchr(p, '[');
    }
    if (p)
        parse_books_array(p);

    /* Drop books listed in "removed". */
    size_t removed_len = 0;
    char  *removed = json_collect_id_list(json, "\"removed\"", &removed_len);
    if (removed != NULL && removed_len > 0) {
        int write = 0;
        for (int read = 0; read < g_lib_count; read++) {
            if (id_in_list(g_lib[read].id, removed))
                continue;
            if (write != read)
                g_lib[write] = g_lib[read];
            write++;
        }
        g_lib_count = write;
    }
    free(removed);

    /* Drop books listed in "removed" by id. */
    {
        const char *q = strstr(json, "\"removedIds\"");
        if (q) {
            q = strchr(q, '[');
            if (q) {
                char *rids = json_collect_id_list(q, "]", NULL);
                if (rids != NULL) {
                    int write = 0;
                    for (int read = 0; read < g_lib_count; read++) {
                        if (id_in_list(g_lib[read].id, rids))
                            continue;
                        if (write != read)
                            g_lib[write] = g_lib[read];
                        write++;
                    }
                    g_lib_count = write;
                    free(rids);
                }
            }
        }
    }

    LOG("[bookshelf] apply_books_from_added: master count=%d\n", g_lib_count);
}

/* ── /sync/delta POST ────────────────────────────────────────────────── */

static void
do_sync(void)
{
    LOG("[bookshelf] do_sync ENTER url_delta=%s\n", g_state.url_delta);
    g_state.sync_state = 1;
    snprintf(g_state.status, sizeof g_state.status, "%s", i18n("status.syncing"));
    /* A previous sync may have hit the server before its cover cache was
     * warm; give failed covers one more chance each sync. */
    for (int i = 0; i < MAX_BOOKS; i++) {
        if (g_covers[i].state == 3)
            g_covers[i].state = 0;
    }

    char req_body[2048];
    int  n = snprintf(req_body, sizeof req_body, "{\"known\":[");
    for (int i = 0; i < g_lib_count && n < (int)sizeof(req_body) - 32; i++) {
        n += snprintf(req_body + n, sizeof req_body - n, "%s\"%s\"", i ? "," : "", g_lib[i].id);
    }
    snprintf(req_body + n, sizeof req_body - n, "]}");

    char *body = NULL;
    int   retsize = 0;
    if (http_post(g_state.url_delta, req_body, &body, &retsize) != 0 || !body) {
        LOG("[bookshelf] do_sync FAILED: url=%s body=%p\n", g_state.url_delta, (void *)body);
        g_state.sync_state = 2;
        snprintf(g_state.status, sizeof g_state.status, "%s", i18n("status.fail"));
        if (body)
            free(body);
        return;
    }
    LOG("[bookshelf] do_sync: body=%p retsize=%d\n", (void *)body, retsize);
    apply_books_from_added(body, 0, NULL);
    free(body);

    apply_filter_and_sort();

    g_state.sync_state = 0;
    /* post state back (best-effort) */
    char state_body[2048];
    n = snprintf(state_body, sizeof state_body, "{\"deviceId\":\"pbemu\",\"known\":[");
    for (int i = 0; i < g_lib_count && n < (int)sizeof(state_body) - 32; i++) {
        n += snprintf(state_body + n, sizeof state_body - n, "%s\"%s\"", i ? "," : "", g_lib[i].id);
    }
    snprintf(state_body + n, sizeof state_body - n, "]}");
    char *resp = NULL;
    int   rl = 0;
    http_post(g_state.url_state, state_body, &resp, &rl);
    if (resp)
        free(resp);
}

/* ── HTTP GET /books (for full re-fetch) ─────────────────────────────── */

static void
do_fetch_all(void)
{
    char *body = NULL;
    int   retsize = 0;
    if (http_get(g_state.url_books, &(int){0}, &body, &retsize) != 0 || !body) {
        snprintf(g_state.status, sizeof g_state.status, "%s", i18n("status.fail"));
        return;
    }
    apply_books_from_added(body, 0, NULL);
    free(body);
    apply_filter_and_sort();
}

/* ── drawing primitives ─────────────────────────────────────────────── */

static void
draw_text_centered(ifont *f, int cx, int cy, const char *text, int color)
{
    if (f == NULL)
        return;
    SetFont(f, color);
    DrawString(cx - StringWidth(text) / 2, cy, text);
}

static void
draw_button(
    int x, int y, int w, int h, int selected, const char *label, int label_size, int label_color)
{
    DrawRect(x, y, w, h, BLACK);
    FillArea(x + 1, y + 1, w - 2, h - 2, selected ? BLACK : WHITE);
    if (label == NULL || label[0] == '\0')
        return;
    ifont *f = OpenFont(DEFAULTFONTB, label_size, 0);
    if (f != NULL) {
        SetFont(f, label_color != 0 ? label_color : (selected ? WHITE : BLACK));
        DrawString(x + (w - StringWidth(label)) / 2, y + (h - label_size) / 2 - 2, label);
        CloseFont(f);
    }
}

static void
draw_top_bar(void)
{
    int w = ScreenWidth();
    int y0 = g_state.panel_h;
    int col = BLACK;

    FillArea(0, y0, w, TOP_BAR_H, WHITE);
    DrawLine(0, y0 + TOP_BAR_H, w, y0 + TOP_BAR_H, col);

    /* Left button: back-arrow when drilled, house icon otherwise. */
    int home_w = 96;
    int home_x = 8;
    int home_y = y0 + (TOP_BAR_H - home_w) / 2;
    if (g_drilled_series[0] != '\0') {
        /* Left-pointing chevron arrow. */
        int ax = home_x + 20;
        int ay = home_y + home_w / 2;
        DrawLine(ax, ay, ax + 30, ay - 30, col);
        DrawLine(ax, ay, ax + 30, ay + 30, col);
        DrawLine(ax + 4, ay, ax + 34, ay - 30, col);
        DrawLine(ax + 4, ay, ax + 34, ay + 30, col);
    } else {
        /* house outline (pentagon + floor break for door) */
        DrawLine(home_x + 5, home_y + 29, home_x + 5, home_y + 85, col);
        DrawLine(home_x + 5, home_y + 29, home_x + 48, home_y - 8, col);
        DrawLine(home_x + 48, home_y - 8, home_x + 91, home_y + 29, col);
        DrawLine(home_x + 91, home_y + 29, home_x + 91, home_y + 85, col);
        DrawLine(home_x + 5, home_y + 85, home_x + 37, home_y + 85, col);
        DrawLine(home_x + 53, home_y + 85, home_x + 91, home_y + 85, col);
        /* door */
        DrawLine(home_x + 37, home_y + 85, home_x + 37, home_y + 61, col);
        DrawLine(home_x + 37, home_y + 61, home_x + 53, home_y + 61, col);
        DrawLine(home_x + 53, home_y + 61, home_x + 53, home_y + 85, col);
    }

    /* Centered title — series name when drilled, app title otherwise. */
    ifont *tf = OpenFont(DEFAULTFONT, 44, 0);
    if (tf != NULL) {
        char title[80];
        if (g_drilled_series[0] != '\0') {
            /* Find the series name from the first view tile. */
            title[0] = '\0';
            for (int i = 0; i < g_view_count; i++) {
                if (g_view[i].book_idx >= 0 &&
                    strcmp(g_state.books[g_view[i].book_idx].series_id, g_drilled_series) == 0) {
                    snprintf(title, sizeof title, "%s", g_state.books[g_view[i].book_idx].series);
                    break;
                }
            }
            if (title[0] == '\0')
                snprintf(title, sizeof title, "Series");
        } else {
            snprintf(title, sizeof title, "%s", i18n("app.title"));
            for (char *p = title; *p; p++)
                if (*p >= 'a' && *p <= 'z')
                    *p = (char)(*p - 32);
        }
        SetFont(tf, col);
        DrawString((w - StringWidth(title)) / 2, y0 + (TOP_BAR_H - 40) / 2, title);
        CloseFont(tf);
    }

    /* Right "menu" button — 96×96 solid black circle with three
     * white hamburger lines. */
    int menu_w = 96;
    int menu_x = w - menu_w - 8;
    int menu_y = y0 + (TOP_BAR_H - menu_w) / 2;
    int menu_cx = menu_x + menu_w / 2;
    int menu_cy = menu_y + menu_w / 2;
    int menu_r = menu_w / 2;
    FillArea(menu_cx - menu_r, menu_cy - menu_r, menu_r * 2, menu_r * 2, col);
    int ml_w = 44;
    FillArea(menu_cx - ml_w / 2, menu_cy - 19, ml_w, 6, WHITE);
    FillArea(menu_cx - ml_w / 2, menu_cy - 3, ml_w, 6, WHITE);
    FillArea(menu_cx - ml_w / 2, menu_cy + 13, ml_w, 6, WHITE);
}

static void
draw_search_row(void)
{
    int w = ScreenWidth();
    int y = g_state.panel_h + TOP_BAR_H;
    FillArea(0, y, w, SEARCH_ROW_H, WHITE);
    DrawLine(0, y + SEARCH_ROW_H - 1, w, y + SEARCH_ROW_H - 1, BLACK);

    ifont *f = OpenFont(DEFAULTFONT, 28, 0);
    if (f == NULL)
        return;

    SetFont(f, BLACK);
    const char *icon = "Q"; /* magnifier */
    DrawString(16, y + (SEARCH_ROW_H - 28) / 2 - 2, icon);

    /* text box border */
    int tx = 64;
    int tw = w - 128;
    int ty = y + 10;
    int th = SEARCH_ROW_H - 20;
    DrawRect(tx, ty, tw, th, BLACK);
    FillArea(tx + 1, ty + 1, tw - 2, th - 2, g_state.search_open ? BLACK : WHITE);

    if (g_state.query[0] != '\0') {
        SetFont(f, g_state.search_open ? WHITE : BLACK);
        DrawString(tx + 10, ty + (th - 28) / 2 - 2, g_state.query);
    } else if (!g_state.search_open) {
        SetFont(f, BLACK);
        DrawString(tx + 10, ty + (th - 28) / 2 - 2, i18n("search.ph"));
    }

    /* cursor when focused */
    if (g_state.search_open) {
        int cursor_x = tx + 10 + StringWidth(g_state.query) + 1;
        DrawLine(cursor_x, ty + 6, cursor_x, ty + th - 6, WHITE);
    }
    CloseFont(f);
}

/* Number of downloads still pending (queued or in flight) — shown as a
 * badge on the Downloads tab so the user can see work is in progress
 * without switching tabs. */
static int
downloads_pending(void)
{
    int n = 0;
    for (int i = 0; i < g_download_count; i++)
        if (g_downloads[i].state == 0 || g_downloads[i].state == 1)
            n++;
    return n;
}

/* Two-tab switcher drawn directly under the search row: Library |
 * Downloads.  The active tab is an inverted (black) pill; the Downloads
 * tab carries a small count badge while any download is pending. */
static void
draw_tab_row(void)
{
    int w = ScreenWidth();
    int y = g_state.panel_h + TOP_BAR_H + SEARCH_ROW_H;
    FillArea(0, y, w, TAB_ROW_H, WHITE);
    DrawLine(0, y + TAB_ROW_H - 1, w, y + TAB_ROW_H - 1, BLACK);

    int tab_w = w / 2;
    int pad = 12;
    int th = TAB_ROW_H - 2 * pad;

    struct {
        const char *label;
        int         active;
        int         x;
    } tabs[2] = {
        {i18n("tab.library"), g_state.tab == TAB_LIBRARY, 0},
        {i18n("tab.downloads"), g_state.tab == TAB_DOWNLOADS, tab_w},
    };

    ifont *f = OpenFont(DEFAULTFONTB, 30, 0);
    if (f == NULL)
        return;
    for (int i = 0; i < 2; i++) {
        int tx = tabs[i].x + pad;
        int tw = tab_w - 2 * pad;
        int ty = y + pad;
        FillArea(tx, ty, tw, th, tabs[i].active ? BLACK : WHITE);
        DrawRect(tx, ty, tw, th, BLACK);
        SetFont(f, tabs[i].active ? WHITE : BLACK);
        int lw = StringWidth(tabs[i].label);
        DrawString(tx + (tw - lw) / 2, ty + (th - 30) / 2 - 2, tabs[i].label);
        /* Pending-count badge on the Downloads tab. */
        if (i == 1) {
            int pend = downloads_pending();
            if (pend > 0) {
                char badge[8];
                snprintf(badge, sizeof badge, "%d", pend);
                int bw = StringWidth(badge) + 14;
                int bx = tx + tw - bw - 6;
                int by = ty + 6;
                FillArea(bx, by, bw, 30, tabs[i].active ? WHITE : BLACK);
                SetFont(f, tabs[i].active ? BLACK : WHITE);
                DrawString(bx + 7, by + 1, badge);
            }
        }
    }
    CloseFont(f);
}

/* -- cover / blurhash helpers ----------------------------------------- */

static void cover_tick(void *ctx);

static const char bh_base83[84] =
    "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz#$%*+,-.:;=?@[]^_{|}~";

static int
bh_value(char c)
{
    for (int i = 0; i < 83; i++) {
        if (bh_base83[i] == c)
            return i;
    }
    return -1;
}

static int
bh_decode83(const char *s, int n)
{
    int v = 0;
    for (int i = 0; i < n; i++) {
        int d = bh_value(s[i]);
        if (d < 0)
            return -1;
        v = v * 83 + d;
    }
    return v;
}

static float
bh_s2l(int v)
{
    float x = v / 255.0f;
    return x <= 0.04045f ? x / 12.92f : powf((x + 0.055f) / 1.055f, 2.4f);
}

static int
bh_l2s(float v)
{
    if (v < 0.0f)
        v = 0.0f;
    if (v > 1.0f)
        v = 1.0f;
    float s = v <= 0.0031308f ? 12.92f * v : 1.055f * powf(v, 1.0f / 2.4f) - 0.055f;
    int   r = (int)(s * 255.0f + 0.5f);
    return r < 0 ? 0 : (r > 255 ? 255 : r);
}

static float
bh_sign_pow(float v, float e)
{
    return (v >= 0.0f ? 1.0f : -1.0f) * powf(fabsf(v), e);
}

static CoverSlot *
cover_slot(const char *id, int create)
{
    CoverSlot *empty = NULL;
    for (int i = 0; i < MAX_BOOKS; i++) {
        if (g_covers[i].id[0] && strcmp(g_covers[i].id, id) == 0)
            return &g_covers[i];
        if (empty == NULL && g_covers[i].id[0] == '\0')
            empty = &g_covers[i];
    }
    if (!create)
        return NULL;
    if (empty == NULL) {
        /* Table full: evict a slot whose book is no longer loaded. */
        for (int i = 0; i < MAX_BOOKS; i++) {
            int inuse = 0;
            for (int j = 0; j < g_lib_count; j++) {
                if (strcmp(g_lib[j].id, g_covers[i].id) == 0) {
                    inuse = 1;
                    break;
                }
            }
            if (!inuse) {
                empty = &g_covers[i];
                break;
            }
        }
    }
    if (empty == NULL)
        empty = &g_covers[0];
    if (empty->cover_bmp) {
        free(empty->cover_bmp);
        empty->cover_bmp = NULL;
    }
    if (empty->bh_bmp) {
        free(empty->bh_bmp);
        empty->bh_bmp = NULL;
    }
    memset(empty, 0, sizeof *empty);
    snprintf(empty->id, sizeof empty->id, "%s", id);
    return empty;
}

/* Mode-aware layout accessors.  Grid mode keeps the fixed 3×2 cover
 * layout; list mode is a single column of short full-width rows, so it
 * fits many more books per page.  Every draw/hit/paging path reads the
 * grid through these so the two modes stay consistent. */
static int
view_cols(void)
{
    return g_state.view_mode == VIEW_LIST ? 1 : COLS;
}

static int
view_rows(void)
{
    if (g_state.view_mode != VIEW_LIST)
        return ROWS;
    int t = g_state.panel_h + TOP_BAR_H + SEARCH_ROW_H + TAB_ROW_H;
    int b = ScreenHeight() - PAGER_H;
    if (g_state.menu_open || g_state.more_open)
        b = ScreenHeight();
    int rows = (b - t - 8) / LIST_ROW_H;
    if (rows < 1)
        rows = 1;
    return rows;
}

static int
view_pagesize(void)
{
    return view_cols() * view_rows();
}

/* Shared grid geometry so the draw loop and the per-tile fetch blit
 * agree on every coordinate. */
static void
grid_geom(int *top, int *bot, int *cell_w, int *cell_h)
{
    int w = ScreenWidth();
    int t = g_state.panel_h + TOP_BAR_H + SEARCH_ROW_H + TAB_ROW_H;
    int b = ScreenHeight() - PAGER_H;
    if (g_state.menu_open || g_state.more_open)
        b = ScreenHeight();
    int avail_h = b - t - 8;
    int avail_w = w - 16;
    int cw, ch;
    if (g_state.view_mode == VIEW_LIST) {
        /* List rows are full-width bands of fixed height; the grid
         * min/max clamps would distort them, so they are skipped. */
        cw = avail_w;
        ch = LIST_ROW_H;
    } else {
        cw = avail_w / COLS;
        ch = avail_h / ROWS;
        if (ch > CELL_MAX_H)
            ch = CELL_MAX_H;
        if (cw > CELL_MAX_W)
            cw = CELL_MAX_W;
        if (ch < CELL_MIN_H)
            ch = CELL_MIN_H;
        if (cw < CELL_MIN_W)
            cw = CELL_MIN_W;
    }
    *top = t;
    *bot = b;
    *cell_w = cw;
    *cell_h = ch;
}

/* Screen rect of tile `idx`, or 0 when it isn't on the current page. */
static int
tile_rect_for_index(int idx, int *x, int *y, int *w, int *h)
{
    int top, bot, cell_w, cell_h;
    (void)bot;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int cols = view_cols();
    int ps = view_pagesize();
    int page_start = g_state.page * ps;
    int rel = idx - page_start;
    if (rel < 0 || rel >= ps || idx >= g_view_count)
        return 0;
    int row = rel / cols;
    int col = rel % cols;
    *x = 8 + col * cell_w;
    *y = top + 4 + row * cell_h;
    *w = cell_w - 8;
    *h = cell_h - 6;
    return 1;
}

/* Centered 2:3 portrait card inside the tile, leaving room below for the
 * title and author lines. */
static void
cover_rect(int tx, int ty, int tw, int th, int *cx, int *cy, int *cw, int *ch)
{
    int inner_w = tw - 2 * THUMB_BORDER;
    int inner_h = th - 2 * THUMB_BORDER;
    int ch0 = inner_h - TEXT_AREA;
    int cw0 = ch0 * 2 / 3;
    if (cw0 > inner_w) {
        cw0 = inner_w;
        ch0 = cw0 * 3 / 2;
    }
    if (ch0 > inner_h)
        ch0 = inner_h;
    if (ch0 < 8)
        ch0 = 8;
    *cw = cw0;
    *ch = ch0;
    *cx = tx + THUMB_BORDER + (inner_w - cw0) / 2;
    *cy = ty + THUMB_BORDER;
}

/* Decode a blurhash string into a small 8-bit greyscale bitmap cached on
 * the slot.  Luminance of the reconstructed linear RGB gives a soft grey
 * placeholder that reads correctly on the 8-bit panel. */
static void
bh_ensure(CoverSlot *s, const Book *b)
{
    if (s == NULL || b->blurhash[0] == '\0' || s->bh_bmp != NULL)
        return;
    int len = (int)strlen(b->blurhash);
    int size_flag = bh_decode83(b->blurhash, 1);
    if (size_flag < 0 || len < 6)
        return;
    int comp_x = (size_flag % 9) + 1;
    int comp_y = (size_flag / 9) + 1;
    int need = 4 + 2 * (comp_x * comp_y);
    if (len < need || comp_x * comp_y > 81)
        return;
    int quant_max = bh_decode83(b->blurhash + 1, 1);
    if (quant_max < 0)
        return;
    float max_ac = (quant_max + 1) / 166.0f;

    float fac[81][3];
    int   dc = bh_decode83(b->blurhash + 2, 4);
    if (dc < 0)
        return;
    fac[0][0] = bh_s2l((dc >> 16) & 255);
    fac[0][1] = bh_s2l((dc >> 8) & 255);
    fac[0][2] = bh_s2l(dc & 255);
    int pos = 6;
    for (int k = 1; k < comp_x * comp_y; k++) {
        int ac = bh_decode83(b->blurhash + pos, 2);
        if (ac < 0)
            return;
        pos += 2;
        int qr = ac / (19 * 19);
        int qg = (ac / 19) % 19;
        int qb = ac % 19;
        fac[k][0] = bh_sign_pow((qr - 9.0f) / 9.0f, 2.0f) * max_ac;
        fac[k][1] = bh_sign_pow((qg - 9.0f) / 9.0f, 2.0f) * max_ac;
        fac[k][2] = bh_sign_pow((qb - 9.0f) / 9.0f, 2.0f) * max_ac;
    }

    ibitmap *bmp = NewBitmap8(BH_W, BH_H);
    if (bmp == NULL)
        return;
    int scan = bmp->scanline;
    for (int y = 0; y < BH_H; y++) {
        for (int x = 0; x < BH_W; x++) {
            float r = 0.0f, g = 0.0f, bl = 0.0f;
            for (int j = 0; j < comp_y; j++) {
                for (int i = 0; i < comp_x; i++) {
                    float basis =
                        cosf((float)M_PI * i * x / BH_W) * cosf((float)M_PI * j * y / BH_H);
                    float *f = fac[i + j * comp_x];
                    r += f[0] * basis;
                    g += f[1] * basis;
                    bl += f[2] * basis;
                }
            }
            float lum = 0.2126f * r + 0.7152f * g + 0.0722f * bl;
            bmp->data[y * scan + x] = (unsigned char)bh_l2s(lum);
        }
    }
    s->bh_bmp = bmp;
}

/* Arm the one-shot fetch timer if any visible tile still needs a cover. */
static void
cover_schedule_next(void)
{
    if (g_cover_armed)
        return;
    int top, bot, cell_w, cell_h;
    (void)top;
    (void)cell_w;
    (void)cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int ps = view_pagesize();
    int page_start = g_state.page * ps;
    int lim = page_start + ps;
    if (lim > g_view_count)
        lim = g_view_count;
    for (int i = page_start; i < lim; i++) {
        CoverSlot *s = cover_slot(g_state.books[g_view[i].book_idx].id, 1);
        if (s != NULL && s->state == 0) {
            g_cover_armed = 1;
            SetWeakTimerEx("bcov", cover_tick, NULL, COVER_FETCH_MS);
            return;
        }
    }
}

/* Blit a book's cover (decoded PNG, blurhash placeholder, or hatch
 * fallback) into the given rect.  Shared by the grid card and the list
 * row so both modes fetch/cache covers identically. */
static void
blit_cover(int cx, int cy, int cw, int ch, const Book *b)
{
    CoverSlot *s = cover_slot(b->id, 1);
    if (s != NULL && s->cover_bmp != NULL) {
        StretchBitmap(cx, cy, cw, ch, s->cover_bmp, 0);
        return;
    }
    if (b->blurhash[0] != '\0') {
        bh_ensure(s, b);
        if (s != NULL && s->bh_bmp != NULL) {
            StretchBitmap(cx, cy, cw, ch, s->bh_bmp, 0);
            return;
        }
    }
    for (int yy = cy; yy < cy + ch; yy += 8)
        DrawLine(cx, yy, cx + cw, yy, LGRAY);
}

/* Series card decoration: draw the cover as the front book of a stack.
 * Two "page" sheets peek out along the top and left edges (offset up and
 * left), so the pile reads as a stack with the single book sitting at the
 * bottom-right.  A count badge sits in the cover's top-right corner. */
static void
draw_series_stack(int cx, int cy, int cw, int ch, int count)
{
    int step = 5;
    /* Back page sheet (furthest up-left). */
    FillArea(cx - 2 * step, cy - 2 * step, cw, ch, WHITE);
    DrawRect(cx - 2 * step, cy - 2 * step, cw, ch, BLACK);
    /* Front page sheet. */
    FillArea(cx - step, cy - step, cw, ch, WHITE);
    DrawRect(cx - step, cy - step, cw, ch, BLACK);
    /* Re-outline the cover so it reads as the top book of the stack. */
    DrawRect(cx, cy, cw, ch, BLACK);

    char badge[8];
    snprintf(badge, sizeof badge, "%d", count);
    ifont *bf = OpenFont(DEFAULTFONTB, 20, 0);
    if (bf != NULL) {
        SetFont(bf, WHITE);
        int bw = StringWidth(badge) + 12;
        int bh = 26;
        int bx = cx + cw - bw - 2;
        int by = cy + 2;
        FillArea(bx, by, bw, bh, BLACK);
        DrawString(bx + 6, by + 2, badge);
        CloseFont(bf);
    }
}

static void
draw_thumbnail(int x, int y, int w, int h, const ViewTile *vt, int vi)
{
    const Book *b = &g_state.books[vt->book_idx];
    int         selected = (vi == g_state.selected);

    FillArea(x, y, w, h, WHITE);
    /* List mode: one full-width row — small 2:3 cover on the left, title
     * and author stacked to its right.  Returns early so the grid card
     * layout below never runs for list rows. */
    if (g_state.view_mode == VIEW_LIST) {
        int pad = 8;
        int chh = h - 2 * pad;
        if (chh < 40)
            chh = 40;
        int cww = chh * 2 / 3;
        int cx = x + pad, cy = y + pad;
        FillArea(cx, cy, cww, chh, WHITE);
        blit_cover(cx, cy, cww, chh, b);
        if (vt->is_series)
            draw_series_stack(cx, cy, cww, chh, vt->series_count);
        if (selected) {
            DrawRect(x + 2, y + 2, w - 4, h - 4, BLACK);
            DrawRect(x + 3, y + 3, w - 6, h - 6, BLACK);
        }
        int tx0 = cx + cww + 16;
        int tw0 = (x + w - pad) - tx0;
        if (tw0 < 64)
            tw0 = 64;
        const char *label = vt->is_series ? vt->series_name : b->title;
        ifont      *f = OpenFont(DEFAULTFONTB, 30, 0);
        if (f != NULL) {
            SetFont(f, BLACK);
            char truncated[MAX_TITLE_LEN];
            snprintf(truncated, sizeof truncated, "%s", label);
            while (StringWidth(truncated) > tw0 && strlen(truncated) > 4)
                truncated[strlen(truncated) - 1] = '\0';
            DrawString(tx0, y + pad + 8, truncated);
            CloseFont(f);
        }
        if (!vt->is_series && b->author[0] != '\0') {
            ifont *af = OpenFont(DEFAULTFONT, 24, 0);
            if (af != NULL) {
                SetFont(af, DGRAY);
                char truncated[80];
                snprintf(truncated, sizeof truncated, "%s", b->author);
                while (StringWidth(truncated) > tw0 && strlen(truncated) > 4)
                    truncated[strlen(truncated) - 1] = '\0';
                DrawString(tx0, y + pad + 8 + 40, truncated);
                CloseFont(af);
            }
        }
        return;
    }

    int cx, cy, cw, ch;
    cover_rect(x, y, w, h, &cx, &cy, &cw, &ch);

    FillArea(cx, cy, cw, ch, WHITE);

    blit_cover(cx, cy, cw, ch, b);

    /* Series cards render as a stack of pages (see draw_series_stack). */
    if (vt->is_series)
        draw_series_stack(cx, cy, cw, ch, vt->series_count);

    /* Selection frame — 2px around cover on tap. */
    if (selected) {
        DrawRect(cx - 2, cy - 2, cw + 4, ch + 4, BLACK);
        DrawRect(cx - 1, cy - 1, cw + 2, ch + 2, BLACK);
    }

    /* Caption: series name for cards, title for books. */
    int         cap_y = cy + ch + 6;
    const char *label = vt->is_series ? vt->series_name : b->title;
    ifont      *f = OpenFont(DEFAULTFONTB, 22, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        char truncated[MAX_TITLE_LEN];
        snprintf(truncated, sizeof truncated, "%s", label);
        while (StringWidth(truncated) > w - 8 && strlen(truncated) > 4)
            truncated[strlen(truncated) - 1] = '\0';
        DrawString(x + 4, cap_y, truncated);
        CloseFont(f);
    }

    /* Second line: author for books, omitted for series cards. */
    if (!vt->is_series && b->author[0] != '\0') {
        ifont *af = OpenFont(DEFAULTFONT, 18, 0);
        if (af != NULL) {
            SetFont(af, DGRAY);
            char truncated[80];
            snprintf(truncated, sizeof truncated, "%s", b->author);
            while (StringWidth(truncated) > w - 8 && strlen(truncated) > 4)
                truncated[strlen(truncated) - 1] = '\0';
            DrawString(x + 4, cap_y + 24, truncated);
            CloseFont(af);
        }
    }
}

/* Rows of download entries that fit in the body once the progress bar is
 * reserved.  Drives the downloads page size so paging never lands on a
 * half-clipped row. */
static int
downloads_rows(void)
{
    int top, bot, cell_w, cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int usable = bot - top - DL_BAR_H - 8;
    int rows = usable / 96;
    return rows < 1 ? 1 : rows;
}

static int
downloads_pagesize(void)
{
    /* The downloads list is a single column, so one page is exactly the
     * number of rows that fit below the progress bar. */
    return downloads_rows();
}
/* Page count for the active tab: the library pages the cover grid, the
 * downloads tab pages the download list.  Always >= 1. */
static int
current_pages(void)
{
    int n, ps;
    if (g_state.tab == TAB_DOWNLOADS) {
        n = g_download_count;
        ps = downloads_pagesize();
    } else {
        n = g_view_count;
        ps = view_pagesize();
    }
    if (ps < 1)
        ps = 1;
    int pages = (n + ps - 1) / ps;
    return pages < 1 ? 1 : pages;
}

/* Single batch progress bar pinned to the top of the Downloads tab: one
 * bar for the whole open batch, filled by done/total, with a striped
 * overlay on the unfilled portion while anything is still in flight. */
static void
draw_dl_progress(int x, int y, int w)
{
    int total = 0, done = 0, failed = 0, active = 0;
    for (int i = 0; i < g_download_count; i++) {
        total++;
        if (g_downloads[i].state == 2)
            done++;
        else if (g_downloads[i].state == 3)
            failed++;
        if (g_downloads[i].state == 0 || g_downloads[i].state == 1)
            active++;
    }
    if (total <= 0)
        return;

    ifont *f = OpenFont(DEFAULTFONT, 22, 0);
    int    label_h = 26;
    char   label[48];
    if (active > 0)
        snprintf(label, sizeof label, i18n("dl.progress"), done, total);
    else
        snprintf(label, sizeof label, i18n("dl.complete"), done);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(x + 4, y + 2, label);
        CloseFont(f);
    }

    int bar_y = y + label_h;
    int bar_h = DL_BAR_H - label_h - 6;
    if (bar_h < 8)
        bar_h = 8;
    int bar_w = w - 2 * x;
    if (bar_w < 16)
        bar_w = 16;
    DrawRect(x, bar_y, bar_w, bar_h, BLACK);
    int settled = done + failed;
    int fill = (settled * bar_w) / total;
    if (fill > 2)
        FillArea(x + 1, bar_y + 1, fill - 2, bar_h - 2, BLACK);
    /* Striped "in progress" overlay across the unfinished portion. */
    if (active > 0) {
        for (int sx = x + 1 + fill; sx < x + bar_w - 1; sx += 6)
            DrawLine(sx, bar_y + 1, sx + 2, bar_y + bar_h - 2, DGRAY);
    }
}

static void
draw_downloads_tab(void)
{
    int top, bot, cell_w, cell_h;
    (void)cell_w;
    (void)cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int w = ScreenWidth();
    FillArea(0, top, w, bot - top, WHITE);
    DrawLine(0, top, w, top, BLACK);

    if (g_download_count == 0) {
        ifont *f = OpenFont(DEFAULTFONT, 30, 0);
        if (f != NULL) {
            SetFont(f, DGRAY);
            const char *msg = i18n("dl.empty");
            DrawString((w - StringWidth(msg)) / 2, top + 60, msg);
            CloseFont(f);
        }
        return;
    }

    /* Progress bar pinned to the top of the body; rows start below it. */
    draw_dl_progress(20, top + 4, w);

    /* Page the list — the pager below is wired to current_pages(). */
    int ps = downloads_pagesize();
    if (ps < 1)
        ps = 1;
    int pages = (g_download_count + ps - 1) / ps;
    if (g_state.page >= pages)
        g_state.page = pages - 1;
    if (g_state.page < 0)
        g_state.page = 0;
    int first = g_state.page * ps;
    int last = first + ps;
    if (last > g_download_count)
        last = g_download_count;
    LOG("[bookshelf] draw_downloads page=%d pages=%d count=%d\n",
        g_state.page,
        pages,
        g_download_count);

    int row_h = 96;
    int y = top + DL_BAR_H + 8;
    for (int i = first; i < last && y + row_h <= bot; i++) {
        const DownloadItem *d = &g_downloads[i];
        ifont              *tf = OpenFont(DEFAULTFONTB, 28, 0);
        if (tf != NULL) {
            SetFont(tf, BLACK);
            char trunc[MAX_TITLE_LEN];
            snprintf(trunc, sizeof trunc, "%s", d->title);
            int maxw = w - 260;
            while (StringWidth(trunc) > maxw && strlen(trunc) > 4)
                trunc[strlen(trunc) - 1] = '\0';
            DrawString(20, y + (row_h - 28) / 2 - 2, trunc);
            CloseFont(tf);
        }
        const char *st;
        int         scol;
        switch (d->state) {
        case 1:
            st = i18n("dl.in_progress");
            scol = BLACK;
            break;
        case 2:
            st = i18n("dl.done");
            scol = DGRAY;
            break;
        case 3:
            st = i18n("dl.failed");
            scol = BLACK;
            break;
        default:
            st = i18n("dl.queued");
            scol = DGRAY;
            break;
        }
        ifont *sf = OpenFont(DEFAULTFONT, 24, 0);
        if (sf != NULL) {
            SetFont(sf, scol);
            DrawString(w - 20 - StringWidth(st), y + (row_h - 24) / 2 - 2, st);
            CloseFont(sf);
        }
        DrawLine(20, y + row_h - 1, w - 20, y + row_h - 1, LGRAY);
        y += row_h;
    }
}

/* Repaint the whole shelf (top bar, search, tabs, body, pager) in the
 * current tab.  Centralises the sequence every state change needs. */
static void
redraw_shelf(void)
{
    if (g_state.launcher_open) {
        draw_overlay_launcher();
        FullUpdate();
        return;
    }
    FillArea(0, g_state.panel_h, ScreenWidth(), ScreenHeight() - g_state.panel_h, WHITE);
    draw_top_bar();
    draw_search_row();
    draw_tab_row();
    if (g_state.tab == TAB_DOWNLOADS)
        draw_downloads_tab();
    else
        draw_grid();
    draw_pager();
    FullUpdate();
}

static void
draw_grid(void)
{
    /* Layout: [system panel] [our top bar] [our search row] [grid] [pager].
     * The system panel renders at the TOP of the screen (PANEL_NO_FB_OFFSET
     * flag), occupying rows [0, panel_h).  Everything we draw is offset
     * below it; the pager sits at the very bottom with no reservation.
     */
    int top, bot, cell_w, cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    /* Clear the grid area first so cells from a previous page don't
     * bleed through.  We do this every redraw, not just on page change,
     * so partial updates stay simple.
     */
    FillArea(0, top, ScreenWidth(), bot - top, WHITE);
    DrawLine(0, top, ScreenWidth(), top, BLACK);
    LOG("[bookshelf] draw_grid view=%d page=%d cell=%dx%d top=%d bot=%d\n",
        g_view_count,
        g_state.page,
        cell_w,
        cell_h,
        top,
        bot);

    int cols = view_cols();
    int rows = view_rows();
    int page_start = g_state.page * view_pagesize();
    int drawn = 0;
    for (int row = 0; row < rows; row++) {
        for (int col = 0; col < cols; col++) {
            int idx = page_start + drawn;
            if (idx >= g_view_count)
                goto done;
            int tx = 8 + col * cell_w;
            int ty = top + 4 + row * cell_h;
            int tw = cell_w - 8;
            int th = cell_h - 6;
            draw_thumbnail(tx, ty, tw, th, &g_view[idx], idx);
            drawn++;
        }
    }
done:
    cover_schedule_next();
}

/* Fetch one not-yet-loaded visible cover per tick, then blit just that
 * tile.  Running on the event loop keeps the SDK single-threaded; the
 * blocking download is short (cached PNGs over the loopback link). */
static void
cover_tick(void *ctx)
{
    (void)ctx;
    LOG("[bookshelf] cover_tick ENTER page=%d view=%d armed->0\n", g_state.page, g_view_count);
    g_cover_armed = 0;

    int top, bot, cell_w, cell_h;
    (void)top;
    (void)bot;
    (void)cell_w;
    (void)cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int ps = view_pagesize();
    int page_start = g_state.page * ps;
    int lim = page_start + ps;
    if (lim > g_view_count)
        lim = g_view_count;

    int target = -1;
    for (int i = page_start; i < lim; i++) {
        CoverSlot *s = cover_slot(g_state.books[g_view[i].book_idx].id, 1);
        if (s != NULL && s->state == 0) {
            target = i;
            break;
        }
    }
    if (target < 0)
        return; /* nothing pending on this page */

    CoverSlot *s = cover_slot(g_state.books[g_view[target].book_idx].id, 1);
    LOG("[bookshelf] cover_tick target=%d id=%s slot=%p\n",
        target,
        g_state.books[g_view[target].book_idx].id,
        (void *)s);
    s->state = 1;

    char url[MAX_URL_LEN + 128];
    snprintf(url,
             sizeof url,
             "%s/api/v1/books/%s/cover?access_token=%s",
             g_state.api_base,
             g_state.books[g_view[target].book_idx].id,
             g_state.api_token);

    int rsize = 0;
    LOG("[bookshelf] cover_tick downloading url=%s\n", url);
    char *data = QuickDownload(url, &rsize, HTTP_TIMEOUT);
    LOG("[bookshelf] cover_tick downloaded data=%p rsize=%d\n", (void *)data, rsize);
    ibitmap *bmp = NULL;
    if (data != NULL && rsize > 8) {
        FILE *f = fopen(COVER_TMP, "wb");
        if (f != NULL) {
            fwrite(data, 1, (size_t)rsize, f);
            fclose(f);
            LOG("[bookshelf] cover_tick LoadPNGStretch begin\n");
            bmp = LoadPNGStretch(COVER_TMP, 240, 360, 0, 0);
            LOG("[bookshelf] cover_tick LoadPNGStretch done bmp=%p\n", (void *)bmp);
        }
    }
    if (data != NULL) {
        LOG("[bookshelf] cover_tick free(data) begin\n");
        free(data);
        LOG("[bookshelf] cover_tick free(data) done\n");
    }

    if (bmp != NULL) {
        if (s->cover_bmp) {
            LOG("[bookshelf] cover_tick free(old cover_bmp) begin\n");
            free(s->cover_bmp);
            LOG("[bookshelf] cover_tick free(old cover_bmp) done\n");
        }
        s->cover_bmp = bmp;
        s->state = 2;
    } else {
        s->state = 3;
    }
    /* The cached bitmap is stored on the slot regardless; only the
     * on-screen blit is skipped while a modal owns the framebuffer, so a
     * single-tile PartialUpdate can't punch a hole through the overlay's
     * dim mask (the full redraw on close then shows the now-cached cover). */
    int modal = g_state.ctx_open || g_state.menu_open || g_state.more_open || g_state.settings_open;
    LOG("[bookshelf] cover_tick blit begin modal=%d\n", modal);

    int tx, ty, tw, th;
    if (!modal && tile_rect_for_index(target, &tx, &ty, &tw, &th)) {
        FillArea(tx, ty, tw, th, WHITE);
        draw_thumbnail(tx, ty, tw, th, &g_view[target], target);
        PartialUpdate(tx, ty, tw, th);
    }
    LOG("[bookshelf] cover_tick blit done, scheduling next\n");
    cover_schedule_next();
    LOG("[bookshelf] cover_tick EXIT\n");
}

static void
draw_pager(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    /* Pager sits at the very bottom; the system panel is at the top. */
    int y = h - PAGER_H;
    FillArea(0, y, w, PAGER_H, WHITE);
    DrawLine(0, y, w, y, BLACK);

    int pages = current_pages();
    if (g_state.page >= pages)
        g_state.page = pages - 1;
    if (g_state.page < 0)
        g_state.page = 0;

    ifont *f = OpenFont(DEFAULTFONTB, 28, 0);
    if (f == NULL)
        return;

    char info[32];
    snprintf(info, sizeof info, i18n("pager.info"), g_state.page + 1, pages);
    SetFont(f, BLACK);
    draw_text_centered(f, w / 2, y + (PAGER_H - 28) / 2 - 2, info, BLACK);

    /* Prev button — 96×64 for e-ink touch target */
    if (g_state.page > 0)
        draw_button(12, y + (PAGER_H - 64) / 2, 96, 64, 0, i18n("pager.prev"), 28, 0);

    /* Next button — 96×64 for e-ink touch target */
    if (g_state.page + 1 < pages)
        draw_button(w - 108, y + (PAGER_H - 64) / 2, 96, 64, 0, i18n("pager.next"), 28, 0);
    CloseFont(f);
}

static void
draw_overlay_menu(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    FillArea(0, g_state.panel_h, w, h - g_state.panel_h, BLACK);
    int pw = w * 3 / 4;
    FillArea(0, g_state.panel_h, pw, h - g_state.panel_h, WHITE);
    DrawLine(pw, g_state.panel_h, pw, h, BLACK);

    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(24, g_state.panel_h + 32, i18n("action.menu"));
        CloseFont(f);
    }

    const char *labels[] = {
        "group.all",
        "group.author",
        "group.series",
        "group.recent",
    };
    int n = (int)(sizeof labels / sizeof labels[0]);
    int y0 = g_state.panel_h + 96;
    int item_h = 88;
    for (int i = 0; i < n; i++) {
        int sel = (i == (int)g_state.group);
        FillArea(12, y0 + i * item_h, pw - 24, item_h - 12, sel ? BLACK : WHITE);
        ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
        if (tf != NULL) {
            SetFont(tf, sel ? WHITE : BLACK);
            DrawString(32, y0 + i * item_h + (item_h - 28) / 2 - 2, i18n(labels[i]));
            CloseFont(tf);
        }
    }
}

static void
draw_overlay_more(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    FillArea(0, g_state.panel_h, w, h - g_state.panel_h, BLACK);
    int pw = w * 3 / 4;
    int px = w - pw;
    FillArea(px, g_state.panel_h, pw, h - g_state.panel_h, WHITE);
    DrawLine(px, g_state.panel_h, px, h, BLACK);

    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(px + 24, g_state.panel_h + 32, i18n("action.more"));
        CloseFont(f);
    }
    const char *labels[] = {
        "action.sync",
        "sort.title_az",
        "sort.title_za",
        "sort.author",
        "sort.series",
        "sort.recent",
        "view.grid",
        "view.list",
        "action.download_all",
        "action.settings",
        "action.system",
        "action.apps",
    };
    int n = (int)(sizeof labels / sizeof labels[0]);
    int y0 = g_state.panel_h + MORE_Y0;
    for (int i = 0; i < n; i++) {
        int sel = 0;
        if (i == 0 && g_state.sync_state == 1)
            sel = 1;
        if (i > 0 && i <= 5 && (i - 1) == (int)g_state.sort)
            sel = 1;
        if (i == MORE_GRID_IDX && g_state.view_mode == VIEW_GRID)
            sel = 1;
        if (i == MORE_LIST_IDX && g_state.view_mode == VIEW_LIST)
            sel = 1;
        FillArea(px + 12, y0 + i * MORE_ITEM_H, pw - 24, MORE_ITEM_H - 12, sel ? BLACK : WHITE);
        ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
        if (tf != NULL) {
            SetFont(tf, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * MORE_ITEM_H + (MORE_ITEM_H - 28) / 2 - 2, i18n(labels[i]));
            CloseFont(tf);
        }
    }
}

static void
draw_status_line(void)
{
    /* Currently unused — status is shown via the search row placeholder
     * and via sync-button feedback.  Kept as an extension point.
     */
}

/* ── settings overlay ────────────────────────────────────────────────── */

static void draw_overlay_settings(void);

/* Which settings row currently owns the on-screen keyboard:
 * 0 = none, 1 = API host, 2 = API key. */
static int g_settings_edit = 0;

/* Scratch buffer the keyboard edits; committed on close. */
static char g_settings_kb_buf[260];

static void
settings_keyboard_handler(char *buffer)
{
    const char *val = buffer ? buffer : "";
    if (g_settings_edit == 1) {
        /* Normalise a bare host[:port] into a full http:// URL so the
         * endpoint builder always gets a scheme. */
        if (strncmp(val, "http://", 7) != 0 && strncmp(val, "https://", 8) != 0) {
            char tmp[260];
            snprintf(tmp, sizeof tmp, "http://%s", val);
            snprintf(g_state.api_base, sizeof g_state.api_base, "%s", tmp);
        } else {
            snprintf(g_state.api_base, sizeof g_state.api_base, "%s", val);
        }
    } else if (g_settings_edit == 2) {
        snprintf(g_state.api_token, sizeof g_state.api_token, "%s", val);
    }
    g_settings_edit = 0;
    draw_overlay_settings();
    /* The on-screen keyboard draws full-screen and wipes the top status
     * strip; re-stamp it before the flush so the panel survives the commit
     * redraw (draw_overlay_settings clears only from panel_h). */
    iv_update_panel(0);
    FullUpdate();
}

/* Full-screen settings page.  Three editable rows (API host, API key,
 * reader app) plus Save and Back buttons.  The API host / key rows open
 * the on-screen keyboard; the reader row cycles through Auto plus every
 * detected reader.  Generous row heights keep the targets comfortable on
 * the 300 DPI e-ink panel. */
#define SETTINGS_ROW_H 120
#define SETTINGS_BTN_H 96

static const char *
settings_reader_label(void)
{
    if (g_state.reader_pref > 0 && g_state.reader_pref <= g_reader_count)
        return g_readers[g_state.reader_pref - 1].label;
    return i18n("settings.reader_auto");
}

static void
settings_draw_row(int y, const char *label, const char *value, int editing)
{
    int w = ScreenWidth();
    int mx = 32; /* left/right margin */
    FillArea(mx, y, w - 2 * mx, SETTINGS_ROW_H - 12, WHITE);
    DrawRect(mx, y, w - 2 * mx, SETTINGS_ROW_H - 12, BLACK);
    if (editing)
        FillArea(mx + 2, y + 2, w - 2 * mx - 4, SETTINGS_ROW_H - 16, BLACK);

    ifont *lf = OpenFont(DEFAULTFONTB, 26, 0);
    if (lf != NULL) {
        SetFont(lf, editing ? WHITE : BLACK);
        DrawString(mx + 16, y + 12, label);
        CloseFont(lf);
    }
    ifont *vf = OpenFont(DEFAULTFONT, 30, 0);
    if (vf != NULL) {
        SetFont(vf, editing ? WHITE : BLACK);
        DrawString(mx + 16, y + 52, value);
        CloseFont(vf);
    }
}

static void
settings_draw_button(int y, const char *label, int filled)
{
    int w = ScreenWidth();
    int mx = 32;
    FillArea(mx, y, w - 2 * mx, SETTINGS_BTN_H - 12, filled ? BLACK : WHITE);
    DrawRect(mx, y, w - 2 * mx, SETTINGS_BTN_H - 12, BLACK);
    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, filled ? WHITE : BLACK);
        int tw = StringWidth(label);
        DrawString((w - tw) / 2, y + (SETTINGS_BTN_H - 12 - 32) / 2, label);
        CloseFont(f);
    }
}

static void
draw_overlay_settings(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    FillArea(0, g_state.panel_h, w, h - g_state.panel_h, WHITE);

    ifont *tf = OpenFont(DEFAULTFONTB, 40, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(32, g_state.panel_h + 28, i18n("settings.title"));
        CloseFont(tf);
    }
    DrawLine(0, g_state.panel_h + 92, w, g_state.panel_h + 92, BLACK);

    int y = g_state.panel_h + 112;
    settings_draw_row(y, i18n("settings.api_host"), g_state.api_base, g_settings_edit == 1);
    y += SETTINGS_ROW_H;
    settings_draw_row(y, i18n("settings.api_key"), g_state.api_token, g_settings_edit == 2);
    y += SETTINGS_ROW_H;
    settings_draw_row(y, i18n("settings.reader"), settings_reader_label(), 0);
    y += SETTINGS_ROW_H + 24;
    settings_draw_button(y, i18n("settings.save"), 1);
    y += SETTINGS_BTN_H;
    settings_draw_button(y, i18n("settings.back"), 0);
}

/* ── hit-testing ─────────────────────────────────────────────────────── */

static int
hit_top_bar(int x, int y)
{
    int bar_top = g_state.panel_h;
    int bar_bot = bar_top + TOP_BAR_H;
    if (y < bar_top || y >= bar_bot)
        return -1;
    int w = ScreenWidth();
    /* Left "home" button — 96×96 region, padded 8 px on the left. */
    if (x >= 8 && x < 8 + 96)
        return 1;
    /* Right "menu" button — 96×96 region, padded 8 px on the right. */
    if (x >= w - 96 - 8 && x < w - 8)
        return 3;
    return -1;
}

static int
hit_search(int x, int y)
{
    int row_top = g_state.panel_h + TOP_BAR_H;
    int row_bot = row_top + SEARCH_ROW_H;
    if (y < row_top || y >= row_bot)
        return -1;
    int w = ScreenWidth();
    int tx = 64, tw = w - 128;
    int ty = row_top + 10;
    int th = SEARCH_ROW_H - 20;
    if (x < tx || x >= tx + tw)
        return -1;
    if (y < ty || y >= ty + th)
        return -1;
    return 1;
}

/* Returns 0 for the Library tab, 1 for the Downloads tab, -1 elsewhere. */
static int
hit_tab_row(int x, int y)
{
    int row_top = g_state.panel_h + TOP_BAR_H + SEARCH_ROW_H;
    int row_bot = row_top + TAB_ROW_H;
    if (y < row_top || y >= row_bot)
        return -1;
    int w = ScreenWidth();
    return (x < w / 2) ? 0 : 1;
}

static int
hit_thumbnail(int x, int y)
{
    int top, bot, cell_w, cell_h;
    grid_geom(&top, &bot, &cell_w, &cell_h);
    int cols = view_cols();
    int rows = view_rows();
    int page_start = g_state.page * view_pagesize();
    for (int row = 0; row < rows; row++) {
        for (int col = 0; col < cols; col++) {
            int idx = page_start + row * cols + col;
            if (idx >= g_view_count)
                return -1;
            int tx = 8 + col * cell_w;
            int ty = top + 4 + row * cell_h;
            int tw = cell_w - 8;
            int th = cell_h - 6;
            if (x >= tx && x < tx + tw && y >= ty && y < ty + th)
                return idx;
        }
    }
    return -1;
}

static int
hit_pager(int x, int y)
{
    int h = ScreenHeight();
    int y0 = h - PAGER_H;
    if (y < y0 || y >= y0 + PAGER_H)
        return 0;
    int w = ScreenWidth();
    /* Prev — 96px wide starting at x=12 */
    if (g_state.page > 0 && x >= 12 && x < 12 + 96)
        return -1;
    /* Next — 96px wide ending at x=w-12 */
    int pages = current_pages();
    if (g_state.page + 1 < pages && x >= w - 108 && x < w - 12)
        return -2;
    return 0;
}

/* ── tap handlers ────────────────────────────────────────────────────── */

static void
on_tap_overlay_menu(int x, int y)
{
    int y0 = 96, item_h = 88;
    int pw = ScreenWidth() * 3 / 4;
    if (x < 0 || x >= pw) {
        g_state.menu_open = 0;
        return;
    }
    y -= g_state.panel_h;
    for (int i = 0; i < 4; i++) {
        if (y >= y0 + i * item_h && y < y0 + i * item_h + item_h) {
            g_state.group = (GroupMode)i;
            g_drilled_series[0] = '\0';
            g_state.menu_open = 0;
            do_sync();
        }
    }
    g_state.menu_open = 0;
}

static void
on_tap_overlay_more(int x, int y)
{
    int pw = ScreenWidth() * 3 / 4;
    int px = ScreenWidth() - pw;
    if (x < px || x >= ScreenWidth()) {
        g_state.more_open = 0;
        return;
    }
    y -= g_state.panel_h;
    if (y >= MORE_Y0 && y < MORE_Y0 + MORE_ITEM_H) {
        g_state.more_open = 0;
        do_sync();
        return;
    }
    /* Settings row opens the full-screen settings page. */
    if (y >= MORE_Y0 + MORE_SETTINGS_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_SETTINGS_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        g_state.settings_open = 1;
        g_settings_edit = 0;
        draw_overlay_settings();
        FullUpdate();
        return;
    }
    /* System menu row launches the firmware's control panel dropdown. */
    if (y >= MORE_Y0 + MORE_SYSTEM_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_SYSTEM_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        LOG("[bookshelf] opening system control panel\n");
        OpenControlPanel(NULL);
        return;
    }
    /* Applications row opens the in-app launcher overlay. */
    if (y >= MORE_Y0 + MORE_APPS_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_APPS_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        launcher_open_set();
        return;
    }
    /* Download-all row queues every book in the library and jumps to the
     * Downloads tab so the user watches the queue drain. */
    if (y >= MORE_Y0 + MORE_DLALL_IDX * MORE_ITEM_H &&
        y < MORE_Y0 + (MORE_DLALL_IDX + 1) * MORE_ITEM_H) {
        g_state.more_open = 0;
        for (int i = 0; i < g_lib_count; i++)
            enqueue_download(&g_lib[i]);
        LOG("[bookshelf] download-all queued=%d\n", g_lib_count);
        g_state.tab = TAB_DOWNLOADS;
        redraw_shelf();
        return;
    }
    for (int i = 1; i < MORE_DLALL_IDX; i++) {
        if (y >= MORE_Y0 + i * MORE_ITEM_H && y < MORE_Y0 + i * MORE_ITEM_H + MORE_ITEM_H) {
            g_state.more_open = 0;
            if (i == MORE_GRID_IDX) {
                g_state.view_mode = VIEW_GRID;
                g_state.page = 0;
            } else if (i == MORE_LIST_IDX) {
                g_state.view_mode = VIEW_LIST;
                g_state.page = 0;
            } else {
                /* i = 1..5 → the five sort modes (title↑/↓, author,
                 * series, recent). */
                g_state.sort = (SortMode)(i - 1);
                apply_filter_and_sort();
            }
            return;
        }
    }
    g_state.more_open = 0;
}

/* Close the settings overlay and repaint the shelf beneath it. */
static void
settings_close(void)
{
    g_state.settings_open = 0;
    g_settings_edit = 0;
    redraw_shelf();
}

/* Persist settings, rebuild the endpoint URLs from the (possibly edited)
 * api_base / api_token, then re-sync so the shelf reflects the new
 * server immediately. */
static void
settings_apply(void)
{
    save_config_file();
    build_endpoint_urls();
    g_state.settings_open = 0;
    g_settings_edit = 0;
    do_sync();
    redraw_shelf();
}

static void
on_tap_overlay_settings(int x, int y)
{
    (void)x; /* rows span the full content width; only y matters */
    y -= g_state.panel_h;

    int y_row1 = 112;
    int y_row2 = y_row1 + SETTINGS_ROW_H;
    int y_row3 = y_row2 + SETTINGS_ROW_H;
    int y_save = y_row3 + SETTINGS_ROW_H + 24;
    int y_back = y_save + SETTINGS_BTN_H;

    if (y >= y_row1 && y < y_row1 + SETTINGS_ROW_H - 12) {
        g_settings_edit = 1;
        snprintf(g_settings_kb_buf, sizeof g_settings_kb_buf, "%s", g_state.api_base);
        draw_overlay_settings();
        FullUpdate();
        OpenKeyboard(i18n("settings.api_host"),
                     g_settings_kb_buf,
                     sizeof g_settings_kb_buf - 1,
                     0,
                     settings_keyboard_handler);
        return;
    }
    if (y >= y_row2 && y < y_row2 + SETTINGS_ROW_H - 12) {
        g_settings_edit = 2;
        snprintf(g_settings_kb_buf, sizeof g_settings_kb_buf, "%s", g_state.api_token);
        draw_overlay_settings();
        FullUpdate();
        OpenKeyboard(i18n("settings.api_key"),
                     g_settings_kb_buf,
                     sizeof g_settings_kb_buf - 1,
                     0,
                     settings_keyboard_handler);
        return;
    }
    if (y >= y_row3 && y < y_row3 + SETTINGS_ROW_H - 12) {
        /* Cycle Auto → reader[0] → reader[1] → … → Auto. */
        g_state.reader_pref = (g_state.reader_pref + 1) % (g_reader_count + 1);
        draw_overlay_settings();
        FullUpdate();
        return;
    }
    if (y >= y_save && y < y_save + SETTINGS_BTN_H - 12) {
        settings_apply();
        return;
    }
    if (y >= y_back && y < y_back + SETTINGS_BTN_H - 12) {
        settings_close();
        return;
    }
}

/* -- app launcher ------------------------------------------------------- *
 * Reproduces the firmware's grouped application grid (the "Apps" screen
 * the original desktop renders from view.json + apps_db.json).  Since
 * bookshelf.app *is* the home-screen replacement, the original grid is
 * gone — this overlay restores it, resolving conditional visibility for
 * the current device profile (Era: touch + audio + en/WW + stock partner)
 * so the grid matches what the real device shows (e.g. Snake hidden on a
 * touch panel).  Tapping a tile launches the app via NewTaskEx. */

/* -- minimal JSON scanner ----------------------------------------------- */

static const char *
js_skip_ws(const char *p)
{
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')
        p++;
    return p;
}

static const char *
js_skip_value(const char *p)
{
    p = js_skip_ws(p);
    if (*p == '"') {
        p++;
        while (*p && *p != '"') {
            if (*p == '\\')
                p++;
            p++;
        }
        return *p == '"' ? p + 1 : NULL;
    }
    if (*p == '{' || *p == '[') {
        int depth = 1;
        p++;
        while (*p && depth > 0) {
            if (*p == '"') {
                p = js_skip_value(p);
                if (!p)
                    return NULL;
                continue;
            }
            if (*p == '{' || *p == '[')
                depth++;
            else if (*p == '}' || *p == ']')
                depth--;
            p++;
        }
        return depth == 0 ? p : NULL;
    }
    while (*p && *p != ',' && *p != '}' && *p != ']' && *p != ' ' && *p != '\n' && *p != '\r' &&
           *p != '\t')
        p++;
    return p;
}

static void
js_copy_string(const char *p, char *out, size_t cap)
{
    if (cap == 0)
        return;
    if (*p != '"') {
        out[0] = '\0';
        return;
    }
    p++;
    size_t i = 0;
    while (*p && *p != '"' && i + 1 < cap) {
        if (*p == '\\' && p[1])
            p++;
        out[i++] = *p++;
    }
    out[i] = '\0';
}

static const char *
js_object_body(const char *p)
{
    p = js_skip_ws(p);
    return *p == '{' ? p + 1 : NULL;
}

static const char *
js_find_member(const char *p, const char *key)
{
    size_t klen = strlen(key);
    while (*p) {
        p = js_skip_ws(p);
        if (*p == '}')
            return NULL;
        if (*p != '"')
            return NULL;
        const char *ks = ++p;
        while (*p && *p != '"') {
            if (*p == '\\')
                p++;
            p++;
        }
        size_t kl = (size_t)(p - ks);
        if (*p == '"')
            p++;
        p = js_skip_ws(p);
        if (*p == ':')
            p++;
        p = js_skip_ws(p);
        if (kl == klen && memcmp(ks, key, klen) == 0)
            return p;
        p = js_skip_value(p);
        if (!p)
            return NULL;
        p = js_skip_ws(p);
        if (*p == ',')
            p++;
    }
    return NULL;
}

/* -- device profile for conditional resolution -------------------------- */

typedef struct {
    const char *device;
    const char *partner;
    const char *has_audio;
    const char *has_cloud;
    const char *language;
    const char *localization;
} LcProfile;

static const LcProfile g_lcprof = {"all", "pocketbook", "true", "false", "en", "WW"};

static const char *const lc_dims[] = {
    "device",
    "partner",
    "has_audio",
    "has_cloud",
    "language",
    "localization",
    "globalcfg",
};
#define LC_NDIMS ((int)(sizeof lc_dims / sizeof lc_dims[0]))

static const char *
lc_prof_val(const char *dim)
{
    if (strcmp(dim, "device") == 0)
        return g_lcprof.device;
    if (strcmp(dim, "partner") == 0)
        return g_lcprof.partner;
    if (strcmp(dim, "has_audio") == 0)
        return g_lcprof.has_audio;
    if (strcmp(dim, "has_cloud") == 0)
        return g_lcprof.has_cloud;
    if (strcmp(dim, "language") == 0)
        return g_lcprof.language;
    if (strcmp(dim, "localization") == 0)
        return g_lcprof.localization;
    return NULL;
}

static const char *
lc_pick_key(const char *obj_body, const char *want)
{
    static char first[32];
    first[0] = '\0';
    int         all_present = 0, def_present = 0;
    const char *p = obj_body;
    while (*p) {
        p = js_skip_ws(p);
        if (*p == '}' || *p != '"')
            break;
        const char *ks = ++p;
        while (*p && *p != '"') {
            if (*p == '\\')
                p++;
            p++;
        }
        size_t kl = (size_t)(p - ks);
        if (*p == '"')
            p++;
        if (first[0] == '\0' && kl < sizeof first) {
            memcpy(first, ks, kl);
            first[kl] = '\0';
        }
        if (want && kl == strlen(want) && memcmp(ks, want, kl) == 0)
            return want;
        if (kl == 3 && memcmp(ks, "all", 3) == 0)
            all_present = 1;
        if (kl == 7 && memcmp(ks, "default", 7) == 0)
            def_present = 1;
        p = js_skip_ws(p);
        if (*p == ':')
            p++;
        p = js_skip_value(p);
        if (!p)
            break;
        p = js_skip_ws(p);
        if (*p == ',')
            p++;
    }
    if (all_present)
        return "all";
    if (def_present)
        return "default";
    return first[0] ? first : NULL;
}

static void lc_resolve(const char *p, const char *cur_dim, char *out, size_t cap);

static void
lc_resolve(const char *p, const char *cur_dim, char *out, size_t cap)
{
    if (cap == 0)
        return;
    out[0] = '\0';
    p = js_skip_ws(p);
    if (!p || !*p)
        return;
    if (*p == '"') {
        js_copy_string(p, out, cap);
        return;
    }
    if (*p != '{')
        return;
    const char *body = p + 1;
    for (int d = 0; d < LC_NDIMS; d++) {
        const char *vp = js_find_member(body, lc_dims[d]);
        if (vp) {
            lc_resolve(vp, lc_dims[d], out, cap);
            return;
        }
    }
    if (!cur_dim) {
        const char *k = lc_pick_key(body, NULL);
        if (k) {
            const char *vp = js_find_member(body, k);
            if (vp)
                lc_resolve(vp, cur_dim, out, cap);
        }
        return;
    }
    if (strcmp(cur_dim, "globalcfg") == 0) {
        const char *p2 = body;
        while (*p2) {
            p2 = js_skip_ws(p2);
            if (*p2 == '}' || *p2 != '"')
                break;
            const char *ks = ++p2;
            while (*p2 && *p2 != '"') {
                if (*p2 == '\\')
                    p2++;
                p2++;
            }
            if (*p2 == '"')
                p2++;
            p2 = js_skip_ws(p2);
            if (*p2 == ':')
                p2++;
            p2 = js_skip_ws(p2);
            const char *inner = js_skip_ws(p2);
            if (*inner == '{') {
                const char *defp = js_find_member(inner + 1, "default");
                if (defp) {
                    lc_resolve(defp, cur_dim, out, cap);
                    return;
                }
            }
            p2 = js_skip_value(p2);
            if (!p2)
                break;
            p2 = js_skip_ws(p2);
            if (*p2 == ',')
                p2++;
        }
        return;
    }
    const char *want = lc_prof_val(cur_dim);
    const char *k = lc_pick_key(body, want);
    if (k) {
        const char *vp = js_find_member(body, k);
        if (vp)
            lc_resolve(vp, cur_dim, out, cap);
    }
}

static int
lc_resolve_bool(const char *p)
{
    char buf[8];
    lc_resolve(p, NULL, buf, sizeof buf);
    return buf[0] != '0';
}

/* -- file reader -------------------------------------------------------- */

static char *
read_text_file(const char *path)
{
    FILE *f = fopen(path, "rb");
    if (!f)
        return NULL;
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz <= 0 || sz > 256 * 1024) {
        fclose(f);
        return NULL;
    }
    char *buf = malloc((size_t)sz + 1);
    if (!buf) {
        fclose(f);
        return NULL;
    }
    size_t nr = fread(buf, 1, (size_t)sz, f);
    fclose(f);
    buf[nr] = '\0';
    return buf;
}

/* -- token translation -------------------------------------------------- */

static const char *
lc_token_en(const char *tok)
{
    static const struct {
        const char *k, *v;
    } tab[] = {
        {"@Audio_books", "Audio books"},
        {"@Browser", "Browser"},
        {"@BookStoreShortName", "Book Store"},
        {"@Legimi", "Legimi"},
        {"@Calc", "Calculator"},
        {"@Calendar", "Calendar"},
        {"@Chess", "Chess"},
        {"@coloring", "Coloring"},
        {"@Sudoku", "Sudoku"},
        {"@digital_frame", "Digital Frame"},
        {"@Gallery", "Gallery"},
        {"@Library", "Library"},
        {"@Notes", "Notes"},
        {"@Onleihe", "Onleihe"},
        {"@Audio_player", "Music"},
        {"@Pocketnews", "RSS News"},
        {"@Settings", "Settings"},
        {"@Snake", "Snake"},
        {"@Scribble", "Scribble"},
        {"@SendToPocketbook", "Send to PB"},
        {"@Dictionary", "Dictionary"},
        {"@Dropbox", "Dropbox"},
        {"@Empik_store", "Empik"},
        {"@Klondike", "Solitaire"},
        {"@Kosynka", "Solitaire"},
        {"@PBOnleiheLibrary", "Onleihe"},
        {"@General", "General"},
        {"@Games", "Games"},
    };
    for (size_t i = 0; i < sizeof tab / sizeof tab[0]; i++) {
        if (strcmp(tok, tab[i].k) == 0)
            return tab[i].v;
    }
    return NULL;
}

static void
lc_translate(const char *raw, char *out, size_t cap)
{
    if (!raw || !*raw || cap == 0) {
        if (cap)
            out[0] = '\0';
        return;
    }
    if (raw[0] == '@') {
        const char *en = lc_token_en(raw);
        if (en) {
            snprintf(out, cap, "%s", en);
            return;
        }
        raw++;
    }
    size_t j = 0;
    int    cap_next = 1;
    for (size_t i = 0; raw[i] && j + 1 < cap; i++) {
        char c = raw[i];
        if (c == '_') {
            out[j++] = ' ';
            cap_next = 1;
        } else if (cap_next && c >= 'a' && c <= 'z') {
            out[j++] = (char)(c - 32);
            cap_next = 0;
        } else {
            out[j++] = c;
            cap_next = 0;
        }
    }
    out[j] = '\0';
}

/* -- launcher data + layout --------------------------------------------- */

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

static LauncherItem g_launcher_items[LAUNCHER_MAX_ITEMS];
static int          g_launcher_count;
static int          g_launcher_pages;
static int          g_launcher_built;

#define LAUNCHER_HEADER_H 104
#define LAUNCHER_PAGER_H  96
#define LAUNCHER_COLS     3
#define LAUNCHER_GROUP_H  64
#define LAUNCHER_CELL_H   232
#define LAUNCHER_ICON_SZ  120
#define LAUNCHER_MARGIN   16

static void
launcher_layout(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    int body_top = g_state.panel_h + LAUNCHER_HEADER_H;
    int body_bot = h - LAUNCHER_PAGER_H;
    int cell_w = (w - 2 * LAUNCHER_MARGIN) / LAUNCHER_COLS;
    int page = 0;
    int col = 0;
    int y = body_top;

    for (int i = 0; i < g_launcher_count; i++) {
        LauncherItem *it = &g_launcher_items[i];
        if (it->kind == 0) {
            if (y + LAUNCHER_GROUP_H > body_bot) {
                page++;
                col = 0;
                y = body_top;
            }
            it->page = page;
            it->x = LAUNCHER_MARGIN;
            it->y = y;
            it->w = w - 2 * LAUNCHER_MARGIN;
            it->h = LAUNCHER_GROUP_H;
            y += LAUNCHER_GROUP_H;
            col = 0;
        } else {
            if (col >= LAUNCHER_COLS) {
                col = 0;
                y += LAUNCHER_CELL_H;
            }
            if (y + LAUNCHER_CELL_H > body_bot) {
                page++;
                col = 0;
                y = body_top;
            }
            it->page = page;
            it->x = LAUNCHER_MARGIN + col * cell_w;
            it->y = y;
            it->w = cell_w;
            it->h = LAUNCHER_CELL_H;
            col++;
        }
    }
    g_launcher_pages = page + 1;
}

static void
launcher_add_app(const char *apps_body, const char *id)
{
    if (g_launcher_count >= LAUNCHER_MAX_ITEMS)
        return;
    const char *def = js_find_member(apps_body, id);
    if (!def)
        return;
    const char *def_body = js_object_body(def);
    if (!def_body)
        return;
    const char *vis = js_find_member(def_body, "visible");
    if (vis && !lc_resolve_bool(vis))
        return;
    LauncherItem *it = &g_launcher_items[g_launcher_count];
    memset(it, 0, sizeof *it);
    it->kind = 1;
    const char *tp = js_find_member(def_body, "title");
    if (tp) {
        char raw[64];
        lc_resolve(tp, NULL, raw, sizeof raw);
        lc_translate(raw, it->text, sizeof it->text);
    }
    if (!it->text[0])
        snprintf(it->text, sizeof it->text, "%s", id);
    const char *pp = js_find_member(def_body, "path");
    if (pp)
        lc_resolve(pp, NULL, it->path, sizeof it->path);
    const char *ip = js_find_member(def_body, "icon");
    if (ip)
        lc_resolve(ip, NULL, it->icon, sizeof it->icon);
    const char *par = js_find_member(def_body, "params");
    if (!par)
        par = js_find_member(def_body, "param");
    if (par) {
        par = js_skip_ws(par);
        if (*par == '[') {
            const char *q = par + 1;
            while (*q && *q != ']' && it->nparams < LAUNCHER_MAX_PARAMS) {
                q = js_skip_ws(q);
                if (*q != '"')
                    break;
                js_copy_string(q, it->params[it->nparams], LAUNCHER_PARAM_LEN);
                it->nparams++;
                q = js_skip_value(q);
                if (!q)
                    break;
                q = js_skip_ws(q);
                if (*q == ',')
                    q++;
            }
        } else if (*par == '"') {
            js_copy_string(par, it->params[0], LAUNCHER_PARAM_LEN);
            it->nparams = 1;
        }
    }
    g_launcher_count++;
}

static void
launcher_build(void)
{
    g_launcher_count = 0;
    g_launcher_pages = 1;

    char *db = read_text_file("/mnt/ext1/system/config/desktop/apps_db.json");
    if (!db)
        db = read_text_file("/ebrmain/config/desktop/apps_db.json");
    char *vw = read_text_file("/mnt/ext1/system/config/desktop/view.json");
    if (!vw)
        vw = read_text_file("/ebrmain/config/desktop/view.json");

    if (!db || !vw) {
        free(db);
        free(vw);
        launcher_layout();
        g_launcher_built = 1;
        return;
    }

    const char *db_root = js_object_body(db);
    const char *db_apps = db_root ? js_find_member(db_root, "applications") : NULL;
    const char *db_apps_body = db_apps ? js_object_body(db_apps) : NULL;
    if (!db_apps_body) {
        free(db);
        free(vw);
        launcher_layout();
        g_launcher_built = 1;
        return;
    }

    const char *vw_root = js_object_body(vw);
    const char *view_obj = vw_root ? js_find_member(vw_root, "view") : NULL;
    const char *view_body = view_obj ? js_object_body(view_obj) : NULL;
    const char *groups = view_body ? js_find_member(view_body, "groups") : NULL;
    if (groups) {
        groups = js_skip_ws(groups);
        if (*groups == '[') {
            const char *q = groups + 1;
            while (*q && *q != ']') {
                q = js_skip_ws(q);
                if (*q != '{') {
                    q = js_skip_value(q);
                    if (!q)
                        break;
                    q = js_skip_ws(q);
                    if (*q == ',')
                        q++;
                    continue;
                }
                const char *grp_body = q + 1;
                const char *tp = js_find_member(grp_body, "title");
                char        raw_title[64] = "";
                char        disp_title[64] = "";
                if (tp) {
                    lc_resolve(tp, NULL, raw_title, sizeof raw_title);
                    lc_translate(raw_title, disp_title, sizeof disp_title);
                }
                const char *apps_arr = js_find_member(grp_body, "apps");
                if (apps_arr) {
                    apps_arr = js_skip_ws(apps_arr);
                    if (*apps_arr == '[') {
                        if (g_launcher_count < LAUNCHER_MAX_ITEMS && disp_title[0]) {
                            LauncherItem *hdr = &g_launcher_items[g_launcher_count++];
                            memset(hdr, 0, sizeof *hdr);
                            hdr->kind = 0;
                            snprintf(hdr->text, sizeof hdr->text, "%s", disp_title);
                        }
                        const char *r = apps_arr + 1;
                        while (*r && *r != ']') {
                            r = js_skip_ws(r);
                            if (*r == '"') {
                                char id[48];
                                js_copy_string(r, id, sizeof id);
                                launcher_add_app(db_apps_body, id);
                                r = js_skip_value(r);
                                if (!r)
                                    break;
                            } else {
                                r = js_skip_value(r);
                                if (!r)
                                    break;
                            }
                            r = js_skip_ws(r);
                            if (*r == ',')
                                r++;
                        }
                    }
                }
                q = js_skip_value(q);
                if (!q)
                    break;
                q = js_skip_ws(q);
                if (*q == ',')
                    q++;
            }
        }
    }

    /* Scan view.json applications for U_* user apps not in any group. */
    const char *vw_apps = vw_root ? js_find_member(vw_root, "applications") : NULL;
    const char *vw_apps_body = vw_apps ? js_object_body(vw_apps) : NULL;
    if (vw_apps_body) {
        int         user_hdr_added = 0;
        const char *p = vw_apps_body;
        while (*p) {
            p = js_skip_ws(p);
            if (*p == '}' || *p != '"')
                break;
            const char *ks = ++p;
            while (*p && *p != '"') {
                if (*p == '\\')
                    p++;
                p++;
            }
            size_t kl = (size_t)(p - ks);
            if (*p == '"')
                p++;
            p = js_skip_ws(p);
            if (*p == ':')
                p++;
            p = js_skip_ws(p);
            if (kl >= 2 && ks[0] == 'U' && ks[1] == '_') {
                const char *def_body2 = (*p == '{') ? p + 1 : NULL;
                int         vis = 1;
                if (def_body2) {
                    const char *v2 = js_find_member(def_body2, "visible");
                    if (v2 && !lc_resolve_bool(v2))
                        vis = 0;
                }
                if (vis && g_launcher_count < LAUNCHER_MAX_ITEMS) {
                    char   id[48];
                    size_t cl = kl < sizeof id - 1 ? kl : sizeof id - 1;
                    memcpy(id, ks, cl);
                    id[cl] = '\0';
                    if (!user_hdr_added) {
                        LauncherItem *hdr = &g_launcher_items[g_launcher_count++];
                        memset(hdr, 0, sizeof *hdr);
                        hdr->kind = 0;
                        snprintf(hdr->text, sizeof hdr->text, "User");
                        user_hdr_added = 1;
                    }
                    LauncherItem *it = &g_launcher_items[g_launcher_count];
                    memset(it, 0, sizeof *it);
                    it->kind = 1;
                    if (def_body2) {
                        const char *tp2 = js_find_member(def_body2, "title");
                        if (tp2)
                            lc_resolve(tp2, NULL, it->text, sizeof it->text);
                        const char *pp2 = js_find_member(def_body2, "path");
                        if (pp2)
                            lc_resolve(pp2, NULL, it->path, sizeof it->path);
                        const char *ip2 = js_find_member(def_body2, "icon");
                        if (ip2)
                            lc_resolve(ip2, NULL, it->icon, sizeof it->icon);
                    }
                    if (!it->text[0])
                        snprintf(it->text, sizeof it->text, "%s", id);
                    g_launcher_count++;
                }
            }
            p = js_skip_value(p);
            if (!p)
                break;
            p = js_skip_ws(p);
            if (*p == ',')
                p++;
        }
    }

    free(db);
    free(vw);
    launcher_layout();
    g_launcher_built = 1;
    LOG("[bookshelf] launcher built: %d items, %d pages\n", g_launcher_count, g_launcher_pages);
}

/* -- launcher draw ------------------------------------------------------ */

static void
draw_launcher_icon(int cx, int cy, const char *icon_name, const char *title)
{
    int      sz = LAUNCHER_ICON_SZ;
    int      x0 = cx - sz / 2;
    int      y0 = cy - sz / 2;
    ibitmap *bm = NULL;
    if (icon_name && icon_name[0] && icon_name[0] != '/')
        bm = GetResource(icon_name, NULL);
    if (!bm && icon_name && icon_name[0] == '/')
        bm = LoadPNG(icon_name, 0);
    if (bm) {
        DrawBitmap(x0, y0, bm);
        return;
    }
    FillArea(x0, y0, sz, sz, WHITE);
    DrawRect(x0, y0, sz, sz, BLACK);
    if (title && title[0]) {
        ifont *f = OpenFont(DEFAULTFONTB, 56, 0);
        if (f) {
            SetFont(f, BLACK);
            char ch[2] = {title[0], 0};
            int  tw = StringWidth(ch);
            DrawString(cx - tw / 2, cy - 28, ch);
            CloseFont(f);
        }
    }
}

static void
draw_overlay_launcher(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    FillArea(0, g_state.panel_h, w, h - g_state.panel_h, WHITE);

    FillArea(0, g_state.panel_h, w, LAUNCHER_HEADER_H, WHITE);
    DrawLine(0,
             g_state.panel_h + LAUNCHER_HEADER_H - 1,
             w,
             g_state.panel_h + LAUNCHER_HEADER_H - 1,
             BLACK);
    ifont *tf = OpenFont(DEFAULTFONTB, 36, 0);
    if (tf) {
        SetFont(tf, BLACK);
        const char *title = i18n("launcher.title");
        int         tw = StringWidth(title);
        DrawString((w - tw) / 2, g_state.panel_h + (LAUNCHER_HEADER_H - 36) / 2, title);
        CloseFont(tf);
    }
    {
        int bx = 16, by = g_state.panel_h + (LAUNCHER_HEADER_H - 56) / 2, bw = 160, bh = 56;
        DrawRect(bx, by, bw, bh, BLACK);
        ifont *bf = OpenFont(DEFAULTFONTB, 28, 0);
        if (bf) {
            SetFont(bf, BLACK);
            DrawString(bx + 16, by + (bh - 28) / 2 - 2, i18n("launcher.back"));
            CloseFont(bf);
        }
    }

    int pg = g_state.launcher_page;
    if (pg < 0)
        pg = 0;
    if (pg >= g_launcher_pages)
        pg = g_launcher_pages - 1;
    if (pg < 0)
        pg = 0;

    if (g_launcher_count == 0) {
        ifont *ef = OpenFont(DEFAULTFONT, 32, 0);
        if (ef) {
            SetFont(ef, BLACK);
            const char *empty = i18n("launcher.empty");
            int         tw = StringWidth(empty);
            DrawString((w - tw) / 2, h / 2, empty);
            CloseFont(ef);
        }
    }

    ifont *hf = OpenFont(DEFAULTFONTB, 28, 0);
    ifont *af = OpenFont(DEFAULTFONT, 24, 0);
    for (int i = 0; i < g_launcher_count; i++) {
        const LauncherItem *it = &g_launcher_items[i];
        if (it->page != pg)
            continue;
        if (it->kind == 0) {
            FillArea(it->x, it->y, it->w, it->h, WHITE);
            DrawLine(it->x, it->y + it->h - 1, it->x + it->w, it->y + it->h - 1, BLACK);
            if (hf) {
                SetFont(hf, BLACK);
                DrawString(it->x + 12, it->y + (it->h - 28) / 2 - 2, it->text);
            }
        } else {
            int cx = it->x + it->w / 2;
            int icon_cy = it->y + 12 + LAUNCHER_ICON_SZ / 2;
            draw_launcher_icon(cx, icon_cy, it->icon, it->text);
            if (af) {
                SetFont(af, BLACK);
                int ly = it->y + 12 + LAUNCHER_ICON_SZ + 8;
                int maxw = it->w - 8;
                if (StringWidth(it->text) <= maxw) {
                    int tw = StringWidth(it->text);
                    DrawString(cx - tw / 2, ly, it->text);
                } else {
                    const char *sp = strrchr(it->text, ' ');
                    if (sp) {
                        char   line1[48];
                        size_t l1 = (size_t)(sp - it->text);
                        if (l1 >= sizeof line1)
                            l1 = sizeof line1 - 1;
                        memcpy(line1, it->text, l1);
                        line1[l1] = '\0';
                        int tw = StringWidth(line1);
                        DrawString(cx - tw / 2, ly, line1);
                        tw = StringWidth(sp + 1);
                        DrawString(cx - tw / 2, ly + 28, sp + 1);
                    } else {
                        char trunc[24];
                        snprintf(trunc, sizeof trunc, "%.20s", it->text);
                        int tw = StringWidth(trunc);
                        DrawString(cx - tw / 2, ly, trunc);
                    }
                }
            }
        }
    }
    if (hf)
        CloseFont(hf);
    if (af)
        CloseFont(af);

    int py = h - LAUNCHER_PAGER_H;
    FillArea(0, py, w, LAUNCHER_PAGER_H, WHITE);
    DrawLine(0, py, w, py, BLACK);
    if (g_launcher_pages > 1) {
        char pbuf[32];
        snprintf(pbuf, sizeof pbuf, "%d / %d", pg + 1, g_launcher_pages);
        ifont *pf = OpenFont(DEFAULTFONT, 28, 0);
        if (pf) {
            SetFont(pf, BLACK);
            int tw = StringWidth(pbuf);
            DrawString((w - tw) / 2, py + (LAUNCHER_PAGER_H - 28) / 2 - 2, pbuf);
            CloseFont(pf);
        }
        if (pg > 0) {
            DrawLine(32, py + 28, 16, py + LAUNCHER_PAGER_H / 2, BLACK);
            DrawLine(16, py + LAUNCHER_PAGER_H / 2, 32, py + LAUNCHER_PAGER_H - 28, BLACK);
        }
        if (pg < g_launcher_pages - 1) {
            DrawLine(w - 32, py + 28, w - 16, py + LAUNCHER_PAGER_H / 2, BLACK);
            DrawLine(w - 16, py + LAUNCHER_PAGER_H / 2, w - 32, py + LAUNCHER_PAGER_H - 28, BLACK);
        }
    }
}

/* -- launcher hit-test + actions ---------------------------------------- */

static void
launch_app(const LauncherItem *it)
{
    if (!it->path[0])
        return;
    const char *base = strrchr(it->path, '/');
    base = base ? base + 1 : it->path;
    char *args[LAUNCHER_MAX_PARAMS + 2];
    int   ai = 0;
    args[ai++] = (char *)it->path;
    for (int i = 0; i < it->nparams && ai < LAUNCHER_MAX_PARAMS + 1; i++)
        args[ai++] = (char *)it->params[i];
    args[ai] = NULL;
    LOG("[bookshelf] launching app path=%s base=%s params=%d\n", it->path, base, it->nparams);
    NewTaskEx(it->path, ai ? args : NULL, base, it->text, NULL, 1u << 30, 0);
}

static void
on_tap_overlay_launcher(int x, int y)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    if (x >= 16 && x < 176 && y >= g_state.panel_h + (LAUNCHER_HEADER_H - 56) / 2 &&
        y < g_state.panel_h + (LAUNCHER_HEADER_H - 56) / 2 + 56) {
        launcher_close();
        return;
    }
    int py = h - LAUNCHER_PAGER_H;
    if (y >= py) {
        if (x < w / 3 && g_state.launcher_page > 0) {
            g_state.launcher_page--;
            draw_overlay_launcher();
            FullUpdate();
        } else if (x >= 2 * w / 3 && g_state.launcher_page < g_launcher_pages - 1) {
            g_state.launcher_page++;
            draw_overlay_launcher();
            FullUpdate();
        }
        return;
    }
    int pg = g_state.launcher_page;
    for (int i = 0; i < g_launcher_count; i++) {
        const LauncherItem *it = &g_launcher_items[i];
        if (it->page != pg || it->kind != 1)
            continue;
        if (x >= it->x && x < it->x + it->w && y >= it->y && y < it->y + it->h) {
            launcher_close();
            launch_app(it);
            return;
        }
    }
}

static void
launcher_open_set(void)
{
    if (!g_launcher_built)
        launcher_build();
    g_state.launcher_open = 1;
    g_state.launcher_page = 0;
    draw_overlay_launcher();
    FullUpdate();
}

static void
launcher_close(void)
{
    g_state.launcher_open = 0;
    redraw_shelf();
}
/* Pop out of a drilled-in series back to the collapsed top-level grid. */
static void
drill_back(void)
{
    g_drilled_series[0] = '\0';
    g_state.page = 0;
    g_state.selected = -1;
    build_view();
    LOG("[bookshelf] drilled back to top level (view=%d)\n", g_view_count);
    FillArea(0, g_state.panel_h, ScreenWidth(), ScreenHeight() - g_state.panel_h, WHITE);
    draw_top_bar();
    draw_search_row();
    draw_tab_row();
    draw_grid();
    draw_pager();
    FullUpdate();
}

static void
on_tap_thumbnail(int vi)
{
    if (vi < 0 || vi >= g_view_count)
        return;
    const ViewTile *vt = &g_view[vi];

    /* Series card → drill into the series. */
    if (vt->is_series) {
        snprintf(g_drilled_series, sizeof g_drilled_series, "%s", vt->series_id);
        g_state.page = 0;
        g_state.selected = -1;
        build_view();
        LOG("[bookshelf] drilled into series '%s' (%d books)\n", vt->series_name, g_view_count);
        FillArea(0, g_state.panel_h, ScreenWidth(), ScreenHeight() - g_state.panel_h, WHITE);
        draw_top_bar();
        draw_search_row();
        draw_tab_row();
        draw_grid();
        draw_pager();
        FullUpdate();
        return;
    }

    /* Flat tile → download (if needed) then open in the configured reader. */
    Book *b = &g_state.books[vt->book_idx];
    book_press_action(b);
}

/* ── downloads, delete, context menu, long-press ───────────────────── */

/* Local path a book downloads to (matches the open-with launch path). */
static void
book_local_path(const Book *b, char *out, size_t cap)
{
    if (b->ext[0])
        snprintf(out, cap, "%s/%s.%s", g_downloads_dir, b->id, b->ext);
    else
        snprintf(out, cap, "%s/%s", g_downloads_dir, b->id);
}

/* Look a book up in the master library by id (NULL if unknown). */
static Book *
find_lib_book(const char *id)
{
    for (int i = 0; i < g_lib_count; i++)
        if (strcmp(g_lib[i].id, id) == 0)
            return &g_lib[i];
    return NULL;
}

/* Sync a book's downloaded flag by probing its on-device file. */
static void
refresh_downloaded(Book *b)
{
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    b->downloaded = (access(path, F_OK) == 0);
    if (b->downloaded)
        snprintf(b->local_path, sizeof b->local_path, "%s", path);
}

/* Find a download-queue entry by id (NULL if absent). */
static DownloadItem *
find_download(const char *id)
{
    for (int i = 0; i < g_download_count; i++)
        if (strcmp(g_downloads[i].id, id) == 0)
            return &g_downloads[i];
    return NULL;
}

/* Add a book to the download queue (no-op if already queued/done) and
 * arm the drain timer. */
static void
enqueue_download(const Book *b)
{
    DownloadItem *d = find_download(b->id);
    if (d != NULL)
        return;
    if (g_download_count >= MAX_DOWNLOADS)
        return;
    DownloadItem *n = &g_downloads[g_download_count++];
    snprintf(n->id, sizeof n->id, "%s", b->id);
    snprintf(n->title, sizeof n->title, "%s", b->title);
    n->state = 0;
    if (!g_download_armed) {
        g_download_armed = 1;
        SetWeakTimerEx("bdl", download_tick, NULL, 120);
    }
}

/* Download one book's file to disk (blocking).  Returns 1 on success. */
static int
download_book_file(Book *b)
{
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);

    char url[MAX_URL_LEN + 128];
    snprintf(url,
             sizeof url,
             "%s/api/v1/books/%s/file?access_token=%s",
             g_state.api_base,
             b->id,
             g_state.api_token);

    int   rsize = 0;
    char *data = QuickDownload(url, &rsize, 60);
    if (data == NULL || rsize <= 0) {
        if (data)
            free(data);
        LOG("[bookshelf] download_book_file FAILED id=%s\n", b->id);
        return 0;
    }
    FILE *f = fopen(path, "wb");
    if (f == NULL) {
        free(data);
        LOG("[bookshelf] download_book_file fopen FAILED path=%s\n", path);
        return 0;
    }
    fwrite(data, 1, (size_t)rsize, f);
    fclose(f);
    free(data);
    b->downloaded = 1;
    snprintf(b->local_path, sizeof b->local_path, "%s", path);
    LOG("[bookshelf] download_book_file OK id=%s path=%s bytes=%d\n", b->id, path, rsize);
    return 1;
}

/* Launch the configured reader on an already-downloaded book. */
static void
launch_reader(Book *b)
{
    char app[80];
    char full_path[160];
    if (g_state.reader_pref > 0 && g_state.reader_pref <= g_reader_count) {
        const char *rpath = g_readers[g_state.reader_pref - 1].path;
        const char *rbase = strrchr(rpath, '/');
        rbase = rbase ? rbase + 1 : rpath;
        snprintf(app, sizeof app, "%s", rbase);
        snprintf(full_path, sizeof full_path, "%s", rpath);
    } else {
        /* Auto: ask the server which app handles this extension. */
        char body[160];
        snprintf(body, sizeof body, "{\"id\":\"%s\",\"ext\":\"%s\"}", b->id, b->ext);
        char *resp = NULL;
        int   rl = 0;
        char  resolved[64] = "eink-reader";
        if (http_post(g_state.url_openwith, body, &resp, &rl) == 0 && resp) {
            char tmp[64];
            if (json_find_key(resp, "app", tmp, sizeof tmp))
                snprintf(resolved, sizeof resolved, "%s", tmp);
            free(resp);
        }
        size_t alen = strlen(resolved);
        if (alen < 4 || strcmp(resolved + alen - 4, ".app") != 0)
            snprintf(app, sizeof app, "%s.app", resolved);
        else
            snprintf(app, sizeof app, "%s", resolved);
        /* Build full path from basename. */
        if (strchr(app, '/') == NULL)
            snprintf(full_path, sizeof full_path, "/ebrmain/bin/%s", app);
        else
            snprintf(full_path, sizeof full_path, "%s", app);
    }
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    char path_copy[MAX_PATH_LEN];
    snprintf(path_copy, sizeof path_copy, "%s", path);
    char *args[2] = {path_copy, NULL};
    LOG("[bookshelf] launching reader app=%s path=%s reader_pref=%d\n",
        app,
        path_copy,
        g_state.reader_pref);
    NewTaskEx(full_path, args, app, b->title, NULL, 1u << 30, 0);
}

/* Press a book: download it if needed, then open it in the reader. */
static void
book_press_action(Book *b)
{
    refresh_downloaded(b);
    if (!b->downloaded) {
        snprintf(g_state.status, sizeof g_state.status, "%s", i18n("dl.in_progress"));
        if (!download_book_file(b)) {
            snprintf(g_state.status, sizeof g_state.status, "%s", i18n("dl.failed"));
            return;
        }
    }
    launch_reader(b);
}

/* Delete a book's local file (server metadata is untouched — there is no
 * delete endpoint).  Marks the book not-downloaded so it can be fetched
 * again on the next press. */
static void
delete_book_file(Book *b)
{
    char path[MAX_PATH_LEN];
    book_local_path(b, path, sizeof path);
    if (unlink(path) == 0)
        LOG("[bookshelf] delete_book_file removed %s\n", path);
    else
        LOG("[bookshelf] delete_book_file unlink failed %s\n", path);
    b->downloaded = 0;
    b->local_path[0] = '\0';
    DownloadItem *d = find_download(b->id);
    if (d != NULL)
        d->state = 3;
}

/* Drain the download queue one item per tick so a "Download all" shows
 * live progress on the Downloads tab instead of blocking the UI for the
 * whole batch. */
static void
download_tick(void *ctx)
{
    (void)ctx;
    g_download_armed = 0;
    DownloadItem *target = NULL;
    for (int i = 0; i < g_download_count; i++) {
        if (g_downloads[i].state == 0) {
            target = &g_downloads[i];
            break;
        }
    }
    if (target == NULL) {
        if (g_state.tab == TAB_DOWNLOADS)
            redraw_shelf();
        return;
    }
    target->state = 1;
    if (g_state.tab == TAB_DOWNLOADS)
        redraw_shelf();

    Book *b = find_lib_book(target->id);
    int   ok = 0;
    if (b != NULL)
        ok = download_book_file(b);
    target->state = ok ? 2 : 3;

    if (g_state.tab == TAB_DOWNLOADS)
        redraw_shelf();
    else
        draw_tab_row(); /* refresh the pending-count badge */

    /* More queued? keep draining. */
    for (int i = 0; i < g_download_count; i++) {
        if (g_downloads[i].state == 0) {
            g_download_armed = 1;
            SetWeakTimerEx("bdl", download_tick, NULL, 120);
            break;
        }
    }
}

/* Queue every member of a series (by series_id). */
static void
download_series(const char *series_id)
{
    int n = 0;
    for (int i = 0; i < g_lib_count; i++) {
        if (strcmp(g_lib[i].series_id, series_id) == 0) {
            enqueue_download(&g_lib[i]);
            n++;
        }
    }
    LOG("[bookshelf] download_series %s queued=%d\n", series_id, n);
}

/* Delete the local files of every member of a series. */
static void
delete_series(const char *series_id)
{
    int n = 0;
    for (int i = 0; i < g_lib_count; i++) {
        if (strcmp(g_lib[i].series_id, series_id) == 0) {
            delete_book_file(&g_lib[i]);
            n++;
        }
    }
    LOG("[bookshelf] delete_series %s removed=%d\n", series_id, n);
}

/* Context menu geometry: a centred modal sheet.  Returns the sheet rect
 * and the y of the first item row. */
static void
context_geom(int *px, int *py, int *pw, int *ph, int n_items)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    *pw = w * 3 / 4;
    *ph = CTX_TITLE_H + n_items * CTX_ITEM_H + CTX_PAD;
    *px = (w - *pw) / 2;
    *py = (h - *ph) / 2;
}

static int
context_item_count(void)
{
    /* Both the book and series menus offer exactly two actions. */
    return 2;
}

/* Draw the long-press context menu over a dimmed shelf. */
static void
draw_context_menu(void)
{
    int w = ScreenWidth();
    int h = ScreenHeight();
    /* Dim mask. */
    for (int yy = g_state.panel_h; yy < h; yy += 2)
        DrawLine(0, yy, w, yy, LGRAY);

    int n = context_item_count();
    int px, py, pw, ph;
    context_geom(&px, &py, &pw, &ph, n);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    /* Title: series name or book title. */
    const char *title;
    if (g_state.ctx_is_series) {
        /* ctx_series_id holds a series id; recover the name from any member. */
        title = "Series";
        for (int i = 0; i < g_lib_count; i++) {
            if (strcmp(g_lib[i].series_id, g_state.ctx_series_id) == 0) {
                title = g_lib[i].series;
                break;
            }
        }
    } else {
        title = g_state.books[g_state.ctx_book_idx].title;
    }
    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        char trunc[MAX_TITLE_LEN];
        snprintf(trunc, sizeof trunc, "%s", title);
        while (StringWidth(trunc) > pw - 2 * CTX_PAD && strlen(trunc) > 4)
            trunc[strlen(trunc) - 1] = '\0';
        DrawString(px + CTX_PAD, py + (CTX_TITLE_H - 28) / 2 - 2, trunc);
        CloseFont(tf);
    }
    DrawLine(px + CTX_PAD, py + CTX_TITLE_H - 1, px + pw - CTX_PAD, py + CTX_TITLE_H - 1, LGRAY);

    const char *labels[2];
    if (g_state.ctx_is_series) {
        labels[0] = i18n("ctx.download_all");
        labels[1] = i18n("ctx.delete_series");
    } else {
        labels[0] = i18n("ctx.download");
        labels[1] = i18n("ctx.delete");
    }
    ifont *f = OpenFont(DEFAULTFONTB, 30, 0);
    if (f == NULL)
        return;
    SetFont(f, BLACK);
    for (int i = 0; i < n; i++) {
        int iy = py + CTX_TITLE_H + i * CTX_ITEM_H;
        DrawString(px + CTX_PAD, iy + (CTX_ITEM_H - 30) / 2 - 2, labels[i]);
        if (i + 1 < n)
            DrawLine(
                px + CTX_PAD, iy + CTX_ITEM_H - 1, px + pw - CTX_PAD, iy + CTX_ITEM_H - 1, LGRAY);
    }
    CloseFont(f);
}

static void
close_context(void)
{
    g_state.ctx_open = 0;
    redraw_shelf();
}

/* Open the context menu for a view tile (series card or book). */
static void
open_context_for_tile(int vi)
{
    if (vi < 0 || vi >= g_view_count)
        return;
    const ViewTile *vt = &g_view[vi];
    g_state.ctx_open = 1;
    g_state.ctx_is_series = vt->is_series;
    if (vt->is_series) {
        snprintf(g_state.ctx_series_id, sizeof g_state.ctx_series_id, "%s", vt->series_id);
        g_state.ctx_book_idx = -1;
    } else {
        g_state.ctx_book_idx = vt->book_idx;
        g_state.ctx_series_id[0] = '\0';
    }
    draw_context_menu();
    FullUpdate();
    LOG("[bookshelf] context menu open series=%d vi=%d\n", vt->is_series, vi);
}

/* Long-press timer fired with the finger still down: open the menu. */
static void
longpress_tick(void *ctx)
{
    (void)ctx;
    if (!g_lp_armed || g_lp_vi < 0)
        return;
    g_lp_armed = 0;
    int vi = g_lp_vi;
    g_lp_vi = -1;
    g_ctx_suppress_up = 1;
    open_context_for_tile(vi);
}

/* Handle a tap while the context menu is open. */
static void
on_tap_context(int x, int y)
{
    int n = context_item_count();
    int px, py, pw, ph;
    context_geom(&px, &py, &pw, &ph, n);
    if (x < px || x >= px + pw || y < py + CTX_TITLE_H || y >= py + ph) {
        close_context();
        return;
    }
    int  item = (y - (py + CTX_TITLE_H)) / CTX_ITEM_H;
    int  is_series = g_state.ctx_is_series;
    int  book_idx = g_state.ctx_book_idx;
    char series_id[MAX_ID_LEN];
    snprintf(series_id, sizeof series_id, "%s", g_state.ctx_series_id);
    g_state.ctx_open = 0;

    if (is_series) {
        if (item == 0)
            download_series(series_id);
        else if (item == 1)
            delete_series(series_id);
    } else {
        Book *b = (book_idx >= 0 && book_idx < g_state.count) ? &g_state.books[book_idx] : NULL;
        if (b != NULL) {
            Book *lib = find_lib_book(b->id);
            Book *target = lib ? lib : b;
            if (item == 0)
                enqueue_download(target);
            else if (item == 1)
                delete_book_file(target);
        }
    }
    redraw_shelf();
}

/* ── event loop ──────────────────────────────────────────────────────── */

static void keyboard_handler(char *buffer);

static int
on_event(int type, int par1, int par2)
{
    if (type == EVT_INIT) {
        memset(&g_state, 0, sizeof g_state);
        g_state.cur_lib = -1;
        g_state.sort = SORT_TITLE_ASC;
        g_state.group = GROUP_ALL;
        g_state.filter = FILTER_ALL;
        g_state.selected = -1;

        /* Keep the system panel visible (battery / wifi / clock).
         * Calling SetPanelType(PANEL_DISABLED) or iv_fullscreen()
         * would hide it, which is what we explicitly do NOT want —
         * the user wants the original PB-app behaviour of leaving
         * the system panel drawn over by the firmware.  We also
         * query PanelHeight() once so all subsequent draws can
         * start below it without per-frame work.
         *
         * The SDK docstring for SetCurrentApplicationAttribute notes
         * that APPLICATION_READER "affects behaviour of panel, for
         * proper work, set this attribute before first access to
         * panel API".  Without it the firmware may treat us as a
         * generic "shell" task (no bottom status bar) instead of a
         * reader-style app (with the persistent Tue 23:13 + battery
         * strip).  Setting it matches what the original sudoku.app
         * and dictionary do.
         */
        SetCurrentApplicationAttribute(APPLICATION_READER, 1);

        /* Set the framebuffer orientation FIRST.  SetOrientation()
         * recomputes the per-task iv_fbinfo (clearing the framebuffer to
         * white and resetting fb_y_offset to 0).  We run it before
         * SetPanelType() so the panel config lands on the final fb layout
         * and is not clobbered by the orientation reset. */
        SetOrientation(0);

        /* Enable the reader-style status bar at the TOP of the screen.
         * SetShowPanelReader(1) sets the panel_conf show flag (offset 0x30)
         * and re-applies the current panel type.  SetPanelType() with the
         * PANEL_NO_FB_OFFSET bit (the same value eink-reader.app uses,
         * PANEL_ENABLED | 1<<3 == 10) keeps fb_y_offset at 0 and makes the
         * firmware's panel painter draw the strip at y=0 (top) instead of
         * the bottom.  Our layout offsets every surface below panel_h. */
        SetShowPanelReader(1);
        SetPanelSeparatorEnabled(1);
        SetPanelTransparent(0);
        SetPanelType(PANEL_ENABLED | PANEL_NO_FB_OFFSET);
        g_state.panel_h = PanelHeight();

        /* Populate and render the panel content.  DrawPanel() fills in the
         * panel_conf content fields (the stock bookshelf.app calls
         * DrawPanel(NULL, NULL, NULL, -1) from its CustomDrawPanel()
         * override); iv_update_panel(0) is the function that actually blits
         * the clock / battery / wifi strip into the framebuffer.  The
         * framework only calls it via iv_actualize_panel() when
         * is_state_changed() is true, which it isn't on a fresh launch, so
         * we force it here.  Arg 0 = reading-mode disabled (normal bar). */
        DrawPanel(NULL, "Bookshelf", NULL, -1);
        iv_update_panel(0);

        /* Force the firmware to actually draw the system panel now.
         * Repaint() enqueues EVT_SHOW (=23) on the event loop, which
         * the firmware handles by calling iv_actualize_panel(), which
         * in turn calls iv_update_panel() (the function that draws the
         * day-of-week + 24h-time strip at the top and the matching
         * strip at the bottom with the down-arrow + lightbulb +
         * battery icons).  Without this call the panel is only
         * redrawn on subsequent state changes (clock minute tick,
         * battery percent change, net state change) — on a freshly
         * launched task with no state change yet, the panel rect is
         * blank.  Repaint() forces an immediate one-shot redraw.
         */
        Repaint();
        LOG("[bookshelf] EVT_INIT panel_h=%d sw=%d sh=%d\n",
            g_state.panel_h,
            ScreenWidth(),
            ScreenHeight());

        struct cfg_out cfg = {
            .api_url = g_state.api_base,
            .api_token = g_state.api_token,
            .cap = sizeof g_state.api_base,
        };
        g_state.api_base[0] = '\0';
        load_config_file(g_argv0, &cfg);
        resolve_config_path(g_argv0);
        detect_readers();
        resolve_downloads_dir();
        g_state.reader_pref = reader_pref_from_path(g_cfg_reader);
        LOG("[bookshelf] reader_pref=%d (cfg `%s`)\n", g_state.reader_pref, g_cfg_reader);

        /* Try firmware language env (PB sets LANG=en_US.utf8 etc). */
        const char *env_lang = getenv("LANG");
        if (env_lang != NULL && env_lang[0] != '\0') {
            if (strncmp(env_lang, "de", 2) == 0)
                snprintf(g_lang, sizeof g_lang, "de");
            else if (strncmp(env_lang, "fr", 2) == 0)
                snprintf(g_lang, sizeof g_lang, "fr");
            else if (strncmp(env_lang, "it", 2) == 0)
                snprintf(g_lang, sizeof g_lang, "it");
            else
                snprintf(g_lang, sizeof g_lang, "en");
        }

        /* Resolve API URL via env vars if config didn't set it. */
        if (g_state.api_base[0] == '\0') {
            const char *env_url = getenv("PBEMU_API_URL");
            const char *env_host = getenv("PBEMU_API_HOST");
            const char *url = env_url ? env_url : (env_host ? env_host : API_BASE_DEFAULT);
            if (strncmp(url, "http://", 7) != 0 && strncmp(url, "https://", 8) != 0) {
                char tmp[200];
                snprintf(tmp, sizeof tmp, "http://%s:8765", url);
                snprintf(g_state.api_base, sizeof g_state.api_base, "%s", tmp);
            } else {
                snprintf(g_state.api_base, sizeof g_state.api_base, "%s", url);
            }
        }

        build_endpoint_urls();

        /* Auto-sync on first launch so the shelf populates without a
         * manual tap.  do_sync() blocks on the network here exactly as
         * it does from the menu path; the draw below then renders the
         * fetched books (and arms the per-tile cover fetcher). */
        do_sync();
        draw_top_bar();
        draw_search_row();
        draw_tab_row();
        draw_grid();
        draw_pager();
        FullUpdate();
        return 1;
    }

    if (type == EVT_SHOW || type == EVT_REPAINT || type == EVT_FOREGROUND) {
        /* Render the system panel strip before drawing app content.
         * The framework's iv_actualize_panel() skips the draw when
         * is_state_changed() returns 0 (no clock/battery/net change),
         * leaving the strip blank after a FullUpdate() flush.  Calling
         * iv_update_panel(0) directly ensures the clock/battery/wifi
         * strip is always present in the framebuffer before we draw
         * our content below it. */
        iv_update_panel(0);
        if (g_state.launcher_open) {
            draw_overlay_launcher();
            FullUpdate();
            return 1;
        }
        if (g_state.settings_open) {
            draw_overlay_settings();
            FullUpdate();
            return 1;
        }
        draw_top_bar();
        draw_search_row();
        draw_tab_row();
        if (g_state.tab == TAB_DOWNLOADS)
            draw_downloads_tab();
        else
            draw_grid();
        draw_pager();
        if (g_state.menu_open)
            draw_overlay_menu();
        else if (g_state.more_open)
            draw_overlay_more();
        FullUpdate();
        return 1;
    }

    if (type == EVT_POINTERDOWN) {
        int x = par1, y = par2;
        /* Arm a long-press only on the Library tab's grid, and only when
         * no modal overlay is up.  The timer (longpress_tick) opens the
         * context menu if the finger stays put. */
        g_lp_armed = 0;
        g_lp_vi = -1;
        if (g_state.tab == TAB_LIBRARY && !g_state.settings_open && !g_state.menu_open &&
            !g_state.more_open && !g_state.ctx_open && !g_state.search_open &&
            !g_state.launcher_open) {
            int vi = hit_thumbnail(x, y);
            if (vi >= 0) {
                g_lp_armed = 1;
                g_lp_vi = vi;
                g_lp_x = x;
                g_lp_y = y;
                SetWeakTimerEx("blp", longpress_tick, NULL, LONGPRESS_MS);
            }
        }
        return 1;
    }

    if (type == EVT_POINTERMOVE) {
        /* A drag away from the press point cancels the pending long-press
         * so scrolling/scrubbing never pops the context menu. */
        if (g_lp_armed) {
            int dx = par1 - g_lp_x, dy = par2 - g_lp_y;
            if (dx * dx + dy * dy > LONGPRESS_SLOP * LONGPRESS_SLOP) {
                g_lp_armed = 0;
                g_lp_vi = -1;
            }
        }
        return 0;
    }

    if (type == EVT_POINTERUP) {
        int x = par1, y = par2;
        LOG("[bookshelf] EVT_POINTERUP x=%d y=%d menu=%d more=%d search=%d\n",
            x,
            y,
            g_state.menu_open,
            g_state.more_open,
            g_state.search_open);
        /* Finger lifted — a pending long-press becomes a normal tap. */
        g_lp_armed = 0;
        g_lp_vi = -1;
        /* Drop the release that opened the context menu (see longpress_tick). */
        if (g_ctx_suppress_up) {
            g_ctx_suppress_up = 0;
            return 1;
        }

        /* Settings overlay owns the whole screen and repaints itself. */
        if (g_state.settings_open) {
            on_tap_overlay_settings(x, y);
            return 1;
        }
        /* Launcher overlay owns the whole screen while open. */
        if (g_state.launcher_open) {
            on_tap_overlay_launcher(x, y);
            return 1;
        }

        /* Context (long-press) menu owns all taps while open: a tap on
         * an item runs it, anything else dismisses the sheet. */
        if (g_state.ctx_open) {
            on_tap_context(x, y);
            return 1;
        }

        /* Overlay taps take priority; outside-of-panel taps close. */
        if (g_state.menu_open) {
            on_tap_overlay_menu(x, y);
            /* Clear entire screen then redraw.  The overlay drew a black
             * mask across the whole screen, so we need to repaint
             * everything underneath.
             */
            redraw_shelf();
            return 1;
        }
        if (g_state.more_open) {
            on_tap_overlay_more(x, y);
            /* If Settings was opened, it already drew itself; don't
             * repaint the shelf over it. */
            if (!g_state.settings_open) {
                redraw_shelf();
            }
            return 1;
        }
        /* Top system strip (the status bar with clock, battery, etc.).
         * Tapping anywhere on it opens the firmware control panel — the
         * same gesture as the real device. */
        if (y < g_state.panel_h) {
            LOG("[bookshelf] system bar tapped -> control panel\n");
            OpenControlPanel(NULL);
            return 1;
        }

        /* Search input */
        if (hit_search(x, y) == 1) {
            g_state.search_open = 1;
            snprintf(g_search_kb_buf, sizeof g_search_kb_buf, "%s", g_state.query);
            OpenKeyboard(
                "Search", g_search_kb_buf, sizeof g_search_kb_buf - 1, 0, keyboard_handler);
            return 1;
        }

        /* Tab switcher — Library / Downloads. */
        int tabhit = hit_tab_row(x, y);
        if (tabhit == 0 && g_state.tab != TAB_LIBRARY) {
            g_state.tab = TAB_LIBRARY;
            g_state.page = 0;
            redraw_shelf();
            return 1;
        }
        if (tabhit == 1 && g_state.tab != TAB_DOWNLOADS) {
            g_state.tab = TAB_DOWNLOADS;
            redraw_shelf();
            return 1;
        }
        if (tabhit >= 0)
            return 1; /* tapped the already-active tab */

        /* Top-bar buttons — shared by both tabs (home/menu/sync must work
         * even while the Downloads tab is showing).  hit_top_bar returns:
         *   1 = home  (left, the firmware-style "back to launcher" button)
         *   3 = menu  (right, opens the in-app group/sort overlay)
         *   2 = sync  (refresh)
         */
        int which = hit_top_bar(x, y);
        if (which == 1) {
            if (g_drilled_series[0] != '\0') {
                drill_back();
                return 1;
            }
            /* Home — close the app and return to the launcher. */
            CloseApp();
            return 1;
        }
        if (which == 3) {
            g_state.more_open = 1;
            draw_overlay_more();
            FullUpdate();
            return 1;
        }
        if (which == 2) {
            do_sync();
            redraw_shelf();
            return 1;
        }

        /* Pager — shared by both tabs; the page count is per-tab, so the
         * same buttons page the downloads list on that tab. */
        int pg = hit_pager(x, y);
        if (pg == -1) {
            g_state.page--;
            redraw_shelf();
            return 1;
        }
        if (pg == -2) {
            g_state.page++;
            redraw_shelf();
            return 1;
        }

        /* Below the pager the body is tab-specific: the Downloads tab has
         * no tappable rows, so swallow the tap; the Library tab falls
         * through to the book-grid hit-test below. */
        if (g_state.tab == TAB_DOWNLOADS)
            return 1;

        /* Book tap */
        int idx = hit_thumbnail(x, y);
        if (idx >= 0) {
            g_state.selected = idx;
            on_tap_thumbnail(idx);
            draw_grid();
            PartialUpdate(0,
                          g_state.panel_h + TOP_BAR_H + SEARCH_ROW_H,
                          ScreenWidth(),
                          ScreenHeight() - g_state.panel_h - TOP_BAR_H - SEARCH_ROW_H);
            return 1;
        }
        return 0;
    }

    if (type == EVT_KEYPRESS) {
        if (par1 == IV_KEY_BACK || par1 == IV_KEY_PREV) {
            if (g_state.ctx_open) {
                close_context();
                return 1;
            }
            if (g_state.settings_open) {
                settings_close();
                return 1;
            }
            if (g_state.launcher_open) {
                launcher_close();
                return 1;
            }
            if (g_state.menu_open) {
                g_state.menu_open = 0;
                redraw_shelf();
                return 1;
            }
            if (g_state.more_open) {
                g_state.more_open = 0;
                redraw_shelf();
                return 1;
            }
            if (g_state.search_open) {
                g_state.search_open = 0;
                draw_search_row();
                PartialUpdate(0, TOP_BAR_H, ScreenWidth(), SEARCH_ROW_H);
                return 1;
            }
            if (g_drilled_series[0] != '\0') {
                drill_back();
                return 1;
            }
            CloseApp();
            return 1;
        }
        return 0;
    }

    if (type == EVT_EXIT)
        return 1;
    return 0;
}

static void
keyboard_handler(char *buffer)
{
    /* buffer aliases g_search_kb_buf (never g_state.query), so this copy
     * is safe and the committed text survives into the filter pass. */
    snprintf(g_state.query, sizeof g_state.query, "%s", buffer ? buffer : "");
    LOG("[bookshelf] search commit: query=`%s`\n", g_state.query);
    g_state.search_open = 0;
    apply_filter_and_sort();
    /* redraw grid + search */
    /* The on-screen keyboard draws full-screen and wipes the top status
     * strip; re-stamp it before redraw_shelf() flushes so the panel
     * survives the commit redraw (redraw_shelf clears only from panel_h). */
    iv_update_panel(0);
    redraw_shelf();
}

int
main(int argc, char **argv)
{
    (void)argc;
    if (argv != NULL && argv[0] != NULL)
        snprintf(g_argv0, sizeof g_argv0, "%s", argv[0]);
    else
        g_argv0[0] = '\0';
    log_open(g_argv0);

    /* Note: the original firmware's bookshelf.app imports
     * SetDefaultOrientation and calls it before InkViewMain(), but
     * calling set_fb_orientation() that early hits a NULL fb on the
     * pbemu shim (and may have issues on real devices where the fb
     * isn't attached until the task is registered).  We instead call
     * SetOrientation(0) inside EVT_INIT, after the shim has attached
     * the main framebuffer (see the attach_shm log lines that precede
     * EVT_INIT).  This produces an identical end-state orientation
     * (portrait) without the early-NULL-fb problem.
     */

    InkViewMain(on_event);
    log_close();
    return 0;
}