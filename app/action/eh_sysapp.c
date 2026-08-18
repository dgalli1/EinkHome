/* eh_sysapp.c — promote/demote bookshelf to the PocketBook home task
 * (part of the bookshelf app; see eh_core.h).
 *
 * The app is safest installed as an ordinary application in the
 * standard PocketBook applications folder where it never touches the
 * boot path.  "Install as system app" (Settings) promotes the RUNNING
 * binary to the firmware's home-task override path (eh_plat_sysapp_dir:
 * /mnt/ext1/system/bin) that monitor.app boots in preference to the
 * stock bookshelf.  Because promotion copies the binary that is
 * actually running and verified, flipping the toggle is the explicit
 * "this version works" confirmation — a freshly copied standard-folder
 * build is never silently promoted.
 *
 * The target directory can be redirected with $EH_SYSAPP_DIR (used by
 * the SDL e2e suite, which has no device paths). */

#include "eh_core.h"
#include "eh_config.h"
#include "eh_model.h"
#include "eh_plat.h"
#include "eh_sysapp.h"

#include <errno.h>
#include <unistd.h>

/* Home-task override dir: platform-owned (eh_plat_sysapp_dir).  Defaults
 * to the real device path the app user can write (/mnt/ext1 is the user
 * partition); the emulator guest and the SDL build cannot, hence the
 * $EH_SYSAPP_DIR test hook lives in the backend. */
const char *
eh_sysapp_dir(void)
{
    return eh_plat_sysapp_dir();
}

/* Resolve the running binary's path.  /proc/self/exe is authoritative
 * (works even if the on-disk file was unlinked, e.g. an unpromoted
 * home-task still running); fall back to argv0. */
static int
sysapp_self_bin(char *out, size_t cap)
{
    ssize_t n = readlink("/proc/self/exe", out, cap - 1);
    if (n > 0) {
        out[n] = '\0';
        return 0;
    }
    if (eh_g_argv0[0] != '\0') {
        snprintf(out, cap, "%s", eh_g_argv0);
        return 0;
    }
    return -1;
}

int
eh_sysapp_detect(void)
{
    char         p[EH_MAX_PATH_LEN];
    struct stat  st;
    snprintf(p, sizeof p, "%s/bookshelf.app", eh_sysapp_dir());
    return iv_stat(p, &st) == 0;
}

/* Stream-copy one file to another, preserving nothing but bytes.  The
 * home-task override must be a raw copy (not a wrapper script): a
 * wrapper's exec would register the home task as the wrapper, breaking
 * the reader's book-open handshake. */
static int
sysapp_copy_file(const char *src, const char *dst)
{
    FILE *in = fopen(src, "rb");
    if (in == NULL)
        return -1;
    FILE *out = fopen(dst, "wb");
    if (out == NULL) {
        fclose(in);
        return -1;
    }
    char buf[8192];
    size_t n;
    while ((n = fread(buf, 1, sizeof buf, in)) > 0) {
        if (fwrite(buf, 1, n, out) != n) {
            fclose(out);
            fclose(in);
            unlink(dst);
            return -1;
        }
    }
    int ok = (ferror(in) == 0) && (fclose(out) == 0);
    fclose(in);
    if (!ok) {
        unlink(dst);
        return -1;
    }
    return 0;
}

int
eh_sysapp_promote(void)
{
    char src[420];
    if (sysapp_self_bin(src, sizeof src) != 0)
        return -1;

    const char *dir = eh_sysapp_dir();
    char dst[EH_MAX_PATH_LEN];
    char cfg[EH_MAX_PATH_LEN];
    snprintf(dst, sizeof dst, "%s/bookshelf.app", dir);
    snprintf(cfg, sizeof cfg, "%s/bookshelf.cfg", dir);

    /* Ensure the install dir exists before writing the cfg.  A fresh
     * promote creates it here; mkdir is a benign side effect (it IS the
     * target dir) and lets a cfg-write failure be reported with no binary
     * installed.  EEXIST is ignored; any other error surfaces on the
     * fopen below. */
    mkdir(dir, 0755);

    /* A fresh cfg so the promoted home task talks to the same API with
     * the same settings as the instance that was verified.  Written
     * before the copy so a failure leaves no binary installed. */
    if (eh_write_config_file(cfg) != 0) {
        eh_LOG("[bookshelf] sysapp: promote cfg write %s failed\n", cfg);
        return -1;
    }

    /* If we are ALREADY the home task, the source IS the target —
     * copying would truncate the running executable.  Skip the copy. */
    int is_home = (strcmp(src, dst) == 0);

    if (!is_home) {
        if (sysapp_copy_file(src, dst) != 0) {
            eh_LOG("[bookshelf] sysapp: promote copy %s -> %s failed\n", src, dst);
            return -1;
        }
        if (chmod(dst, 0755) != 0)
            eh_LOG("[bookshelf] sysapp: chmod %s: %s\n", dst, strerror(errno));
    }

    eh_LOG("[bookshelf] sysapp: %s installed as home task (%s)\n",
           dst, is_home ? "was already" : "promoted");
    return 0;
}

int
eh_sysapp_unpromote(void)
{
    const char *dir = eh_sysapp_dir();
    char dst[EH_MAX_PATH_LEN];
    char cfg[EH_MAX_PATH_LEN];
    snprintf(dst, sizeof dst, "%s/bookshelf.app", dir);
    snprintf(cfg, sizeof cfg, "%s/bookshelf.cfg", dir);
    /* Missing files are a successful unpromote. */
    unlink(dst);
    unlink(cfg);
    eh_LOG("[bookshelf] sysapp: home-task override removed; stock home returns on reboot\n");
    return 0;
}