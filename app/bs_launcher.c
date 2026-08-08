/* bs_launcher.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* -- app launcher ------------------------------------------------------- *
 * Reproduces the firmware's grouped application grid (the "Apps" screen
 * the original desktop renders from view.json + apps_db.json).  Since
 * bookshelf.app *is* the home-screen replacement, the original grid is
 * gone — this overlay restores it, resolving conditional visibility for
 * the current device profile (Era: touch + audio + en/WW + stock partner)
 * so the grid matches what the real device shows (e.g. Snake hidden on a
 * touch panel).  Tapping a tile launches the app via NewTaskEx. */

/* -- minimal JSON scanner ----------------------------------------------- */

const char *
js_skip_ws(const char *p)
{
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')
        p++;
    return p;
}

const char *
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

void
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

const char *
js_object_body(const char *p)
{
    p = js_skip_ws(p);
    return *p == '{' ? p + 1 : NULL;
}

const char *
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

const LcProfile g_lcprof = {"all", "pocketbook", "true", "false", "en", "WW"};

const char *const lc_dims[] = {
    "device",
    "partner",
    "has_audio",
    "has_cloud",
    "language",
    "localization",
    "globalcfg",
};

const char *
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

const char *
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

void
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
            ++p2;
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

int
lc_resolve_bool(const char *p)
{
    char buf[8];
    lc_resolve(p, NULL, buf, sizeof buf);
    return buf[0] != '0';
}

/* -- file reader -------------------------------------------------------- */

char *
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

const char *
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

LauncherItem g_launcher_items[LAUNCHER_MAX_ITEMS];
int          g_launcher_count;
int          g_launcher_body_h;
int          g_launcher_built;

/* Lay every item out in one continuous column (headers span the full
 * width, app cells flow three per row).  The overlay scrolls this column
 * vertically; nothing is paginated, so a group heading can never clip
 * the last row of the previous group. */
void
launcher_layout(void)
{
    int w = ScreenWidth();
    int cell_w = (w - 2 * LAUNCHER_MARGIN) / LAUNCHER_COLS;
    int col = 0;
    int y = 0;

    for (int i = 0; i < g_launcher_count; i++) {
        LauncherItem *it = &g_launcher_items[i];
        if (it->kind == 0) {
            if (col > 0) {
                /* Finish a partial row before the next heading so the
                 * heading never overlaps the previous group's tiles. */
                y += LAUNCHER_CELL_H;
                col = 0;
            }
            it->x = LAUNCHER_MARGIN;
            it->y = y;
            it->w = w - 2 * LAUNCHER_MARGIN;
            it->h = LAUNCHER_GROUP_H;
            y += LAUNCHER_GROUP_H;
        } else {
            if (col >= LAUNCHER_COLS) {
                col = 0;
                y += LAUNCHER_CELL_H;
            }
            it->x = LAUNCHER_MARGIN + col * cell_w;
            it->y = y;
            it->w = cell_w;
            it->h = LAUNCHER_CELL_H;
            col++;
        }
    }
    if (col > 0)
        y += LAUNCHER_CELL_H;
    g_launcher_body_h = y;
}

void
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

/* 1 when an app item with the given path is already in the launcher
 * list (the firmware's GetUnregisteredUserApplication() matches user
 * apps by full path the same way). */
static int
launcher_has_path(const char *path)
{
    for (int i = 0; i < g_launcher_count; i++) {
        if (g_launcher_items[i].kind == 1 && strcmp(g_launcher_items[i].path, path) == 0)
            return 1;
    }
    return 0;
}

/* 1 when a user-apps group header is already present. */
static int
launcher_has_user_header(void)
{
    for (int i = 0; i < g_launcher_count; i++) {
        if (g_launcher_items[i].kind == 0 && (strcmp(g_launcher_items[i].text, "User") == 0 ||
                                              strcmp(g_launcher_items[i].text, "Users") == 0))
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
launcher_scan_ext1_apps(void)
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
        char path[160];
        snprintf(path, sizeof path, "/mnt/ext1/applications/%s", e->d_name);
        struct stat st;
        if (iv_stat(path, &st) != 0)
            continue;
        if (launcher_has_path(path))
            continue;
        if (g_launcher_count >= LAUNCHER_MAX_ITEMS)
            break;
        if (!launcher_has_user_header()) {
            LauncherItem *hdr = &g_launcher_items[g_launcher_count++];
            memset(hdr, 0, sizeof *hdr);
            hdr->kind = 0;
            snprintf(hdr->text, sizeof hdr->text, "Users");
        }
        LauncherItem *it = &g_launcher_items[g_launcher_count];
        memset(it, 0, sizeof *it);
        it->kind = 1;
        size_t tl = len - 4;
        if (tl >= sizeof it->text)
            tl = sizeof it->text - 1;
        memcpy(it->text, e->d_name, tl);
        it->text[tl] = '\0';
        snprintf(it->path, sizeof it->path, "%s", path);
        g_launcher_count++;
    }
    closedir(d);
}

void
launcher_build(void)
{
    g_launcher_count = 0;
    g_launcher_body_h = 0;

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
                        snprintf(hdr->text, sizeof hdr->text, "Users");
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

    /* Register user apps from /mnt/ext1/applications that the firmware
     * has not recorded in view.json yet (same scan the stock bookshelf
     * runs in AppDataManager::scanUnregisteredUserApplication). */
    launcher_scan_ext1_apps();

    free(db);
    free(vw);
    launcher_layout();
    g_launcher_built = 1;
    LOG("[bookshelf] launcher built: %d items, %d body height\n",
        g_launcher_count,
        g_launcher_body_h);
}

/* -- launcher draw ------------------------------------------------------ */

void
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

void
draw_overlay_launcher(void)
{
    int w = ScreenWidth();
    int h = content_bottom();
    int body_top = LAUNCHER_HEADER_H;
    int body_h = content_bottom() - body_top;

    /* Clamp the scroll offset: the column's last row stops at the bottom
     * edge; a column shorter than the window never scrolls. */
    int max_scroll = g_launcher_body_h - body_h;
    if (max_scroll < 0)
        max_scroll = 0;
    if (g_state.launcher_scroll < 0)
        g_state.launcher_scroll = 0;
    if (g_state.launcher_scroll > max_scroll)
        g_state.launcher_scroll = max_scroll;
    int scroll = g_state.launcher_scroll;

    FillArea(0, 0, w, content_bottom(), WHITE);

    /* Fixed header: title + Back button. */
    FillArea(0, 0, w, LAUNCHER_HEADER_H, WHITE);
    DrawLine(0, LAUNCHER_HEADER_H - 1, w, LAUNCHER_HEADER_H - 1, BLACK);
    ifont *tf = OpenFont(DEFAULTFONTB, 36, 0);
    if (tf) {
        SetFont(tf, BLACK);
        const char *title = i18n("launcher.title");
        int         tw = StringWidth(title);
        DrawString((w - tw) / 2, (LAUNCHER_HEADER_H - 36) / 2, title);
        CloseFont(tf);
    }
    {
        int bx = 16, by = (LAUNCHER_HEADER_H - 56) / 2, bw = 160, bh = 56;
        DrawRect(bx, by, bw, bh, BLACK);
        ifont *bf = OpenFont(DEFAULTFONTB, 28, 0);
        if (bf) {
            SetFont(bf, BLACK);
            DrawString(bx + 16, by + (bh - 28) / 2 - 2, i18n("launcher.back"));
            CloseFont(bf);
        }
    }

    /* Scrollable body, clipped so rows never bleed into the header. */
    SetClip(0, body_top, w, body_h);
    if (g_launcher_count == 0) {
        ifont *ef = OpenFont(DEFAULTFONT, 32, 0);
        if (ef) {
            SetFont(ef, BLACK);
            const char *empty = i18n("launcher.empty");
            int         tw = StringWidth(empty);
            DrawString((w - tw) / 2, body_top + body_h / 2, empty);
            CloseFont(ef);
        }
    }

    ifont *hf = OpenFont(DEFAULTFONTB, 28, 0);
    ifont *af = OpenFont(DEFAULTFONT, 24, 0);
    for (int i = 0; i < g_launcher_count; i++) {
        const LauncherItem *it = &g_launcher_items[i];
        int                 sy = it->y - scroll + body_top;
        if (sy + it->h <= body_top || sy >= h)
            continue;
        if (it->kind == 0) {
            FillArea(it->x, sy, it->w, it->h, WHITE);
            DrawLine(it->x, sy + it->h - 1, it->x + it->w, sy + it->h - 1, BLACK);
            if (hf) {
                SetFont(hf, BLACK);
                DrawString(it->x + 12, sy + (it->h - 28) / 2 - 2, it->text);
            }
        } else {
            int cx = it->x + it->w / 2;
            int icon_cy = sy + 12 + LAUNCHER_ICON_SZ / 2;
            draw_launcher_icon(cx, icon_cy, it->icon, it->text);
            if (af) {
                SetFont(af, BLACK);
                int ly = sy + 12 + LAUNCHER_ICON_SZ + 8;
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
    SetClip(0, 0, w, h);
}

/* -- launcher hit-test + actions ---------------------------------------- */

void
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
    /*
     * Flags 0xa5 = TASK_HIDDEN | TASK_NOUPDATEONFOCUS | TASK_OUTOFSTACK |
     * TASK_MAKEACTIVE.  TASK_MAKEACTIVE is the load-bearing bit: without it
     * monitor.app registers the launched task but never brings it to the
     * foreground, so a plain `NewTaskEx(…, 0x25, …)` leaves the app running
     * invisibly in the background.  That is exactly how the browser worked
     * (webbrowser.sh delegates to openbook → start.app, whose launch carries
     * TASK_MAKEACTIVE) while calc.app appeared to "not start" (direct ELF
     * launch with 0x25, no activation).  The pre-0x25 flags match what the
     * stock bookshelf passes; the previous 1u<<30 bit is not a defined
     * TASK_* flag and made monitor.app treat the task registration oddly on
     * the live device. */
    /*
     * Draw a centered hourglass and leave it up while the app starts; the
     * launched task (TASK_MAKEACTIVE) overwrites it once it becomes the
     * foreground task and draws.  The caller suppresses the shelf redraw for
     * this path, so the screen freezes on the hourglass instead of falling
     * back to a static shelf that makes a slow launch look like a no-op.
     *
     * The firmware's own ShowHourglassForceAt() is not used here: its
     * animation is driven by monitor.app via REQ_HOURGLASS and never lands
     * in the app framebuffer (verified: nothing appears).  Drawing the
     * theme's hourglass bitmap directly with DrawBitmap() is guaranteed to
     * show on any build.
     */
    {
        ibitmap *hg = GetResource("hourglass", NULL);
        if (hg != NULL) {
            int x = (ScreenWidth() - hg->width) / 2;
            int y = (content_bottom() - hg->height) / 2;
            /* White backing so the glyph reads over the frozen launcher. */
            FillArea(x - 12, y - 12, hg->width + 24, hg->height + 24, WHITE);
            DrawRect(x - 12, y - 12, hg->width + 24, hg->height + 24, BLACK);
            DrawBitmap(x, y, hg);
            PartialUpdate(x - 12, y - 12, hg->width + 24, hg->height + 24);
        }
    }
    if (NewTaskEx(it->path, ai ? args : NULL, base, it->text, NULL, 0x25 | TASK_MAKEACTIVE, 0) < 0) {
        /* Launch failed: drop the hourglass and bring the launcher back so
         * the user is not stuck staring at an indefinite spinner. */
        HideHourglass();
        launcher_open_set();
    }
}

void
on_tap_overlay_launcher(int x, int y)
{
    int body_top = LAUNCHER_HEADER_H;
    if (x >= 16 && x < 176 && y >= (LAUNCHER_HEADER_H - 56) / 2 &&
        y < (LAUNCHER_HEADER_H - 56) / 2 + 56) {
        launcher_close();
        return;
    }
    if (y < body_top || y >= content_bottom())
        return;
    int by = y - body_top + g_state.launcher_scroll;
    for (int i = 0; i < g_launcher_count; i++) {
        const LauncherItem *it = &g_launcher_items[i];
        if (it->kind != 1)
            continue;
        if (x >= it->x && x < it->x + it->w && by >= it->y && by < it->y + it->h) {
            /* Launch the app.  Close the launcher state WITHOUT redrawing
             * the shelf: launch_app() puts up a centered hourglass that
             * stays until the launched task draws.  A redraw here would
             * flash the shelf back and make a slow app start look like the
             * tap did nothing. */
            g_state.launcher_open = 0;
            g_state.launcher_drag = 0;
            g_state.launcher_moved = 0;
            launch_app(it);
            return;
        }
    }
}

void
launcher_open_set(void)
{
    if (!g_launcher_built)
        launcher_build();
    g_state.launcher_open = 1;
    g_state.launcher_scroll = 0;
    g_state.launcher_drag = 0;
    g_state.launcher_moved = 0;
    draw_overlay_launcher();
    FullUpdate();
}

void
launcher_close(void)
{
    g_state.launcher_open = 0;
    g_state.launcher_drag = 0;
    g_state.launcher_moved = 0;
    redraw_shelf();
}

/* Pop out of a drilled-in series back to the collapsed top-level grid. */
void
drill_back(void)
{
    g_drilled_series[0] = '\0';
    g_state.page = g_state.saved_page;
    view_rebuild();
    LOG("[bookshelf] drilled back to top level (view=%d)\n", g_view_total);
    FillArea(0, 0, ScreenWidth(), content_bottom(), WHITE);
    draw_top_bar();
    draw_grid();
    draw_pager();
    FullUpdate();
}

void
on_tap_thumbnail(int vi)
{
    TileRow tr;
    if (!view_fetch_row(vi, &tr))
        return;

    /* Series card → drill into the series. */
    if (tr.is_series) {
        snprintf(g_drilled_series, sizeof g_drilled_series, "%s", tr.series_id);
        snprintf(g_drilled_series_name, sizeof g_drilled_series_name, "%s", tr.series_name);
        g_state.saved_page = g_state.page;
        g_state.page = 0;
        view_rebuild();
        LOG("[bookshelf] drilled into series '%s' (%d books)\n",
            g_drilled_series_name,
            g_view_total);
        FillArea(0, 0, ScreenWidth(), content_bottom(), WHITE);
        draw_top_bar();
        draw_grid();
        draw_pager();
        FullUpdate();
        return;
    }

    /* Flat tile → download (if needed) then open in the configured reader. */
    book_press_action(&tr.book);
}
