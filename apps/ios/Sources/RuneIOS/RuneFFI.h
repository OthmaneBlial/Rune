#ifndef RUNE_FFI_H
#define RUNE_FFI_H

#include <stddef.h>
#include <stdint.h>

typedef struct {
    char *stdout;
    char *stderr;
    int32_t status;
} RuneOutput;

typedef struct {
    uint8_t *data;
    size_t length;
    int32_t status;
    char *message;
} RuneFile;

typedef struct {
    int32_t kind;
    const char *stdout;
    const char *stderr;
    int32_t status;
    const char *current_directory;
} RuneEvent;

typedef void (*RuneEventCallback)(const RuneEvent *event, void *user_data);

#define RUNE_EVENT_OUTPUT 1
#define RUNE_EVENT_STATUS 2

void *rune_session_new(const char *root);
// Create a session with an independent bounded persistence namespace.
void *rune_session_new_named(const char *root, const char *session_id);
void rune_session_destroy(void *handle);
// Request cooperative cancellation at the next Rust execution boundary.
void rune_session_cancel(const void *handle);
// Update one validated Rust-owned configuration value without history entry.
RuneOutput rune_session_set_configuration(
    void *handle,
    const char *key,
    const char *value
);
// Reset Rust-owned configuration without history entry.
RuneOutput rune_session_reset_configuration(void *handle);
RuneOutput rune_session_put_file(
    void *handle,
    const char *path,
    const uint8_t *data,
    size_t length
);
RuneFile rune_session_get_file(const void *handle, const char *path);
// Execute and persist the session before returning.
RuneOutput rune_session_execute(void *handle, const char *input);
// Execute a newline-delimited script and persist the session before returning.
RuneOutput rune_session_execute_script(void *handle, const char *script);
// Execute and synchronously deliver borrowed output/status events.
RuneOutput rune_session_execute_with_events(
    void *handle,
    const char *input,
    RuneEventCallback callback,
    void *user_data
);
// Execute a script and synchronously deliver borrowed output/status events.
RuneOutput rune_session_execute_script_with_events(
    void *handle,
    const char *script,
    RuneEventCallback callback,
    void *user_data
);
char *rune_session_current_directory(const void *handle);
char *rune_session_history(const void *handle);
char *rune_session_configuration(const void *handle);
char *rune_session_commands(const void *handle);
// Returns bounded command or sandbox-path replacement tokens.
char *rune_session_complete(const void *handle, const char *input);
RuneOutput rune_session_startup_output(void *handle);
void rune_string_free(char *value);
void rune_file_bytes_free(uint8_t *data, size_t length);

#endif
