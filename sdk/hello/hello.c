/*
 * hello.c — minimal libinkview hello-world for the pbemu emulator.
 *
 * Compile (inside the pbdev container):
 *
 *   arm-linux-gnueabi-gcc \
 *       -I<PATH>/sdk/pocketbook-sdk-b288/include \
 *       -L<PATH>/sdk/pocketbook-sdk-b288/lib \
 *       -Wall -Wextra -O2 \
 *       sdk/hello/hello.c -o /tmp/hello \
 *       -Wl,-rpath,<PATH>/sdk/pocketbook-sdk-b288/lib \
 *       -Wl,--allow-shlib-undefined \
 *       <PATH>/U633_6.8.2817/ebrmain/cramfs/lib/libz.so.1.2.11 \
 *       -linkview -lhwconfig -lpthread
 *
 * Notes:
 *   - `--allow-shlib-undefined` is safe because libinkview.so's transitive
 *     deps (libpng12, libjpeg8, libtiff5, libfreetype6, libssl1.0.0,
 *     libicuuc58, libcurl4, ...) are only needed at run-time inside the
 *     guest container, where the firmware's own copies of those libs are
 *     present under /ebrmain/lib.
 *   - libz.so.1.2.11 is taken from the firmware tree so the binary does not
 *     have to depend on a host armel zlib.
 *
 * Behaviour: opens an inkview full-screen window, draws a centred greeting,
 * then exits on any pointer-up or keypress event. Intentionally trivial —
 * its only job is to prove the SDK headers and shared libraries link and
 * run inside qemu-arm + libshim.so.
 */

#include <inkview.h>

#include <stdio.h>

static ifont *g_font_std;
static ifont *g_font_bold;

static int
on_event(int type, int par1, int par2)
{
    (void)par1;
    (void)par2;

    if (type == EVT_INIT) {
        g_font_std = OpenFont(DEFAULTFONT, 24, 0);
        g_font_bold = OpenFont(DEFAULTFONTB, 48, 0);
        ClearScreen();
        if (g_font_bold != NULL) {
            SetFont(g_font_bold, BLACK);
            DrawTextRect(0,
                         ScreenHeight() / 2 - 80,
                         ScreenWidth(),
                         80,
                         "Hello from",
                         ALIGN_CENTER | VALIGN_MIDDLE);
        }
        if (g_font_std != NULL) {
            SetFont(g_font_std, BLACK);
            DrawTextRect(0,
                         ScreenHeight() / 2,
                         ScreenWidth(),
                         60,
                         "pbemu SDK-B288",
                         ALIGN_CENTER | VALIGN_MIDDLE);
            DrawTextRect(0,
                         ScreenHeight() - 80,
                         ScreenWidth(),
                         60,
                         "tap anywhere to exit",
                         ALIGN_CENTER | VALIGN_MIDDLE);
        }
        FullUpdate();
        return 1;
    }
    if (type == EVT_POINTERUP || type == EVT_KEYPRESS) {
        CloseApp();
        return 1;
    }
    if (type == EVT_EXIT) {
        if (g_font_std != NULL) {
            CloseFont(g_font_std);
            g_font_std = NULL;
        }
        if (g_font_bold != NULL) {
            CloseFont(g_font_bold);
            g_font_bold = NULL;
        }
        return 1;
    }
    return 0;
}

int
main(int argc, char **argv)
{
    (void)argc;
    (void)argv;
    InkViewMain(on_event);
    return 0;
}
