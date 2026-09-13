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
char *rune_session_current_directory(const void *handle);
char *rune_session_history(const void *handle);
char *rune_session_commands(const void *handle);
void rune_string_free(char *value);

#endif
