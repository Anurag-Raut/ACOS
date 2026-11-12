// Stub: liblog is not vendored; the geometry kernel never logs, only libui's headers mention it.
#pragma once
#include <cstdio>
#include <cstdlib>
#define ALOGD(...) ((void)0)
#define ALOGE(...) ((void)0)
#define ALOGW(...) ((void)0)
#define ALOGV(...) ((void)0)
#define ALOGI(...) ((void)0)
#define LOG_ALWAYS_FATAL(...) std::abort()
#define LOG_ALWAYS_FATAL_IF(cond, ...) \
    do { if (cond) std::abort(); } while (0)
