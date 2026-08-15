#ifndef IV_H_
#define IV_H_
/*
 * inkview.h — PB firmware inkview API, host (SDL) reimplementation.
 *
 * This mirrors the subset of the pocketbook-sdk-b288 inkview.h that the
 * app actually compiles against + every symbol it links (see the app's
 * dynamic symbol table).  Struct layouts (ibitmap/ifont/irect/icanvas),
 * event codes, key codes and colour constants are byte-identical to the
 * firmware SDK so the app behaves the same on a PC.  The functions are
 * implemented over SDL2 in bs_plat_sdl.c.
 *
 * Only what the app uses is declared here; the firmware SDK's full surface
 * is NOT copied.  When a new app code path needs another inkview symbol,
 * add it here AND implement it in bs_plat_sdl.c.
 */

#include <stddef.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <dirent.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── device / screen queries ────────────────────────────────────────── */
void     InitInkview(int reg_flags);
int      ScreenWidth(void);
int      ScreenHeight(void);
int      PanelHeight(void);
char    *GetDeviceModel(void);
char    *GetSoftwareVersion(void);
int      GetBatteryPower(void);
void     BanSleep(int sec);
int      QueryNetwork(void);

/* ── orientation / app capability (no-op on PC) ─────────────────────── */
void     SetOrientation(int n);
void     SetDefaultOrientation(int n);
void     SetPanelType(int n);
void     IvSetAppCapability(int caps);

/* ── drawing primitives ─────────────────────────────────────────────── */
void     SetClip(int x, int y, int w, int h);
void     DrawLine(int x1, int y1, int x2, int y2, int color);
void     DrawRect(int x, int y, int w, int h, int color);
void     FillArea(int x, int y, int w, int h, int color);
void     DrawString(int x, int y, const char *s);
int      StringWidth(const char *s);
void     FullUpdate(void);
void     PartialUpdate(int x, int y, int w, int h);
void     Repaint(void);
void     DrawBitmap(int x, int y, const void *b);
void     StretchBitmap(int x, int y, int w, int h, const void *src, int flags);

/* ── fonts ──────────────────────────────────────────────────────────── */
typedef enum {
    FONT_STD = 0,
    FONT_BOLD = 1,
    FONT_ITALIC = 2,
    FONT_BOLDITALIC = 3,
} FONT_TYPE;

typedef struct ifont_s {
    char          *name;
    char          *family;
    int            size;
    unsigned char  aa;
    unsigned char  isbold;
    unsigned char  isitalic;
    unsigned char  _r1;
    unsigned short charset;
    unsigned short _r2;
    int            color;
    int            height;
    int            linespacing;
    int            baseline;
    void          *fdata;
} ifont;

#define DEFAULTFONT  iv_get_default_font(FONT_STD)
#define DEFAULTFONTB iv_get_default_font(FONT_BOLD)
#define DEFAULTFONTI iv_get_default_font(FONT_ITALIC)

char  *iv_get_default_font(FONT_TYPE fonttype);
ifont *OpenFont(const char *name, int size, int aa);
void   CloseFont(ifont *f);
void   SetFont(const ifont *font, int color);

/* ── bitmaps & images ───────────────────────────────────────────────── */
typedef struct ibitmap_s {
    unsigned short width;
    unsigned short height;
    unsigned short depth;
    unsigned short scanline;
    unsigned char  data[];
} ibitmap;

typedef struct irect_s {
    int x, y, w, h, flags;
} irect;

typedef enum PixelFormat_e {
    kFmtGrayscale8,
    kFmtRGB24,
} PixelFormat;

ibitmap *LoadPNG(const char *path, int flags);
ibitmap *LoadPNGStretch(const char *path, int width, int height, int proportional, int dither);
ibitmap *LoadPNGToFormat(const char *path, PixelFormat format);
ibitmap *LoadJPEGToFormat(const char *path, PixelFormat format);
ibitmap *BitmapStretchCopy(const ibitmap *bmp, int sx, int sy, int sw, int sh,
                           int width, int height);
ibitmap *GetResource(const char *name, const ibitmap *deflt);

/* ── canvas (RGB24 colour path, Kaleido) ────────────────────────────── */
typedef struct icanvas_s {
    int width;
    int height;
    int scanline;
    int depth;
    int clipx1, clipx2;
    int clipy1, clipy2;
    unsigned char *addr;
} icanvas;

icanvas *GetCanvas(void);
void     lockCanvasDrawing(void);
void     unlockCanvasDrawing(void);

/* ── timers ─────────────────────────────────────────────────────────── */
typedef void (*iv_timerprocEx)(void *context);
void SetWeakTimerEx(const char *name, iv_timerprocEx tp, void *context, int ms);
void ClearTimerByName(const char *name);

/* ── keyboard ───────────────────────────────────────────────────────── */
typedef void (*iv_keyboardhandler)(char *text);
void OpenKeyboard(const char *title, char *buffer, int maxlen, int flags,
                  iv_keyboardhandler hproc);
void CloseKeyboard(void);
void GetKeyboardRect(irect *rect);

/* ── task launch / control panel ────────────────────────────────────── */
int  NewTaskEx(const char *path, char *const args[], const char *appname,
               const char *name, const ibitmap *icon, unsigned int flags,
               int run_as_reader_if_needed);
int  OpenBook(const char *path, const char *parameters, int flags);
int  DrawPanel(const void *icon, const char *text, const char *title, int percent);
/* control_panel request struct — the app only passes NULL, so the
 * concrete layout is not copied here (kept opaque). */
struct control_panel;
int OpenControlPanel(struct control_panel *ctx);

/* ── misc ───────────────────────────────────────────────────────────── */
void   HideHourglass(void);
long   iv_ipc_cmd(long type, long param);
void   iv_update_panel(int readingModeEnable);
int    iv_stat(const char *name, struct stat *st);
void  *QuickDownload(const char *url, int *retsize, int timeout);
void  *QuickDownloadExt(const char *url, int *retsize, int timeout, char *cookie, char *post);
void  *QuickDownloadExt3(const char *url, int *retsize, int timeout, char *cookie,
                         char *post, int *error_code);

/* ── event loop entry (app calls via bs_plat_boot) ──────────────────── */
void InkViewMain(void *cb);

/* ── colours (contract: identical to firmware SDK) ──────────────────── */
#define BLACK 0x000000
#define DGRAY 0x555555
#define LGRAY 0xaaaaaa
#define WHITE 0xffffff

/* ── event codes (identical to firmware SDK) ────────────────────────── */
#define EVT_INIT          21
#define EVT_EXIT          22
#define EVT_SHOW          23
#define EVT_KEYPRESS      25
#define EVT_POINTERUP     29
#define EVT_POINTERDOWN   30
#define EVT_POINTERMOVE   31
#define EVT_POINTERLONG   34
#define EVT_REPAINT       43
#define EVT_FOREGROUND    151

/* ── key codes (identical to firmware SDK) ──────────────────────────── */
#define IV_KEY_MENU  0x17
#define IV_KEY_PREV  0x18
#define IV_KEY_NEXT  0x19
#define IV_KEY_HOME  0x1a
#define IV_KEY_BACK  0x1b
#define IV_KEY_PREV2 0x1c
#define IV_KEY_NEXT2 0x1d

/* ── task flags (NewTaskEx) ─────────────────────────────────────────── */
#define TASK_HIDDEN          (1 << 0)
#define TASK_COPYLASTFB      (1 << 1)
#define TASK_NOUPDATEONFOCUS (1 << 2)
#define TASK_SINGLEINSTANCE  (1 << 3)
#define TASK_SPYEVENTS       (1 << 4)
#define TASK_OUTOFSTACK      (1 << 5)
#define TASK_NOFORCEDKILL    (1 << 6)
#define TASK_MAKEACTIVE      (1 << 7)

/* ── bitmap stretch flags ───────────────────────────────────────────── */
#define STRETCH   (1 << 0)

/* ── keyboard flags ─────────────────────────────────────────────────── */
#define KBD_NORMAL   0
#define KBD_PASSEVENTS 0x8000

#ifdef __cplusplus
}
#endif

#endif /* IV_H_ */