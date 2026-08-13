/* bs_overlays.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_browser.h"
#include "bs_model.h"
#include "bs_ui.h"

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

    const char *labels[] = {
        "group.all",
        "group.author",
        "group.series",
        "group.recent",
    };
    int n = (int)(sizeof labels / sizeof labels[0]);
    /* The menu sheet reuses the More overlay's row geometry (same
     * 96/88 values — one name, one layout). */
    int y0 = BS_MORE_Y0;
    int item_h = BS_MORE_ITEM_H;
    ifont *tf = OpenFont(DEFAULTFONTB, 28, 0);
    for (int i = 0; i < n; i++) {
        int sel = (i == (int)bs_g_state.group);
        FillArea(12, y0 + i * item_h, pw - 24, item_h - 12, sel ? BLACK : WHITE);
        if (tf != NULL) {
            SetFont(tf, sel ? WHITE : BLACK);
            DrawString(32, y0 + i * item_h + (item_h - 28) / 2 - 2, bs_i18n(labels[i]));
        }
    }
    if (tf != NULL)
        CloseFont(tf);
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
