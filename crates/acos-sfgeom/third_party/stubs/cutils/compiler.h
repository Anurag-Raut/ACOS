// Stub: libcutils is not vendored; Transform.cpp uses CC_LIKELY/CC_UNLIKELY only.
#pragma once
#define CC_LIKELY(exp) (__builtin_expect(!!(exp), 1))
#define CC_UNLIKELY(exp) (__builtin_expect(!!(exp), 0))
#define CC_UNUSED __attribute__((unused))
