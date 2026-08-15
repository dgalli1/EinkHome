/* bs_overlays.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_browser.h"
#include "bs_model.h"
#include "bs_ui.h"

/* Sort-mode label key for the current sort (drawer value + chooser). */
static const char *
sort_label(void)
{
    switch (bs_g_state.sort) {
    case BS_SORT_AUTHOR: return "sort.author";
    case BS_SORT_SERIES: return "sort.series";
    case BS_SORT_RECENT: return "sort.recent";
    default:             return "sort.title_az";
    }
}

/* Grouping-dimension label key (drawer value + chooser rows). */
static const char *
dim_label(BsGroupDim d)
{
    switch (d) {
    case BS_GROUP_BY_SERIES: return "group.series";
    case BS_GROUP_BY_AUTHOR: return "group.author";
    case BS_GROUP_BY_YEAR:   return "group.year";
    case BS_GROUP_BY_GENRE:  return "group.genre";
    default:                 return "group.all";
    }
}

/* Human summary of the active group path ("Author > Series"), or the
 * "All books" label when ungrouped. */
static void
group_path_summary(char *out, size_t cap)
{
    if (bs_g_group_depth == 0) {
        snprintf(out, cap, "%s", bs_i18n("group.all"));
        return;
    }
    size_t n = 0;
    for (int i = 0; i < bs_g_group_depth; i++) {
        int written = snprintf(out + n, cap > n ? cap - n : 0,
                               i == 0 ? "%s" : " > %s",
                               bs_i18n(dim_label(bs_g_group_path[i])));
        if (written < 0)
            return;
        n += (size_t)written;
        if (n >= cap)
            return; /* truncated, still NUL-terminated */
    }
}

void
bs_draw_overlay_menu(void)
{
    int w = ScreenWidth();
    FillArea(0, 0, w, bs_content_bottom(), BLACK);
    int pw = w * 3 / 4;
    FillArea(0, 0, pw, bs_content_bottom(), WHITE);
    DrawLine(pw, 0, pw, bs_content_bottom(), BLACK);

    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(24, 32, bs_i18n("action.menu"));
        CloseFont(f);
    }

    char gpath[BS_MAX_TITLE_LEN + 32];
    group_path_summary(gpath, sizeof gpath);

    const char *consttbl[] = { "action.group_by", "action.sort_by" };
    const char *pval[] = { gpath, bs_i18n(sort_label()) };
    int y0 = BS_MORE_Y0, item_h = BS_MORE_ITEM_H;
    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    for (int i = 0; i < 2; i++) {
        FillArea(12, y0 + i * item_h, pw - 24, item_h - 12, WHITE);
        DrawRect(12, y0 + i * item_h, pw - 24, item_h - 12, BLACK);
        if (tf != NULL) {
            SetFont(tf, BLACK);
            DrawString(32, y0 + i * item_h + (item_h - 28) / 2 - 2,
                       bs_i18n(consttbl[i]));
            /* Right-aligned current value. */
            int tw = StringWidth(pval[i]);
            SetFont(tf, DGRAY);
            DrawString(pw - 32 - tw, y0 + i * item_h + (item_h - 28) / 2 - 2,
                       pval[i]);
        }
    }
    if (tf != NULL)
        CloseFont(tf);
}

/* ── group / sort choosers (source-chooser style sheets) ────────────── */

/* Row list for the group chooser: All books + every dimension with data
 * in the current source.  Returns the row count. */
int
bs_group_options(BsGroupDim out[], int cap)
{
    int n = 0;
    static const BsGroupDim cand[] = {
        BS_GROUP_BY_SERIES, BS_GROUP_BY_AUTHOR, BS_GROUP_BY_YEAR,
        BS_GROUP_BY_GENRE,
    };
    if (n < cap)
        out[n++] = BS_GROUP_ALL;
    for (unsigned int i = 0; i < sizeof cand / sizeof cand[0]; i++)
        if (n < cap && bs_view_dim_available(cand[i]))
            out[n++] = cand[i];
    return n;
}

/* Level (1-based) at which *dim* sits in the current group path, or 0. */
static int
group_path_level(BsGroupDim dim)
{
    for (int i = 0; i < bs_g_group_depth && i < BS_GROUP_MAX_LEVELS; i++)
        if (bs_g_group_path[i] == dim)
            return i + 1;
    return 0;
}

static void
bs_group_geom(int *px, int *py, int *pw, int *ph)
{
    BsGroupDim opts[1 + 4];
    int n = bs_group_options(opts, 1 + 4);
    int w = ScreenWidth();
    *pw = w * 3 / 4;
    *ph = 72 + n * 96 + 24;
    *px = (w - *pw) / 2;
    *py = (bs_content_bottom() - *ph) / 2;
}

void
bs_draw_overlay_group(void)
{
    int pw, ph, px, py;
    bs_group_geom(&px, &py, &pw, &ph);

    bs_dim_content(0);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    char gpath[BS_MAX_TITLE_LEN + 32];
    group_path_summary(gpath, sizeof gpath);

    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + BS_CTX_PAD, py + 16, bs_i18n("action.group_by"));
        SetFont(tf, DGRAY);
        DrawString(px + BS_CTX_PAD, py + 46, gpath);
        CloseFont(tf);
    }
    DrawLine(px + BS_CTX_PAD, py + 76, px + pw - BS_CTX_PAD, py + 76, LGRAY);

    BsGroupDim opts[1 + 4];
    int n = bs_group_options(opts, 1 + 4);
    int y0 = py + 84;
    ifont *f = OpenFont(DEFAULTFONTB, 26, 0);
    for (int i = 0; i < n; i++) {
        BsGroupDim d = opts[i];
        int lvl = (d == BS_GROUP_ALL) ? (bs_g_group_depth == 0 ? 1 : 0)
                                      : group_path_level(d);
        int sel = lvl != 0;
        FillArea(px + 12, y0 + i * 96, pw - 24, 96 - 12, sel ? BLACK : WHITE);
        DrawRect(px + 12, y0 + i * 96, pw - 24, 96 - 12, BLACK);
        if (f != NULL) {
            char line[BS_MAX_TITLE_LEN + 16];
            if (sel && lvl > 1)
                snprintf(line, sizeof line, "%s  (%d)", bs_i18n(dim_label(d)), lvl);
            else
                snprintf(line, sizeof line, "%s", bs_i18n(dim_label(d)));
            SetFont(f, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * 96 + (96 - 26) / 2 - 2, line);
        }
    }
    if (f != NULL)
        CloseFont(f);
}

void
bs_draw_overlay_sort(void)
{
    int w = ScreenWidth();
    int pw = w * 3 / 4;
    int n = 4;
    int ph = 72 + n * 96 + 24;
    int px = (w - pw) / 2;
    int py = (bs_content_bottom() - ph) / 2;

    bs_dim_content(0);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + BS_CTX_PAD, py + 16, bs_i18n("action.sort_by"));
        SetFont(tf, DGRAY);
        DrawString(px + BS_CTX_PAD, py + 46, bs_i18n(sort_label()));
        CloseFont(tf);
    }
    DrawLine(px + BS_CTX_PAD, py + 76, px + pw - BS_CTX_PAD, py + 76, LGRAY);

    const char *labels[4] = {
        "sort.title_az", "sort.author", "sort.series", "sort.recent",
    };
    int y0 = py + 84;
    ifont *f = OpenFont(DEFAULTFONTB, 26, 0);
    for (int i = 0; i < n; i++) {
        int sel = (i == (int)bs_g_state.sort);
        FillArea(px + 12, y0 + i * 96, pw - 24, 96 - 12, sel ? BLACK : WHITE);
        DrawRect(px + 12, y0 + i * 96, pw - 24, 96 - 12, BLACK);
        if (f != NULL) {
            SetFont(f, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * 96 + (96 - 26) / 2 - 2, bs_i18n(labels[i]));
        }
    }
    if (f != NULL)
        CloseFont(f);
}

void
bs_draw_overlay_more(void)
{
    int w = ScreenWidth();
    FillArea(0, 0, w, bs_content_bottom(), BLACK);
    int pw = w * 3 / 4;
    int px = w - pw;
    FillArea(px, 0, pw, bs_content_bottom(), WHITE);
    DrawLine(px, 0, px, bs_content_bottom(), BLACK);

    ifont *f = OpenFont(DEFAULTFONTB, 32, 0);
    if (f != NULL) {
        SetFont(f, BLACK);
        DrawString(px + 24, 32, bs_i18n("action.more"));
        CloseFont(f);
    }
    const char *labels[] = {
        "action.sync",
        "sort.title_az",
        "sort.author",
        "sort.series",
        "sort.recent",
        "view.grid",
        "view.list",
        "action.download_all",
        "action.settings",
        "action.apps",
    };
    int n = (int)(sizeof labels / sizeof labels[0]);
    int y0 = BS_MORE_Y0;
    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    for (int i = 0; i < n; i++) {
        int sel = 0;
        if (i == 0 && bs_g_state.sync_state == 1)
            sel = 1;
        if (i >= 1 && i <= 4 && (i - 1) == (int)bs_g_state.sort)
            sel = 1;
        if (i == BS_MORE_GRID_IDX && bs_g_state.view_mode == BS_VIEW_GRID)
            sel = 1;
        if (i == BS_MORE_LIST_IDX && bs_g_state.view_mode == BS_VIEW_LIST)
            sel = 1;
        FillArea(px + 12, y0 + i * BS_MORE_ITEM_H, pw - 24, BS_MORE_ITEM_H - 12, sel ? BLACK : WHITE);
        if (tf != NULL) {
            SetFont(tf, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * BS_MORE_ITEM_H + (BS_MORE_ITEM_H - 28) / 2 - 2, bs_i18n(labels[i]));
        }
    }
    if (tf != NULL)
        CloseFont(tf);
}

/* ── source chooser ──────────────────────────────────────────────────── */

/* Sheet geometry of the source chooser (top-bar button right of home):
 * a centred 3/4-width sheet with the title row and three source rows. */
void
bs_source_geom(int *px, int *py, int *pw, int *ph)
{
    int w = ScreenWidth();
    *pw = w * 3 / 4;
    *ph = 72 + 3 * 96 + 24;
    *px = (w - *pw) / 2;
    *py = (bs_content_bottom() - *ph) / 2;
}

void
bs_draw_overlay_source(void)
{
    int pw, ph, px, py;
    bs_source_geom(&px, &py, &pw, &ph);

    /* Dim the content area behind the sheet. */
    bs_dim_content(0);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 32, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + BS_CTX_PAD, py + 18, bs_i18n("source.title"));
        CloseFont(tf);
    }
    DrawLine(px + BS_CTX_PAD, py + 64, px + pw - BS_CTX_PAD, py + 64, LGRAY);

    const char *labels[3] = {
        bs_i18n("source.kavita"),
        bs_i18n("source.local"),
        bs_i18n("source.folder"),
    };
    int y0 = py + 80;
    ifont *f = OpenFont(DEFAULTFONTB, 28, 0);
    for (int i = 0; i < 3; i++) {
        int sel = (bs_g_state.source == i);
        FillArea(px + 12, y0 + i * 96, pw - 24, 96 - 12, sel ? BLACK : WHITE);
        DrawRect(px + 12, y0 + i * 96, pw - 24, 96 - 12, sel ? BLACK : WHITE);
        if (f != NULL) {
            SetFont(f, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * 96 + (96 - 28) / 2 - 2, labels[i]);
        }
    }
    if (f != NULL)
        CloseFont(f);
}

/* ── settings overlay ────────────────────────────────────────────────── */

/* Which settings row currently owns the on-screen keyboard:
 * 0 = none, 1 = API host, 2 = API key. */
int bs_g_settings_edit = 0;

/* Scratch buffer the keyboard edits; committed on close. */
char bs_g_settings_kb_buf[260];

void
bs_settings_keyboard_handler(char *buffer)
{
    const char *val = buffer ? buffer : "";
    if (bs_g_settings_edit == 1) {
        /* Normalise a bare host[:port] into a full http:// URL so the
         * endpoint builder always gets a scheme.  Cap the host portion
         * first so the prefixed URL always fits g_state.api_base: only
         * the host's tail can be cut, never the "http://" prefix, so
         * the committed value is never a truncated-mid-URL. */
        if (strncmp(val, "http://", 7) != 0 && strncmp(val, "https://", 8) != 0) {
            char tmp[sizeof bs_g_state.api_base];
            snprintf(tmp, sizeof tmp, "%.*s", (int)(sizeof bs_g_state.api_base - 8), val);
            bs_utf8_cap(tmp, sizeof tmp);
            int n = snprintf(bs_g_state.api_base, sizeof bs_g_state.api_base, "http://%s", tmp);
            if (n < 0 || n >= (int)sizeof bs_g_state.api_base)
                bs_LOG("[bookshelf] settings: api host truncated to %d bytes\n",
                    (int)sizeof bs_g_state.api_base - 1);
        } else {
            snprintf(bs_g_state.api_base, sizeof bs_g_state.api_base, "%s", val);
        }
    } else if (bs_g_settings_edit == 2) {
        snprintf(bs_g_state.api_token, sizeof bs_g_state.api_token, "%s", val);
    }
    bs_g_settings_edit = 0;
    /* The on-screen keyboard draws full-screen and wipes the bottom
     * status strip; re-stamp it BEFORE the draw so the panel survives
     * the commit repaint.  Draw the settings page without flushing,
     * then a single full-screen FullUpdate repaints the content and the
     * panel band in one refresh — the same pattern as bs_main.c's
     * keyboard_handler (draw no-flush → one FullUpdate). */
    bs_stamp_panel();
    bs_draw_overlay_settings();
    FullUpdate();
}

/* Full-screen settings page.  A header bar with a Back button and the
 * centred title (same shape as the launcher header), then four editable
 * rows (API host, API key, reader app, download folder) plus Save and
 * Show logs buttons.  The API host / key rows open the on-screen
 * keyboard; the reader row cycles through Auto plus every detected
 * reader.  Generous row heights keep the targets comfortable on the
 * 300 DPI e-ink panel. */
const char *
bs_settings_reader_label(void)
{
    if (bs_g_state.reader_pref > 0 && bs_g_state.reader_pref <= bs_g_reader_count)
        return bs_g_readers[bs_g_state.reader_pref - 1].label;
    return bs_i18n("settings.reader_auto");
}

void
bs_settings_draw_row(int y, const char *label, const char *value, int editing,
                  ifont *lf, ifont *vf)
{
    int w = ScreenWidth();
    int mx = 32; /* left/right margin */
    FillArea(mx, y, w - 2 * mx, BS_SETTINGS_ROW_H - 12, WHITE);
    DrawRect(mx, y, w - 2 * mx, BS_SETTINGS_ROW_H - 12, BLACK);
    if (editing)
        FillArea(mx + 2, y + 2, w - 2 * mx - 4, BS_SETTINGS_ROW_H - 16, BLACK);

    if (lf != NULL) {
        SetFont(lf, editing ? WHITE : BLACK);
        DrawString(mx + 16, y + 12, label);
    }
    if (vf != NULL) {
        SetFont(vf, editing ? WHITE : BLACK);
        DrawString(mx + 16, y + 52, value);
    }
}

void
bs_settings_draw_button(int y, const char *label, int filled, ifont *f)
{
    int w = ScreenWidth();
    int mx = 32;
    FillArea(mx, y, w - 2 * mx, BS_SETTINGS_BTN_H - 12, filled ? BLACK : WHITE);
    DrawRect(mx, y, w - 2 * mx, BS_SETTINGS_BTN_H - 12, BLACK);
    if (f != NULL) {
        SetFont(f, filled ? WHITE : BLACK);
        int tw = StringWidth(label);
        DrawString((w - tw) / 2, y + (BS_SETTINGS_BTN_H - 12 - 32) / 2, label);
    }
}

void
bs_draw_overlay_settings(void)
{
    int w = ScreenWidth();
    FillArea(0, 0, w, bs_content_bottom(), WHITE);

    /* Shared overlay header: Back chevron + centred title. */
    bs_draw_overlay_header(bs_i18n("settings.title"));

    /* Downloads folder: the pending picker choice, else the resolved
     * effective directory — shown relative to /mnt/ext1. */
    char        dl_shown[256];
    const char *dl = bs_g_settings_dl_dir[0] ? bs_g_settings_dl_dir : bs_g_downloads_dir;
    bs_user_path_display(dl, dl_shown, sizeof dl_shown);
    bs_utf8_fit_width(dl_shown, sizeof dl_shown, w - 2 * 32 - 16);

    int y = 112;
    /* Row/button fonts opened once for the whole settings pass. */
    ifont *lf = OpenFont(DEFAULTFONTB, 26, 0);
    ifont *vf = OpenFont(DEFAULTFONT, 30, 0);
    ifont *bf = OpenFont(DEFAULTFONTB, 32, 0);
    bs_settings_draw_row(y, bs_i18n("settings.api_host"), bs_g_state.api_base, bs_g_settings_edit == 1, lf, vf);
    y += BS_SETTINGS_ROW_H;
    bs_settings_draw_row(y, bs_i18n("settings.api_key"), bs_g_state.api_token, bs_g_settings_edit == 2, lf, vf);
    y += BS_SETTINGS_ROW_H;
    bs_settings_draw_row(y, bs_i18n("settings.reader"), bs_settings_reader_label(), 0, lf, vf);
    y += BS_SETTINGS_ROW_H;
    bs_settings_draw_row(y, bs_i18n("settings.dl_dir"), dl_shown, 0, lf, vf);
    y += BS_SETTINGS_ROW_H + 24;
    bs_settings_draw_button(y, bs_i18n("settings.save"), 1, bf);
    y += BS_SETTINGS_BTN_H;
    bs_settings_draw_button(y, bs_i18n("settings.logs"), 0, bf);
    if (bf != NULL)
        CloseFont(bf);
    if (vf != NULL)
        CloseFont(vf);
    if (lf != NULL)
        CloseFont(lf);
}
