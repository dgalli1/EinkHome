/* eh_plat_pb_launcher.c — PocketBook launcher data source (app/platform/).
 *
 * Implements the launcher side of the platform seam for the PocketBook
 * backend: the app-grid items come from the firmware's view.json +
 * apps_db.json desktop configs plus a scan of /mnt/ext1/applications for
 * *.app entries (AppDataManager::scanUnregisteredUserApplication).  This
 * parser (the eh_lc_* conditional-resolution engine, the @Token i18n
 * table, the pb_build_* walk, the user-app scan) is PocketBook-firmware
 * knowledge and lives HERE, not in the neutral app/action/eh_launcher.c
 * (whose layout/draw/tap code is platform-independent).  The SDL backend
 * has its own freedesktop .desktop source (eh_plat_sdl.c). */

#include "eh_core.h"
#include "cJSON.h"
#include "eh_launcher.h"

#include <dirent.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>

/* The caller-provided item buffer (eh_plat_launcher_build's items/cap)
 * the parser fills, threaded through the helpers.  The original parser
 * wrote the app's globals directly; targeting the passed array matches
 * the SDL backend's contract (callers own the array). */
static BsLauncherItem *g_lc_items;
static int g_lc_cap;
static int g_lc_count;

/* -- device profile for conditional resolution -------------------------- */

static const char *const eh_lc_dims[] = {
    "device",
    "partner",
    "has_audio",
    "has_cloud",
    "language",
    "localization",
    "globalcfg",
};

static const char *
eh_lc_prof_val(const char *dim)
{
    if (strcmp(dim, "device") == 0)
        return eh_g_lcprof.device;
    if (strcmp(dim, "partner") == 0)
        return eh_g_lcprof.partner;
    if (strcmp(dim, "has_audio") == 0)
        return eh_g_lcprof.has_audio;
    if (strcmp(dim, "has_cloud") == 0)
        return eh_g_lcprof.has_cloud;
    if (strcmp(dim, "language") == 0)
        return eh_g_lcprof.language;
    if (strcmp(dim, "localization") == 0)
        return eh_g_lcprof.localization;
    return NULL;
}

static const char *
eh_lc_pick_key(const cJSON *obj, const char *want)
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

/* Forward declaration: the conditional-resolution engine is defined
 * below (after the profile-helper functions) but lc_resolve_fallback /
 * lc_resolve_globalcfg / lc_resolve_dim call it before its definition. */
static void eh_lc_resolve(const cJSON *v, const char *cur_dim, char *out,
                          size_t cap);

/* No current dimension: pick a fallback key from the object and resolve
 * it with a NULL dimension. */
static void
lc_resolve_fallback(const cJSON *v, char *out, size_t cap)
{
    const char *k = eh_lc_pick_key(v, NULL);
    if (k != NULL) {
        const cJSON *vp = cJSON_GetObjectItemCaseSensitive(v, k);
        if (vp != NULL)
            eh_lc_resolve(vp, NULL, out, cap);
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
        eh_lc_resolve(m, cur_dim, out, cap);
}

/* Current dimension set: resolve the profile-mapped key. */
static void
lc_resolve_dim(const cJSON *v, const char *cur_dim, char *out, size_t cap)
{
    const char *want = eh_lc_prof_val(cur_dim);
    const char *k = eh_lc_pick_key(v, want);
    if (k != NULL) {
        const cJSON *vp = cJSON_GetObjectItemCaseSensitive(v, k);
        if (vp != NULL)
            eh_lc_resolve(vp, cur_dim, out, cap);
    }
}

static void
eh_lc_resolve(const cJSON *v, const char *cur_dim, char *out, size_t cap)
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
    for (int d = 0; d < (int)(sizeof eh_lc_dims / sizeof eh_lc_dims[0]); d++) {
        const cJSON *vp = cJSON_GetObjectItemCaseSensitive(v, eh_lc_dims[d]);
        if (vp != NULL) {
            eh_lc_resolve(vp, eh_lc_dims[d], out, cap);
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

static int
eh_lc_resolve_bool(const cJSON *v)
{
    if (v != NULL && cJSON_IsBool(v))
        return cJSON_IsTrue(v);
    char buf[8];
    eh_lc_resolve(v, NULL, buf, sizeof buf);
    /* Explicit falsey spellings resolve to false; an empty value (a
     * missing key) and any other value stay TRUE (present/visible),
     * matching the old buf[0] != '0' default. */
    static const char *const falsey[] = {"0", "false", "no", "off"};
    for (size_t i = 0; i < sizeof falsey / sizeof falsey[0]; i++) {
        if (strcasecmp(buf, falsey[i]) == 0)
            return 0;
    }
    return 1;
}

/* -- token translation -------------------------------------------------- */

static const char *
eh_lc_token_en(const char *tok)
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

static void
eh_lc_translate(const char *raw, char *out, size_t cap)
{
    if (!raw || !*raw || cap == 0) {
        if (cap)
            out[0] = '\0';
        return;
    }
    if (raw[0] == '@') {
        const char *en = eh_lc_token_en(raw);
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

/* -- launcher data source (view.json + apps_db.json + *.app scan) ------- */

/* Resolve the item's display title from the "title" entry, falling back
 * to the raw app id when empty. */
static void
launcher_set_title(BsLauncherItem *it, const cJSON *def, const char *id)
{
    const cJSON *tp = cJSON_GetObjectItemCaseSensitive(def, "title");
    if (tp != NULL) {
        char raw[64];
        eh_lc_resolve(tp, NULL, raw, sizeof raw);
        eh_lc_translate(raw, it->text, sizeof it->text);
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
            if (it->nparams >= EH_LAUNCHER_MAX_PARAMS)
                break;
            if (cJSON_IsString(q))
                snprintf(it->params[it->nparams++], EH_LAUNCHER_PARAM_LEN,
                         "%s", q->valuestring);
        }
    } else if (cJSON_IsString(par)) {
        snprintf(it->params[0], EH_LAUNCHER_PARAM_LEN, "%s", par->valuestring);
        it->nparams = 1;
    }
}

static void
eh_launcher_add_app(const cJSON *apps, const char *id)
{
    if (g_lc_count >= g_lc_cap)
        return;
    const cJSON *def = cJSON_GetObjectItemCaseSensitive(apps, id);
    if (!cJSON_IsObject(def))
        return;
    const cJSON *vis = cJSON_GetObjectItemCaseSensitive(def, "visible");
    if (vis != NULL && !eh_lc_resolve_bool(vis))
        return;
    BsLauncherItem *it = &g_lc_items[g_lc_count];
    memset(it, 0, sizeof *it);
    it->kind = 1;
    launcher_set_title(it, def, id);
    const cJSON *pp = cJSON_GetObjectItemCaseSensitive(def, "path");
    if (pp != NULL)
        eh_lc_resolve(pp, NULL, it->path, sizeof it->path);
    const cJSON *ip = cJSON_GetObjectItemCaseSensitive(def, "icon");
    if (ip != NULL)
        eh_lc_resolve(ip, NULL, it->icon, sizeof it->icon);
    launcher_set_params(it, def);
    g_lc_count++;
}

/* 1 when an app item with the given path is already in the launcher
 * list (the firmware's GetUnregisteredUserApplication() matches user
 * apps by full path the same way). */
static int
launcher_has_path(const char *path)
{
    for (int i = 0; i < g_lc_count; i++) {
        if (g_lc_items[i].kind == 1 && strcmp(g_lc_items[i].path, path) == 0)
            return 1;
    }
    return 0;
}

/* 1 when a user-apps group header is already present. */
static int
launcher_has_user_header(void)
{
    for (int i = 0; i < g_lc_count; i++) {
        if (g_lc_items[i].kind == 0 && (strcmp(g_lc_items[i].text, "User") == 0 ||
                                     strcmp(g_lc_items[i].text, "Users") == 0))
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
static void
eh_launcher_scan_ext1_apps(void)
{
    const char *apps_dir = eh_plat_launcher_user_apps_dir();
    if (apps_dir == NULL)
        return;
    DIR *d = opendir(apps_dir);
    if (d == NULL)
        return;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        size_t len = strlen(e->d_name);
        if (len <= 4)
            continue;
        if (strcasecmp(e->d_name + len - 4, ".app") != 0)
            continue;
        char path[EH_MAX_PATH_LEN];
        snprintf(path, sizeof path, "%s/%s", apps_dir, e->d_name);
        struct stat st;
        if (iv_stat(path, &st) != 0)
            continue;
        if (launcher_has_path(path))
            continue;
        if (!launcher_has_user_header() && g_lc_count < g_lc_cap) {
            BsLauncherItem *hdr = &g_lc_items[g_lc_count++];
            memset(hdr, 0, sizeof *hdr);
            hdr->kind = 0;
            snprintf(hdr->text, sizeof hdr->text, "Users");
        }
        if (g_lc_count >= g_lc_cap)
            break;
        BsLauncherItem *it = &g_lc_items[g_lc_count];
        memset(it, 0, sizeof *it);
        it->kind = 1;
        size_t tl = len - 4;
        if (tl >= sizeof it->text)
            tl = sizeof it->text - 1;
        memcpy(it->text, e->d_name, tl);
        it->text[tl] = '\0';
        snprintf(it->path, sizeof it->path, "%s", path);
        g_lc_count++;
    }
    closedir(d);
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
            eh_lc_resolve(tp, NULL, raw_title, sizeof raw_title);
            eh_lc_translate(raw_title, disp_title, sizeof disp_title);
        }
        const cJSON *apps_arr = cJSON_GetObjectItemCaseSensitive(g, "apps");
        if (cJSON_IsArray(apps_arr)) {
            if (g_lc_count < g_lc_cap && disp_title[0]) {
                BsLauncherItem *hdr = &g_lc_items[g_lc_count++];
                memset(hdr, 0, sizeof *hdr);
                hdr->kind = 0;
                snprintf(hdr->text, sizeof hdr->text, "%s", disp_title);
            }
            const cJSON *a;
            cJSON_ArrayForEach(a, apps_arr) {
                if (cJSON_IsString(a) && a->valuestring != NULL)
                    eh_launcher_add_app(db_apps, a->valuestring);
            }
        }
    }
}

/* Add a single U_* user app from view.json to the launcher list, filling
 * in its title/path/icon (falling back to the key as the title). */
static void
pb_build_user_app(const cJSON *item, const char *key)
{
    BsLauncherItem *li = &g_lc_items[g_lc_count];
    memset(li, 0, sizeof *li);
    li->kind = 1;
    if (cJSON_IsObject(item)) {
        const cJSON *tp2 = cJSON_GetObjectItemCaseSensitive(item, "title");
        if (tp2 != NULL)
            eh_lc_resolve(tp2, NULL, li->text, sizeof li->text);
        const cJSON *pp2 = cJSON_GetObjectItemCaseSensitive(item, "path");
        if (pp2 != NULL)
            eh_lc_resolve(pp2, NULL, li->path, sizeof li->path);
        const cJSON *ip2 = cJSON_GetObjectItemCaseSensitive(item, "icon");
        if (ip2 != NULL)
            eh_lc_resolve(ip2, NULL, li->icon, sizeof li->icon);
    }
    if (!li->text[0])
        snprintf(li->text, sizeof li->text, "%s", key);
    g_lc_count++;
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
            if (v2 != NULL && !eh_lc_resolve_bool(v2))
                vis = 0;
        }
        if (!vis)
            continue;
        if (!user_hdr_added && g_lc_count < g_lc_cap) {
            BsLauncherItem *hdr = &g_lc_items[g_lc_count++];
            memset(hdr, 0, sizeof *hdr);
            hdr->kind = 0;
            snprintf(hdr->text, sizeof hdr->text, "Users");
            user_hdr_added = 1;
        }
        if (g_lc_count >= g_lc_cap)
            continue;
        char id[48];
        snprintf(id, sizeof id, "%s", key);
        pb_build_user_app(it, id);
    }
}

static void
pb_launcher_build(void)
{
    g_lc_count = 0;

    /* The desktop-config file locations are platform-owned (PB try
     * /mnt/ext1/system/config/desktop then /ebrmain/config/desktop). */
    char *db_txt = NULL;
    const char *const *db_paths = eh_plat_launcher_desktop_paths("db");
    for (const char *const *p = db_paths; p != NULL && *p != NULL && db_txt == NULL; p++)
        db_txt = eh_read_text_file(*p);
    char *vw_txt = NULL;
    const char *const *vw_paths = eh_plat_launcher_desktop_paths("view");
    for (const char *const *p = vw_paths; p != NULL && *p != NULL && vw_txt == NULL; p++)
        vw_txt = eh_read_text_file(*p);

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
    eh_launcher_scan_ext1_apps();

    cJSON_Delete(db);
    cJSON_Delete(vw);
}

/* Launcher desktop-config candidates, tried in order (the firmware
 * rewrites the desktop JSONs into both locations; the SDK vintages only
 * ship one). */
static const char *const g_lc_db_paths[] = {
    "/mnt/ext1/system/config/desktop/apps_db.json",
    "/ebrmain/config/desktop/apps_db.json",
    NULL,
};
static const char *const g_lc_view_paths[] = {
    "/mnt/ext1/system/config/desktop/view.json",
    "/ebrmain/config/desktop/view.json",
    NULL,
};

const char *const *
eh_plat_launcher_desktop_paths(const char *kind)
{
    if (kind != NULL && strcmp(kind, "view") == 0)
        return g_lc_view_paths;
    return g_lc_db_paths;
}

const char *
eh_plat_launcher_user_apps_dir(void)
{
    return "/mnt/ext1/applications";
}

int
eh_plat_launcher_build(BsLauncherItem *items, int cap)
{
    g_lc_items = items;
    g_lc_cap = cap;
    g_lc_count = 0;
    if (items != NULL && cap > 0)
        memset(items, 0, sizeof items[0] * (size_t)cap);
    pb_launcher_build();
    return g_lc_count;
}