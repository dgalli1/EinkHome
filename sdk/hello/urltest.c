#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <inkview.h>

static char g_api_base[160];

static void
build_urls(void)
{
    const char *env_url = getenv("PBEMU_API_URL");
    const char *env_host = getenv("PBEMU_API_HOST");
    const char *url = env_url ? env_url : (env_host ? env_host : "http://169.254.1.2:8765");
    if (strncmp(url, "http://", 7) != 0 && strncmp(url, "https://", 8) != 0) {
        char tmp[200];
        snprintf(tmp, sizeof tmp, "http://%s:8765", url);
        snprintf(g_api_base, sizeof g_api_base, "%s", tmp);
    } else {
        snprintf(g_api_base, sizeof g_api_base, "%s", url);
    }
}

int
main(int argc, char **argv)
{
    (void)argc;
    (void)argv;
    InitInkview(0);
    build_urls();
    fprintf(stderr, "PBEMU_API_URL = %s\n", getenv("PBEMU_API_URL") ?: "(unset)");
    fprintf(stderr, "PBEMU_API_HOST = %s\n", getenv("PBEMU_API_HOST") ?: "(unset)");
    fprintf(stderr, "Resolved api_base = %s\n", g_api_base);
    fprintf(stderr, "Books URL would be: %s/api/v1/books?limit=200&access_token=...\n", g_api_base);
    CloseApp();
    return 0;
}
