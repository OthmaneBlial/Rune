#ifndef RUNE_FFI_H
#define RUNE_FFI_H

#include <stddef.h>
#include <stdbool.h>
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

typedef struct {
    int32_t status_code;
    size_t body_length;
    int32_t error;
} RuneNetworkResponse;

typedef bool (*RuneNetworkRequestCallback)(
    void *user_data,
    const char *method,
    const char *url,
    const char *headers,
    const uint8_t *body,
    size_t body_length,
    uint8_t *response_buffer,
    size_t response_capacity,
    RuneNetworkResponse *response
);

typedef struct {
    const uint8_t *data;
    size_t length;
} RuneToolchainSlice;

typedef struct {
    RuneToolchainSlice key;
    RuneToolchainSlice value;
} RuneToolchainEnvironmentEntry;

typedef struct {
    size_t path_length;
    size_t media_type_length;
    size_t data_offset;
    size_t data_length;
} RuneToolchainArtifactBuffer;

typedef struct {
    size_t stdout_length;
    size_t stderr_length;
    int32_t status;
    size_t artifact_count;
    int32_t error;
} RuneToolchainResponse;

typedef bool (*RuneToolchainRequestCallback)(
    void *user_data,
    int32_t kind,
    const char *program_name,
    RuneToolchainSlice source,
    const RuneToolchainSlice *args,
    size_t argument_count,
    const RuneToolchainEnvironmentEntry *environment,
    size_t environment_count,
    RuneToolchainSlice stdin_data,
    uint8_t *stdout_buffer,
    size_t stdout_capacity,
    uint8_t *stderr_buffer,
    size_t stderr_capacity,
    RuneToolchainArtifactBuffer *artifact_buffers,
    size_t artifact_capacity,
    uint8_t *artifact_path_buffers,
    size_t artifact_path_capacity,
    uint8_t *artifact_media_type_buffers,
    size_t artifact_media_type_capacity,
    uint8_t *artifact_data_buffer,
    size_t artifact_data_capacity,
    RuneToolchainResponse *response
);

#define RUNE_TOOLCHAIN_C 1
#define RUNE_TOOLCHAIN_CPP 2
#define RUNE_TOOLCHAIN_TEX 3

typedef struct {
    size_t text_length;
    int32_t error;
} RuneClipboardResponse;

typedef bool (*RuneClipboardReadCallback)(
    void *user_data,
    uint8_t *buffer,
    size_t capacity,
    RuneClipboardResponse *response
);

typedef bool (*RuneClipboardWriteCallback)(
    void *user_data,
    const uint8_t *text,
    size_t length
);

typedef bool (*RuneOpenCallback)(
    void *user_data,
    const char *target,
    int32_t target_kind
);

#define RUNE_OPEN_URL 1
#define RUNE_OPEN_FILE 2

void *rune_session_new(const char *root);
// Create a session with an independent bounded persistence namespace.
void *rune_session_new_named(const char *root, const char *session_id);
// Create a session using Documents, Library, and tmp as Rune roots.
void *rune_session_new_with_layout(
    const char *home,
    const char *library,
    const char *temporary
);
// Create a named session using Documents, Library, and tmp as Rune roots.
void *rune_session_new_named_with_layout(
    const char *home,
    const char *library,
    const char *temporary,
    const char *session_id
);
void rune_session_destroy(void *handle);
// Request cooperative cancellation at the next Rust execution boundary.
void rune_session_cancel(const void *handle);
// Install or clear the synchronous native URL transport capability.
int32_t rune_session_set_network_callback(
    void *handle,
    RuneNetworkRequestCallback callback,
    void *user_data
);
// Install or clear the bounded native text clipboard capability.
int32_t rune_session_set_clipboard_callbacks(
    void *handle,
    RuneClipboardReadCallback read,
    RuneClipboardWriteCallback write,
    void *user_data
);
// Install or clear the explicit external URL/file opening capability.
int32_t rune_session_set_open_callback(
    void *handle,
    RuneOpenCallback callback,
    void *user_data
);
// Install or clear one explicit C, C++, or TeX provider.
int32_t rune_session_set_toolchain_callback(
    void *handle,
    int32_t kind,
    RuneToolchainRequestCallback callback,
    void *user_data
);
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
// Return newest-first history matches; null means invalid/oversized query.
char *rune_session_history_search(const void *handle, const char *query);
char *rune_session_configuration(const void *handle);
char *rune_session_commands(const void *handle);
// Returns bounded command or sandbox-path replacement tokens.
char *rune_session_complete(const void *handle, const char *input);
RuneOutput rune_session_startup_output(void *handle);
void rune_string_free(char *value);
void rune_file_bytes_free(uint8_t *data, size_t length);

#endif
