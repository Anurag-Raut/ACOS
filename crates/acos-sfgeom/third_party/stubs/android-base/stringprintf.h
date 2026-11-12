// Stub: libbase is not vendored; Rect.cpp/Transform.cpp only need StringPrintf for debug strings.
#pragma once
#include <cstdarg>
#include <cstdio>
#include <string>
namespace android::base {
inline std::string StringPrintf(const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    va_list ap2;
    va_copy(ap2, ap);
    int len = std::vsnprintf(nullptr, 0, fmt, ap);
    va_end(ap);
    std::string out;
    if (len > 0) {
        out.resize(static_cast<size_t>(len));
        std::vsnprintf(out.data(), out.size() + 1, fmt, ap2);
    }
    va_end(ap2);
    return out;
}
inline void StringAppendF(std::string* out, const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    va_list ap2;
    va_copy(ap2, ap);
    int len = std::vsnprintf(nullptr, 0, fmt, ap);
    va_end(ap);
    if (len > 0) {
        size_t old = out->size();
        out->resize(old + static_cast<size_t>(len));
        std::vsnprintf(out->data() + old, static_cast<size_t>(len) + 1, fmt, ap2);
    }
    va_end(ap2);
}
} // namespace android::base
