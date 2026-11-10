/*
 * Additional C shims: stdio translation for bionic's `__sF`.
 *
 * Bionic's `stdin`/`stdout`/`stderr` are `&__sF[0..2]`; glibc's are its own
 * `FILE*`. Any guest that passes a bionic stream pointer to a libc stdio call
 * would corrupt memory if forwarded directly. These trampolines map the guest
 * pointer through `acos_map_stream` (Rust) before calling the real libc.
 *
 * The `fprintf` family is variadic, so it must live in C.
 */

#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

extern uintptr_t acos_map_stream(uintptr_t stream);

int acos_fprintf(uintptr_t stream, const char *fmt, ...) {
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vfprintf((FILE *)acos_map_stream(stream), fmt, ap);
    va_end(ap);
    return n;
}

int acos_vfprintf(uintptr_t stream, const char *fmt, va_list ap) {
    return vfprintf((FILE *)acos_map_stream(stream), fmt, ap);
}

int acos_fputs(const char *s, uintptr_t stream) {
    return fputs(s, (FILE *)acos_map_stream(stream));
}

int acos_fputc(int c, uintptr_t stream) {
    return fputc(c, (FILE *)acos_map_stream(stream));
}

size_t acos_fwrite(const void *p, size_t size, size_t n, uintptr_t stream) {
    return fwrite(p, size, n, (FILE *)acos_map_stream(stream));
}

size_t acos_fread(void *p, size_t size, size_t n, uintptr_t stream) {
    return fread(p, size, n, (FILE *)acos_map_stream(stream));
}

int acos_fclose(uintptr_t stream) {
    return fclose((FILE *)acos_map_stream(stream));
}

int acos_fflush(uintptr_t stream) {
    if (stream == 0) {
        return fflush(NULL);
    }
    return fflush((FILE *)acos_map_stream(stream));
}

uintptr_t acos_fopen(const char *path, const char *mode) {
    return (uintptr_t)fopen(path, mode);
}

uintptr_t acos_fdopen(int fd, const char *mode) {
    return (uintptr_t)fdopen(fd, mode);
}
