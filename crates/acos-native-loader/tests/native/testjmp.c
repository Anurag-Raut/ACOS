/*
 * bionic setjmp/longjmp acceptance library (W181-A).
 *
 * Built with the NDK clang (bionic headers, bionic jmp_buf sizes) and executed
 * through the ACOS native loader, so every call below goes through the
 * loader's shims for `setjmp`, `_setjmp`, `sigsetjmp`, `longjmp`, `_longjmp`
 * and `siglongjmp`.
 *
 * Each entry returns 0 on success; a non-zero value names the failed check.
 * `tj_run_all` returns a bitmask of failed entries (0 == all pass).
 *
 * The buffer stays inside one C frame: `setjmp` is called at the shim entry
 * and `longjmp` resumes the guest caller of the shim, i.e. the call site really
 * returns twice. Signal-mask checks pin bionic's documented split:
 * `setjmp` == `sigsetjmp(env, 1)`, `_setjmp` saves registers only.
 */

#include <setjmp.h>
#include <signal.h>
#include <string.h>

static jmp_buf jb;
static sigjmp_buf sjb;

/* Longjmp from a deeper frame: the resumed frame must still be live. */
static int tj_jump_from_callee(int value) {
    longjmp(jb, value);
    return -1;
}

int tj_setjmp_roundtrip(void) {
    volatile int stage = 0;
    int rc = setjmp(jb);
    if (rc == 0) {
        stage = 1;
        return tj_jump_from_callee(42);
    }
    /* rc == 42, and stage survived the unwind. */
    return (rc == 42 && stage == 1) ? 0 : 1;
}

int tj_underscore_roundtrip(void) {
    volatile int stage = 0;
    int rc = _setjmp(jb);
    if (rc == 0) {
        stage = 1;
        _longjmp(jb, 7);
    }
    return (rc == 7 && stage == 1) ? 0 : 2;
}

/* longjmp(env, 0) must resume with 1, not 0. */
int tj_zero_value(void) {
    volatile int calls = 0;
    int rc = _setjmp(jb);
    if (calls++ == 0) {
        _longjmp(jb, 0);
    }
    return (rc == 1 && calls == 2) ? 0 : 3;
}

int tj_sigsetjmp_nomask(void) {
    volatile int stage = 0;
    int rc = sigsetjmp(sjb, 0);
    if (rc == 0) {
        stage = 1;
        siglongjmp(sjb, 3);
    }
    return (rc == 3 && stage == 1) ? 0 : 4;
}

/* bionic: setjmp saves the signal mask, longjmp restores it. */
int tj_setjmp_saves_mask(void) {
    sigset_t set, got;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR2);
    sigprocmask(SIG_BLOCK, &set, NULL);

    int rc = setjmp(jb);
    if (rc == 0) {
        /* Unblock between the save and the jump: the restore must undo it. */
        sigprocmask(SIG_UNBLOCK, &set, NULL);
        longjmp(jb, 4);
    }
    memset(&got, 0, sizeof(got));
    sigprocmask(SIG_SETMASK, NULL, &got);
    int blocked = sigismember(&got, SIGUSR2);
    sigprocmask(SIG_UNBLOCK, &set, NULL);
    return (rc == 4 && blocked == 1) ? 0 : 5;
}

/* bionic: _setjmp does not save the mask, so the unblock sticks. */
int tj_underscore_leaves_mask(void) {
    sigset_t set, got;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR2);
    sigprocmask(SIG_BLOCK, &set, NULL);

    int rc = _setjmp(jb);
    if (rc == 0) {
        sigprocmask(SIG_UNBLOCK, &set, NULL);
        _longjmp(jb, 6);
    }
    memset(&got, 0, sizeof(got));
    sigprocmask(SIG_SETMASK, NULL, &got);
    int blocked = sigismember(&got, SIGUSR2);
    sigprocmask(SIG_UNBLOCK, &set, NULL);
    return (rc == 6 && blocked == 0) ? 0 : 6;
}

/* sigsetjmp(env, 1) + siglongjmp restores the mask. */
int tj_sigsetjmp_saves_mask(void) {
    sigset_t set, got;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR1);
    sigprocmask(SIG_BLOCK, &set, NULL);

    int rc = sigsetjmp(sjb, 1);
    if (rc == 0) {
        sigprocmask(SIG_UNBLOCK, &set, NULL);
        siglongjmp(sjb, 5);
    }
    memset(&got, 0, sizeof(got));
    sigprocmask(SIG_SETMASK, NULL, &got);
    int blocked = sigismember(&got, SIGUSR1);
    sigprocmask(SIG_UNBLOCK, &set, NULL);
    return (rc == 5 && blocked == 1) ? 0 : 7;
}

int tj_run_all(void) {
    int failed = 0;
    failed |= tj_setjmp_roundtrip() ? 1 : 0;
    failed |= tj_underscore_roundtrip() ? 2 : 0;
    failed |= tj_zero_value() ? 4 : 0;
    failed |= tj_sigsetjmp_nomask() ? 8 : 0;
    failed |= tj_setjmp_saves_mask() ? 16 : 0;
    failed |= tj_underscore_leaves_mask() ? 32 : 0;
    failed |= tj_sigsetjmp_saves_mask() ? 64 : 0;
    return failed;
}
