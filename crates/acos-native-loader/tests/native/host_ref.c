/*
 * Host-glibc reference library for the load-time comparison.
 *
 * It has a similar shape to the guest test libraries (a handful of relative
 * relocations and a constructor) but is linked against the host glibc and is
 * loaded with the host `dlopen`. The comparison is explicitly NOT
 * like-for-like: glibc's dynamic linker does far more than this loader.
 */

#include <stddef.h>

static const char *ptrs[] = {"0", "1", "2", "3"};

static int ctor_ran = 0;

__attribute__((constructor)) static void host_ref_ctor(void) { ctor_ran = 1; }

int host_ref_ctor_ran(void) { return ctor_ran; }

int host_ref_count(void) { return (int)(sizeof(ptrs) / sizeof(ptrs[0])); }
