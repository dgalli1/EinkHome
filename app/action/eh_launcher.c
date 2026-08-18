/* eh_launcher.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_downloads.h"
#include "eh_launcher.h"
#include "eh_model.h"
#include "eh_net.h"
#include "eh_store.h"
#include "eh_ui.h"

/* -- app launcher ------------------------------------------------------- *
 * Reproduces the firmware's grouped application grid (the "Apps" screen
 * the original desktop renders from view.json + apps_db.json).  Since
 * bookshelf.app *is* the home-screen replacement, the original grid is
 * gone — this overlay restores it, resolving conditional visibility for
 * the current device profile (Era: touch + audio + en/WW + stock partner)
 * so the grid matches what the real device shows (e.g. Snake hidden on a
 * touch panel).  Tapping a tile launches the app via NewTaskEx. */

/* -- device profile for conditional resolution -------------------------- */

BsLcProfile eh_g_lcprof = {"all", "pocketbook", "true", "false", "en", "WW"};

/* -- file reader -------------------------------------------------------- */

char *
eh_read_text_file(const char *path)
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

/* -- launcher data + layout --------------------------------------------- */

BsLauncherItem eh_g_launcher_items[EH_LAUNCHER_MAX_ITEMS];
int          eh_g_launcher_count;
int          eh_g_launcher_body_h;
int          eh_g_launcher_built;

/* Lay every item out in one continuous column (headers span the full
 * width, app cells flow three per row).  The overlay scrolls this column
 * vertically; nothing is paginated, so a group heading can never clip
 * the last row of the previous group. */
static void
eh_launcher_layout(void)
{
    int w = ScreenWidth();
    int cell_w = (w - 2 * EH_LAUNCHER_MARGIN) / EH_LAUNCHER_COLS;
    int col = 0;
    int y = 0;

    for (int i = 0; i < eh_g_launcher_count; i++) {
        BsLauncherItem *it = &eh_g_launcher_items[i];
        if (it->kind == 0) {
            if (col > 0) {
                /* Finish a partial row before the next heading so the
                 * heading never overlaps the previous group's tiles. */
                y += EH_LAUNCHER_CELL_H;
                col = 0;
            }
            it->x = EH_LAUNCHER_MARGIN;
            it->y = y;
            it->w = w - 2 * EH_LAUNCHER_MARGIN;
            it->h = EH_LAUNCHER_GROUP_H;
            y += EH_LAUNCHER_GROUP_H;
        } else {
            if (col >= EH_LAUNCHER_COLS) {
                col = 0;
                y += EH_LAUNCHER_CELL_H;
            }
            it->x = EH_LAUNCHER_MARGIN + col * cell_w;
            it->y = y;
            it->w = cell_w;
            it->h = EH_LAUNCHER_CELL_H;
            col++;
        }
    }
    if (col > 0)
        y += EH_LAUNCHER_CELL_H;
    eh_g_launcher_body_h = y;
}

/* The launcher's app list comes from the platform backend behind the
 * seam (eh_plat_launcher_build): on PocketBook it is the firmware's
 * view.json + apps_db.json + the /mnt/ext1/applications scan (parsed in
 * app/platform/eh_plat_pb_launcher.c); on PC it is the freedesktop
 * .desktop files (eh_plat_sdl.c).  The UI (layout / draw / tap) below is
 * platform-independent. */
void
eh_launcher_build(void)
{
    eh_g_launcher_count = 0;
    eh_g_launcher_body_h = 0;
    eh_g_launcher_count = eh_plat_launcher_build(
        eh_g_launcher_items, EH_LAUNCHER_MAX_ITEMS);
    eh_launcher_layout();
    eh_g_launcher_built = 1;
    eh_LOG("[bookshelf] launcher built: %d items, %d body height\n",
        eh_g_launcher_count,
        eh_g_launcher_body_h);
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
#define EH_LAUNCHER_ICON_CACHE 16

typedef struct {
    char      name[64]; /* LauncherItem.icon (max 63 chars + NUL) */
    ibitmap  *bm;
    int       age; /* monotonically increasing LRU stamp */
} BsLauncherIconSlot;

static BsLauncherIconSlot g_icon_cache[EH_LAUNCHER_ICON_CACHE];
static int              g_icon_cache_age;

static ibitmap *
launcher_icon_get(const char *name)
{
    ibitmap *bm = NULL;
    if (name != NULL && name[0] != '\0') {
        if (name[0] != '/')
            bm = GetResource(name, NULL);
        if (bm == NULL)
            bm = LoadPNG(name, 0);
    }
    return bm;
}

/* Clear the cache at teardown/exit.  The SDK has no bitmap free API, so
 * this only drops the references (the libinkview bitmaps are reclaimed
 * by process exit, exactly like the cover slots). */
void
eh_launcher_icons_free(void)
{
    for (int i = 0; i < EH_LAUNCHER_ICON_CACHE; i++) {
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
    for (int i = 0; i < EH_LAUNCHER_ICON_CACHE; i++) {
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
    for (int i = 1; i < EH_LAUNCHER_ICON_CACHE; i++) {
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
eh_draw_launcher_icon(int cx, int cy, const char *icon_name, const char *title)
{
    int      sz = EH_LAUNCHER_ICON_SZ;
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
    int body_h = eh_content_bottom() - EH_OVERLAY_HEADER_H;
    if (eh_g_launcher_body_h - body_h > 0)
        body_h -= EH_SCROLL_BTN_H;
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
        const char *empty = eh_i18n("launcher.empty");
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
    int ly = sy + 12 + EH_LAUNCHER_ICON_SZ + 8;
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
eh_draw_overlay_launcher(void)
{
    int w = ScreenWidth();
    int h = eh_content_bottom();
    int body_top = EH_OVERLAY_HEADER_H;
    int body_h = launcher_body_h();

    /* Clamp the scroll offset: the column's last row stops at the bottom
     * edge; a column shorter than the window never scrolls. */
    int max_scroll = eh_g_launcher_body_h - body_h;
    if (max_scroll < 0)
        max_scroll = 0;
    if (eh_g_state.launcher_scroll < 0)
        eh_g_state.launcher_scroll = 0;
    if (eh_g_state.launcher_scroll > max_scroll)
        eh_g_state.launcher_scroll = max_scroll;
    int scroll = eh_g_state.launcher_scroll;

    FillArea(0, 0, w, eh_content_bottom(), WHITE);

    /* Shared overlay header: Back chevron + centred title. */
    eh_draw_overlay_header(eh_i18n("launcher.title"));

    /* Scrollable body, clipped so rows never bleed into the header. */
    SetClip(0, body_top, w, body_h);
    if (eh_g_launcher_count == 0)
        launcher_draw_empty(w, body_top, body_h);

    ifont *hf = OpenFont(DEFAULTFONTB, 28, 0);
    ifont *af = OpenFont(DEFAULTFONT, 24, 0);
    /* SetClip is not reliable on every SDK/emulator path, so rows must
     * fit the visible body outright: a row whose bottom would spill
     * past the reserved scroll-button band is skipped until it scrolls
     * into view (page scrolls align rows to the body). */
    int body_bottom = body_top + body_h;
    for (int i = 0; i < eh_g_launcher_count; i++) {
        const BsLauncherItem *it = &eh_g_launcher_items[i];
        int                 sy = it->y - scroll + body_top;
        if (sy + it->h <= body_top || sy + it->h > body_bottom)
            continue;
        if (it->kind == 0) {
            launcher_draw_heading(hf, it, sy);
        } else {
            int cx = it->x + it->w / 2;
            int icon_cy = sy + 12 + EH_LAUNCHER_ICON_SZ / 2;
            eh_draw_launcher_icon(cx, icon_cy, it->icon, it->text);
            if (af)
                launcher_draw_app_label(af, it, cx, sy);
        }
    }
    SetClip(0, 0, w, h);
    /* Stock corner scroll buttons while the column overflows.  Drawn
     * after the clip reset: the body clip (SetClip above) would
     * otherwise cut them — the button band sits below the body. */
    eh_draw_scroll_buttons(scroll > 0, scroll < max_scroll);
    if (hf)
        CloseFont(hf);
    if (af)
        CloseFont(af);
}

/* -- launcher hit-test + actions ---------------------------------------- */

void
eh_launch_app(const BsLauncherItem *it)
{
    if (!it->path[0])
        return;
    const char *base = strrchr(it->path, '/');
    base = base ? base + 1 : it->path;
    char *args[EH_LAUNCHER_MAX_PARAMS + 2];
    int   ai = 0;
    args[ai++] = (char *)it->path;
    for (int i = 0; i < it->nparams && ai < EH_LAUNCHER_MAX_PARAMS + 1; i++)
        args[ai++] = (char *)it->params[i];
    args[ai] = NULL;
    eh_LOG("[bookshelf] launching app path=%s base=%s params=%d\n", it->path, base, it->nparams);
    /*
     * Draw a centered hourglass and leave it up while the app starts; on
     * PocketBook the launched task (TASK_MAKEACTIVE, see eh_plat_pb.c
     * eh_plat_launch_app) overwrites it once it becomes the foreground task
     * and draws.  The caller suppresses the shelf redraw for this path, so
     * the screen freezes on the hourglass instead of falling back to a
     * static shelf that makes a slow launch look like a no-op.  The actual
     * launch (NewTaskEx on PB, fork/exec on PC) is behind the platform seam.
     */
    eh_show_hourglass();
    if (eh_plat_launch_app(it, args, ai) < 0) {
        /* Launch failed: drop the hourglass and bring the launcher back so
         * the user is not stuck staring at an indefinite spinner. */
        HideHourglass();
        eh_launcher_open_set();
    }
}

/* Handle a tap on a corner scroll button: page the column by one body
 * height.  Returns 1 when a button was hit (and a redraw was issued). */
static int
launcher_tap_scroll(int x, int y)
{
    int dir = eh_hit_scroll_button(x, y);
    if (dir == 0)
        return 0;
    int body_h = launcher_body_h();
    int max_scroll = eh_g_launcher_body_h - body_h;
    if (max_scroll < 0)
        max_scroll = 0;
    eh_g_state.launcher_scroll += dir * body_h;
    if (eh_g_state.launcher_scroll < 0)
        eh_g_state.launcher_scroll = 0;
    if (eh_g_state.launcher_scroll > max_scroll)
        eh_g_state.launcher_scroll = max_scroll;
    eh_draw_overlay_launcher();
    eh_flush_content();
    return 1;
}

/* Find the tapped app cell under (x, by) and launch it.  Returns 1 when
 * an app was launched. */
static int
launcher_tap_app(int x, int by)
{
    for (int i = 0; i < eh_g_launcher_count; i++) {
        const BsLauncherItem *it = &eh_g_launcher_items[i];
        if (it->kind != 1)
            continue;
        if (x >= it->x && x < it->x + it->w && by >= it->y && by < it->y + it->h) {
            /* Launch the app.  Close the launcher state WITHOUT redrawing
             * the shelf: launch_app() puts up a centered hourglass that
             * stays until the launched task draws.  A redraw here would
             * flash the shelf back and make a slow app start look like the
             * tap did nothing. */
            eh_g_state.overlay = EH_OV_NONE;
            eh_g_state.launcher_drag = 0;
            eh_g_state.launcher_moved = 0;
            eh_launch_app(it);
            return 1;
        }
    }
    return 0;
}

void
eh_on_tap_overlay_launcher(int x, int y)
{
    int body_top = EH_OVERLAY_HEADER_H;
    int rx, ry, rw, rh;
    eh_overlay_back_rect(&rx, &ry, &rw, &rh);
    if (x >= rx && x < rx + rw && y >= ry && y < ry + rh) {
        eh_launcher_close();
        return;
    }
    /* Corner scroll buttons: page up/down the column. */
    if (launcher_tap_scroll(x, y))
        return;
    if (y < body_top || y >= eh_content_bottom())
        return;
    int by = y - body_top + eh_g_state.launcher_scroll;
    launcher_tap_app(x, by);
}

void
eh_launcher_open_set(void)
{
    if (!eh_g_launcher_built)
        eh_launcher_build();
    eh_g_state.overlay = EH_OV_LAUNCHER;
    eh_g_state.launcher_scroll = 0;
    eh_g_state.launcher_drag = 0;
    eh_g_state.launcher_moved = 0;
    eh_draw_overlay_launcher();
    eh_flush_content();
}

void
eh_launcher_close(void)
{
    eh_g_state.overlay = EH_OV_NONE;
    eh_g_state.launcher_drag = 0;
    eh_g_state.launcher_moved = 0;
    eh_redraw_shelf();
}

void
eh_on_tap_thumbnail(int vi)
{
    BsTileRow tr;
    if (!eh_view_fetch_row(vi, &tr))
        return;

    /* A stack card only ever appears in a dimension-grouped view (None
     * stays flat, so a card always drills within the active grouping).
     * Tapping one drills into the group. */
    if (tr.is_series) {
        eh_group_drill(tr.series_id); /* series_id = raw group value */
        return;
    }

    /* Flat tile → download (if needed) then open in the configured reader. */
    eh_book_press_action(&tr.book);
}
