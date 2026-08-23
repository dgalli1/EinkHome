/*
 * main.c — PocketBook shim that boots the Rust toolkit shelf.
 *
 * The only purpose of this file is to satisfy libinkview's task model the
 * way the stock bookshelf (and sdk/hello/hello.c) does: InitInkview then
 * InkViewMain, forwarding events to the Rust facade (eh_pb).  All layout,
 * drawing and hit-testing lives in the Rust toolkit; the shim stays ~40
 * lines.
 *
 * Link with build_armel.sh using the Rust staticlib as a link input:
 *
 *   cargo build --release --target armv7-unknown-linux-gnueabi -p eh_pb
 *   PBEMU_APP_LIB=eh_ui/target/armv7-unknown-linux-gnueabi/release/libeh_pb.a \
 *     sdk/build_armel.sh sdk/pb-demo/main.c --output build/pb-demo.app
 */

#include <inkview.h>

#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>

/* Rust facade (eh_pb crate). */
extern int eh_pb_init(void);
extern int eh_pb_on_event(int type, int par1, int par2);
extern int eh_pb_panel_height(void);

/* Exported by libinkview but absent from this SDK vintage's headers —
 * same weak pattern the main app's eh_plat_pb.c uses. */
extern void IvSetAppCapability(int caps) __attribute__((weak));
extern void SetDefaultOrientation(int n) __attribute__((weak));

/*
 * stat64-family shims (required at LINK time).  Rust std on 32-bit arm
 * references plain `stat64`/`fstat64`/`lstat64`/`fstatat64`, but glibc only
 * provides those as header macros mapping to `__xstat64`/`__fxstat64`/
 * `__lxstat64`/`__fxstatat64` (normally via libc_nonshared at a crt link).
 * The firmware libc.so.6 exports the __x* names directly, so these four
 * shims restore the aliases the shared-lib-only link is missing.
 * _STAT_VER=1 on arm glibc.
 */
#include <sys/stat.h>
#ifndef _STAT_VER
#define _STAT_VER 1
#endif
extern int __xstat64(int ver, const char *path, struct stat64 *buf);
extern int __fxstat64(int ver, int fd, struct stat64 *buf);
extern int __lxstat64(int ver, const char *path, struct stat64 *buf);
extern int __fxstatat64(int ver, int fd, const char *path, struct stat64 *buf, int flag);

int stat64(const char *path, struct stat64 *buf)  { return __xstat64(_STAT_VER, path, buf); }
int fstat64(int fd, struct stat64 *buf)            { return __fxstat64(_STAT_VER, fd, buf); }
int lstat64(const char *path, struct stat64 *buf)  { return __lxstat64(_STAT_VER, path, buf); }
int fstatat64(int fd, const char *path, struct stat64 *buf, int flag)
{ return __fxstatat64(_STAT_VER, fd, path, buf, flag); }
/* MuPDF's posix directory/stat code calls plain stat/lstat; same
 * shared-lib-only redirect as the 64-bit variants above. */
extern int __xstat(int ver, const char *path, struct stat *buf);
extern int __lxstat(int ver, const char *path, struct stat *buf);
int stat(const char *path, struct stat *buf)   { return __xstat(_STAT_VER, path, buf); }
int lstat(const char *path, struct stat *buf)  { return __lxstat(_STAT_VER, path, buf); }

/*
 * malloc-family forwarders.  The shared-lib-only link (libc passed by full
 * path, no libc_nonshared) leaves `realloc`/`malloc`/`free`/`calloc`
 * unversioned, so they bind to ld-linux.so.3's dl-minimal versions during
 * dl-open — which assert (`ptr == alloc_last_block`) on any realloc that
 * isn't the tail block.  Under a normal `-lc` link, glibc's libc_nonshared
 * interposes the real libc allocator instead.  These strong definitions
 * forward to __libc_malloc/__libc_realloc/__libc_free, matching what the
 * working C bookshelf gets from its link.
 */
extern void *__libc_malloc(size_t n);
extern void *__libc_realloc(void *p, size_t n);
extern void __libc_free(void *p);
extern void *__libc_calloc(size_t n, size_t m);

void *malloc(size_t n)   { return __libc_malloc(n); }
void *realloc(void *p, size_t n) { return __libc_realloc(p, n); }
void  free(void *p)      { return __libc_free(p); }
void *calloc(size_t n, size_t m) { return __libc_calloc(n, m); }

static int
on_event(int type, int par1, int par2)
{
    int rc = eh_pb_on_event(type, par1, par2);
    if (type == EVT_EXIT) {
        CloseApp();
    }
    return rc;
}

int
main(int argc, char **argv)
{
    (void)argc;
    (void)argv;

    /* Do NOT call InitInkview: the task machinery + shim initialise inkview
     * as the ELF loads (the "Starting task ... flags: 0x4110" banner).  An
     * explicit second InitInkview triggers OpenTheme + an ld.so realloc
     * assert on glibc 2.23.  The proven pattern (sdk/hello/hello.c) is just
     * InkViewMain; the first EVT_INIT/EVT_SHOW event initialises the GUI
     * via eh_pb_on_event -> init_once. */

    InkViewMain(on_event);
    return 0;
}