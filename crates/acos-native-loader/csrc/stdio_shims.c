/*
 * Additional C shims: stdio translation for bionic's `__sF` / FILE*.
 *
 * Bionic `FILE` is 152 bytes on LP64 and pre-M guests spell `stderr` as
 * `&__sF[2]`; anything else the guest passes is either a host `FILE*` our
 * `fopen`/`fdopen`/`tmpfile`/`popen` shims handed out (and registered), or an
 * invalid pointer. `acos_map_stream` maps known streams and returns 0 for
 * everything else; here we fail closed with errno=EBADF instead of letting a
 * bogus pointer reach glibc and crash.
 *
 * The `fprintf` family is variadic, so it must live in C.
 */

#include <errno.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

extern uintptr_t acos_map_stream(uintptr_t stream);
extern void acos_register_file(uintptr_t f);
extern void acos_unregister_file(uintptr_t f);

/* Map a guest stream, or return 0 for an invalid stream. */
static FILE *real_stream(uintptr_t stream) {
    uintptr_t mapped = acos_map_stream(stream);
    if (mapped == 0) {
        errno = EBADF;
        return NULL;
    }
    return (FILE *)mapped;
}

int acos_fprintf(uintptr_t stream, const char *fmt, ...) {
    FILE *f = real_stream(stream);
    if (f == NULL) {
        return -1;
    }
    va_list ap;
    int n;
    va_start(ap, fmt);
    n = vfprintf(f, fmt, ap);
    va_end(ap);
    return n;
}

int acos_vfprintf(uintptr_t stream, const char *fmt, va_list ap) {
    FILE *f = real_stream(stream);
    if (f == NULL) {
        return -1;
    }
    return vfprintf(f, fmt, ap);
}

int acos_fputs(const char *s, uintptr_t stream) {
    FILE *f = real_stream(stream);
    if (f == NULL) {
        return EOF;
    }
    return fputs(s, f);
}

int acos_fputc(int c, uintptr_t stream) {
    FILE *f = real_stream(stream);
    if (f == NULL) {
        return EOF;
    }
    return fputc(c, f);
}

size_t acos_fwrite(const void *p, size_t size, size_t n, uintptr_t stream) {
    FILE *f = real_stream(stream);
    if (f == NULL) {
        return 0;
    }
    return fwrite(p, size, n, f);
}

size_t acos_fread(void *p, size_t size, size_t n, uintptr_t stream) {
    FILE *f = real_stream(stream);
    if (f == NULL) {
        return 0;
    }
    return fread(p, size, n, f);
}

int acos_fclose(uintptr_t stream) {
    FILE *f = real_stream(stream);
    if (f == NULL) {
        return EOF;
    }
    acos_unregister_file((uintptr_t)f);
    return fclose(f);
}

int acos_fflush(uintptr_t stream) {
    if (stream == 0) {
        return fflush(NULL);
    }
    FILE *f = real_stream(stream);
    if (f == NULL) {
        return EOF;
    }
    return fflush(f);
}

uintptr_t acos_fopen(const char *path, const char *mode) {
    FILE *f = fopen(path, mode);
    if (f != NULL) {
        acos_register_file((uintptr_t)f);
    }
    return (uintptr_t)f;
}

uintptr_t acos_fdopen(int fd, const char *mode) {
    FILE *f = fdopen(fd, mode);
    if (f != NULL) {
        acos_register_file((uintptr_t)f);
    }
    return (uintptr_t)f;
}

uintptr_t acos_tmpfile(void) {
    FILE *f = tmpfile();
    if (f != NULL) {
        acos_register_file((uintptr_t)f);
    }
    return (uintptr_t)f;
}

uintptr_t acos_popen(const char *command, const char *mode) {
    FILE *f = popen(command, mode);
    if (f != NULL) {
        acos_register_file((uintptr_t)f);
    }
    return (uintptr_t)f;
}
