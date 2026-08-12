#ifndef BS_CONFIG_H
#define BS_CONFIG_H

/* bs_config.h — Configuration + log module (bs_config.c): config-path resolution, key/value
 * config load/save, the log file, and small string helpers (trim_ws,
 * dirname_of). */

#include "bs_core.h"

extern FILE *bs_g_log;

extern char bs_g_cfg_reader[220];

extern char bs_g_config_path[600];

void bs_log_open(const char *argv0);

void bs_log_close(void);

char *bs_trim_ws(char *s);

int bs_read_kv_file(const char *path, bs_cfg_kv_cb cb, void *user);

void bs_cfg_set_kv(const char *key, const char *value, void *user);

void bs_dirname_of(const char *path, char *out, size_t out_cap);

void bs_load_config_file(const char *argv0, struct bs_cfg_out *out);

void bs_resolve_config_path(const char *argv0);

const char *bs_log_path(void);

#endif /* BS_CONFIG_H */
