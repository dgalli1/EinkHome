/* bs_config.c — part of the bookshelf app (see bookshelf.h) */

#include "bookshelf.h"

/* ── log file ────────────────────────────────────────────────────────── */

FILE *g_log = NULL;

void
log_open(const char *argv0)
{
    char        path[300];
    const char *home = getenv("PBEMU_LOG_DIR");
    if (home != NULL && home[0] != '\0') {
        snprintf(path, sizeof path, "%s/bookshelf.log", home);
    } else if (argv0 != NULL && strchr(argv0, '/') != NULL) {
        char dir[260];
        snprintf(dir, sizeof dir, "%s", argv0);
        char *slash = strrchr(dir, '/');
        if (slash != NULL)
            *slash = '\0';
        snprintf(path, sizeof path, "%s/bookshelf.log", dir);
    } else {
        snprintf(path, sizeof path, "/tmp/bookshelf.log");
    }
    g_log = fopen(path, "a");
    if (g_log == NULL)
        g_log = fopen("/tmp/bookshelf.log", "a");
    if (g_log != NULL) {
        setvbuf(g_log, NULL, _IOLBF, 0);
        fprintf(g_log, "--- bookshelf.app log opened (argv0=%s) ---\n", argv0 ? argv0 : "(null)");
    }
}

void
log_close(void)
{
    if (g_log != NULL) {
        fclose(g_log);
        g_log = NULL;
    }
}

void
LOG(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    if (g_log != NULL) {
        va_start(ap, fmt);
        vfprintf(g_log, fmt, ap);
        va_end(ap);
    }
}

/* ── config file reader ──────────────────────────────────────────────── */

char *
trim_ws(char *s)
{
    if (s == NULL)
        return s;
    while (*s == ' ' || *s == '\t' || *s == '\r' || *s == '\n')
        s++;
    char *end = s + strlen(s);
    while (end > s && (end[-1] == ' ' || end[-1] == '\t' || end[-1] == '\r' || end[-1] == '\n'))
        end--;
    *end = '\0';
    return s;
}

int
read_kv_file(const char *path, cfg_kv_cb cb, void *user)
{
    FILE *f = fopen(path, "r");
    if (f == NULL)
        return -1;
    char line[512];
    int  lineno = 0;
    while (fgets(line, sizeof line, f) != NULL) {
        lineno++;
        char *p = trim_ws(line);
        if (*p == '\0' || *p == '#' || *p == ';')
            continue;
        char *eq = strchr(p, '=');
        if (eq == NULL) {
            LOG("[bookshelf] %s:%d: ignoring `%s`\n", path, lineno, p);
            continue;
        }
        *eq = '\0';
        char *k = trim_ws(p);
        char *v = trim_ws(eq + 1);
        cb(k, v, user);
    }
    fclose(f);
    return 0;
}

/* Raw `reader=` value from the config file, resolved to reader_pref after
 * detect_readers() runs (the reader table must exist first). */
char g_cfg_reader[220];

void
cfg_set_kv(const char *key, const char *value, void *user)
{
    struct cfg_out *out = user;
    if (strcmp(key, "api_url") == 0 || strcmp(key, "url") == 0) {
        snprintf(out->api_url, out->cap, "%s", value);
    } else if (strcmp(key, "api_token") == 0 || strcmp(key, "token") == 0) {
        snprintf(out->api_token, out->cap, "%s", value);
    } else if (strcmp(key, "language") == 0 || strcmp(key, "lang") == 0) {
        snprintf(g_lang, sizeof g_lang, "%.3s", value);
        for (char *p = g_lang; *p; p++)
            *p = (char)tolower((unsigned char)*p);
    } else if (strcmp(key, "reader") == 0) {
        snprintf(g_cfg_reader, sizeof g_cfg_reader, "%s", value);
    } else {
        LOG("[bookshelf] config: unknown key `%s`\n", key);
    }
}

void
dirname_of(const char *path, char *out, size_t out_cap)
{
    if (path == NULL || out_cap == 0) {
        if (out_cap > 0)
            out[0] = '\0';
        return;
    }
    snprintf(out, out_cap, "%s", path);
    char *slash = strrchr(out, '/');
    if (slash != NULL)
        *slash = '\0';
    else
        out[0] = '\0';
}

void
load_config_file(const char *argv0, struct cfg_out *out)
{
    snprintf(out->api_token, out->cap, "%s", TOKEN_DEFAULT);
    char path[512];

    if (argv0 != NULL && strchr(argv0, '/') != NULL) {
        dirname_of(argv0, path, sizeof path);
        if (path[0] != '\0') {
            char candidate[512];
            snprintf(candidate, sizeof candidate, "%s/bookshelf.cfg", path);
            if (read_kv_file(candidate, cfg_set_kv, out) == 0)
                LOG("[bookshelf] config: %s\n", candidate);
        }
    }
    if (read_kv_file("/etc/pbemu/bookshelf.cfg", cfg_set_kv, out) == 0)
        LOG("[bookshelf] config: /etc/pbemu/bookshelf.cfg\n");
    /* A settings save that had to fall back to /tmp (unwritable app dir,
     * e.g. the emulator guest) is re-applied last so it overrides the
     * read-only base config on the next launch. */
    if (read_kv_file(CONFIG_TMP_PATH, cfg_set_kv, out) == 0)
        LOG("[bookshelf] config: %s (override)\n", CONFIG_TMP_PATH);
}

/* Resolved path of the config file actually loaded (or the preferred
 * write location when none existed).  save_config_file() rewrites this
 * file so settings changes survive a restart.
 *
 * On-device the app's own directory (next to the binary) is writable, so
 * settings persist there.  In the emulator the guest runs as a non-root
 * qemu-arm process whose binary dir (/mnt/ext1/system/bin) is NOT
 * writable — the same reason its log falls back to /tmp — so we fall back
 * to /tmp/bookshelf.cfg, which the guest can write and which the loader
 * re-reads as an override on the next launch. */
char g_config_path[600];

void
resolve_config_path(const char *argv0)
{
    char primary[512];
    primary[0] = '\0';
    if (argv0 != NULL && strchr(argv0, '/') != NULL) {
        char dir[512];
        dirname_of(argv0, dir, sizeof dir);
        if (dir[0] != '\0')
            snprintf(primary, sizeof primary, "%s/%s", dir, CONFIG_FILENAME);
    }
    if (primary[0] == '\0')
        snprintf(primary, sizeof primary, "/etc/pbemu/%s", CONFIG_FILENAME);

    /* Prefer the primary when it's writable (either the file exists and
     * is writable, or its directory is writable so we can create it);
     * otherwise use the guest-writable /tmp fallback. */
    if (access(primary, W_OK) == 0) {
        snprintf(g_config_path, sizeof g_config_path, "%s", primary);
        return;
    }
    char dir_copy[600];
    snprintf(dir_copy, sizeof dir_copy, "%s", primary);
    char *slash = strrchr(dir_copy, '/');
    if (slash != NULL)
        *slash = '\0';
    if (dir_copy[0] != '\0' && access(dir_copy, W_OK) == 0) {
        snprintf(g_config_path, sizeof g_config_path, "%s", primary);
        return;
    }
    snprintf(g_config_path, sizeof g_config_path, "%s", CONFIG_TMP_PATH);
}
