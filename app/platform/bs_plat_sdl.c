/* bs_plat_sdl.c — native PC backend over SDL2 (see app/platform/bs_plat.h).
 *
 * Renders the app into an SDL2 window (Wayland or X11, whichever SDL
 * picks) and maps SDL input onto the inkview event surface the app
 * drives.  It implements both the bs_plat_* backend functions AND the
 * full inkview API subset the app links (declared in app/platform/sdl/).
 *
 * Design notes:
 *  - The app draws on a logical 1072x1448 canvas (the default U633
 *    panel) and calls FullUpdate()/PartialUpdate() to commit.  We render
 *    into an offscreen RGBA surface at that size and blit it (scaled to
 *    fit) to the SDL window on every commit — a windowed CRT, not e-ink.
 *  - Mouse = pointer: button down/up -> EVT_POINTERDOWN/UP, motion ->
 *    EVT_POINTERMOVE, coordinates in the logical canvas space.
 *  - Physical keys map onto the IV_KEY_* codes the app's key handler
 *    understands (MENU/HOME/PREV/NEXT/BACK); typed text goes to the
 *    OpenKeyboard buffer live and the handler fires on Enter.
 *  - HTTP (QuickDownload*) uses libcurl so the synced library actually
 *    loads; everything else is a faithful no-op / sensible default.
 *
 * Build: `make pc` (host gcc + SDL2/SDL2_ttf/curl).  Define
 * BS_PLATFORM_SDL so app/platform/bs_plat.h selects the compat headers.
 */

#include <SDL.h>
#include <SDL_ttf.h>
#include <SDL_image.h>
#include <curl/curl.h>

#include <stdlib.h>
#include <string.h>
#include <stdio.h>

#include "bs_core.h"
#include "bs_ui.h"
#include "bs_launcher.h"
#include "sdl/inkview.h"
#include "sdl/hwconfig.h"

/* ── window / canvas state ──────────────────────────────────────────── */
#define PC_W 1072
#define PC_H 1448

static SDL_Window   *g_win;
static SDL_Renderer *g_ren;
static SDL_Texture  *g_tex;
static uint32_t     *g_px;      /* logical RGBA canvas (PC_W*PC_H) */
static int (*g_on_event)(int, int, int);
static int g_running;
static int g_text_input;
static int g_dump_pending;
static Uint32 g_dump_at;
static char g_dump_path[512];

/* Clip rect — the app's SetClip()/drawing contract.  The launcher (and
 * other overlays) depend on it to keep scrolled rows from bleeding into
 * the header: on the device firmware SetClip actually clips the
 * renderer, so the SDL backend must clip its own pixel writes to match
 * (a no-op here lets a scrolled row's label paint over the header). */
static int g_clip = 0;   /* clip enabled */
static int g_cx0, g_cy0, g_cx1, g_cy1; /* inclusive-ish bounds */

static void dump_frame(const char *path);
#ifdef BS_ENABLE_TEST_IPC
static void AppendIpcText(const char *s);
static void ipc_init(void);
static void ipc_poll_and_process(void);
#endif

/* Window / input note: the window is resizable and
 * SDL_RenderSetLogicalSize is active, so SDL already reports pointer
 * coordinates in the logical 1072x1448 space — no window→logical
 * scaling is needed here (double-scaling would put taps out of range
 * and miss every button). */

/* ── colour handling ────────────────────────────────────────────────── */
/* The app's colour constants are grayscale-equal RGB values
 * (BLACK=0x000000, DGRAY=0x555555, LGRAY=0xaaaaaa, WHITE=0xffffff).
 * Convert an inkview colour to the 32-bit RGBA the canvas stores. */
static uint32_t
col32(int color)
{
    uint8_t b = (uint8_t)(color & 0xff);
    uint8_t g = (uint8_t)((color >> 8) & 0xff);
    uint8_t r = (uint8_t)((color >> 16) & 0xff);
    return 0xff000000u | ((uint32_t)r << 0) | ((uint32_t)g << 8) | ((uint32_t)b << 16);
}

/* ── drawing: inkview surface over the canvas ───────────────────────── */

/* 1 when (x,y) is inside the current clip rect.  Used by every pixel
 * writer so scrolled content stays within the region the app clipped. */
static int
px_visible(int x, int y)
{
    if (!g_clip) return 1;
    return x >= g_cx0 && x < g_cx1 && y >= g_cy0 && y < g_cy1;
}

void
SetClip(int x, int y, int w, int h)
{
    if (w <= 0 || h <= 0) {
        g_clip = 0;              /* nothing visible */
        return;
    }
    g_clip = 1;
    g_cx0 = x;
    g_cy0 = y;
    g_cx1 = x + w;
    g_cy1 = y + h;
}

void
FillArea(int x, int y, int w, int h, int color)
{
    if (w <= 0 || h <= 0) return;
    uint32_t c = col32(color);
    for (int j = y; j < y + h && j < PC_H; j++) {
        if (j < 0) continue;
        for (int i = x; i < x + w && i < PC_W; i++) {
            if (i < 0) continue;
            if (!px_visible(i, j)) continue;
            g_px[(size_t)j * PC_W + (size_t)i] = c;
        }
    }
}

void
DrawLine(int x1, int y1, int x2, int y2, int color)
{
    /* Simple horizontal/vertical + diagonal Bresenham. */
    uint32_t c = col32(color);
    int dx = abs(x2 - x1), sx = x1 < x2 ? 1 : -1;
    int dy = -abs(y2 - y1), sy = y1 < y2 ? 1 : -1;
    int err = dx + dy, x = x1, y = y1;
    for (;;) {
        if (x >= 0 && x < PC_W && y >= 0 && y < PC_H && px_visible(x, y))
            g_px[(size_t)y * PC_W + (size_t)x] = c;
        if (x == x2 && y == y2) break;
        int e2 = 2 * err;
        if (e2 >= dy) { err += dy; x += sx; }
        if (e2 <= dx) { err += dx; y += sy; }
    }
}

void
DrawRect(int x, int y, int w, int h, int color)
{
    DrawLine(x, y, x + w, y, color);
    DrawLine(x, y + h, x + w, y + h, color);
    DrawLine(x, y, x, y + h, color);
    DrawLine(x + w, y, x + w, y + h, color);
}


/* ── fonts (SDL_ttf) ────────────────────────────────────────────────── */
struct cairofont { TTF_Font *f; int px; int bold; };

char *
iv_get_default_font(FONT_TYPE fonttype)
{
    static const char *normal = "Noto Sans";
    static const char *bold = "Noto Sans Bold";
    return fonttype == FONT_BOLD ? (char *)bold : (char *)normal;
}

/* Session font cache: fonts stay alive for the whole run, exactly like
 * the app's own font/bitmap handling (the SDK never frees fonts while
 * they may be the "current" font).  The app calls OpenFont/CloseFont per
 * draw pass and relies on SetFont-then-StringWidth/CloseFont ordering;
 * if CloseFont freed the TTF, a later StringWidth with a stale current
 * font would read freed memory (a SIGSEGV in the log viewer's word wrap,
 * which measures text before SetFont has run).  Caching fonts keeps
 * g_cur_font valid for the session, matching PB/SDK semantics. */
#define BSFONT_CACHE_MAX 128
static ifont       *g_font_cache[BSFONT_CACHE_MAX];
static int          g_font_cache_n;
static ifont       *g_cur_font;
static int          g_cur_color;

static ifont *
font_cache_find(int px, int bold)
{
    for (int i = 0; i < g_font_cache_n; i++) {
        if (g_font_cache[i]->size == px &&
            (int)g_font_cache[i]->isbold == bold)
            return g_font_cache[i];
    }
    return NULL;
}

ifont *
OpenFont(const char *name, int size, int aa)
{
    (void)aa;
    int bold = (name != NULL && strstr(name, "Bold") != NULL);
    ifont *hit = font_cache_find(size, bold);
    if (hit != NULL)
        return hit;
    if (g_font_cache_n >= BSFONT_CACHE_MAX)
        return NULL;

    struct cairofont *cf = calloc(1, sizeof *cf);
    if (!cf) return NULL;
    cf->px = size;
    cf->bold = bold;
    cf->f = TTF_OpenFont(cf->bold ? "/usr/share/fonts/noto/NotoSans-Bold.ttf"
                                  : "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                         size);
    if (!cf->f) {
        cf->f = TTF_OpenFont(cf->bold ? "/usr/share/fonts/TTF/NotoSans-Bold.ttf"
                                      : "/usr/share/fonts/TTF/NotoSans-Regular.ttf",
                             size);
    }
    if (!cf->f) {
        fprintf(stderr, "[pc] SDL_ttf: %s\n", TTF_GetError());
        free(cf);
        return NULL;
    }
    ifont *f = calloc(1, sizeof *f);
    if (!f) { TTF_CloseFont(cf->f); free(cf); return NULL; }
    f->name = strdup(name ? name : "Noto Sans");
    f->family = strdup("Noto Sans");
    f->size = size;
    f->isbold = (unsigned char)bold;
    f->height = size;
    f->baseline = size;
    f->fdata = cf;
    g_font_cache[g_font_cache_n++] = f; /* cached for the session */
    return f;
}

void
CloseFont(ifont *f)
{
    /* Fonts are cached for the session (see OpenFont); nothing to free
     * here — the cached TTF + ifont stay valid so a later StringWidth
     * with this as the "current" font never reads freed memory. */
    (void)f;
}

void
SetFont(const ifont *font, int color)
{
    g_cur_font = (ifont *)font;
    g_cur_color = color;
}

int
StringWidth(const char *s)
{
    if (!g_cur_font || !g_cur_font->fdata)
        return 0;
    struct cairofont *cf = g_cur_font->fdata;
    if (cf->f == NULL) return 0;
    int w;
    if (TTF_SizeUTF8(cf->f, s ? s : "", &w, NULL) != 0) return 0;
    return w;
}

/* Alpha-blend one glyph pixel row (at canvas y=yy) onto the canvas.
 * Extracted from DrawString to keep that hot loop's complexity down. */
static void
draw_glyph_row(int yy, int gx, int gw, const uint8_t *row, SDL_Color fg)
{
    for (int i = 0; i < gw; i++) {
        int xx = gx + i;
        if (xx < 0 || xx >= PC_W) continue;
        if (!px_visible(xx, yy)) continue;
        /* Alpha blend the glyph onto the canvas. */
        uint8_t a = row[(size_t)i * 4 + 3];
        if (a == 0) continue;
        uint32_t dst = g_px[(size_t)yy * PC_W + (size_t)xx];
        uint32_t src = ((uint32_t)fg.a << 24) |
                       ((uint32_t)row[(size_t)i * 4] << 0) |
                       ((uint32_t)row[(size_t)i * 4 + 1] << 8) |
                       ((uint32_t)row[(size_t)i * 4 + 2] << 16);
        uint32_t out = 0;
        for (int k = 0; k < 3; k++) {
            int dv = (dst >> (k * 8)) & 0xff;
            int sv = (src >> (k * 8)) & 0xff;
            int v = (int)((sv * a + dv * (255 - a)) / 255u);
            out |= (uint32_t)(v & 0xff) << (k * 8);
        }
        g_px[(size_t)yy * PC_W + (size_t)xx] = (0xffu << 24) | out;
    }
}

void
DrawString(int x, int y, const char *s)
{
    if (!g_cur_font || !g_cur_font->fdata || !s) return;
    struct cairofont *cf = g_cur_font->fdata;
    SDL_Color fg = {
        (Uint8)(g_cur_color & 0xff),
        (Uint8)((g_cur_color >> 8) & 0xff),
        (Uint8)((g_cur_color >> 16) & 0xff), 0xff };
    SDL_Surface *glyph = TTF_RenderUTF8_Blended(cf->f, s, fg);
    if (!glyph) return;
    uint8_t *sp = (uint8_t *)glyph->pixels;
    for (int j = 0; j < glyph->h; j++) {
        int yy = y + j;
        if (yy < 0 || yy >= PC_H) continue;
        draw_glyph_row(yy, x, glyph->w, sp + (size_t)j * glyph->pitch, fg);
    }
    SDL_FreeSurface(glyph);
}

/* ── bitmaps ────────────────────────────────────────────────────────── */

static ibitmap *
new_bmp(int w, int h, int depth)
{
    ibitmap *b = calloc(1, sizeof(ibitmap) + (size_t)w * h * (size_t)(depth / 8));
    if (!b) return NULL;
    b->width = (unsigned short)w;
    b->height = (unsigned short)h;
    b->depth = (unsigned short)depth;
    b->scanline = (unsigned short)((size_t)w * (size_t)(depth / 8));
    return b;
}

static void
bmp_blit(int x, int y, const ibitmap *b, int dw, int dh)
{
    if (!b) return;
    int sw = b->width, sh = b->height;
    for (int j = 0; j < dh; j++) {
        int sy = sh == 0 ? 0 : (j * sh) / dh;
        int yy = y + j;
        if (yy < 0 || yy >= PC_H) continue;
        for (int i = 0; i < dw; i++) {
            int sx = sw == 0 ? 0 : (i * sw) / dw;
            int xx = x + i;
            if (xx < 0 || xx >= PC_W) continue;
            if (!px_visible(xx, yy)) continue;
            if (b->depth == 24) {
                const uint8_t *p = &b->data[((size_t)sy * b->scanline) + (size_t)sx * 3];
                g_px[(size_t)yy * PC_W + (size_t)xx] = 0xff000000u |
                    ((uint32_t)p[0] << 0) | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16);
            } else {
                uint8_t v = b->data[(size_t)sy * b->scanline + (size_t)sx];
                g_px[(size_t)yy * PC_W + (size_t)xx] = 0xff000000u |
                    ((uint32_t)v << 0) | ((uint32_t)v << 8) | ((uint32_t)v << 16);
            }
        }
    }
}

void
DrawBitmap(int x, int y, const void *b)
{
    const ibitmap *bm = (const ibitmap *)b;
    if (bm) bmp_blit(x, y, bm, bm->width, bm->height);
}

void
StretchBitmap(int x, int y, int w, int h, const void *src, int flags)
{
    (void)flags;
    bmp_blit(x, y, (const ibitmap *)src, w, h);
}

ibitmap *
BitmapStretchCopy(const ibitmap *bmp, int sx, int sy, int sw, int sh,
                  int width, int height)
{
    (void)sx; (void)sy; (void)sw; (void)sh;
    ibitmap *out = new_bmp(width, height, bmp ? bmp->depth : 8);
    if (!out) return NULL;
    for (int j = 0; j < height; j++)
        for (int i = 0; i < width; i++) {
            int oy = bmp && bmp->height ? (j * bmp->height) / height : 0;
            int ox = bmp && bmp->width ? (i * bmp->width) / width : 0;
            if (out->depth == 24)
                memcpy(&out->data[((size_t)j * out->scanline) + (size_t)i * 3],
                       &bmp->data[((size_t)oy * bmp->scanline) + (size_t)ox * 3], 3);
            else
                out->data[(size_t)j * out->scanline + (size_t)i] =
                    bmp->data[(size_t)oy * bmp->scanline + (size_t)ox];
        }
    return out;
}

ibitmap *
LoadPNG(const char *path, int flags)
{
    (void)flags;
    return LoadPNGToFormat(path, kFmtGrayscale8);
}

/* Decode image -> SDL_Surface (SDL2_image), convert to our ibitmap. */
static ibitmap *
surface_to_bmp(SDL_Surface *s, PixelFormat fmt)
{
    if (!s) return NULL;
    /* Normalize to a known layout: 8-bit gray or 24-bit RGB. */
    SDL_Surface *conv;
    if (fmt == kFmtGrayscale8)
        conv = SDL_ConvertSurfaceFormat(s, SDL_PIXELFORMAT_INDEX8, 0);
    else
        conv = SDL_ConvertSurfaceFormat(s, SDL_PIXELFORMAT_RGB24, 0);
    SDL_FreeSurface(s);
    s = conv;
    if (!s) return NULL;
    int depth = fmt == kFmtGrayscale8 ? 8 : 24;
    ibitmap *b = new_bmp(s->w, s->h, depth);
    if (!b) { SDL_FreeSurface(s); return NULL; }
    /* Copy rows; convert to gray if the source has a palette. */
    if (fmt == kFmtGrayscale8 && s->format->palette) {
        for (int j = 0; j < s->h; j++) {
            const uint8_t *row = (const uint8_t *)s->pixels + (size_t)j * s->pitch;
            for (int i = 0; i < s->w; i++) {
                SDL_Color p = s->format->palette->colors[row[i]];
                b->data[(size_t)j * b->scanline + (size_t)i] =
                    (uint8_t)((p.r * 77u + p.g * 150u + p.b * 29u) >> 8);
            }
        }
    } else {
        for (int j = 0; j < s->h; j++) {
            const uint8_t *row = (const uint8_t *)s->pixels + (size_t)j * s->pitch;
            for (int i = 0; i < s->w; i++) {
                if (depth == 24) {
                    b->data[(size_t)j * b->scanline + (size_t)i * 3 + 0] = row[i * 3 + 0];
                    b->data[(size_t)j * b->scanline + (size_t)i * 3 + 1] = row[i * 3 + 1];
                    b->data[(size_t)j * b->scanline + (size_t)i * 3 + 2] = row[i * 3 + 2];
                } else {
                    b->data[(size_t)j * b->scanline + (size_t)i] = row[i];
                }
            }
        }
    }
    SDL_FreeSurface(s);
    return b;
}

ibitmap *
LoadPNGToFormat(const char *path, PixelFormat format)
{
    return surface_to_bmp(IMG_Load(path), format);
}

ibitmap *
LoadJPEGToFormat(const char *path, PixelFormat format)
{
    return surface_to_bmp(IMG_Load(path), format);
}

ibitmap *
LoadPNGStretch(const char *path, int width, int height, int proportional, int dither)
{
    (void)proportional; (void)dither;
    ibitmap *b = surface_to_bmp(IMG_Load(path), kFmtRGB24);
    if (!b) return NULL;
    ibitmap *r = BitmapStretchCopy(b, 0, 0, b->width, b->height, width, height);
    if (r) free(b);
    return r ? r : b;
}

ibitmap *
GetResource(const char *name, const ibitmap *deflt)
{
    (void)name;
    return (ibitmap *)deflt; /* app's hourglass placeholder path */
}
/* ── canvas (RGB24 colour path) ─────────────────────────────────────── */
static uint32_t *g_canvas_lock;
/* Dedicated RGB24 framebuffer exposed via GetCanvas() so the app's
 * colour-cover path (blit_cover_color24, gated on cv->depth==24) writes
 * RGB byte-triples exactly as it does on the Kaleido device.  Merged
 * into g_px on FullUpdate. */
static uint8_t *g_canvas24;

icanvas *
GetCanvas(void)
{
    static icanvas cv;
    if (g_canvas24 == NULL)
        return NULL;
    cv.width = PC_W;
    cv.height = PC_H;
    cv.scanline = PC_W * 3;   /* 24bpp: 3 bytes per row pixel */
    cv.depth = 24;
    cv.addr = g_canvas24;
    return &cv;
}

void
lockCanvasDrawing(void)
{
    g_canvas_lock = g_px;
}

void
unlockCanvasDrawing(void)
{
    g_canvas_lock = NULL;
}

/* ── screen / device ────────────────────────────────────────────────── */
int
ScreenWidth(void)
{ return PC_W; }

int
ScreenHeight(void)
{ return PC_H; }

int
PanelHeight(void)
{ return 0; }

int
GetBatteryPower(void)
{ return 100; }

int
QueryNetwork(void)
{
    /* Test hook: BS_OFFLINE=1 reports "no active connection" so the app
     * behaves like a real device with WiFi off — it skips the boot
     * auto-sync and skips remote cover fetches (see bs_main.c
     * init_sync_tick and bs_grid.c cover_tick).  This is how the SDL
     * e2e suite simulates no internet (the emulator gets the same from
     * the firmware's real QueryNetwork). */
    if (getenv("BS_OFFLINE") != NULL)
        return 0;
    return 0x0f00; /* net_state ACTIVE; see QuickDownload's connection bits */
}

void
BanSleep(int sec)
{ (void)sec; }

char *
GetSoftwareVersion(void)
{ return "PC.1.0.0"; }

/* ── timers (SDL) ───────────────────────────────────────────────────── */
struct pc_timer {
    const char      *name;
    iv_timerprocEx   cb;
    void            *ctx;
    Uint32           fire_at;
    int              period;
    struct pc_timer *next;
};
static struct pc_timer *g_timers;

void
SetWeakTimerEx(const char *name, iv_timerprocEx tp, void *context, int ms)
{
    struct pc_timer *t;
    for (t = g_timers; t; t = t->next)
        if (strcmp(t->name, name) == 0) break;
    if (!t) {
        t = calloc(1, sizeof *t);
        if (!t) return;
        t->name = strdup(name);
        t->next = g_timers;
        g_timers = t;
    }
    t->cb = tp;
    t->ctx = context;
    t->fire_at = SDL_GetTicks() + (Uint32)ms;
}

void
ClearTimerByName(const char *name)
{
    struct pc_timer **pp = &g_timers;
    while (*pp) {
        if (strcmp((*pp)->name, name) == 0) {
            struct pc_timer *gone = *pp;
            *pp = gone->next;
            free((void *)gone->name);
            free(gone);
            return;
        }
        pp = &(*pp)->next;
    }
}

static void
run_timers(void)
{
    /* PocketBook weak timers are ONE-SHOT: they fire once, then are gone.
     * A callback that wants repetition calls SetWeakTimerEx() again (e.g.
     * bootslice_tick / suggest_debounce_tick re-arm themselves while their
     * work continues).  Firing each armed timer exactly once and clearing
     * it preserves that contract — auto-repeating every "weak" timer here
     * would make one-shot timers (initsync, bootslice tail) fire forever,
     * spamming the log and driving endless sync/cover work. */
    Uint32 now = SDL_GetTicks();
    for (;;) {
        struct pc_timer *hit = NULL;
        for (struct pc_timer *t = g_timers; t; t = t->next) {
            if ((int)(now - t->fire_at) >= 0) { hit = t; break; }
        }
        if (!hit) break;
        /* Fire it once and clear; the callback re-arms if it wants more. */
        iv_timerprocEx cb = hit->cb;
        void *ctx = hit->ctx;
        ClearTimerByName(hit->name);
        if (cb) cb(ctx);
    }
}

/* ── keyboard (SDL text input) ──────────────────────────────────────── */
struct pc_keyboard {
    char             *buf;
    int               cap;
    iv_keyboardhandler handler;
    int               open;
};
static struct pc_keyboard g_kb;

void
OpenKeyboard(const char *title, char *buffer, int maxlen, int flags,
             iv_keyboardhandler hproc)
{
    (void)title; (void)flags;
    g_kb.buf = buffer;
    g_kb.cap = maxlen;
    g_kb.handler = hproc;
    g_kb.open = 1;
    if (buffer) buffer[0] = '\0';
    SDL_StartTextInput();
    g_text_input = 1;
}

void
CloseKeyboard(void)
{
    g_kb.open = 0;
    SDL_StopTextInput();
    g_text_input = 0;
}

void
GetKeyboardRect(irect *rect)
{
    if (rect) { rect->x = 0; rect->y = PC_H * 2 / 3; rect->w = PC_W; rect->h = PC_H / 3; }
}

/* Feed text into the OpenKeyboard buffer live (IPC "type" command),
 * mirroring what SDL_TEXTINPUT does: append to the buffer, then a
 * repaint so the app's suggest/commit logic sees the change. */
#ifdef BS_ENABLE_TEST_IPC
void
AppendIpcText(const char *s)
{
    if (!g_kb.open || !g_kb.buf || !s) return;
    size_t cur = strlen(g_kb.buf);
    size_t n = strlen(s);
    if (cur + n < (size_t)g_kb.cap) {
        memcpy(g_kb.buf + cur, s, n + 1);
        if (g_on_event) g_on_event(EVT_REPAINT, 0, 0);
    }
}
#endif

/* ── task launch / control panel (no-ops on PC) ─────────────────────── */
int
NewTaskEx(const char *path, char *const args[], const char *appname,
          const char *name, const ibitmap *icon, unsigned int flags,
          int run_as_reader_if_needed)
{
    (void)args; (void)appname; (void)name; (void)icon; (void)flags;
    (void)run_as_reader_if_needed;
    fprintf(stderr, "[pc] NewTaskEx('%s') — not launching on PC\n", path ? path : "?");
    return 0;
}

int
OpenBook(const char *path, const char *parameters, int flags)
{
    (void)path; (void)parameters; (void)flags;
    return 0;
}

int
DrawPanel(const void *icon, const char *text, const char *title, int percent)
{
    (void)icon; (void)text; (void)title; (void)percent;
    return 0;
}

int
OpenControlPanel(struct control_panel *ctx)
{
    (void)ctx;
    return 0;
}

/* ── misc ───────────────────────────────────────────────────────────── */
void
HideHourglass(void)
{}

long
iv_ipc_cmd(long type, long param)
{
    (void)type; (void)param;
    return 0;
}

void
iv_update_panel(int readingModeEnable)
{
    (void)readingModeEnable;
}

int
iv_stat(const char *name, struct stat *st)
{
    return stat(name, st);
}

void
Repaint(void)
{ FullUpdate(); }

static void
dump_frame(const char *path)
{
    if (path == NULL || g_px == NULL) return;
    FILE *f = fopen(path, "wb");
    if (!f) { fprintf(stderr, "[pc] dump_frame: cannot open %s\n", path); return; }
    fprintf(f, "P6\n%d %d\n255\n", PC_W, PC_H);
    for (int j = 0; j < PC_H; j++)
        for (int i = 0; i < PC_W; i++) {
            uint32_t p = g_px[(size_t)j * PC_W + (size_t)i];
            fputc((int)((p >> 0) & 0xff), f);
            fputc((int)((p >> 8) & 0xff), f);
            fputc((int)((p >> 16) & 0xff), f);
        }
    fclose(f);
    fprintf(stderr, "[pc] frame dumped to %s\n", path);
}

/* Composite the RGB24 colour-cover overlay (GetCanvas /
 * blit_cover_color24) into the display buffer g_px and clear it.
 * The canvas defaults to white; only pixels actually written by a
 * cover blit are non-white, so this overlays covers without touching
 * the 8-bit/grey drawn content.
 *
 * The clear matters: without it, covers accumulate in the overlay
 * across frames and get stamped back on top of every redraw —
 * navigation (page flip, menu, launcher) redraws the shelf in g_px
 * but the stale cover pixels re-merge over it, so the window looks
 * frozen even though the app is reacting.
 *
 * IMPORTANT: compositing EARLY (before a modal is drawn over the
 * grid) is what keeps a popup on top of the covers.  bs_redraw_shelf
 * draws the grid covers into the overlay and THEN the sync/download
 * popup into g_px, then flushes once — if the covers were only merged
 * at that final flush, they would stamp back over the popup's sheet.
 * bs_plat_cover_flush() (called by the app between the body and the
 * popups) forces the merge at the right point. */
static void
sdl_merge_covers(void)
{
    if (g_canvas24 == NULL)
        return;
    for (size_t j = 0; j < PC_H; j++) {
        for (size_t i = 0; i < PC_W; i++) {
            const uint8_t *c = g_canvas24 + (j * PC_W + i) * 3;
            if (c[0] == 0xff && c[1] == 0xff && c[2] == 0xff)
                continue; /* untouched: leave the 8-bit pixel */
            g_px[j * PC_W + i] = 0xff000000u |
                ((uint32_t)c[0] << 0) | ((uint32_t)c[1] << 8) |
                ((uint32_t)c[2] << 16);
        }
    }
    memset(g_canvas24, 0xff, (size_t)PC_W * PC_H * 3);
}

/* Platform seam: composite + clear the cover overlay now (see
 * sdl_merge_covers).  The app calls this after drawing the shelf body
 * and before any modal/popup that may overlap the grid, so the popup
 * is drawn above the covers instead of being re-covered by them at the
 * next flush. */
void
bs_plat_cover_flush(void)
{
    sdl_merge_covers();
}

void
FullUpdate(void)
{
    if (!g_ren || !g_tex || !g_px) return;
    sdl_merge_covers();
    /* Debug: BS_DUMP_FRAME=path writes one PPM of the canvas (for
     * headless visual verification — e.g. in CI / docs).  The very
     * first FullUpdate fires before covers load; use BS_DUMP_AFTER_MS
     * to dump a settled frame instead (see the main loop). */
    const char *dump = getenv("BS_DUMP_FRAME");
    if (dump && g_dump_pending == 0) {
        snprintf(g_dump_path, sizeof g_dump_path, "%s", dump);
        g_dump_pending = 1;
        const char *after = getenv("BS_DUMP_AFTER_MS");
        g_dump_at = SDL_GetTicks() +
            (after ? (Uint32)strtoul(after, NULL, 10) : 0u);
    }
    SDL_UpdateTexture(g_tex, NULL, g_px, PC_W * 4);
    SDL_RenderClear(g_ren);
    SDL_RenderCopy(g_ren, g_tex, NULL, NULL);
    SDL_RenderPresent(g_ren);
}

void
PartialUpdate(int x, int y, int w, int h)
{
    (void)x; (void)y; (void)w; (void)h;
    FullUpdate();
}

/* ── HTTP (libcurl) ─────────────────────────────────────────────────── */
struct curl_buf { char *data; size_t len; };

static size_t
curl_write_cb(char *ptr, size_t size, size_t nmemb, void *userdata)
{
    struct curl_buf *b = userdata;
    size_t n = size * nmemb;
    char *np = realloc(b->data, b->len + n + 1);
    if (!np) return 0;
    memcpy(np + b->len, ptr, n);
    b->data = np;
    b->len += n;
    b->data[b->len] = '\0';
    return n;
}

static void *
http_fetch(const char *url, const char *post, int timeout, int *retsize, int *error_code)
{
    struct curl_buf b = {0, 0};
    CURL *c = curl_easy_init();
    if (!c) return NULL;
    curl_easy_setopt(c, CURLOPT_URL, url);
    curl_easy_setopt(c, CURLOPT_TIMEOUT, (long)timeout);
    curl_easy_setopt(c, CURLOPT_WRITEFUNCTION, curl_write_cb);
    curl_easy_setopt(c, CURLOPT_WRITEDATA, &b);
    curl_easy_setopt(c, CURLOPT_FOLLOWLOCATION, 1L);
    if (post) {
        curl_easy_setopt(c, CURLOPT_POST, 1L);
        curl_easy_setopt(c, CURLOPT_POSTFIELDS, post);
    }
    CURLcode rc = curl_easy_perform(c);
    long http_code = 0;
    curl_easy_getinfo(c, CURLINFO_RESPONSE_CODE, &http_code);
    curl_easy_cleanup(c);
    if (error_code) *error_code = (rc == CURLE_OK) ? (int)http_code : -1;
    if (retsize) *retsize = (int)b.len;
    return b.data;
}

void *
QuickDownload(const char *url, int *retsize, int timeout)
{ return http_fetch(url, NULL, timeout, retsize, NULL); }

void *
QuickDownloadExt(const char *url, int *retsize, int timeout, char *cookie, char *post)
{ (void)cookie; return http_fetch(url, post, timeout, retsize, NULL); }

void *
QuickDownloadExt3(const char *url, int *retsize, int timeout, char *cookie,
                  char *post, int *error_code)
{ (void)cookie; return http_fetch(url, post, timeout, retsize, error_code); }

/* ── hwconfig surface (host view) ───────────────────────────────────── */
unsigned int
device_number(void)
{ return 0; }

bool
device_has_audio(void)
{ return true; }

bool
device_has_touchpanel(void)
{ return true; }

int
device_display_colormask(void)
{ return 1; } /* PC shows colour */

/* ── SDL event loop ─────────────────────────────────────────────────── */

static int map_scancode_to_ivkey(SDL_Scancode sc)
{
    switch (sc) {
    case SDL_SCANCODE_MENU:       return IV_KEY_MENU;
    case SDL_SCANCODE_HOME:       return IV_KEY_HOME;
    case SDL_SCANCODE_BACKSPACE:
    case SDL_SCANCODE_ESCAPE:     return IV_KEY_BACK;
    case SDL_SCANCODE_PAGEUP:     return IV_KEY_PREV;
    case SDL_SCANCODE_PAGEDOWN:   return IV_KEY_NEXT;
    case SDL_SCANCODE_UP:         return IV_KEY_PREV2;
    case SDL_SCANCODE_DOWN:       return IV_KEY_NEXT2;
    default: return -1;
    }
}

static void
sdl_on_quit(void)
{
    g_running = 0;
    if (g_on_event) g_on_event(EVT_EXIT, 0, 0);
}

static void
sdl_on_mouse(int type, int x, int y)
{
    if (g_on_event) g_on_event(type, x, y);
}

static void
sdl_on_text_input(const char *text)
{
    if (g_kb.open && g_kb.buf) {
        size_t cur = strlen(g_kb.buf);
        size_t n = strlen(text);
        if (cur + n < (size_t)g_kb.cap) {
            memcpy(g_kb.buf + cur, text, n + 1);
            if (g_on_event) g_on_event(EVT_REPAINT, 0, 0);
        }
    }
}

static void
sdl_on_key_down(const SDL_KeyboardEvent *k)
{
    int iv = map_scancode_to_ivkey(k->keysym.scancode);
    if (k->keysym.scancode == SDL_SCANCODE_RETURN &&
        g_kb.open && g_kb.handler) {
        CloseKeyboard();
        g_kb.handler(g_kb.buf);
    } else if (iv >= 0 && g_on_event) {
        g_on_event(EVT_KEYPRESS, iv, k->repeat ? 0 : 0);
    }
}

static void
sdl_on_window_event(const SDL_WindowEvent *we)
{
    if (we->event == SDL_WINDOWEVENT_RESIZED)
        FullUpdate();
}

static void
poll_sdl(void)
{
    SDL_Event e;
    while (SDL_PollEvent(&e)) {
        switch (e.type) {
        case SDL_QUIT:
            sdl_on_quit();
            break;
        case SDL_MOUSEBUTTONDOWN:
            sdl_on_mouse(EVT_POINTERDOWN, e.button.x, e.button.y);
            break;
        case SDL_MOUSEBUTTONUP:
            sdl_on_mouse(EVT_POINTERUP, e.button.x, e.button.y);
            break;
        case SDL_MOUSEMOTION:
            sdl_on_mouse(EVT_POINTERMOVE, e.motion.x, e.motion.y);
            break;
        case SDL_TEXTINPUT:
            sdl_on_text_input(e.text.text);
            break;
        case SDL_KEYDOWN:
            sdl_on_key_down(&e.key);
            break;
        case SDL_WINDOWEVENT:
            sdl_on_window_event(&e.window);
            break;
        default:
            break;
        }
    }
}

/* ── backend API ────────────────────────────────────────────────────── */

void
bs_plat_start_services(void)
{} /* no monitor on a PC */

static void
sdl_init_subsystems(void)
{
    if (SDL_Init(SDL_INIT_VIDEO | SDL_INIT_TIMER) != 0) {
        fprintf(stderr, "[pc] SDL_Init: %s\n", SDL_GetError());
        exit(1);
    }
    if (TTF_Init() != 0) {
        fprintf(stderr, "[pc] TTF_Init: %s\n", TTF_GetError());
        exit(1);
    }
    if (curl_global_init(CURL_GLOBAL_DEFAULT) != 0) {
        fprintf(stderr, "[pc] curl_global_init failed\n");
        exit(1);
    }
}

static void
sdl_create_window_state(void)
{
    /* Window scale: the logical canvas is 1072x1448; default to ~60%
     * of it so the window fits a 1080p desktop, and let BS_SCALE
     * override (e.g. BS_SCALE=1.2 for a larger window).  The render is
     * set to logical size so SDL scales the framebuffer to whatever the
     * window is; pointer events are mapped back to logical coords. */
    double scale = 0.6;
    const char *scale_env = getenv("BS_SCALE");
    if (scale_env != NULL && scale_env[0] != '\0') {
        double s = strtod(scale_env, NULL);
        if (s > 0.1 && s <= 4.0) scale = s;
    }
    g_win = SDL_CreateWindow("EinkHome (PC)", SDL_WINDOWPOS_CENTERED,
                             SDL_WINDOWPOS_CENTERED,
                             (int)(PC_W * scale), (int)(PC_H * scale),
                             SDL_WINDOW_RESIZABLE);
    if (!g_win) {
        fprintf(stderr, "[pc] SDL_CreateWindow: %s\n", SDL_GetError());
        exit(1);
    }
    g_ren = SDL_CreateRenderer(g_win, -1, 0);
    if (!g_ren) {
        fprintf(stderr, "[pc] SDL_CreateRenderer: %s\n", SDL_GetError());
        exit(1);
    }
    /* Logical presentation: the whole logical canvas maps to the window
     * (letterboxed if the aspect differs), and SDL scales automatically. */
    SDL_RenderSetLogicalSize(g_ren, PC_W, PC_H);
    g_tex = SDL_CreateTexture(g_ren, SDL_PIXELFORMAT_RGBA32,
                              SDL_TEXTUREACCESS_STREAMING, PC_W, PC_H);
    if (!g_tex) {
        fprintf(stderr, "[pc] SDL_CreateTexture: %s\n", SDL_GetError());
        exit(1);
    }
    g_px = calloc((size_t)PC_W * PC_H, sizeof(uint32_t));
    if (!g_px) exit(1);
    /* Baseline: white canvas. */
    for (size_t i = 0; i < (size_t)PC_W * PC_H; i++)
        g_px[i] = 0xffffffffu;
    /* RGB24 backing for GetCanvas / blit_cover_color24; defaults white. */
    g_canvas24 = malloc((size_t)PC_W * PC_H * 3);
    if (!g_canvas24) exit(1);
    memset(g_canvas24, 0xff, (size_t)PC_W * PC_H * 3);
}

/* Debug: BS_AUTO_LAUNCHER=1 opens the app launcher right after boot
 * (headless verification of the .desktop-backed launcher listing).
 * bs_launcher_open_set() builds the list (through the platform seam
 * → bs_plat_launcher_build → .desktop parser) and draws it. */
static void
sdl_boot_auto_launcher(void)
{
    bs_launcher_open_set();
    FullUpdate();
    /* Give the build + draw a moment, then dump and exit. */
    for (int i = 0; i < 60; i++) {
        run_timers();
        SDL_Delay(16);
    }
    const char *dump = getenv("BS_DUMP_FRAME");
    if (dump) {
        FILE *f = fopen(dump, "wb");
        if (f) {
            fprintf(f, "P6\n%d %d\n255\n", PC_W, PC_H);
            for (int j = 0; j < PC_H; j++)
                for (int i = 0; i < PC_W; i++) {
                    uint32_t p = g_px[(size_t)j * PC_W + (size_t)i];
                    fputc((int)((p >> 0) & 0xff), f);
                    fputc((int)((p >> 8) & 0xff), f);
                    fputc((int)((p >> 16) & 0xff), f);
                }
            fclose(f);
            fprintf(stderr, "[pc] launcher frame dumped to %s\n", dump);
        }
    }
    g_running = 0;
}

static void
sdl_teardown(void)
{
    SDL_DestroyTexture(g_tex);
    SDL_DestroyRenderer(g_ren);
    SDL_DestroyWindow(g_win);
    free(g_px);
    free(g_canvas24);
    TTF_Quit();
    SDL_Quit();
    curl_global_cleanup();
}

void
bs_plat_boot(int (*on_event)(int, int, int))
{
    g_on_event = on_event;
    sdl_init_subsystems();
    sdl_create_window_state();

    g_running = 1;
#ifdef BS_ENABLE_TEST_IPC
    ipc_init();
#endif
    g_on_event(EVT_INIT, 0, 0);
    g_on_event(EVT_SHOW, 0, 0);

    if (getenv("BS_AUTO_LAUNCHER") != NULL) {
        sdl_boot_auto_launcher();
    }

    while (g_running) {
        Uint32 now_ms = SDL_GetTicks();
        /* Delayed frame dump: dump once BS_DUMP_AFTER_MS have elapsed
         * (headless verification of a *settled* frame, e.g. after covers
         * load — the early FullUpdate dump fires before them). */
        if (g_dump_pending && g_dump_at && (int)(now_ms - g_dump_at) >= 0) {
            g_dump_pending = 0;
            dump_frame(g_dump_path);
        }
        Uint32 deadline = now_ms + 16;
        /* Process one batch, then let a timer fire if due. */
#ifdef BS_ENABLE_TEST_IPC
        ipc_poll_and_process();
#endif
        SDL_PumpEvents();
        poll_sdl();
        run_timers();
        int delay = (int)(deadline - SDL_GetTicks());
        if (delay > 0) SDL_Delay((Uint32)delay);
    }

    sdl_teardown();
}

int
bs_plat_panel_height(int *self_panel)
{
    /* The PC has no firmware panel/status bar: return 0 so the app
     * treats the whole window as content (bs_content_bottom() =
     * ScreenHeight()), and self_panel=0 so no strip is drawn either. */
    *self_panel = 0;
    return 0;
}

void
bs_plat_panel_init(void)
{} /* no firmware panel on a PC */

void
bs_plat_stamp_panel(int self_panel)
{
    (void)self_panel;
}

void
bs_plat_device_profile(BsLcProfile *out, const char *lang)
{
    /* PC is touch + audio + colour; every conditional falls to "all". */
    snprintf(out->device, sizeof out->device, "all");
    snprintf(out->has_audio, sizeof out->has_audio, "%s",
             device_has_audio() ? "true" : "false");
    if (lang != NULL && lang[0] != '\0')
        snprintf(out->language, sizeof out->language, "%.2s", lang);
    bs_LOG("[bookshelf] device_profile device=%s audio=%s lang=%s\n",
           out->device, out->has_audio, out->language);
}

/* PC build has no device config (/mnt/ext1 does not exist); the caller
 * falls back to the LANG environment variable. */
int
bs_plat_device_language(char *out, unsigned cap)
{
    (void)out;
    (void)cap;
    return -1;
}

void
bs_plat_log_identity(void)
{
    bs_LOG("[bookshelf] model=PC fw=%s\n", GetSoftwareVersion());
}

/* ── app launcher: freedesktop .desktop discovery ──────────────────── */
/* The launcher's app list comes from the standard application dirs.  We
 * scan /usr/share/applications and $HOME/.local/share/applications for
 * *.desktop entries and map Name=, Exec=, Icon= onto the platform-neutral
 * BsLauncherItem.  Hidden=true / NoDisplay=true entries are skipped. */

static int
parse_desktop_line(const char *line, const char *key, char *out, size_t cap)
{
    size_t kl = strlen(key);
    if (strncmp(line, key, kl) != 0 || line[kl] != '=')
        return 0;
    snprintf(out, cap, "%s", line + kl + 1);
    size_t n = strlen(out);
    while (n > 0 && (out[n - 1] == '\n' || out[n - 1] == '\r'))
        out[--n] = '\0';
    return 1;
}

/* Scan one argv token out of the Exec= string (quoted or bare),
 * returning the position after it.  Extracted from parse_desktop_exec. */
static const char *
desktop_exec_token(const char *p, char *tok, size_t cap)
{
    if (*p == '"' || *p == '\'') {
        char q = *p++;
        size_t ti = 0;
        while (*p && *p != q && ti + 1 < cap) tok[ti++] = *p++;
        if (*p == q) p++;
        tok[ti] = '\0';
    } else {
        size_t ti = 0;
        while (*p && *p != ' ' && *p != '\t' && ti + 1 < cap)
            tok[ti++] = *p++;
        tok[ti] = '\0';
    }
    return p;
}

static void
parse_desktop_exec(const char *exec, char *out_cmd, size_t cmd_cap,
                   char params[][BS_LAUNCHER_PARAM_LEN], int *nparams)
{
    /* Exec= may contain %-field codes (%U, %f, ...) and quoted args we
     * strip — keep the command and the plain argv tokens. */
    const char *p = exec;
    while (*p == ' ' || *p == '\t') p++;
    size_t ci = 0;
    while (*p && *p != ' ' && *p != '\t' && ci + 1 < cmd_cap)
        out_cmd[ci++] = *p++;
    out_cmd[ci] = '\0';
    *nparams = 0;
    char tok[BS_LAUNCHER_PARAM_LEN];
    while (*p) {
        if (*p == '%') { /* skip %U / %f / %i / %c / %k field codes */
            p += 2;
            continue;
        }
        if (*p == ' ' || *p == '\t') { p++; continue; }
        p = desktop_exec_token(p, tok, sizeof tok);
        if (tok[0] && *nparams < BS_LAUNCHER_MAX_PARAMS) {
            snprintf(params[(*nparams)++], BS_LAUNCHER_PARAM_LEN, "%s", tok);
        }
    }
}

/* Parse one .desktop line into the item fields.  Mirrors the original
 * loop's continue-vs-parse flow: leaves the values untouched if the
 * line is a comment or a key we don't care about. */
static void
desktop_parse_line(char *line, char *name, size_t ncap, char *exec, size_t ecap,
                   char *icon, size_t icap, int *hidden, int *nodisplay,
                   int *type_app)
{
    char *p = line;
    while (*p == ' ') p++;
    if (p[0] == '#') return;
    if (parse_desktop_line(p, "Name", name, ncap)) return;
    if (parse_desktop_line(p, "Exec", exec, ecap)) return;
    if (parse_desktop_line(p, "Icon", icon, icap)) return;
    char v[16];
    if (parse_desktop_line(p, "Hidden", v, sizeof v) && strcmp(v, "true") == 0)
        *hidden = 1;
    if (parse_desktop_line(p, "NoDisplay", v, sizeof v) && strcmp(v, "true") == 0)
        *nodisplay = 1;
    if (parse_desktop_line(p, "Type", v, sizeof v) && strcmp(v, "Application") != 0)
        *type_app = 0;
}

static void
desktop_file_to_item(const char *path, BsLauncherItem *it, int *n)
{
    FILE *f = fopen(path, "r");
    if (!f) return;
    char line[512];
    char name[96] = "", exec[BS_MAX_PATH_LEN] = "", icon[64] = "";
    int hidden = 0, nodisplay = 0, type_app = 1;
    while (fgets(line, sizeof line, f)) {
        desktop_parse_line(line, name, sizeof name, exec, sizeof exec,
                           icon, sizeof icon, &hidden, &nodisplay, &type_app);
    }
    fclose(f);
    if (hidden || nodisplay || !type_app || !exec[0] || !name[0])
        return;
    memset(it, 0, sizeof *it);
    it->kind = 1;
    snprintf(it->text, sizeof it->text, "%s", name);
    char cmd[BS_MAX_PATH_LEN];
    parse_desktop_exec(exec, cmd, sizeof cmd, it->params, &it->nparams);
    if (cmd[0]) {
        snprintf(it->path, sizeof it->path, "%s", cmd);
    } else {
        snprintf(it->path, sizeof it->path, "%s", exec);
    }
    snprintf(it->icon, sizeof it->icon, "%s", icon);
    (*n)++;
}

/* Scans one applications dir for *.desktop files.  Returns items added. */
static int
scan_desktop_dir(const char *dir, BsLauncherItem *items, int cap)
{
    DIR *d = opendir(dir);
    if (!d) return 0;
    int n = 0;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (n >= cap) break;
        size_t len = strlen(e->d_name);
        if (len <= 8)
            continue;
        if (strcmp(e->d_name + len - 8, ".desktop") != 0)
            continue;
        char path[512];
        snprintf(path, sizeof path, "%s/%s", dir, e->d_name);
        /* Skip symlinks/dirs casually (only regular files). */
        struct stat st;
        if (stat(path, &st) != 0 || !S_ISREG(st.st_mode))
            continue;
        desktop_file_to_item(path, &items[n], &n);
    }
    closedir(d);
    return n;
}

int
bs_plat_launcher_build(BsLauncherItem *items, int cap)
{
    int n = 0;
    n += scan_desktop_dir("/usr/share/applications", items + n, cap - n);
    const char *home = getenv("HOME");
    if (home && n < cap) {
        char p[512];
        snprintf(p, sizeof p, "%s/.local/share/applications", home);
        n += scan_desktop_dir(p, items + n, cap - n);
    }
    return n;
}

int
bs_plat_launch_app(const BsLauncherItem *it, char **argv, int argc)
{
    (void)it; (void)argc;
    if (!argv || !argv[0]) return -1;
    /* Copy the arg list (execvp needs a mutable NULL-terminated array). */
    char **av = argv;
    int ai = 0;
    while (ai < argc && av[ai]) ai++;
    pid_t pid = fork();
    if (pid < 0) return -1;
    if (pid == 0) {
        execvp(av[0], av);
        _exit(127);
    }
    return 0;
}

/* ── IPC control socket (headless test driving; test builds only) ──── */
/* A UNIX-socket control plane so the e2e tests can drive the running
 * app without the emulator: send pointer + key events, query the frame
 * hash, dump the frame to PPM, etc.  Commands are newline-delimited
 * text ("cmd arg1 arg2\n"), answers are one text line terminated by \n.
 *
 * Compiled in ONLY when BS_ENABLE_TEST_IPC is defined (the test build —
 * see sdk/build_pc.sh).  The plain `make pc` desktop build and the
 * PocketBook builds never carry a control socket.
 *
 *   tap x y             POINTERDOWN then POINTERUP at logical (x,y)
 *   down x y / up x y / move x y
 *   key <0xIVKEY|sym>   EVT_KEYPRESS (par1=key code, par2=0)
 *   type TEXT           feed text to the OpenKeyboard buffer
 *   kb_commit           close the OpenKeyboard and fire its handler
 *                       (equivalent to a real RETURN press)
 *   hash                FNV1a-64 of the RGBA canvas -> "hash=0x%016llx"
 *   shot PATH           write the canvas to PATH as P6 PPM
 *   state               "state=<overlay>:<tab>:<page>"
 *   quit                exit the app cleanly
 *
 * Socket path: $BS_SOCKET, else /tmp/bookshelf-<pid>.sock.  A blank or
 * "off" BS_SOCKET disables the control plane.  Because the socket is
 * per-process, parallel test runs (each with its own pid / BS_SOCKET)
 * never collide. */

#ifdef BS_ENABLE_TEST_IPC /* control socket: test builds only */
#include <sys/socket.h>
#include <sys/un.h>
#include <poll.h>

static int g_ipc_listen = -1;
static int g_ipc_client = -1;

static uint64_t
fnv1a_64(const uint8_t *data, size_t len) /* same constants as pbemu */
{
    uint64_t h = 14695981039346656037ULL;
    for (size_t i = 0; i < len; i++) {
        h ^= data[i];
        h *= 1099511628211ULL;
    }
    return h;
}

static void
ipc_reply(const char *fmt, const char *arg)
{
    if (g_ipc_client < 0) return;
    char buf[256];
    // cppcheck-suppress wrongPrintfScanfArgNum -- fmt carries exactly one %s for the single arg.
    snprintf(buf, sizeof buf, fmt, arg ? arg : "");
    (void)write(g_ipc_client, buf, strlen(buf));
}

static int
ipc_is_pointer_cmd(const char *cmd)
{
    return strcmp(cmd, "tap") == 0 || strcmp(cmd, "down") == 0 ||
           strcmp(cmd, "up") == 0 || strcmp(cmd, "move") == 0;
}

static int
ipc_is_text_cmd(const char *cmd)
{
    return strcmp(cmd, "type") == 0 || strcmp(cmd, "kb_commit") == 0;
}

static int
ipc_is_query_cmd(const char *cmd)
{
    return strcmp(cmd, "key") == 0 || strcmp(cmd, "shot") == 0 ||
           strcmp(cmd, "hash") == 0 || strcmp(cmd, "state") == 0;
}

static void
ipc_pointer_group(const char *cmd, int n, int ax, int ay)
{
    if (strcmp(cmd, "tap") == 0 && n >= 3) {
        if (g_on_event) { g_on_event(EVT_POINTERDOWN, ax, ay); g_on_event(EVT_POINTERUP, ax, ay); }
        ipc_reply("ok\n", NULL);
    } else if (strcmp(cmd, "down") == 0 && n >= 3) {
        if (g_on_event) g_on_event(EVT_POINTERDOWN, ax, ay);
        ipc_reply("ok\n", NULL);
    } else if (strcmp(cmd, "up") == 0 && n >= 3) {
        if (g_on_event) g_on_event(EVT_POINTERUP, ax, ay);
        ipc_reply("ok\n", NULL);
    } else if (strcmp(cmd, "move") == 0 && n >= 3) {
        if (g_on_event) g_on_event(EVT_POINTERMOVE, ax, ay);
        ipc_reply("ok\n", NULL);
    } else {
        ipc_reply("err unknown cmd\n", NULL);
    }
}

static void
ipc_text_group(const char *cmd, int n, const char *a)
{
    if (strcmp(cmd, "type") == 0 && n >= 2) {
        AppendIpcText(a);
        ipc_reply("ok\n", NULL);
    } else if (strcmp(cmd, "kb_commit") == 0) {
        /* Commit the open keyboard exactly like a real RETURN press
         * (see the SDL_SCANCODE_RETURN handler): close it and fire the
         * app's handler with the buffer.  Test builds need this because
         * IPC "key" synthesises EVT_KEYPRESS, which bypasses the SDL
         * scancode path that would otherwise call the handler. */
        if (g_kb.open && g_kb.handler) {
            CloseKeyboard();
            g_kb.handler(g_kb.buf);
        }
        ipc_reply("ok\n", NULL);
    } else {
        ipc_reply("err unknown cmd\n", NULL);
    }
}

static void
ipc_query_group(const char *cmd, int n, const char *a)
{
    if (strcmp(cmd, "key") == 0 && n >= 2) {
        int code = (strncmp(a, "0x", 2) == 0) ? (int)strtol(a, NULL, 16)
                                               : atoi(a);
        if (g_on_event) g_on_event(EVT_KEYPRESS, code, 0);
        ipc_reply("ok\n", NULL);
    } else if (strcmp(cmd, "shot") == 0 && n >= 2) {
        dump_frame(a);
        ipc_reply("ok\n", NULL);
    } else if (strcmp(cmd, "hash") == 0) {
        char h[32];
        uint64_t v = fnv1a_64((const uint8_t *)g_px, (size_t)PC_W * PC_H * 4u);
        snprintf(h, sizeof h, "hash=0x%016llx\n", (unsigned long long)v);
        ipc_reply("%s", h);
    } else if (strcmp(cmd, "state") == 0) {
        char s[64];
        snprintf(s, sizeof s, "state=%d:%d:%d\n", bs_g_state.overlay,
                 bs_g_state.tab, bs_g_state.page);
        ipc_reply("%s", s);
    } else {
        ipc_reply("err unknown cmd\n", NULL);
    }
}

static void
ipc_handle(const char *line)
{
    char cmd[16];
    char a[128], b[128];
    int  n = sscanf(line, "%15s %127s %127s", cmd, a, b);
    if (n < 1) return;
    int ax = atoi(a), ay = atoi(b);
    if (ipc_is_pointer_cmd(cmd)) {
        ipc_pointer_group(cmd, n, ax, ay);
        return;
    }
    if (ipc_is_text_cmd(cmd)) {
        ipc_text_group(cmd, n, a);
        return;
    }
    if (ipc_is_query_cmd(cmd)) {
        ipc_query_group(cmd, n, a);
        return;
    }
    if (strcmp(cmd, "quit") == 0) {
        g_running = 0;
        ipc_reply("ok\n", NULL);
        return;
    }
    ipc_reply("err unknown cmd\n", NULL);
}

static void
ipc_poll_and_process(void)
{
    if (g_ipc_listen < 0) return;
    struct pollfd fds[2];
    int nfds = 0;
    fds[nfds].fd = g_ipc_listen; fds[nfds].events = POLLIN; nfds++;
    if (g_ipc_client >= 0) { fds[nfds].fd = g_ipc_client; fds[nfds].events = POLLIN; nfds++; }
    if (poll(fds, (nfds_t)nfds, 0) <= 0) return;
    for (int i = 0; i < nfds; i++) {
        if (!(fds[i].revents & POLLIN)) continue;
        if (fds[i].fd == g_ipc_listen) {
            int c = accept(g_ipc_listen, NULL, NULL);
            if (c >= 0 && g_ipc_client < 0) { g_ipc_client = c; }
            else if (c >= 0) { (void)write(c, "busy\n", 5); close(c); }
        } else {
            char buf[2048];
            ssize_t m = read(g_ipc_client, buf, sizeof buf - 1);
            if (m <= 0) { close(g_ipc_client); g_ipc_client = -1; }
            else {
                buf[m] = '\0';
                char *save = NULL;
                for (char *p = strtok_r(buf, "\n", &save); p;
                     p = strtok_r(NULL, "\n", &save)) {
                    ipc_handle(p);
                }
            }
        }
    }
}

static void
ipc_init(void)
{
    const char *env = getenv("BS_SOCKET");
    if (env != NULL && (env[0] == '\0' || strcmp(env, "off") == 0))
        return; /* control plane disabled */
    struct sockaddr_un sa;
    memset(&sa, 0, sizeof sa);
    sa.sun_family = AF_UNIX;
    char path[sizeof sa.sun_path];
    if (env != NULL)
        snprintf(path, sizeof path, "%s", env);
    else
        snprintf(path, sizeof path, "/tmp/bookshelf-%d.sock", (int)getpid());
    snprintf(sa.sun_path, sizeof sa.sun_path, "%s", path);
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) return;
    unlink(path);
    if (bind(fd, (struct sockaddr *)&sa, sizeof sa) != 0 ||
        listen(fd, 8) != 0) {
        close(fd);
        return;
    }
    g_ipc_listen = fd;
}

#endif /* BS_ENABLE_TEST_IPC */