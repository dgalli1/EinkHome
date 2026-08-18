/* eh_overlays.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_browser.h"
#include "eh_model.h"
#include "eh_ui.h"

/* Sort-mode label key for the current sort (drawer value + chooser). */
static const char *
sort_label(void)
{
    switch (eh_g_state.sort) {
    case EH_SORT_AUTHOR: return "sort.author";
    case EH_SORT_SERIES: return "sort.series";
    case EH_SORT_RECENT: return "sort.recent";
    default:             return "sort.title_az";
    }
}

/* Grouping-preset label key (drawer value + chooser rows). */
static const char *
group_label(BsGroupPreset g)
{
    switch (g) {
    case EH_GROUP_NONE:          return "group.none";
    case EH_GROUP_SERIES:        return "group.series";
    case EH_GROUP_AUTHOR:        return "group.author";
    case EH_GROUP_YEAR:          return "group.year";
    case EH_GROUP_GENRE:         return "group.genre";
    case EH_GROUP_AUTHOR_SERIES: return "group.author_series";
    default:                     return "group.none";
    }
}

/* Human label of the active grouping. */
static void
group_summary(char *out, size_t cap)
{
    snprintf(out, cap, "%s", eh_i18n(group_label(eh_g_group)));
}

/* ── group / sort choosers (source-chooser style sheets) ────────────── */

/* Row list for the group chooser: None + the Author->Series preset +
 * every single dimension with data in the current source.  Returns the
 * row count. */
int
eh_group_options(BsGroupPreset out[], int cap)
{
    int n = 0;
    if (n < cap)
        out[n++] = EH_GROUP_NONE;
    if (n < cap && eh_view_dim_available(EH_GROUP_BY_AUTHOR) &&
        eh_view_dim_available(EH_GROUP_BY_SERIES))
        out[n++] = EH_GROUP_AUTHOR_SERIES;
    if (n < cap && eh_view_dim_available(EH_GROUP_BY_SERIES))
        out[n++] = EH_GROUP_SERIES;
    if (n < cap && eh_view_dim_available(EH_GROUP_BY_AUTHOR))
        out[n++] = EH_GROUP_AUTHOR;
    if (n < cap && eh_view_dim_available(EH_GROUP_BY_YEAR))
        out[n++] = EH_GROUP_YEAR;
    if (n < cap && eh_view_dim_available(EH_GROUP_BY_GENRE))
        out[n++] = EH_GROUP_GENRE;
    return n;
}

static void
eh_group_geom(int *px, int *py, int *pw, int *ph)
{
    BsGroupPreset opts[1 + 5];
    int n = eh_group_options(opts, 1 + 5);
    int w = ScreenWidth();
    *pw = w * 3 / 4;
    *ph = 72 + n * 96 + 24;
    *px = (w - *pw) / 2;
    *py = (eh_content_bottom() - *ph) / 2;
}

void
eh_draw_overlay_group(void)
{
    int pw, ph, px, py;
    eh_group_geom(&px, &py, &pw, &ph);

    eh_dim_content(0);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    char gpath[EH_MAX_TITLE_LEN + 32];
    group_summary(gpath, sizeof gpath);

    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + EH_CTX_PAD, py + 16, eh_i18n("action.group_by"));
        SetFont(tf, DGRAY);
        DrawString(px + EH_CTX_PAD, py + 46, gpath);
        CloseFont(tf);
    }
    DrawLine(px + EH_CTX_PAD, py + 76, px + pw - EH_CTX_PAD, py + 76, LGRAY);

    BsGroupPreset opts[1 + 5];
    int n = eh_group_options(opts, 1 + 5);
    int y0 = py + 84;
    ifont *f = OpenFont(DEFAULTFONTB, 26, 0);
    for (int i = 0; i < n; i++) {
        BsGroupPreset g = opts[i];
        int sel = (eh_g_group == g);
        FillArea(px + 12, y0 + i * 96, pw - 24, 96 - 12, sel ? BLACK : WHITE);
        DrawRect(px + 12, y0 + i * 96, pw - 24, 96 - 12, BLACK);
        if (f != NULL) {
            SetFont(f, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * 96 + (96 - 26) / 2 - 2,
                       eh_i18n(group_label(g)));
        }
    }
    if (f != NULL)
        CloseFont(f);
}

void
eh_draw_overlay_sort(void)
{
    int w = ScreenWidth();
    int pw = w * 3 / 4;
    int n = 4;
    int ph = 72 + n * 96 + 24;
    int px = (w - pw) / 2;
    int py = (eh_content_bottom() - ph) / 2;

    eh_dim_content(0);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + EH_CTX_PAD, py + 16, eh_i18n("action.sort_by"));
        SetFont(tf, DGRAY);
        DrawString(px + EH_CTX_PAD, py + 46, eh_i18n(sort_label()));
        CloseFont(tf);
    }
    DrawLine(px + EH_CTX_PAD, py + 76, px + pw - EH_CTX_PAD, py + 76, LGRAY);

    const char *labels[4] = {
        "sort.title_az", "sort.author", "sort.series", "sort.recent",
    };
    int y0 = py + 84;
    ifont *f = OpenFont(DEFAULTFONTB, 26, 0);
    for (int i = 0; i < n; i++) {
        int sel = (i == (int)eh_g_state.sort);
        FillArea(px + 12, y0 + i * 96, pw - 24, 96 - 12, sel ? BLACK : WHITE);
        DrawRect(px + 12, y0 + i * 96, pw - 24, 96 - 12, BLACK);
        if (f != NULL) {
            SetFont(f, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * 96 + (96 - 26) / 2 - 2, eh_i18n(labels[i]));
        }
    }
    if (f != NULL)
        CloseFont(f);
}

void
eh_draw_overlay_more(void)
{
    int w = ScreenWidth();
    FillArea(0, 0, w, eh_content_bottom(), BLACK);
    int pw = w * 3 / 4;
    int px = w - pw;
    FillArea(px, 0, pw, eh_content_bottom(), WHITE);
    DrawLine(px, 0, px, eh_content_bottom(), BLACK);

    /* No title header: the drawer is a plain row list starting at the
     * first button. */
    char gpath[EH_MAX_TITLE_LEN + 32];
    group_summary(gpath, sizeof gpath);

    const char *labels[EH_MORE_N_ITEMS] = {
        "action.group_by",
        "action.sort_by",
        "action.download_all",
        "action.settings",
        "action.apps",
    };
    const char *vals[EH_MORE_N_ITEMS] = { gpath, eh_i18n(sort_label()) };
    int y0 = EH_MORE_Y0;
    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    for (int i = 0; i < EH_MORE_N_ITEMS; i++) {
        int sel = (i == EH_MORE_GROUP_IDX && eh_g_group != EH_GROUP_NONE);
        FillArea(px + 12, y0 + i * EH_MORE_ITEM_H, pw - 24, EH_MORE_ITEM_H - 12, sel ? BLACK : WHITE);
        if (tf != NULL) {
            SetFont(tf, sel ? WHITE : BLACK);
            DrawString(px + 32, y0 + i * EH_MORE_ITEM_H + (EH_MORE_ITEM_H - 28) / 2 - 2, eh_i18n(labels[i]));
            if (vals[i] != NULL) {
                int tw = StringWidth(vals[i]);
                SetFont(tf, sel ? WHITE : DGRAY);
                DrawString(px + pw - 32 - tw, y0 + i * EH_MORE_ITEM_H + (EH_MORE_ITEM_H - 28) / 2 - 2, vals[i]);
            }
        }
    }
    if (tf != NULL)
        CloseFont(tf);
}

/* ── source chooser ──────────────────────────────────────────────────── */

/* Sheet geometry of the source chooser (top-bar button right of home):
 * a centred 3/4-width sheet with the title row and three source rows. */
void
eh_source_geom(int *px, int *py, int *pw, int *ph)
{
    int w = ScreenWidth();
    *pw = w * 3 / 4;
    *ph = 72 + 3 * 96 + 24;
    *px = (w - *pw) / 2;
    *py = (eh_content_bottom() - *ph) / 2;
}

void
eh_draw_overlay_source(void)
{
    int pw, ph, px, py;
    eh_source_geom(&px, &py, &pw, &ph);

    /* Dim the content area behind the sheet. */
    eh_dim_content(0);
    FillArea(px, py, pw, ph, WHITE);
    DrawRect(px, py, pw, ph, BLACK);
    DrawRect(px + 1, py + 1, pw - 2, ph - 2, BLACK);

    ifont *tf = OpenFont(DEFAULTFONTB, 32, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(px + EH_CTX_PAD, py + 18, eh_i18n("source.title"));
        CloseFont(tf);
    }
    DrawLine(px + EH_CTX_PAD, py + 64, px + pw - EH_CTX_PAD, py + 64, LGRAY);

    const char *labels[3] = {
        eh_i18n("source.kavita"),
        eh_i18n("source.local"),
        eh_i18n("source.folder"),
    };
    int y0 = py + 80;
    ifont *f = OpenFont(DEFAULTFONTB, 28, 0);
    for (int i = 0; i < 3; i++) {
        int sel = (eh_g_state.source == i);
        FillArea(px + 12, y0 + i * 96, pw - 24, 96 - 12, sel ? BLACK : WHITE);
        DrawRect(px + 12, y0 + i * 96, pw - 24, 96 - 12, BLACK);
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
int eh_g_settings_edit = 0;

/* Scratch buffer the keyboard edits; committed on close. */
char eh_g_settings_kb_buf[260];

void
eh_settings_keyboard_handler(char *buffer)
{
    const char *val = buffer ? buffer : "";
    if (eh_g_settings_edit == 1) {
        /* Normalise a bare host[:port] into a full http:// URL so the
         * endpoint builder always gets a scheme.  Cap the host portion
         * first so the prefixed URL always fits g_state.api_base: only
         * the host's tail can be cut, never the "http://" prefix, so
         * the committed value is never a truncated-mid-URL. */
        if (strncmp(val, "http://", 7) != 0 && strncmp(val, "https://", 8) != 0) {
            char tmp[sizeof eh_g_state.api_base];
            snprintf(tmp, sizeof tmp, "%.*s", (int)(sizeof eh_g_state.api_base - 8), val);
            eh_utf8_cap(tmp, sizeof tmp);
            int n = snprintf(eh_g_state.api_base, sizeof eh_g_state.api_base, "http://%s", tmp);
            if (n < 0 || n >= (int)sizeof eh_g_state.api_base)
                eh_LOG("[bookshelf] settings: api host truncated to %d bytes\n",
                    (int)sizeof eh_g_state.api_base - 1);
        } else {
            snprintf(eh_g_state.api_base, sizeof eh_g_state.api_base, "%s", val);
        }
    } else if (eh_g_settings_edit == 2) {
        snprintf(eh_g_state.api_token, sizeof eh_g_state.api_token, "%s", val);
    }
    eh_g_settings_edit = 0;
    /* The on-screen keyboard draws full-screen and wipes the bottom
     * status strip; re-stamp it BEFORE the draw so the panel survives
     * the commit repaint.  Draw the settings page without flushing,
     * then a single full-screen FullUpdate repaints the content and the
     * panel band in one refresh — the same pattern as eh_main.c's
     * keyboard_handler (draw no-flush → one FullUpdate). */
    eh_stamp_panel();
    eh_draw_overlay_settings();
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
eh_settings_reader_label(void)
{
    if (eh_g_state.reader_pref > 0 && eh_g_state.reader_pref <= eh_g_reader_count)
        return eh_g_readers[eh_g_state.reader_pref - 1].label;
    return eh_i18n("settings.reader_auto");
}

void
eh_settings_draw_row(int y, const char *label, const char *value, int editing,
                  ifont *lf, ifont *vf)
{
    int w = ScreenWidth();
    int mx = 32; /* left/right margin */
    FillArea(mx, y, w - 2 * mx, EH_SETTINGS_ROW_H - 12, WHITE);
    DrawRect(mx, y, w - 2 * mx, EH_SETTINGS_ROW_H - 12, BLACK);
    if (editing)
        FillArea(mx + 2, y + 2, w - 2 * mx - 4, EH_SETTINGS_ROW_H - 16, BLACK);

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
eh_settings_draw_button(int y, const char *label, int filled, ifont *f)
{
    int w = ScreenWidth();
    int mx = 32;
    FillArea(mx, y, w - 2 * mx, EH_SETTINGS_BTN_H - 12, filled ? BLACK : WHITE);
    DrawRect(mx, y, w - 2 * mx, EH_SETTINGS_BTN_H - 12, BLACK);
    if (f != NULL) {
        SetFont(f, filled ? WHITE : BLACK);
        int tw = StringWidth(label);
        DrawString((w - tw) / 2, y + (EH_SETTINGS_BTN_H - 12 - 32) / 2, label);
    }
}

void
eh_draw_overlay_settings(void)
{
    int w = ScreenWidth();
    FillArea(0, 0, w, eh_content_bottom(), WHITE);

    /* Shared overlay header: Back chevron + centred title. */
    eh_draw_overlay_header(eh_i18n("settings.title"));

    /* Downloads folder: the pending picker choice, else the resolved
     * effective directory — shown relative to /mnt/ext1. */
    char        dl_shown[256];
    const char *dl = eh_g_settings_dl_dir[0] ? eh_g_settings_dl_dir : eh_g_downloads_dir;
    eh_user_path_display(dl, dl_shown, sizeof dl_shown);
    eh_utf8_fit_width(dl_shown, sizeof dl_shown, w - 2 * 32 - 16);

    int y = 112;
    /* Row/button fonts opened once for the whole settings pass. */
    ifont *lf = OpenFont(DEFAULTFONTB, 26, 0);
    ifont *vf = OpenFont(DEFAULTFONT, 30, 0);
    ifont *bf = OpenFont(DEFAULTFONTB, 32, 0);
    eh_settings_draw_row(y, eh_i18n("settings.api_host"), eh_g_state.api_base, eh_g_settings_edit == 1, lf, vf);
    y += EH_SETTINGS_ROW_H;
    eh_settings_draw_row(y, eh_i18n("settings.api_key"), eh_g_state.api_token, eh_g_settings_edit == 2, lf, vf);
    y += EH_SETTINGS_ROW_H;
    eh_settings_draw_row(y, eh_i18n("settings.reader"), eh_settings_reader_label(), 0, lf, vf);
    y += EH_SETTINGS_ROW_H;
    eh_settings_draw_row(y, eh_i18n("settings.dl_dir"), dl_shown, 0, lf, vf);
    y += EH_SETTINGS_ROW_H;
    /* Install as system app: copies the running binary to the firmware's
     * home-task override path.  On = EinkHome boots as the home screen;
     * off = plain app (stock home).  See eh_sysapp.c. */
    eh_settings_draw_row(y, eh_i18n("settings.system_app"),
                         eh_g_state.sys_app_on ? eh_i18n("settings.sysapp_on")
                                               : eh_i18n("settings.sysapp_off"),
                         0, lf, vf);
    y += EH_SETTINGS_ROW_H + 24;
    eh_settings_draw_button(y, eh_i18n("settings.save"), 1, bf);
    y += EH_SETTINGS_BTN_H;
    eh_settings_draw_button(y, eh_i18n("settings.logs"), 0, bf);
    y += EH_SETTINGS_BTN_H;
    eh_settings_draw_button(y, eh_i18n("settings.licenses"), 0, bf);
    if (bf != NULL)
        CloseFont(bf);
    if (vf != NULL)
        CloseFont(vf);
    if (lf != NULL)
        CloseFont(lf);
}
