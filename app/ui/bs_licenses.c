/* bs_licenses.c — third-party licenses viewer (Settings → Licenses).
 *
 * A full-screen overlay, shaped like the log viewer (bs_logview.c):
 * the shared overlay header plus a scrollable body and the stock corner
 * scroll buttons.  Two views share the overlay:
 *
 *   - LIST   (bs_g_state.lic_sel < 0): one row per bundled license —
 *             component name over its licence type.  Tapping a row
 *             opens that license's full text; Back closes to the shelf.
 *   - DETAIL (lic_sel >= 0): the license text, word-wrapped with the
 *             same row metrics as the log viewer and page-scrolled.
 *             Back returns to the list.
 *
 * The license texts ship as C strings (bs_licenses.c), so this viewer
 * works identically on the device, the emulator and the PC build. */

#include "bs_core.h"
#include "bs_licenses.h"
#include "bs_ui.h"

/* ── word wrap (detail view) ───────────────────────────────────────── */

/* A wrapped display row: a span of the license text. */
typedef struct {
    const char *p;
    int         len;
    int         blank; /* a paragraph-gap row (empty source line) */
} BsLicRow;

/* Width of a non-NUL-terminated span (the SDK only measures C
 * strings). */
static int
span_width(const char *p, int len)
{
    char tmp[1024];
    if (len > (int)sizeof tmp - 1)
        len = (int)sizeof tmp - 1;
    memcpy(tmp, p, (size_t)len);
    tmp[len] = '\0';
    return StringWidth(tmp);
}

/* Place one word of a source line starting at *ws into rows, advancing
 * *ws through `rows`/`*count`.  Returns:
 *   1  space run skipped (*ws advanced past it, no word placed)
 *   2  line ran out of rows (no room on a fresh row)
 *   0  word placed */
static int
lic_wrap_word(const char *end, int maxw, BsLicRow *rows, int cap,
              int *count, const char **ws)
{
    const char *we = *ws;
    while (we < end && *we != ' ')
        we++;
    if (we == *ws) { /* collapse space runs */
        (*ws)++;
        return 1;
    }
    int wordw = span_width(*ws, (int)(we - *ws));
    int curw = rows[*count].len > 0 ? span_width(rows[*count].p, rows[*count].len) : 0;
    if (rows[*count].len > 0 && curw + wordw + 6 > maxw) {
        (*count)++;
        if (*count >= cap)
            return 2;
    }
    if (rows[*count].len == 0)
        rows[*count].p = *ws;
    rows[*count].len += (int)(we - *ws);
    if (we < end)
        rows[*count].len++; /* the separating space */
    *ws = we;
    return 0;
}

/* Word-wrap one non-blank source line [line, end) into rows.  `count`
 * is the starting slot and is advanced past the wrapped rows. */
static void
lic_wrap_words(BsLicRow *rows, int *count, int cap,
               const char *line, const char *end, int maxw)
{
    const char *ws = line;
    while (ws < end && *count < cap) {
        int r = lic_wrap_word(end, maxw, rows, cap, count, &ws);
        if (r == 1) /* space run */
            continue;
        if (r == 2) /* no room on a fresh row */
            break;
    }
    if (*count < cap && rows[*count].len > 0)
        (*count)++; /* finalise the trailing partial row */
}

/* Greedy word wrap of the license text into display rows no wider than
 * `maxw` px.  Rows point into `text` (never modified); blank source
 * lines become `blank` gap rows so paragraph shape survives.  Returns
 * the row count (capped).  `rows` must be zeroed (blank/len sentinel). */
static int
lic_wrap_rows(const char *text, int maxw, BsLicRow *rows, int cap)
{
    int         count = 0;
    const char *line = text;
    while (*line != '\0' && count < cap) {
        const char *nl = strchr(line, '\n');
        size_t      llen = nl ? (size_t)(nl - line) : strlen(line);
        if (llen == 0) {
            /* blank source line → a dedicated gap row */
            rows[count].blank = 1;
            count++;
            if (count >= cap)
                break;
        } else {
            lic_wrap_words(rows, &count, cap, line, line + llen, maxw);
        }
        if (nl == NULL)
            break;
        line = nl + 1;
    }
    return count;
}

/* ── rendering ──────────────────────────────────────────────────────── */

/* LIST view: one row per license (name over type), scrollable. */
static void
lic_draw_list(int w, int h, int n)
{
    bs_draw_overlay_header(bs_i18n("licenses.title"));
    int body_top = BS_LIC_LIST_TOP;
    int btn_y = h - 8 - BS_SCROLL_BTN_H;
    int body_h = btn_y - body_top - 8;
    int rows_vis = body_h / BS_LIC_LIST_H;
    if (rows_vis < 1)
        rows_vis = 1;
    int max_first = n - rows_vis;
    if (max_first < 0)
        max_first = 0;
    int first = bs_g_state.lic_scroll;
    if (first > max_first)
        first = max_first;
    if (first < 0)
        first = 0;
    bs_g_state.lic_scroll = first;

    ifont *nf = OpenFont(DEFAULTFONTB, 30, 0);
    ifont *lf = OpenFont(DEFAULTFONT, 24, 0);
    for (int i = 0; i < rows_vis && first + i < n; i++) {
        const BsLicense *lic = bs_license(first + i);
        int             y = body_top + i * BS_LIC_LIST_H;
        FillArea(16, y, w - 32, BS_LIC_LIST_H - 12, WHITE);
        DrawRect(16, y, w - 32, BS_LIC_LIST_H - 12, BLACK);
        if (nf != NULL) {
            SetFont(nf, BLACK);
            DrawString(32, y + 14, lic->name);
        }
        if (lf != NULL) {
            SetFont(lf, DGRAY);
            DrawString(32, y + 14 + 36, lic->license);
        }
    }
    if (lf != NULL)
        CloseFont(lf);
    if (nf != NULL)
        CloseFont(nf);
    bs_draw_scroll_buttons(first > 0, first < max_first);
}

/* DETAIL view: the selected license's full text, word-wrapped and
 * page-scrolled. */
static void
lic_draw_detail(int w, int h, int n)
{
    if (bs_g_state.lic_sel >= n)
        bs_g_state.lic_sel = n - 1;
    if (bs_g_state.lic_sel < 0)
        bs_g_state.lic_sel = 0;
    const BsLicense *lic = bs_license(bs_g_state.lic_sel);

    bs_draw_overlay_header(lic->name);
    /* One-line attribution band under the header: type · where it is
     * used. */
    ifont *pf = OpenFont(DEFAULTFONT, 20, 0);
    if (pf != NULL) {
        SetFont(pf, DGRAY);
        char shown[256];
        snprintf(shown, sizeof shown, "%s  ·  %s", lic->license, lic->note);
        bs_utf8_fit_width(shown, sizeof shown, w - 2 * 32);
        DrawString((w - StringWidth(shown)) / 2, BS_OVERLAY_HEADER_H + 10, shown);
        CloseFont(pf);
    }

    int body_top = BS_LOG_BODY_TOP;
    int btn_y = h - 8 - BS_SCROLL_BTN_H;
    int body_h = btn_y - body_top - 8;
    if (body_h < BS_LOG_ROW_H)
        body_h = BS_LOG_ROW_H;
    int rows_vis = body_h / BS_LOG_ROW_H;

    BsLicRow rows[BS_LIC_MAX_ROWS];
    memset(rows, 0, sizeof rows); /* blank/len sentinel needs zeroed rows */
    int nrows = lic_wrap_rows(lic->text, w - 48, rows, BS_LIC_MAX_ROWS);
    int maxf = nrows - rows_vis;
    if (maxf < 0)
        maxf = 0;
    int first = bs_g_state.lic_scroll > maxf ? maxf : bs_g_state.lic_scroll;
    if (first < 0)
        first = 0;
    bs_g_state.lic_scroll = first;

    ifont *lf = OpenFont(DEFAULTFONT, BS_LOG_FONT_PX, 0);
    if (lf != NULL) {
        SetFont(lf, BLACK);
        for (int i = 0; i < rows_vis && first + i < nrows; i++) {
            const BsLicRow *r = &rows[first + i];
            if (r->blank)
                continue;
            int len = r->len;
            if (len > 480)
                len = 480;
            char tmp[512];
            // NOLINTNEXTLINE(clang-analyzer-core.NonNullParamChecker) — r->blank rows are skipped above, so r->p is non-NULL here.
            memcpy(tmp, r->p, (size_t)len);
            tmp[len] = '\0';
            bs_utf8_fit_width(tmp, sizeof tmp, w - 48);
            DrawString(24, body_top + i * BS_LOG_ROW_H, tmp);
        }
        CloseFont(lf);
    }
    bs_draw_scroll_buttons(first > 0, first < maxf);
}

void
bs_draw_licenses_view(void)
{
    int w = ScreenWidth();
    int h = bs_content_bottom();
    FillArea(0, 0, w, h, WHITE);

    int n = bs_license_count();

    /* LIST view: one row per license (name over type). */
    if (bs_g_state.lic_sel < 0) {
        lic_draw_list(w, h, n);
        return;
    }

    /* DETAIL view: the selected license's full text. */
    lic_draw_detail(w, h, n);
}