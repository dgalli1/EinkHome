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
    char  *buf = malloc(n + 1);
    if (buf == NULL) {
        fclose(f);
        return NULL;
    }
    size_t got = fread(buf, 1, n, f);
    fclose(f);
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
    tmp[len] = '\0';
    return StringWidth(tmp);
}

/* Greedy word wrap of the log text into display rows no wider than
 * `maxw` px.  Rows point into `text` (never modified).  Returns the
 * row count. */
static int
log_wrap_rows(const char *text, int maxw, BsLogRow *rows, int cap)
{
    int         count = 0;
    const char *line = text;
    while (*line != '\0' && count < cap) {
        const char *nl = strchr(line, '\n');
        size_t      llen = nl ? (size_t)(nl - line) : strlen(line);
        const char *end = line + llen;
        const char *ws = line;
        while (ws < end) {
            const char *we = ws;
            while (we < end && *we != ' ')
                we++;
            if (we == ws) { /* collapse space runs */
                ws++;
                continue;
            }
            int wordw = span_width(ws, (int)(we - ws));
            int curw = rows[count].len > 0 ? span_width(rows[count].p, rows[count].len) : 0;
            if (rows[count].len > 0 && curw + wordw + 6 > maxw) {
                count++;
                if (count >= cap)
                    goto done;
            }
            if (rows[count].len == 0)
                rows[count].p = ws;
            rows[count].len += (int)(we - ws);
            if (we < end)
                rows[count].len++; /* the separating space */
            ws = we;
        }
        if (rows[count].len > 0) {
            count++;
            if (count >= cap)
                goto done;
        }
        if (nl == NULL)
            break;
        line = nl + 1;
    }
done:
    return count;
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
    int nrows = log_wrap_rows(text, maxw, rows, cap);
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

/* Full-screen log viewer: the app log tail, line-wrapped, page-scrolled
 * with the two bottom buttons; Back returns to the shelf. */
void
bs_draw_log_view(void)
{
    int w = ScreenWidth();
    int h = bs_content_bottom();
    FillArea(0, 0, w, h, WHITE);

    /* Header: back button + title + file path. */
    FillArea(BS_LOG_BACK_X, BS_LOG_BACK_Y, BS_LOG_BACK_W, BS_LOG_BACK_H, WHITE);
    DrawRect(BS_LOG_BACK_X, BS_LOG_BACK_Y, BS_LOG_BACK_W, BS_LOG_BACK_H, BLACK);
    ifont *bf = OpenFont(DEFAULTFONTB, 26, 0);
    if (bf != NULL) {
        SetFont(bf, BLACK);
        int tw = StringWidth(bs_i18n("log.back"));
        DrawString(BS_LOG_BACK_X + (BS_LOG_BACK_W - tw) / 2,
                   BS_LOG_BACK_Y + (BS_LOG_BACK_H - 26) / 2,
                   bs_i18n("log.back"));
        CloseFont(bf);
    }
    ifont *tf = OpenFont(DEFAULTFONTB, 34, 0);
    if (tf != NULL) {
        SetFont(tf, BLACK);
        DrawString(BS_LOG_BACK_X + BS_LOG_BACK_W + 16, BS_LOG_BACK_Y + 8, bs_i18n("log.title"));
        CloseFont(tf);
    }
    ifont *pf = OpenFont(DEFAULTFONT, 20, 0);
    if (pf != NULL) {
        SetFont(pf, DGRAY);
        char shown[200];
        snprintf(shown, sizeof shown, "%s", bs_log_path());
        bs_utf8_fit_width(shown, sizeof shown, w - BS_LOG_BACK_X - BS_LOG_BACK_W - 32);
        DrawString(BS_LOG_BACK_X + BS_LOG_BACK_W + 16, BS_LOG_BACK_Y + 46, shown);
        CloseFont(pf);
    }
    DrawLine(0, BS_LOG_BACK_Y + BS_LOG_BACK_H + 8, w, BS_LOG_BACK_Y + BS_LOG_BACK_H + 8, BLACK);

    int body_top = BS_LOG_BACK_Y + BS_LOG_BACK_H + 16;
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
        first = bs_g_state.log_scroll < 0 ? max_first : bs_g_state.log_scroll;
        if (first > max_first)
            first = max_first;
        if (first < 0)
            first = 0;
        bs_g_state.log_scroll = first;

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
