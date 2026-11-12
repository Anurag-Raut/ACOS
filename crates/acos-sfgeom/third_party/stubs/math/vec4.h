// Stub: libmath's vec4 is not in the AOSP sparse checkout; only mat4 rows need it.
#pragma once
#include <cstddef>
namespace android {
template <typename T>
struct tvec4 {
    T x = 0;
    T y = 0;
    T z = 0;
    T w = 0;
    T& operator[](size_t i) { return (&x)[i]; }
    const T& operator[](size_t i) const { return (&x)[i]; }
};
using vec4 = tvec4<float>;
} // namespace android
