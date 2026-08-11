#ifndef BS_CONFIG_H
#define BS_CONFIG_H

/* bs_config.h — Configuration + log module (bs_config.c): config-path resolution, key/value
 * config load/save, the log file, and small string helpers (trim_ws,
 * dirname_of). */

#include "bookshelf.h"

extern FILE *g_log;

extern char g_cfg_reader[220];

extern char g_config_path[600];

void log_open(const char *argv0);

void log_close(void);

char *trim_ws(char *s);

int read_kv_file(const char *path, cfg_kv_cb cb, void *user);

void cfg_set_kv(const char *key, const char *value, void *user);

void dirname_of(const char *path, char *out, size_t out_cap);

void load_config_file(const char *argv0, struct cfg_out *out);

void resolve_config_path(const char *argv0);

const char *log_path(void);

#endif /* BS_CONFIG_H */
