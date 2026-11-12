// Stub: libmath's vec2 is not in the AOSP sparse checkout; ui::Transform needs only this POD.
#pragma once
namespace android {
template <typename T>
struct tvec2 {
    T x = 0;
    T y = 0;
    constexpr tvec2() = default;
    constexpr tvec2(T _x, T _y) : x(_x), y(_y) {}
    T& operator[](size_t i) { return (&x)[i]; }
    const T& operator[](size_t i) const { return (&x)[i]; }
};
using vec2 = tvec2<float>;
} // namespace android
