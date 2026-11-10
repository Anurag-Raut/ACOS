/*
 * ACOS native-loader test library (C).
 *
 * Exercises: malloc/free/memcpy/strlen/snprintf, errno, __android_log_print,
 * pthread mutex + pthread_create/join, `__thread` ELF TLS, a global
 * constructor, a statically-initialised pointer (needs R_X86_64_RELATIVE), a
 * qsort callback, getenv/clock_gettime/time.
 */

#include <android/log.h>
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define TAG "acos-testc"

/* Statically initialised pointer -> relative relocation in .data.rel.ro. */
static const char *static_str = "static-initialized";
static int ctor_ran = 0;

__attribute__((constructor)) static void tc_ctor(void) {
    ctor_ran = 1;
    __android_log_print(ANDROID_LOG_INFO, TAG, "constructor ran");
}

__attribute__((destructor)) static void tc_dtor(void) {
    __android_log_print(ANDROID_LOG_INFO, TAG, "destructor ran");
}

int tc_ctor_ran(void) { return ctor_ran; }
const char *tc_static_str(void) { return static_str; }

int tc_mem(void) {
    char *p = malloc(64);
    if (!p) return 0;
    memcpy(p, "hello", 6);
    p[5] = 0;
    int r = (int)strlen(p);
    free(p);
    char buf[32];
    snprintf(buf, sizeof(buf), "%d", r);
    return r == 5 && strcmp(buf, "5") == 0;
}

int tc_errno(void) {
    /* A failing syscall reliably sets the same errno in bionic and glibc. */
    char b = 0;
    errno = 0;
    ssize_t r = read(-1, &b, 1);
    return r == -1 && errno == EBADF;
}

int tc_log(void) {
    __android_log_print(ANDROID_LOG_INFO, TAG, "log from guest %d", 42);
    return 1;
}

static int cmp_int(const void *a, const void *b) {
    int x = *(const int *)a, y = *(const int *)b;
    return (x > y) - (x < y);
}

int tc_qsort(void) {
    int a[5] = {5, 3, 1, 4, 2};
    qsort(a, 5, sizeof(int), cmp_int);
    return a[0] == 1 && a[4] == 5;
}

int tc_env_time(void) {
    const char *e = getenv("ACOS_TEST_ENV");
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    time_t t = time(NULL);
    return (e && strcmp(e, "present") == 0) && ts.tv_sec > 0 && t > 0;
}

/* --- threads + TLS ---------------------------------------------------------- */

static __thread int tls_counter;

struct targ {
    int *out;
};

static void *tfn(void *arg) {
    struct targ *a = (struct targ *)arg;
    tls_counter = 123;
    for (int i = 0; i < 5; i++) tls_counter++;
    a->out[0] = tls_counter;
    return (void *)(uintptr_t)0x1234;
}

static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;

int tc_threads(void) {
    int out = 0;
    struct targ a = {&out};
    pthread_mutex_lock(&mtx);
    pthread_mutex_unlock(&mtx);
    pthread_t th;
    if (pthread_create(&th, NULL, tfn, &a) != 0) return 0;
    void *ret = NULL;
    if (pthread_join(th, &ret) != 0) return 0;
    return out == 128 && (uintptr_t)ret == 0x1234;
}

void tc_tls_set(int v) { tls_counter = v; }
int tc_tls_get(void) { return tls_counter; }

int tc_run_all(void) {
    int mask = 0;
    if (!tc_mem()) mask |= 1;
    if (!tc_errno()) mask |= 2;
    if (!tc_log()) mask |= 4;
    if (!tc_qsort()) mask |= 8;
    if (!tc_env_time()) mask |= 16;
    if (!tc_threads()) mask |= 32;
    if (!tc_ctor_ran()) mask |= 64;
    if (strcmp(tc_static_str(), "static-initialized") != 0) mask |= 128;
    return mask;
}
