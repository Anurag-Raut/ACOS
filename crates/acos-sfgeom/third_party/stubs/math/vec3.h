// Stub: libmath's vec3 is not in the AOSP sparse checkout; ui::Transform::mMatrix needs 3 columns.
#pragma once
#include <cstddef>
namespace android {
template <typename T>
struct tvec3 {
    T x = 0;
    T y = 0;
    T z = 0;
    constexpr tvec3() = default;
    constexpr tvec3(T _x, T _y, T _z) : x(_x), y(_y), z(_z) {}
    T& operator[](size_t i) { return (&x)[i]; }
    const T& operator[](size_t i) const { return (&x)[i]; }
};
using vec3 = tvec3<float>;
using vec3f = tvec3<float>;
} // namespace android
