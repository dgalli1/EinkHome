/* eh_config.c — part of the bookshelf app (see eh_core.h) */

#include "eh_core.h"
#include "eh_config.h"
#include "eh_model.h"

/* ── log file ────────────────────────────────────────────────────────── */

FILE       *eh_g_log = NULL;
static char g_log_path[300];

/* Wall-clock prefix for every log line: `[HH:MM:SS] ` (local time). */
static void
stamp(FILE *f)
{
    time_t     t = time(NULL);
    struct tm *ltm = localtime(&t);

    if (ltm != NULL)
        fprintf(f, "[%02d:%02d:%02d] ",
                ltm->tm_hour, ltm->tm_min, ltm->tm_sec);
}

const char *
eh_log_path(void)
{
    return g_log_path;
}

void
eh_log_open(const char *argv0)
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
        snprintf(path, sizeof path, "%s/bookshelf.log", eh_plat_write_root());
    }
    eh_g_log = fopen(path, "a");
    if (eh_g_log == NULL) {
        /* The app dir may not be writable (emulator guest); the log
         * then lives in the platform's scratch root.  Only record the
         * fallback path once its fopen actually succeeds, so
         * g_log_path never claims a file that was not opened. */
        snprintf(path, sizeof path, "%s/bookshelf.log", eh_plat_write_root());
        eh_g_log = fopen(path, "a");
        if (eh_g_log != NULL)
            snprintf(g_log_path, sizeof g_log_path, "%s", path);
    } else {
        snprintf(g_log_path, sizeof g_log_path, "%s", path);
    }
    if (eh_g_log != NULL) {
        setvbuf(eh_g_log, NULL, _IOLBF, 0);
        stamp(eh_g_log);
        fprintf(eh_g_log, "--- bookshelf.app log opened (argv0=%s) ---\n", argv0 ? argv0 : "(null)");
    }
}

void
eh_LOG(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    stamp(stderr);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    if (eh_g_log != NULL) {
        va_start(ap, fmt);
        stamp(eh_g_log);
        vfprintf(eh_g_log, fmt, ap);
        va_end(ap);
    }
}

/* ── config file reader ──────────────────────────────────────────────── */

static char *
eh_trim_ws(char *s)
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
eh_read_kv_file(const char *path, eh_cfg_kv_cb cb, void *user)
{
    FILE *f = fopen(path, "r");
    if (f == NULL)
        return -1;
    char line[512];
    int  lineno = 0;
    while (fgets(line, sizeof line, f) != NULL) {
        lineno++;
        char *p = eh_trim_ws(line);
        if (*p == '\0' || *p == '#' || *p == ';')
            continue;
        char *eq = strchr(p, '=');
        if (eq == NULL) {
            eh_LOG("[bookshelf] %s:%d: ignoring `%s`\n", path, lineno, p);
            continue;
        }
        *eq = '\0';
        char *k = eh_trim_ws(p);
        char *v = eh_trim_ws(eq + 1);
        cb(k, v, user);
    }
    fclose(f);
    return 0;
}

/* Raw `reader=` value from the config file, resolved to reader_pref after
 * detect_readers() runs (the reader table must exist first). */
char eh_g_cfg_reader[220];

/* Store the (trimmed) language string, lowercased, in eh_g_lang. */
static void
cfg_set_language(const char *value)
{
    snprintf(eh_g_lang, sizeof eh_g_lang, "%.3s", value);
    for (char *p = eh_g_lang; *p; p++)
        *p = (char)tolower((unsigned char)*p);
}

/* Resolve a `source=` value into the enum-typed state. */
static void
cfg_set_source(const char *value)
{
    if (strcmp(value, "local") == 0)
        eh_g_state.source = EH_SOURCE_LOCAL;
    else if (strcmp(value, "folder") == 0)
        eh_g_state.source = EH_SOURCE_FOLDER;
    else
        eh_g_state.source = EH_SOURCE_KAVITA;
}

void
eh_cfg_set_kv(const char *key, const char *value, void *user)
{
    struct eh_cfg_out *out = user;
    if (strcmp(key, "api_url") == 0 || strcmp(key, "url") == 0) {
        snprintf(out->api_url, out->url_cap, "%s", value);
    } else if (strcmp(key, "api_token") == 0 || strcmp(key, "token") == 0) {
        snprintf(out->api_token, out->token_cap, "%s", value);
    } else if (strcmp(key, "language") == 0 || strcmp(key, "lang") == 0) {
        cfg_set_language(value);
    } else if (strcmp(key, "reader") == 0) {
        snprintf(eh_g_cfg_reader, sizeof eh_g_cfg_reader, "%s", value);
    } else if (strcmp(key, "downloads_dir") == 0 || strcmp(key, "download_dir") == 0) {
        snprintf(eh_g_cfg_downloads_dir, sizeof eh_g_cfg_downloads_dir, "%s", value);
    } else if (strcmp(key, "source") == 0) {
        cfg_set_source(value);
    } else {
        eh_LOG("[bookshelf] config: unknown key `%s`\n", key);
    }
}

void
eh_dirname_of(const char *path, char *out, size_t out_cap)
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
eh_load_config_file(const char *argv0, struct eh_cfg_out *out)
{
    snprintf(out->api_token, out->token_cap, "%s", EH_TOKEN_DEFAULT);
    char path[512];

    if (argv0 != NULL && strchr(argv0, '/') != NULL) {
        eh_dirname_of(argv0, path, sizeof path);
        if (path[0] != '\0') {
            char candidate[512];
            snprintf(candidate, sizeof candidate, "%s/bookshelf.cfg", path);
            if (eh_read_kv_file(candidate, eh_cfg_set_kv, out) == 0)
                eh_LOG("[bookshelf] config: %s\n", candidate);
        }
    }
    {
        char base[400];
        snprintf(base, sizeof base, "%s/%s", eh_plat_config_base_dir(),
                 EH_CONFIG_FILENAME);
        if (eh_read_kv_file(base, eh_cfg_set_kv, out) == 0)
            eh_LOG("[bookshelf] config: %s\n", base);
    }
    /* A settings save that had to fall back to the scratch root
     * (unwritable app dir, e.g. the emulator guest) is re-applied last
     * so it overrides the read-only base config on the next launch. */
    {
        char tmp[400];
        snprintf(tmp, sizeof tmp, "%s/%s", eh_plat_write_root(),
                 EH_CONFIG_FILENAME);
        if (eh_read_kv_file(tmp, eh_cfg_set_kv, out) == 0)
            eh_LOG("[bookshelf] config: %s (override)\n", tmp);
    }
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
char eh_g_config_path[600];

void
eh_resolve_config_path(const char *argv0)
{
    char primary[600];
    primary[0] = '\0';
    if (argv0 != NULL && strchr(argv0, '/') != NULL) {
        char dir[512];
        eh_dirname_of(argv0, dir, sizeof dir);
        if (dir[0] != '\0')
            snprintf(primary, sizeof primary, "%s/%s", dir, EH_CONFIG_FILENAME);
    }
    if (primary[0] == '\0')
        snprintf(primary, sizeof primary, "%s/%s", eh_plat_config_base_dir(),
                 EH_CONFIG_FILENAME);

    /* Prefer the primary when its DIRECTORY is writable — settings and
     * the library store are created next to the config file, so a
     * writable config file alone is not enough (e.g. a world-writable
     * cfg in an app dir the guest cannot write would point the store
     * at a directory where no new file can be created).  Otherwise use
     * the guest-writable /tmp fallback. */
    char dir_copy[600];
    snprintf(dir_copy, sizeof dir_copy, "%s", primary);
    char *slash = strrchr(dir_copy, '/');
    if (slash != NULL)
        *slash = '\0';
    if (dir_copy[0] != '\0' && access(dir_copy, W_OK) == 0) {
        snprintf(eh_g_config_path, sizeof eh_g_config_path, "%s", primary);
        return;
    }
    snprintf(eh_g_config_path, sizeof eh_g_config_path, "%s/%s",
             eh_plat_write_root(), EH_CONFIG_FILENAME);
}
