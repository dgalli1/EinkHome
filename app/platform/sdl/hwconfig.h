#ifndef HWCONFIG_H_
#define HWCONFIG_H_
/*
 * hwconfig.h — PB firmware hardware-config probe API, host (SDL) impl.
 *
 * Mirrors the subset of pocketbook-sdk-b288 hwconfig.h the app links:
 * the device capability probes.  On a PC these report a generic colour,
 * audio-enabled device ("pc") — the launcher resolves to the "all"
 * branch of every view.json/apps_db.json conditional, which is correct.
 */

#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

unsigned int device_number(void);
bool         device_has_audio(void);
bool         device_has_touchpanel(void);
int          device_display_colormask(void);

#ifdef __cplusplus
}
#endif

#endif /* HWCONFIG_H_ */