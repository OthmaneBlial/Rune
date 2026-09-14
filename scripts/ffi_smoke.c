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

typedef struct {
    int output_events;
    int status_events;
    int invalid_events;
} EventObservation;

static void observe_event(const RuneEvent *event, void *user_data) {
    EventObservation *observation = user_data;
    if (event == NULL || observation == NULL || event->current_directory == NULL) {
        if (observation != NULL) {
            observation->invalid_events++;
        }
        return;
    }
    if (event->kind == RUNE_EVENT_OUTPUT) {
        if (event->stdout_data == NULL || strcmp(event->stdout_data, "ffi-event") != 0) {
            observation->invalid_events++;
        } else {
            observation->output_events++;
        }
    } else if (event->kind == RUNE_EVENT_STATUS && event->status == 0) {
        observation->status_events++;
    } else {
        observation->invalid_events++;
    }
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

    EventObservation observation = {0, 0, 0};
    RuneOutput streamed = rune_session_execute_with_events(
        session,
        "printf ffi-event",
        observe_event,
        &observation
    );
    if (streamed.status != 0 || observation.output_events != 1 ||
        observation.status_events != 1 || observation.invalid_events != 0) {
        rune_string_free(streamed.stdout_data);
        rune_string_free(streamed.stderr_data);
        rune_session_destroy(session);
        return fail("borrowed execution events did not cross the public ABI");
    }
    rune_string_free(streamed.stdout_data);
    rune_string_free(streamed.stderr_data);

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
