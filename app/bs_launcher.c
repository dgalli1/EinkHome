/* bs_launcher.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"
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
lc_pick_key(const cJSON *obj, const char *want)
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

void
lc_resolve(const cJSON *v, const char *cur_dim, char *out, size_t cap)
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
    for (int d = 0; d < LC_NDIMS; d++) {
        const cJSON *vp = cJSON_GetObjectItemCaseSensitive(v, lc_dims[d]);
        if (vp != NULL) {
            lc_resolve(vp, lc_dims[d], out, cap);
            return;
        }
    }
    if (!cur_dim) {
        const char *k = lc_pick_key(v, NULL);
        if (k != NULL) {
            const cJSON *vp = cJSON_GetObjectItemCaseSensitive(v, k);
            if (vp != NULL)
                lc_resolve(vp, cur_dim, out, cap);
        }
        return;
    }
    if (strcmp(cur_dim, "globalcfg") == 0) {
        /* The globalcfg variant: the first member whose value is an
         * object carrying a "default" wins. */
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
            lc_resolve(m, cur_dim, out, cap);
        return;
    }
    const char *want = lc_prof_val(cur_dim);
    const char *k = lc_pick_key(v, want);
    if (k != NULL) {
        const cJSON *vp = cJSON_GetObjectItemCaseSensitive(v, k);
        if (vp != NULL)
            lc_resolve(vp, cur_dim, out, cap);
    }
}

int
lc_resolve_bool(const cJSON *v)
{
    if (v != NULL && cJSON_IsBool(v))
        return cJSON_IsTrue(v);
    char buf[8];
    lc_resolve(v, NULL, buf, sizeof buf);
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
launcher_add_app(const cJSON *apps, const char *id)
{
    if (g_launcher_count >= LAUNCHER_MAX_ITEMS)
        return;
    const cJSON *def = cJSON_GetObjectItemCaseSensitive(apps, id);
    if (!cJSON_IsObject(def))
        return;
    const cJSON *vis = cJSON_GetObjectItemCaseSensitive(def, "visible");
    if (vis != NULL && !lc_resolve_bool(vis))
        return;
    LauncherItem *it = &g_launcher_items[g_launcher_count];
    memset(it, 0, sizeof *it);
    it->kind = 1;
    const cJSON *tp = cJSON_GetObjectItemCaseSensitive(def, "title");
    if (tp != NULL) {
        char raw[64];
        lc_resolve(tp, NULL, raw, sizeof raw);
        lc_translate(raw, it->text, sizeof it->text);
    }
    if (!it->text[0])
        snprintf(it->text, sizeof it->text, "%s", id);
    const cJSON *pp = cJSON_GetObjectItemCaseSensitive(def, "path");
    if (pp != NULL)
        lc_resolve(pp, NULL, it->path, sizeof it->path);
    const cJSON *ip = cJSON_GetObjectItemCaseSensitive(def, "icon");
    if (ip != NULL)
        lc_resolve(ip, NULL, it->icon, sizeof it->icon);
    const cJSON *par = cJSON_GetObjectItemCaseSensitive(def, "params");
    if (!cJSON_IsArray(par))
        par = cJSON_GetObjectItemCaseSensitive(def, "param");
    if (cJSON_IsArray(par)) {
        const cJSON *q;
        cJSON_ArrayForEach(q, par) {
            if (it->nparams >= LAUNCHER_MAX_PARAMS)
                break;
            if (cJSON_IsString(q))
                snprintf(it->params[it->nparams++], LAUNCHER_PARAM_LEN,
                         "%s", q->valuestring);
        }
    } else if (cJSON_IsString(par)) {
        snprintf(it->params[0], LAUNCHER_PARAM_LEN, "%s", par->valuestring);
        it->nparams = 1;
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
        char path[MAX_PATH_LEN];
        snprintf(path, sizeof path, "/mnt/ext1/applications/%s", e->d_name);
        struct stat st;
        if (iv_stat(path, &st) != 0)
            continue;
        if (launcher_has_path(path))
            continue;
        if (!launcher_has_user_header() && g_launcher_count < LAUNCHER_MAX_ITEMS) {
            LauncherItem *hdr = &g_launcher_items[g_launcher_count++];
            memset(hdr, 0, sizeof *hdr);
            hdr->kind = 0;
            snprintf(hdr->text, sizeof hdr->text, "Users");
        }
        if (g_launcher_count >= LAUNCHER_MAX_ITEMS)
            break;
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

    char *db_txt = read_text_file("/mnt/ext1/system/config/desktop/apps_db.json");
    if (!db_txt)
        db_txt = read_text_file("/ebrmain/config/desktop/apps_db.json");
    char *vw_txt = read_text_file("/mnt/ext1/system/config/desktop/view.json");
    if (!vw_txt)
        vw_txt = read_text_file("/ebrmain/config/desktop/view.json");

    cJSON *db = db_txt ? cJSON_Parse(db_txt) : NULL;
    cJSON *vw = vw_txt ? cJSON_Parse(vw_txt) : NULL;
    free(db_txt);
    free(vw_txt);

    const cJSON *db_apps = db ? cJSON_GetObjectItemCaseSensitive(db, "applications") : NULL;
    if (db == NULL || vw == NULL || !cJSON_IsObject(db_apps)) {
        cJSON_Delete(db);
        cJSON_Delete(vw);
        launcher_layout();
        g_launcher_built = 1;
        return;
    }

    const cJSON *view_obj = cJSON_GetObjectItemCaseSensitive(vw, "view");
    const cJSON *groups = cJSON_IsObject(view_obj)
        ? cJSON_GetObjectItemCaseSensitive(view_obj, "groups")
        : NULL;
    if (cJSON_IsArray(groups)) {
        const cJSON *g;
        cJSON_ArrayForEach(g, groups) {
            if (!cJSON_IsObject(g))
                continue;
            const cJSON *tp = cJSON_GetObjectItemCaseSensitive(g, "title");
            char        raw_title[64] = "";
            char        disp_title[64] = "";
            if (tp != NULL) {
                lc_resolve(tp, NULL, raw_title, sizeof raw_title);
                lc_translate(raw_title, disp_title, sizeof disp_title);
            }
            const cJSON *apps_arr = cJSON_GetObjectItemCaseSensitive(g, "apps");
            if (cJSON_IsArray(apps_arr)) {
                if (g_launcher_count < LAUNCHER_MAX_ITEMS && disp_title[0]) {
                    LauncherItem *hdr = &g_launcher_items[g_launcher_count++];
                    memset(hdr, 0, sizeof *hdr);
                    hdr->kind = 0;
                    snprintf(hdr->text, sizeof hdr->text, "%s", disp_title);
                }
                const cJSON *a;
                cJSON_ArrayForEach(a, apps_arr) {
                    if (cJSON_IsString(a) && a->valuestring != NULL)
                        launcher_add_app(db_apps, a->valuestring);
                }
            }
        }
    }

    /* Scan view.json applications for U_* user apps not in any group. */
    const cJSON *vw_apps = cJSON_GetObjectItemCaseSensitive(vw, "applications");
    if (cJSON_IsObject(vw_apps)) {
        int user_hdr_added = 0;
        const cJSON *it;
        cJSON_ArrayForEach(it, vw_apps) {
            const char *key = it->string;
            if (key == NULL || key[0] != 'U' || key[1] != '_')
                continue;
            int vis = 1;
            if (cJSON_IsObject(it)) {
                const cJSON *v2 = cJSON_GetObjectItemCaseSensitive(it, "visible");
                if (v2 != NULL && !lc_resolve_bool(v2))
                    vis = 0;
            }
            if (!vis)
                continue;
            if (!user_hdr_added && g_launcher_count < LAUNCHER_MAX_ITEMS) {
                LauncherItem *hdr = &g_launcher_items[g_launcher_count++];
                memset(hdr, 0, sizeof *hdr);
                hdr->kind = 0;
                snprintf(hdr->text, sizeof hdr->text, "Users");
                user_hdr_added = 1;
            }
            if (g_launcher_count >= LAUNCHER_MAX_ITEMS)
                continue;
            char id[48];
            snprintf(id, sizeof id, "%s", key);
            LauncherItem *li = &g_launcher_items[g_launcher_count];
            memset(li, 0, sizeof *li);
            li->kind = 1;
            if (cJSON_IsObject(it)) {
                const cJSON *tp2 = cJSON_GetObjectItemCaseSensitive(it, "title");
                if (tp2 != NULL)
                    lc_resolve(tp2, NULL, li->text, sizeof li->text);
                const cJSON *pp2 = cJSON_GetObjectItemCaseSensitive(it, "path");
                if (pp2 != NULL)
                    lc_resolve(pp2, NULL, li->path, sizeof li->path);
                const cJSON *ip2 = cJSON_GetObjectItemCaseSensitive(it, "icon");
                if (ip2 != NULL)
                    lc_resolve(ip2, NULL, li->icon, sizeof li->icon);
            }
            if (!li->text[0])
                snprintf(li->text, sizeof li->text, "%s", id);
            g_launcher_count++;
        }
    }

    /* Register user apps from /mnt/ext1/applications that the firmware
     * has not recorded in view.json yet (same scan the stock bookshelf
     * runs in AppDataManager::scanUnregisteredUserApplication). */
    launcher_scan_ext1_apps();

    cJSON_Delete(db);
    cJSON_Delete(vw);
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
        /* Center the bitmap inside the icon box.  The firmware icon
         * resources come in various native sizes; anchoring them at the
         * box's top-left made small glyphs drift toward the corner
         * (and off the label's centre line).  Oversized icons are
         * scaled down, aspect-preserving, to fit the box. */
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
    /* Stock corner scroll buttons while the column overflows. */
    draw_scroll_buttons(scroll > 0, scroll < max_scroll);
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
    if (NewTaskEx(it->path, ai ? args : NULL, base, it->text, NULL, 0x25 | TASK_MAKEACTIVE, 0) <
        0) {
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
    /* Corner scroll buttons: page up/down the column. */
    int dir = hit_scroll_button(x, y);
    if (dir != 0) {
        int body_h = content_bottom() - body_top;
        int max_scroll = g_launcher_body_h - body_h;
        if (max_scroll < 0)
            max_scroll = 0;
        g_state.launcher_scroll += dir * body_h;
        if (g_state.launcher_scroll < 0)
            g_state.launcher_scroll = 0;
        if (g_state.launcher_scroll > max_scroll)
            g_state.launcher_scroll = max_scroll;
        draw_overlay_launcher();
        flush_content();
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
            g_state.overlay = OV_NONE;
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
    g_state.overlay = OV_LAUNCHER;
    g_state.launcher_scroll = 0;
    g_state.launcher_drag = 0;
    g_state.launcher_moved = 0;
    draw_overlay_launcher();
    flush_content();
}

void
launcher_close(void)
{
    g_state.overlay = OV_NONE;
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
    flush_content();
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
        g_state.saved_page = g_state.page;
        g_state.page = 0;
        view_rebuild();
        LOG("[bookshelf] drilled into series '%s' (%d books)\n", tr.series_name, g_view_total);
        FillArea(0, 0, ScreenWidth(), content_bottom(), WHITE);
        draw_top_bar();
        draw_grid();
        draw_pager();
        flush_content();
        return;
    }

    /* Flat tile → download (if needed) then open in the configured reader. */
    book_press_action(&tr.book);
}
