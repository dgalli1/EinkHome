#include <stdio.h>
#include <stdlib.h>
#include <inkview.h>

int
main(int argc, char **argv)
{
    (void)argc;
    (void)argv;
    InitInkview(0);
    fprintf(stderr, "PBEMU_API_URL = %s\n", getenv("PBEMU_API_URL") ?: "(unset)");
    fprintf(stderr, "PBEMU_API_HOST = %s\n", getenv("PBEMU_API_HOST") ?: "(unset)");
    CloseApp();
    return 0;
}
