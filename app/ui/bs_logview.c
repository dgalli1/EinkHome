/* bs_logview.c — part of the bookshelf app (see bs_core.h) */

#include "bs_core.h"
#include "bs_config.h"
#include "bs_ui.h"

/* ── log viewer (Settings → Show logs) ──────────────────────────────── */

/* Read the log tail: at most `cap` bytes, aligned to a line boundary.
 * Returns a malloc'd NUL-terminated buffer, or NULL when the log does
 * not exist yet. */
static char *
log_tail_read(size_t cap)
{
    const char *path = bs_log_path();
    FILE       *f = fopen(path, "r");
    if (f == NULL)
        return NULL;
    if (fseek(f, 0, SEEK_END) != 0) {
        fclose(f);
        return NULL;
    }
    long size = ftell(f);
    long start = 0;
    if (size > (long)cap) {
        start = size - (long)cap;
        if (fseek(f, start, SEEK_SET) != 0) {
            fclose(f);
            return NULL;
        }
        int c;
        while ((c = fgetc(f)) != EOF && c != '\n')
            ;
    } else {
        /* Back to the beginning: the SEEK_END above left the position
         * at EOF. */
        if (fseek(f, 0, SEEK_SET) != 0) {
            fclose(f);
            return NULL;
        }
    }
    size_t n = (size_t)(size - start);
    // NOLINTNEXTLINE(clang-analyzer-optin.portability.UnixAPI) — n >= 0 and the +1 guarantees a non-zero allocation.
    char  *buf = malloc(n + 1);
    if (buf == NULL) {
        fclose(f);
        return NULL;
    }
    size_t got = fread(buf, 1, n, f);
    fclose(f);
    // NOLINTNEXTLINE(clang-analyzer-security.ArrayBound) — got <= n (fread caps at n).
    buf[got] = '\0';
    return buf;
}

/* A wrapped display row: a span of the log buffer. */
typedef struct {
    const char *p;
    int         len;
} BsLogRow;

/* Width of a non-NUL-terminated span (the SDK only measures C
 * strings). */
static int
span_width(const char *p, int len)
{
    char tmp[1024];
    if (len > (int)sizeof tmp - 1)
        len = (int)sizeof tmp - 1;
    memcpy(tmp, p, (size_t)len);
    // NOLINTNEXTLINE(clang-analyzer-security.ArrayBound) — len <= sizeof tmp - 1, indexed forward into tmp.
    tmp[len] = '\0';
    return StringWidth(tmp);
}

/* Greedy word wrap of ONE line [line_start, line_end) into dst, at most
 * `mcap` rows (forward order).  Rows point into the line's text (never
 * modified).  Returns the row count. */
static int
log_wrap_line(const char *line_start, const char *line_end, int maxw,
              BsLogRow *dst, int mcap)
{
    int         count = 0;
    const char *ws = line_start;
    while (ws < line_end && count < mcap) {
        const char *we = ws;
        while (we < line_end && *we != ' ')
            we++;
        if (we == ws) { /* collapse space runs */
            ws++;
            continue;
        }
        int wordw = span_width(ws, (int)(we - ws));
        int curw = dst[count].len > 0 ? span_width(dst[count].p, dst[count].len) : 0;
        if (dst[count].len > 0 && curw + wordw + 6 > maxw) {
            count++;
            if (count >= mcap)
                break;
        }
        if (dst[count].len == 0)
            dst[count].p = ws;
        dst[count].len += (int)(we - ws);
        if (we < line_end)
            dst[count].len++; /* the separating space */
        ws = we;
    }
    if (count < mcap && dst[count].len > 0)
        count++; /* finalise the trailing partial row */
    return count;
}

/* Greedy word wrap of the log tail into at most `cap` rows, anchored on
 * the NEWEST content: lines are walked backward from the last one and
 * the resulting rows are returned oldest → newest (row 0 = oldest kept).
 *
 * A forward wrap of a big log would fill the cap-bounded row array with
 * the OLDEST rows of the tail window and never wrap the newest lines,
 * so an open viewer would show stale content instead of the current
 * tail.  Rows point into `text` (never modified).  Returns the row
 * count. */
static int
log_wrap_rows_last(const char *text, int maxw, BsLogRow *rows, int cap)
{
    int        n = 0;
    const char *text_end = text + strlen(text);
    const char *line_end = text_end;
    BsLogRow   tmp[cap]; /* per-line staging (<cap rows; VLAs supported) */
    while (n < cap && line_end > text) {
        /* Back up over this line, skipping its trailing LF. */
        const char *line_start = line_end - 1;
        while (line_start > text && line_start[-1] != '\n')
            line_start--;
        const char *seg_end = line_end;
        if (line_end - line_start > 0 && line_end[-1] == '\n')
            seg_end = line_end - 1;
        /* log_wrap_line treats dst[0].len==0 as "start a fresh row";
         * the staging VLA must be zeroed per line or leftover state from
         * the previous line accumulates into corrupt rows. */
        memset(tmp, 0, sizeof tmp);
        int lc = log_wrap_line(line_start, seg_end, maxw, tmp, cap - n);
        /* Store the line's rows newest-first so the newest overall row
         * is at the front of the kept set (flipped below). */
        for (int i = lc - 1; i >= 0 && n < cap; i--)
            rows[n++] = tmp[i];
        line_end = line_start;
    }
    /* Flip the kept set so the caller sees oldest → newest: row 0 is
     * the oldest kept row, the last row is the current log tail. */
    for (int i = 0, j = n - 1; i < j; i++, j--) {
        BsLogRow t = rows[i];
        rows[i] = rows[j];
        rows[j] = t;
    }
    return n;
}

/* Cached wrap of the log tail.  Each scroll tap used to re-read up to
 * 160 KB from flash and re-wrap the whole buffer (a per-word memcpy +
 * StringWidth per word).  The last wrapped pass is cached and keyed by
 * the log file's (size, mtime); it is only rebuilt when the file
 * changed (i.e. the log grew).  Rows point into the owned `text`
 * buffer, so the text must outlive the rows — both live in the cache. */
typedef struct {
    long    size;
    long    mtime;
    int     maxw; /* wrap width the rows were laid out with */
    int     cap;  /* rows array capacity */
    char   *text; /* owned copy (160 KB tail); rows point into it */
    BsLogRow *rows;
    int     nrows;
} BsLogWrapCache;

static BsLogWrapCache g_log_wrap;

static void
log_wrap_cache_clear(void)
{
    free(g_log_wrap.text);
    free(g_log_wrap.rows);
    g_log_wrap.text = NULL;
    g_log_wrap.rows = NULL;
    g_log_wrap.nrows = 0;
    g_log_wrap.size = -1;
    g_log_wrap.mtime = -1;
}

/* Return a valid cached wrap for the current log tail, rebuilding it
 * only when the file (size, mtime) changed or a different width/cap is
 * needed.  NULL when the log does not exist. */
static const BsLogWrapCache *
log_wrap_get(int maxw, int cap)
{
    struct stat st;
    if (iv_stat(bs_log_path(), &st) != 0) {
        log_wrap_cache_clear();
        return NULL;
    }
    if (g_log_wrap.text != NULL && g_log_wrap.size == st.st_size &&
        g_log_wrap.mtime == st.st_mtime && g_log_wrap.maxw == maxw &&
        g_log_wrap.cap >= cap)
        return &g_log_wrap;

    /* Rebuild: read the tail and wrap it once. */
    char *text = log_tail_read(160 * 1024);
    if (text == NULL) {
        log_wrap_cache_clear();
        return NULL;
    }
    /* Zeroed rows: log_wrap_rows uses `.len == 0` as its empty-row
     * sentinel and dereferences `.p` whenever `.len > 0`, so the
     * array must NOT come from a plain malloc. */
    BsLogRow *rows = calloc((size_t)cap, sizeof(BsLogRow));
    if (rows == NULL) {
        free(text);
        log_wrap_cache_clear();
        return NULL;
    }
    int nrows = log_wrap_rows_last(text, maxw, rows, cap);
    log_wrap_cache_clear();
    g_log_wrap.text = text;
    g_log_wrap.rows = rows;
    g_log_wrap.nrows = nrows;
    g_log_wrap.size = st.st_size;
    g_log_wrap.mtime = st.st_mtime;
    g_log_wrap.maxw = maxw;
    g_log_wrap.cap = cap;
    return &g_log_wrap;
}

/* Resolve the first visible row of the log tail's last full page using
 * the current view geometry — the materialised position the viewer
 * shows while pinned.  Shared by bs_draw_log_view (pinning) and the
 * scroll handler (paging up from a pinned tail).  Returns 0 when the
 * log is absent or fits entirely on one page. */
int
bs_log_view_tail_first(void)
{
    int w = ScreenWidth();
    int h = bs_content_bottom();
    int btn_y = h - 8 - BS_SCROLL_BTN_H;
    int body_h = btn_y - BS_LOG_BODY_TOP - 8;
    if (body_h < BS_LOG_ROW_H)
        body_h = BS_LOG_ROW_H;
    int              rows_vis = body_h / BS_LOG_ROW_H;
    const BsLogWrapCache *wc = log_wrap_get(w - 48, rows_vis * 8);
    if (wc == NULL)
        return 0;
    int maxf = wc->nrows - rows_vis;
    return maxf < 0 ? 0 : maxf;
}

/* Full-screen log viewer: the app log tail, line-wrapped, page-scrolled
 * with the two bottom buttons; Back returns to the shelf. */
void
bs_draw_log_view(void)
{
    int w = ScreenWidth();
    int h = bs_content_bottom();
    FillArea(0, 0, w, h, WHITE);

    /* Shared overlay header: Back chevron + centred title.  The log
     * file path rides along as a small grey line in its own band just
     * below the header border (inside the header it would collide with
     * the centred title). */
    bs_draw_overlay_header(bs_i18n("log.title"));
    ifont *pf = OpenFont(DEFAULTFONT, 20, 0);
    if (pf != NULL) {
        SetFont(pf, DGRAY);
        char shown[200];
        snprintf(shown, sizeof shown, "%s", bs_log_path());
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

    int   first = 0;
    int   max_first = 0;
    const BsLogWrapCache *wc = log_wrap_get(w - 48, rows_vis * 8);
    if (wc == NULL) {
        ifont *ef = OpenFont(DEFAULTFONT, 26, 0);
        if (ef != NULL) {
            SetFont(ef, DGRAY);
            DrawString(32, body_top + 40, bs_i18n("log.empty"));
            CloseFont(ef);
        }
    } else {
        const BsLogRow *rows = wc->rows;
        int           nrows = wc->nrows;
        int maxf = nrows - rows_vis;
        if (maxf < 0)
            maxf = 0;
        max_first = maxf;
        /* log_scroll < 0 means pinned to the tail: keep re-pinning on
         * every redraw so new lines stay visible while the viewer is
         * open.  Only an explicit scroll (see bs_on_tap_log_view)
         * materialises a concrete first-line index. */
        if (bs_g_state.log_scroll < 0) {
            first = max_first;
        } else {
            first = bs_g_state.log_scroll;
            if (first > max_first)
                first = max_first;
            if (first < 0)
                first = 0;
            bs_g_state.log_scroll = first;
        }

        ifont *lf = OpenFont(DEFAULTFONT, BS_LOG_FONT_PX, 0);
        if (lf != NULL) {
            SetFont(lf, BLACK);
            for (int i = 0; i < rows_vis && first + i < nrows; i++) {
                int         len = rows[first + i].len;
                const char *p = rows[first + i].p;
                if (len > 480)
                    len = 480;
                char tmp[512];
                memcpy(tmp, p, (size_t)len);
                tmp[len] = '\0';
                bs_utf8_fit_width(tmp, sizeof tmp, w - 48);
                DrawString(24, body_top + i * BS_LOG_ROW_H, tmp);
            }
            CloseFont(lf);
        }
    }

    /* Stock corner scroll buttons: older = up, newer = down. */
    bs_draw_scroll_buttons(first > 0, first < max_first);
}
