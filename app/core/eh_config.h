#ifndef EH_CONFIG_H
#define EH_CONFIG_H

/* eh_config.h — Configuration + log module (eh_config.c): config-path resolution, key/value
 * config load/save, the log file, and small string helpers (trim_ws,
 * dirname_of). */

#include "eh_core.h"

extern FILE *eh_g_log;

extern char eh_g_cfg_reader[220];

extern char eh_g_config_path[600];

void eh_log_open(const char *argv0);

int eh_read_kv_file(const char *path, eh_cfg_kv_cb cb, void *user);

void eh_cfg_set_kv(const char *key, const char *value, void *user);

void eh_dirname_of(const char *path, char *out, size_t out_cap);

void eh_load_config_file(const char *argv0, struct eh_cfg_out *out);

void eh_resolve_config_path(const char *argv0);

const char *eh_log_path(void);

#endif /* EH_CONFIG_H */
