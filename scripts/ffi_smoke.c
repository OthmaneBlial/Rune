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
    const char *expected_output;
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
        if (event->stdout_data == NULL || observation->expected_output == NULL ||
            strstr(event->stdout_data, observation->expected_output) == NULL) {
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

    RuneTerminalCursor cursor = rune_session_terminal_cursor(session);
    if (!cursor.visible) {
        rune_session_destroy(session);
        return fail("terminal cursor was unexpectedly hidden at session start");
    }

    RuneOutput hide_cursor = rune_session_execute(session, "printf '\033[?25l'");
    if (hide_cursor.status != 0 || rune_session_terminal_cursor(session).visible) {
        rune_string_free(hide_cursor.stdout_data);
        rune_string_free(hide_cursor.stderr_data);
        rune_session_destroy(session);
        return fail("CSI cursor-hide state did not cross the public ABI");
    }
    rune_string_free(hide_cursor.stdout_data);
    rune_string_free(hide_cursor.stderr_data);

    RuneOutput show_cursor = rune_session_execute(session, "printf '\033[?25h'");
    if (show_cursor.status != 0 || !rune_session_terminal_cursor(session).visible) {
        rune_string_free(show_cursor.stdout_data);
        rune_string_free(show_cursor.stderr_data);
        rune_session_destroy(session);
        return fail("CSI cursor-show state did not cross the public ABI");
    }
    rune_string_free(show_cursor.stdout_data);
    rune_string_free(show_cursor.stderr_data);

    RuneOutput underline_cursor = rune_session_execute(session, "printf '\033[3 q'");
    RuneTerminalCursor shaped_cursor = rune_session_terminal_cursor(session);
    if (underline_cursor.status != 0 || shaped_cursor.shape != 2) {
        rune_string_free(underline_cursor.stdout_data);
        rune_string_free(underline_cursor.stderr_data);
        rune_session_destroy(session);
        return fail("DECSCUSR cursor shape did not cross the public ABI");
    }
    rune_string_free(underline_cursor.stdout_data);
    rune_string_free(underline_cursor.stderr_data);

    EventObservation observation = {"ffi-event", 0, 0, 0};
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

    EventObservation script_observation = {"ffi-script", 0, 0, 0};
    RuneOutput scripted = rune_session_execute_script_with_events(
        session,
        "printf ffi-script",
        observe_event,
        &script_observation
    );
    if (scripted.status != 0 || script_observation.output_events < 1 ||
        script_observation.status_events < 1 || script_observation.invalid_events != 0) {
        rune_string_free(scripted.stdout_data);
        rune_string_free(scripted.stderr_data);
        rune_session_destroy(session);
        return fail("borrowed script events did not cross the public ABI");
    }
    rune_string_free(scripted.stdout_data);
    rune_string_free(scripted.stderr_data);

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
