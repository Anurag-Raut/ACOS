/* Dependency library for loader dependency-order + interposition tests. */

#include <android/log.h>

#define TAG "acos-testdep"

extern void order_note(int which);

__attribute__((constructor)) static void dep_ctor(void) {
    order_note(1); /* must run *before* the user library's constructor */
    __android_log_print(ANDROID_LOG_INFO, TAG, "dep constructor ran");
}

/* Defined here; but `libtestuser.so` loads first and interposes it. */
int interpose_target(void) { return 2; }

/* Undefined here; resolved from libtestuser.so when the whole graph is bound. */
extern int hook(void);

int dep_calls_hook(void) { return hook(); }

/* Taking the address forces a dynamic R_X86_64_64 relocation to the global
 * symbol `interpose_target`; the loader must bind it to the first object in
 * the global search order (libtestuser.so), i.e. return 1. */
int (*dep_interpose_ptr)(void) = interpose_target;

int dep_interpose_call(void) { return dep_interpose_ptr(); }

int dep_only(void) { return 7; }

const char *dep_greet(void) { return "dep-hello"; }
