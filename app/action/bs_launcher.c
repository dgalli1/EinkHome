/* bs_launcher.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "cJSON.h"
#include "bs_downloads.h"
#include "bs_launcher.h"
#include "bs_model.h"
#include "bs_net.h"
#include "bs_store.h"
#include "bs_ui.h"

/* -- app launcher ------------------------------------------------------- *
 * Reproduces the firmware's grouped application grid (the "Apps" screen
 * the original desktop renders from view.json + apps_db.json).  Since
 * bookshelf.app *is* the home-screen replacement, the original grid is
 * gone — this overlay restores it, resolving conditional visibility for
 * the current device profile (Era: touch + audio + en/WW + stock partner)
 * so the grid matches what the real device shows (e.g. Snake hidden on a
 * touch panel).  Tapping a tile launches the app via NewTaskEx. */

/* -- device profile for conditional resolution -------------------------- */

BsLcProfile bs_g_lcprof = {"all", "pocketbook", "true", "false", "en", "WW"};

const char *const bs_lc_dims[] = {
    "device",
    "partner",
    "has_audio",
    "has_cloud",
    "language",
    "localization",
    "globalcfg",
};

const char *
bs_lc_prof_val(const char *dim)
{
    if (strcmp(dim, "device") == 0)
        return bs_g_lcprof.device;
    if (strcmp(dim, "partner") == 0)
        return bs_g_lcprof.partner;
    if (strcmp(dim, "has_audio") == 0)
        return bs_g_lcprof.has_audio;
    if (strcmp(dim, "has_cloud") == 0)
        return bs_g_lcprof.has_cloud;
    if (strcmp(dim, "language") == 0)
        return bs_g_lcprof.language;
    if (strcmp(dim, "localization") == 0)
        return bs_g_lcprof.localization;
    return NULL;
}

const char *
bs_lc_pick_key(const cJSON *obj, const char *want)
{
    const char *first = NULL;
    int         all_present = 0, def_present = 0;
    const cJSON *it;
    cJSON_ArrayForEach(it, obj) {
        const char *k = it->string;
        if (k == NULL)
            break;
        if (first == NULL)
            first = k;
        if (want != NULL && strcmp(k, want) == 0)
            return want;
        if (strcmp(k, "all") == 0)
            all_present = 1;
        if (strcmp(k, "default") == 0)
            def_present = 1;
    }
    if (all_present)
        return "all";
    if (def_present)
        return "default";
    return first;
}

/* No current dimension: pick a fallback key from the object and resolve
 * it with a NULL dimension. */
static void
lc_resolve_fallback(const cJSON *v, char *out, size_t cap)
{
    const char *k = bs_lc_pick_key(v, NULL);
    if (k != NULL) {
        const cJSON *vp = cJSON_GetObjectItemCaseSensitive(v, k);
        if (vp != NULL)
            bs_lc_resolve(vp, NULL, out, cap);
    }
}

/* The globalcfg variant: the first member whose value is an object
 * carrying a "default" wins. */
static void
lc_resolve_globalcfg(const cJSON *v, const char *cur_dim, char *out, size_t cap)
{
    const cJSON *m = NULL;
    const cJSON *it;
    cJSON_ArrayForEach(it, v) {
        if (!cJSON_IsObject(it))
            continue;
        const cJSON *defp = cJSON_GetObjectItemCaseSensitive(it, "default");
        if (defp != NULL) {
            m = defp;
            break;
        }
    }
    if (m != NULL)
        bs_lc_resolve(m, cur_dim, out, cap);
}

/* Current dimension set: resolve the profile-mapped key. */
static void
lc_resolve_dim(const cJSON *v, const char *cur_dim, char *out, size_t cap)
{
    const char *want = bs_lc_prof_val(cur_dim);
    const char *k = bs_lc_pick_key(v, want);
    if (k != NULL) {
        const cJSON *vp = cJSON_GetObjectItemCaseSensitive(v, k);
        if (vp != NULL)
            bs_lc_resolve(vp, cur_dim, out, cap);
    }
}

void
bs_lc_resolve(const cJSON *v, const char *cur_dim, char *out, size_t cap)
{
    if (cap == 0)
        return;
    out[0] = '\0';
    if (v == NULL)
        return;
    if (cJSON_IsString(v)) {
        snprintf(out, cap, "%s", v->valuestring);
        return;
    }
    if (!cJSON_IsObject(v))
        return;
    for (int d = 0; d < BS_LC_NDIMS; d++) {
        const cJSON *vp = cJSON_GetObjectItemCaseSensitive(v, bs_lc_dims[d]);
        if (vp != NULL) {
            bs_lc_resolve(vp, bs_lc_dims[d], out, cap);
            return;
        }
    }
    if (!cur_dim) {
        lc_resolve_fallback(v, out, cap);
        return;
    }
    if (strcmp(cur_dim, "globalcfg") == 0) {
        lc_resolve_globalcfg(v, cur_dim, out, cap);
        return;
    }
    lc_resolve_dim(v, cur_dim, out, cap);
}

int
bs_lc_resolve_bool(const cJSON *v)
{
    if (v != NULL && cJSON_IsBool(v))
        return cJSON_IsTrue(v);
    char buf[8];
    bs_lc_resolve(v, NULL, buf, sizeof buf);
    return buf[0] != '0';
}

/* -- file reader -------------------------------------------------------- */

char *
bs_read_text_file(const char *path)
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
    // NOLINTNEXTLINE(clang-analyzer-security.ArrayBound) — nr <= sz (fread caps) and buf is sz+1 bytes.
    buf[nr] = '\0';
    return buf;
}

/* -- token translation -------------------------------------------------- */

const char *
bs_lc_token_en(const char *tok)
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
        {"@Users", "Users"},
        {"@Empty", "Empty"},
    };
    for (size_t i = 0; i < sizeof tab / sizeof tab[0]; i++) {
        if (strcmp(tok, tab[i].k) == 0)
            return tab[i].v;
    }
    return NULL;
}

void
bs_lc_translate(const char *raw, char *out, size_t cap)
{
    if (!raw || !*raw || cap == 0) {
        if (cap)
            out[0] = '\0';
        return;
    }
    if (raw[0] == '@') {
        const char *en = bs_lc_token_en(raw);
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

BsLauncherItem bs_g_launcher_items[BS_LAUNCHER_MAX_ITEMS];
int          bs_g_launcher_count;
int          bs_g_launcher_body_h;
int          bs_g_launcher_built;

/* Lay every item out in one continuous column (headers span the full
 * width, app cells flow three per row).  The overlay scrolls this column
 * vertically; nothing is paginated, so a group heading can never clip
 * the last row of the previous group. */
void
bs_launcher_layout(void)
{
    int w = ScreenWidth();
    int cell_w = (w - 2 * BS_LAUNCHER_MARGIN) / BS_LAUNCHER_COLS;
    int col = 0;
    int y = 0;

    for (int i = 0; i < bs_g_launcher_count; i++) {
        BsLauncherItem *it = &bs_g_launcher_items[i];
        if (it->kind == 0) {
            if (col > 0) {
                /* Finish a partial row before the next heading so the
                 * heading never overlaps the previous group's tiles. */
                y += BS_LAUNCHER_CELL_H;
                col = 0;
            }
            it->x = BS_LAUNCHER_MARGIN;
            it->y = y;
            it->w = w - 2 * BS_LAUNCHER_MARGIN;
            it->h = BS_LAUNCHER_GROUP_H;
            y += BS_LAUNCHER_GROUP_H;
        } else {
            if (col >= BS_LAUNCHER_COLS) {
                col = 0;
                y += BS_LAUNCHER_CELL_H;
            }
            it->x = BS_LAUNCHER_MARGIN + col * cell_w;
            it->y = y;
            it->w = cell_w;
            it->h = BS_LAUNCHER_CELL_H;
            col++;
        }
    }
    if (col > 0)
        y += BS_LAUNCHER_CELL_H;
    bs_g_launcher_body_h = y;
}

/* Resolve the item's display title from the "title" entry, falling back
 * to the raw app id when empty. */
static void
launcher_set_title(BsLauncherItem *it, const cJSON *def, const char *id)
{
    const cJSON *tp = cJSON_GetObjectItemCaseSensitive(def, "title");
    if (tp != NULL) {
        char raw[64];
        bs_lc_resolve(tp, NULL, raw, sizeof raw);
        bs_lc_translate(raw, it->text, sizeof it->text);
    }
    if (!it->text[0])
        snprintf(it->text, sizeof it->text, "%s", id);
}

/* Copy the optional "params"/"param" argument list into the item. */
static void
launcher_set_params(BsLauncherItem *it, const cJSON *def)
{
    const cJSON *par = cJSON_GetObjectItemCaseSensitive(def, "params");
    if (!cJSON_IsArray(par))
        par = cJSON_GetObjectItemCaseSensitive(def, "param");
    if (cJSON_IsArray(par)) {
        const cJSON *q;
        cJSON_ArrayForEach(q, par) {
            if (it->nparams >= BS_LAUNCHER_MAX_PARAMS)
                break;
            if (cJSON_IsString(q))
                snprintf(it->params[it->nparams++], BS_LAUNCHER_PARAM_LEN,
                         "%s", q->valuestring);
        }
    } else if (cJSON_IsString(par)) {
        snprintf(it->params[0], BS_LAUNCHER_PARAM_LEN, "%s", par->valuestring);
        it->nparams = 1;
    }
}

void
bs_launcher_add_app(const cJSON *apps, const char *id)
{
    if (bs_g_launcher_count >= BS_LAUNCHER_MAX_ITEMS)
        return;
    const cJSON *def = cJSON_GetObjectItemCaseSensitive(apps, id);
    if (!cJSON_IsObject(def))
        return;
    const cJSON *vis = cJSON_GetObjectItemCaseSensitive(def, "visible");
    if (vis != NULL && !bs_lc_resolve_bool(vis))
        return;
    BsLauncherItem *it = &bs_g_launcher_items[bs_g_launcher_count];
    memset(it, 0, sizeof *it);
    it->kind = 1;
    launcher_set_title(it, def, id);
    const cJSON *pp = cJSON_GetObjectItemCaseSensitive(def, "path");
    if (pp != NULL)
        bs_lc_resolve(pp, NULL, it->path, sizeof it->path);
    const cJSON *ip = cJSON_GetObjectItemCaseSensitive(def, "icon");
    if (ip != NULL)
        bs_lc_resolve(ip, NULL, it->icon, sizeof it->icon);
    launcher_set_params(it, def);
    bs_g_launcher_count++;
}

/* 1 when an app item with the given path is already in the launcher
 * list (the firmware's GetUnregisteredUserApplication() matches user
 * apps by full path the same way). */
static int
launcher_has_path(const char *path)
{
    for (int i = 0; i < bs_g_launcher_count; i++) {
        if (bs_g_launcher_items[i].kind == 1 && strcmp(bs_g_launcher_items[i].path, path) == 0)
            return 1;
    }
    return 0;
}

/* 1 when a user-apps group header is already present. */
static int
launcher_has_user_header(void)
{
    for (int i = 0; i < bs_g_launcher_count; i++) {
        if (bs_g_launcher_items[i].kind == 0 && (strcmp(bs_g_launcher_items[i].text, "User") == 0 ||
                                              strcmp(bs_g_launcher_items[i].text, "Users") == 0))
            return 1;
    }
    return 0;
}

/* Register user-installed apps from /mnt/ext1/applications that the
 * firmware has not (yet) recorded in view.json.  This mirrors the stock
 * bookshelf's AppDataManager::scanUnregisteredUserApplication(): it
 * walks the directory for *.app files (regular files or symlinks) and
 * appends each one that is not already in the list by path, under a
 * "Users" group header (the firmware's "@Users" group).  Without this,
 * freshly installed apps never show up until the firmware's own
 * bookshelf has run and rewritten the desktop JSONs. */
void
bs_launcher_scan_ext1_apps(void)
{
    DIR *d = opendir("/mnt/ext1/applications");
    if (d == NULL)
        return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        size_t len = strlen(e->d_name);
        if (len <= 4)
            continue;
        if (strcasecmp(e->d_name + len - 4, ".app") != 0)
            continue;
        char path[BS_MAX_PATH_LEN];
        snprintf(path, sizeof path, "/mnt/ext1/applications/%s", e->d_name);
        struct stat st;
        if (iv_stat(path, &st) != 0)
            continue;
        if (launcher_has_path(path))
            continue;
        if (!launcher_has_user_header() && bs_g_launcher_count < BS_LAUNCHER_MAX_ITEMS) {
            BsLauncherItem *hdr = &bs_g_launcher_items[bs_g_launcher_count++];
            memset(hdr, 0, sizeof *hdr);
            hdr->kind = 0;
            snprintf(hdr->text, sizeof hdr->text, "Users");
        }
        if (bs_g_launcher_count >= BS_LAUNCHER_MAX_ITEMS)
            break;
        BsLauncherItem *it = &bs_g_launcher_items[bs_g_launcher_count];
        memset(it, 0, sizeof *it);
        it->kind = 1;
        size_t tl = len - 4;
        if (tl >= sizeof it->text)
            tl = sizeof it->text - 1;
        memcpy(it->text, e->d_name, tl);
        it->text[tl] = '\0';
        snprintf(it->path, sizeof it->path, "%s", path);
        bs_g_launcher_count++;
    }
    closedir(d);
}

/* The launcher's app list comes from the platform backend behind the
 * seam (bs_plat_launcher_build): on PocketBook it is the firmware's
 * view.json + apps_db.json + the /mnt/ext1/applications scan (this
 * function); on PC it is the freedesktop .desktop files.  The UI
 * (layout / draw / tap) below is platform-independent. */
void
bs_launcher_build(void)
{
    bs_g_launcher_count = 0;
    bs_g_launcher_body_h = 0;
    bs_g_launcher_count = bs_plat_launcher_build(
        bs_g_launcher_items, BS_LAUNCHER_MAX_ITEMS);
    bs_launcher_layout();
    bs_g_launcher_built = 1;
    bs_LOG("[bookshelf] launcher built: %d items, %d body height\n",
        bs_g_launcher_count,
        bs_g_launcher_body_h);
}

/* Add every app listed under a view.json "groups" array entry, with a
 * group header row when the group has a resolved title. */
static void
pb_build_groups(const cJSON *groups, const cJSON *db_apps)
{
    const cJSON *g;
    cJSON_ArrayForEach(g, groups) {
        if (!cJSON_IsObject(g))
            continue;
        const cJSON *tp = cJSON_GetObjectItemCaseSensitive(g, "title");
        char        raw_title[64] = "";
        char        disp_title[64] = "";
        if (tp != NULL) {
            bs_lc_resolve(tp, NULL, raw_title, sizeof raw_title);
            bs_lc_translate(raw_title, disp_title, sizeof disp_title);
        }
        const cJSON *apps_arr = cJSON_GetObjectItemCaseSensitive(g, "apps");
        if (cJSON_IsArray(apps_arr)) {
            if (bs_g_launcher_count < BS_LAUNCHER_MAX_ITEMS && disp_title[0]) {
                BsLauncherItem *hdr = &bs_g_launcher_items[bs_g_launcher_count++];
                memset(hdr, 0, sizeof *hdr);
                hdr->kind = 0;
                snprintf(hdr->text, sizeof hdr->text, "%s", disp_title);
            }
            const cJSON *a;
            cJSON_ArrayForEach(a, apps_arr) {
                if (cJSON_IsString(a) && a->valuestring != NULL)
                    bs_launcher_add_app(db_apps, a->valuestring);
            }
        }
    }
}

/* Add a single U_* user app from view.json to the launcher list, filling
 * in its title/path/icon (falling back to the key as the title). */
static void
pb_build_user_app(const cJSON *item, const char *key)
{
    BsLauncherItem *li = &bs_g_launcher_items[bs_g_launcher_count];
    memset(li, 0, sizeof *li);
    li->kind = 1;
    if (cJSON_IsObject(item)) {
        const cJSON *tp2 = cJSON_GetObjectItemCaseSensitive(item, "title");
        if (tp2 != NULL)
            bs_lc_resolve(tp2, NULL, li->text, sizeof li->text);
        const cJSON *pp2 = cJSON_GetObjectItemCaseSensitive(item, "path");
        if (pp2 != NULL)
            bs_lc_resolve(pp2, NULL, li->path, sizeof li->path);
        const cJSON *ip2 = cJSON_GetObjectItemCaseSensitive(item, "icon");
        if (ip2 != NULL)
            bs_lc_resolve(ip2, NULL, li->icon, sizeof li->icon);
    }
    if (!li->text[0])
        snprintf(li->text, sizeof li->text, "%s", key);
    bs_g_launcher_count++;
}

/* Scan view.json applications for U_* user apps not in any group and add
 * each visible one, under a "Users" header. */
static void
pb_build_vw_apps(const cJSON *vw_apps)
{
    int user_hdr_added = 0;
    const cJSON *it;
    cJSON_ArrayForEach(it, vw_apps) {
        const char *key = it->string;
        if (key == NULL || key[0] != 'U' || key[1] != '_')
            continue;
        int vis = 1;
        if (cJSON_IsObject(it)) {
            const cJSON *v2 = cJSON_GetObjectItemCaseSensitive(it, "visible");
            if (v2 != NULL && !bs_lc_resolve_bool(v2))
                vis = 0;
        }
        if (!vis)
            continue;
        if (!user_hdr_added && bs_g_launcher_count < BS_LAUNCHER_MAX_ITEMS) {
            BsLauncherItem *hdr = &bs_g_launcher_items[bs_g_launcher_count++];
            memset(hdr, 0, sizeof *hdr);
            hdr->kind = 0;
            snprintf(hdr->text, sizeof hdr->text, "Users");
            user_hdr_added = 1;
        }
        if (bs_g_launcher_count >= BS_LAUNCHER_MAX_ITEMS)
            continue;
        char id[48];
        snprintf(id, sizeof id, "%s", key);
        pb_build_user_app(it, id);
    }
}

void
bs_launcher_build_pb(void)
{
    bs_g_launcher_count = 0;
    bs_g_launcher_body_h = 0;

    char *db_txt = bs_read_text_file("/mnt/ext1/system/config/desktop/apps_db.json");
    if (!db_txt)
        db_txt = bs_read_text_file("/ebrmain/config/desktop/apps_db.json");
    char *vw_txt = bs_read_text_file("/mnt/ext1/system/config/desktop/view.json");
    if (!vw_txt)
        vw_txt = bs_read_text_file("/ebrmain/config/desktop/view.json");

    cJSON *db = db_txt ? cJSON_Parse(db_txt) : NULL;
    cJSON *vw = vw_txt ? cJSON_Parse(vw_txt) : NULL;
    free(db_txt);
    free(vw_txt);

    const cJSON *db_apps = db ? cJSON_GetObjectItemCaseSensitive(db, "applications") : NULL;
    if (db == NULL || vw == NULL || !cJSON_IsObject(db_apps)) {
        cJSON_Delete(db);
        cJSON_Delete(vw);
        return;
    }

    const cJSON *view_obj = cJSON_GetObjectItemCaseSensitive(vw, "view");
    const cJSON *groups = cJSON_IsObject(view_obj)
        ? cJSON_GetObjectItemCaseSensitive(view_obj, "groups")
        : NULL;
    if (cJSON_IsArray(groups))
        pb_build_groups(groups, db_apps);

    /* Scan view.json applications for U_* user apps not in any group. */
    const cJSON *vw_apps = cJSON_GetObjectItemCaseSensitive(vw, "applications");
    if (cJSON_IsObject(vw_apps))
        pb_build_vw_apps(vw_apps);

    /* Register user apps from /mnt/ext1/applications that the firmware
     * has not recorded in view.json yet (same scan the stock bookshelf
     * runs in AppDataManager::scanUnregisteredUserApplication). */
    bs_launcher_scan_ext1_apps();

    cJSON_Delete(db);
    cJSON_Delete(vw);
}

/* -- launcher draw ------------------------------------------------------ */

/* Decoded-icon cache.  A launcher drag repaints ~15 icons per
 * POINTERMOVE; decoding each PNG/GetResource from flash every frame is
 * the dominant cost.  Cache the decoded ibitmap keyed by icon name in a
 * small fixed-size LRU (same shape as the cover slots) so each icon is
 * decoded at most once per session.  Like the cover slots, the decoded
 * bitmaps are never explicitly freed — the SDK exposes no bitmap free
 * API and libinkview bitmaps are reclaimed at process exit, so the
 * cache just drops references on eviction. */
#define BS_LAUNCHER_ICON_CACHE 16

typedef struct {
    char      name[64]; /* LauncherItem.icon (max 63 chars + NUL) */
    ibitmap  *bm;
    int       age; /* monotonically increasing LRU stamp */
} BsLauncherIconSlot;

static BsLauncherIconSlot g_icon_cache[BS_LAUNCHER_ICON_CACHE];
static int              g_icon_cache_age;

static ibitmap *
launcher_icon_get(const char *name)
{
    ibitmap *bm = NULL;
    if (name != NULL && name[0] != '\0') {
        if (name[0] != '/')
            bm = GetResource(name, NULL);
        if (bm == NULL && name[0] == '/')
            bm = LoadPNG(name, 0);
    }
    return bm;
}

/* Clear the cache at teardown/exit.  The SDK has no bitmap free API, so
 * this only drops the references (the libinkview bitmaps are reclaimed
 * by process exit, exactly like the cover slots). */
void
bs_launcher_icons_free(void)
{
    for (int i = 0; i < BS_LAUNCHER_ICON_CACHE; i++) {
        g_icon_cache[i].bm = NULL;
        g_icon_cache[i].name[0] = '\0';
        g_icon_cache[i].age = 0;
    }
    g_icon_cache_age = 0;
}

/* Find a cached decode of icon_name; on a hit bump its LRU stamp and
 * return it, else NULL. */
static ibitmap *
launcher_cache_find(const char *icon_name)
{
    for (int i = 0; i < BS_LAUNCHER_ICON_CACHE; i++) {
        if (g_icon_cache[i].bm != NULL &&
            strcmp(g_icon_cache[i].name, icon_name) == 0) {
            g_icon_cache[i].age = ++g_icon_cache_age;
            return g_icon_cache[i].bm;
        }
    }
    return NULL;
}

/* Decode icon_name and insert it into the LRU cache, evicting the
 * least-recently-used slot.  Returns the decoded bitmap or NULL if it
 * could not be decoded. */
static ibitmap *
launcher_cache_insert(const char *icon_name)
{
    ibitmap *bm = launcher_icon_get(icon_name);
    if (bm == NULL)
        return NULL;
    int slot = 0;
    for (int i = 1; i < BS_LAUNCHER_ICON_CACHE; i++) {
        if (g_icon_cache[i].bm == NULL) {
            slot = i;
            break;
        }
        if (g_icon_cache[slot].bm == NULL ||
            g_icon_cache[i].age < g_icon_cache[slot].age)
            slot = i;
    }
    snprintf(g_icon_cache[slot].name, sizeof g_icon_cache[slot].name,
             "%s", icon_name);
    g_icon_cache[slot].bm = bm;
    g_icon_cache[slot].age = ++g_icon_cache_age;
    return bm;
}

/* Center the bitmap inside the icon box, scaling down any oversized icon
 * aspect-preserving. */
static void
launcher_draw_bitmap(ibitmap *bm, int x0, int y0, int sz)
{
    int bw = bm->width;
    int bh = bm->height;
    if (bw > sz || bh > sz) {
        if (bw > bh) {
            bh = bh * sz / bw;
            bw = sz;
        } else {
            bw = bw * sz / bh;
            bh = sz;
        }
        StretchBitmap(x0 + (sz - bw) / 2, y0 + (sz - bh) / 2, bw, bh, bm, STRETCH);
    } else {
        DrawBitmap(x0 + (sz - bw) / 2, y0 + (sz - bh) / 2, bm);
    }
}

/* No icon available: draw an empty placeholder box with a centred
 * single-letter glyph taken from the first title character. */
static void
launcher_draw_placeholder(int x0, int y0, int sz, int cx, int cy,
                          const char *title)
{
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

void
bs_draw_launcher_icon(int cx, int cy, const char *icon_name, const char *title)
{
    int      sz = BS_LAUNCHER_ICON_SZ;
    int      x0 = cx - sz / 2;
    int      y0 = cy - sz / 2;
    ibitmap *bm = NULL;
    if (icon_name && icon_name[0]) {
        /* LRU hit: reuse the cached decode, bump its stamp. */
        bm = launcher_cache_find(icon_name);
        if (bm == NULL)
            bm = launcher_cache_insert(icon_name);
    }
    if (bm) {
        launcher_draw_bitmap(bm, x0, y0, sz);
        return;
    }
    launcher_draw_placeholder(x0, y0, sz, cx, cy, title);
}

/* Scrollable body height: when the column overflows, reserve the corner
 * scroll-button band so the last row never sits underneath the buttons
 * (the log viewer reserves the same band).  A column that fits keeps
 * the full height and draws no buttons. */
static int
launcher_body_h(void)
{
    int body_h = bs_content_bottom() - BS_OVERLAY_HEADER_H;
    if (bs_g_launcher_body_h - body_h > 0)
        body_h -= BS_SCROLL_BTN_H;
    if (body_h < 0)
        body_h = 0;
    return body_h;
}

/* Centred "launcher.empty" hint drawn when the launcher has no items. */
static void
launcher_draw_empty(int w, int body_top, int body_h)
{
    ifont *ef = OpenFont(DEFAULTFONT, 32, 0);
    if (ef) {
        SetFont(ef, BLACK);
        const char *empty = bs_i18n("launcher.empty");
        int         tw = StringWidth(empty);
        DrawString((w - tw) / 2, body_top + body_h / 2, empty);
        CloseFont(ef);
    }
}

/* Draw a group heading row (band + baseline rule + title). */
static void
launcher_draw_heading(ifont *hf, const BsLauncherItem *it, int sy)
{
    FillArea(it->x, sy, it->w, it->h, WHITE);
    DrawLine(it->x, sy + it->h - 1, it->x + it->w, sy + it->h - 1, BLACK);
    if (hf) {
        SetFont(hf, BLACK);
        DrawString(it->x + 12, sy + (it->h - 28) / 2 - 2, it->text);
    }
}

/* Draw an app cell label under the icon, wrapping at the last space or
 * truncating to 20 chars when there is no space to break on. */
static void
launcher_draw_app_label(ifont *af, const BsLauncherItem *it, int cx, int sy)
{
    SetFont(af, BLACK);
    int ly = sy + 12 + BS_LAUNCHER_ICON_SZ + 8;
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

void
bs_draw_overlay_launcher(void)
{
    int w = ScreenWidth();
    int h = bs_content_bottom();
    int body_top = BS_OVERLAY_HEADER_H;
    int body_h = launcher_body_h();

    /* Clamp the scroll offset: the column's last row stops at the bottom
     * edge; a column shorter than the window never scrolls. */
    int max_scroll = bs_g_launcher_body_h - body_h;
    if (max_scroll < 0)
        max_scroll = 0;
    if (bs_g_state.launcher_scroll < 0)
        bs_g_state.launcher_scroll = 0;
    if (bs_g_state.launcher_scroll > max_scroll)
        bs_g_state.launcher_scroll = max_scroll;
    int scroll = bs_g_state.launcher_scroll;

    FillArea(0, 0, w, bs_content_bottom(), WHITE);

    /* Shared overlay header: Back chevron + centred title. */
    bs_draw_overlay_header(bs_i18n("launcher.title"));

    /* Scrollable body, clipped so rows never bleed into the header. */
    SetClip(0, body_top, w, body_h);
    if (bs_g_launcher_count == 0)
        launcher_draw_empty(w, body_top, body_h);

    ifont *hf = OpenFont(DEFAULTFONTB, 28, 0);
    ifont *af = OpenFont(DEFAULTFONT, 24, 0);
    /* SetClip is not reliable on every SDK/emulator path, so rows must
     * fit the visible body outright: a row whose bottom would spill
     * past the reserved scroll-button band is skipped until it scrolls
     * into view (page scrolls align rows to the body). */
    int body_bottom = body_top + body_h;
    for (int i = 0; i < bs_g_launcher_count; i++) {
        const BsLauncherItem *it = &bs_g_launcher_items[i];
        int                 sy = it->y - scroll + body_top;
        if (sy + it->h <= body_top || sy + it->h > body_bottom)
            continue;
        if (it->kind == 0) {
            launcher_draw_heading(hf, it, sy);
        } else {
            int cx = it->x + it->w / 2;
            int icon_cy = sy + 12 + BS_LAUNCHER_ICON_SZ / 2;
            bs_draw_launcher_icon(cx, icon_cy, it->icon, it->text);
            if (af)
                launcher_draw_app_label(af, it, cx, sy);
        }
    }
    SetClip(0, 0, w, h);
    /* Stock corner scroll buttons while the column overflows.  Drawn
     * after the clip reset: the body clip (SetClip above) would
     * otherwise cut them — the button band sits below the body. */
    bs_draw_scroll_buttons(scroll > 0, scroll < max_scroll);
    if (hf)
        CloseFont(hf);
    if (af)
        CloseFont(af);
}

/* -- launcher hit-test + actions ---------------------------------------- */

void
bs_launch_app(const BsLauncherItem *it)
{
    if (!it->path[0])
        return;
    const char *base = strrchr(it->path, '/');
    base = base ? base + 1 : it->path;
    char *args[BS_LAUNCHER_MAX_PARAMS + 2];
    int   ai = 0;
    args[ai++] = (char *)it->path;
    for (int i = 0; i < it->nparams && ai < BS_LAUNCHER_MAX_PARAMS + 1; i++)
        args[ai++] = (char *)it->params[i];
    args[ai] = NULL;
    bs_LOG("[bookshelf] launching app path=%s base=%s params=%d\n", it->path, base, it->nparams);
    /*
     * Draw a centered hourglass and leave it up while the app starts; on
     * PocketBook the launched task (TASK_MAKEACTIVE, see bs_plat_pb.c
     * bs_plat_launch_app) overwrites it once it becomes the foreground task
     * and draws.  The caller suppresses the shelf redraw for this path, so
     * the screen freezes on the hourglass instead of falling back to a
     * static shelf that makes a slow launch look like a no-op.  The actual
     * launch (NewTaskEx on PB, fork/exec on PC) is behind the platform seam.
     */
    bs_show_hourglass();
    if (bs_plat_launch_app(it, args, ai) < 0) {
        /* Launch failed: drop the hourglass and bring the launcher back so
         * the user is not stuck staring at an indefinite spinner. */
        HideHourglass();
        bs_launcher_open_set();
    }
}

/* Handle a tap on a corner scroll button: page the column by one body
 * height.  Returns 1 when a button was hit (and a redraw was issued). */
static int
launcher_tap_scroll(int x, int y)
{
    int dir = bs_hit_scroll_button(x, y);
    if (dir == 0)
        return 0;
    int body_h = launcher_body_h();
    int max_scroll = bs_g_launcher_body_h - body_h;
    if (max_scroll < 0)
        max_scroll = 0;
    bs_g_state.launcher_scroll += dir * body_h;
    if (bs_g_state.launcher_scroll < 0)
        bs_g_state.launcher_scroll = 0;
    if (bs_g_state.launcher_scroll > max_scroll)
        bs_g_state.launcher_scroll = max_scroll;
    bs_draw_overlay_launcher();
    bs_flush_content();
    return 1;
}

/* Find the tapped app cell under (x, by) and launch it.  Returns 1 when
 * an app was launched. */
static int
launcher_tap_app(int x, int by)
{
    for (int i = 0; i < bs_g_launcher_count; i++) {
        const BsLauncherItem *it = &bs_g_launcher_items[i];
        if (it->kind != 1)
            continue;
        if (x >= it->x && x < it->x + it->w && by >= it->y && by < it->y + it->h) {
            /* Launch the app.  Close the launcher state WITHOUT redrawing
             * the shelf: launch_app() puts up a centered hourglass that
             * stays until the launched task draws.  A redraw here would
             * flash the shelf back and make a slow app start look like the
             * tap did nothing. */
            bs_g_state.overlay = BS_OV_NONE;
            bs_g_state.launcher_drag = 0;
            bs_g_state.launcher_moved = 0;
            bs_launch_app(it);
            return 1;
        }
    }
    return 0;
}

void
bs_on_tap_overlay_launcher(int x, int y)
{
    int body_top = BS_OVERLAY_HEADER_H;
    int rx, ry, rw, rh;
    bs_overlay_back_rect(&rx, &ry, &rw, &rh);
    if (x >= rx && x < rx + rw && y >= ry && y < ry + rh) {
        bs_launcher_close();
        return;
    }
    /* Corner scroll buttons: page up/down the column. */
    if (launcher_tap_scroll(x, y))
        return;
    if (y < body_top || y >= bs_content_bottom())
        return;
    int by = y - body_top + bs_g_state.launcher_scroll;
    launcher_tap_app(x, by);
}

void
bs_launcher_open_set(void)
{
    if (!bs_g_launcher_built)
        bs_launcher_build();
    bs_g_state.overlay = BS_OV_LAUNCHER;
    bs_g_state.launcher_scroll = 0;
    bs_g_state.launcher_drag = 0;
    bs_g_state.launcher_moved = 0;
    bs_draw_overlay_launcher();
    bs_flush_content();
}

void
bs_launcher_close(void)
{
    bs_g_state.overlay = BS_OV_NONE;
    bs_g_state.launcher_drag = 0;
    bs_g_state.launcher_moved = 0;
    bs_redraw_shelf();
}

/* Pop out of a drilled-in series back to the collapsed top-level grid. */
void
bs_drill_back(void)
{
    bs_g_drilled_series[0] = '\0';
    bs_g_state.page = bs_g_state.saved_page;
    bs_view_rebuild();
    bs_LOG("[bookshelf] drilled back to top level (view=%d)\n", bs_g_view_total);
    FillArea(0, 0, ScreenWidth(), bs_content_bottom(), WHITE);
    bs_draw_top_bar();
    bs_draw_grid();
    bs_draw_pager();
    bs_flush_content();
}

void
bs_on_tap_thumbnail(int vi)
{
    BsTileRow tr;
    if (!bs_view_fetch_row(vi, &tr))
        return;

    /* A card is either a series stack (All books) or a dimension-group
     * stack.  Both drill in. */
    if (tr.is_series) {
        if (bs_group_active()) {
            bs_group_drill(tr.series_id); /* series_id = raw group value */
            return;
        }
        snprintf(bs_g_drilled_series, sizeof bs_g_drilled_series, "%s", tr.series_id);
        bs_g_state.saved_page = bs_g_state.page;
        bs_g_state.page = 0;
        bs_view_rebuild();
        bs_LOG("[bookshelf] drilled into series '%s' (%d books)\n", tr.series_name, bs_g_view_total);
        FillArea(0, 0, ScreenWidth(), bs_content_bottom(), WHITE);
        bs_draw_top_bar();
        bs_draw_grid();
        bs_draw_pager();
        bs_flush_content();
        return;
    }

    /* Flat tile → download (if needed) then open in the configured reader. */
    bs_book_press_action(&tr.book);
}
