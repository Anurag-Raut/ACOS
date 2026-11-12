// Stub: libbase is not vendored; ui/Region.h only needs WARN_UNUSED.
#pragma once
#define WARN_UNUSED __attribute__((warn_unused_result))
#define DISALLOW_COPY_AND_ASSIGN(T) \
    T(const T&) = delete;           \
    void operator=(const T&) = delete
