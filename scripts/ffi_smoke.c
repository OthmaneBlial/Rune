#include <errno.h>
#include <ftw.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "RuneFFI.h"

static int fail(const char *message) {
    fprintf(stderr, "ffi smoke failed: %s\n", message);
    return 1;
}

static int remove_path(const char *path, const struct stat *info, int type, struct FTW *state) {
    (void)info;
    (void)type;
    (void)state;
    return remove(path);
}

int main(void) {
    char root[] = "/tmp/rune-ffi-smoke-XXXXXX";
    if (mkdtemp(root) == NULL) {
        perror("mkdtemp");
        return 1;
    }

    void *session = rune_session_new(root);
    if (session == NULL) {
        return fail("rune_session_new returned null");
    }

    RuneOutput output = rune_session_execute(session, "printf ffi-ok");
    if (output.status != 0 || output.stdout_data == NULL || strcmp(output.stdout_data, "ffi-ok") != 0) {
        rune_string_free(output.stdout_data);
        rune_string_free(output.stderr_data);
        rune_session_destroy(session);
        return fail("execute result did not cross the public ABI");
    }

    rune_string_free(output.stdout_data);
    rune_string_free(output.stderr_data);

    char *directory = rune_session_current_directory(session);
    if (directory == NULL || directory[0] == '\0') {
        rune_string_free(directory);
        rune_session_destroy(session);
        return fail("current directory did not cross the public ABI");
    }

    rune_string_free(directory);
    rune_session_destroy(session);
    if (nftw(root, remove_path, 64, FTW_DEPTH | FTW_PHYS) != 0 && errno != ENOENT) {
        perror("remove temporary FFI root");
        return 1;
    }

    puts("FFI C smoke passed.");
    return 0;
}
