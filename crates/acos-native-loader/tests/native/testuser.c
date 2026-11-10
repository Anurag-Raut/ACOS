/*
 * User library depending on libtestdep.so. Verifies dependency ordering and
 * ELF symbol interposition: this object is registered before its dependency, so
 * its definitions win in the global search order.
 */

#include <android/log.h>
#include <string.h>

#define TAG "acos-testuser"

extern const char *dep_greet(void);
extern int dep_calls_hook(void);
extern int dep_interpose_call(void);
extern int dep_only(void);
extern int interpose_target(void);

static int order[8];
static int order_n = 0;

void order_note(int which) {
    if (order_n < 8) order[order_n++] = which;
}

int user_order(int i) { return (i >= 0 && i < order_n) ? order[i] : -1; }

__attribute__((constructor)) static void user_ctor(void) {
    order_note(2); /* must run *after* the dependency's constructor */
    __android_log_print(ANDROID_LOG_INFO, TAG, "user constructor ran");
}

/* Interposes libtestdep's `interpose_target` (returns 1, not 2). */
int interpose_target(void) { return 1; }

/* Resolves libtestdep's undefined `hook`. */
int hook(void) { return 1; }

int user_run(void) {
    int m = 0;
    if (strcmp(dep_greet(), "dep-hello") != 0) m |= 1;
    if (dep_calls_hook() != 1) m |= 2;
    if (dep_interpose_call() != 1) m |= 4;
    if (dep_only() != 7) m |= 8;
    if (interpose_target() != 1) m |= 16;
    return m; /* 0 == all pass */
}
