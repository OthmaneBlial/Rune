#ifndef RUNE_FFI_H
#define RUNE_FFI_H

#include <stdint.h>

typedef struct {
    char *stdout;
    char *stderr;
    int32_t status;
} RuneOutput;

void *rune_session_new(const char *root);
void rune_session_destroy(void *handle);
RuneOutput rune_session_execute(void *handle, const char *input);
RuneOutput rune_session_execute_script(void *handle, const char *script);
char *rune_session_current_directory(const void *handle);
char *rune_session_history(const void *handle);
char *rune_session_configuration(const void *handle);
char *rune_session_commands(const void *handle);
RuneOutput rune_session_startup_output(void *handle);
void rune_string_free(char *value);

#endif
