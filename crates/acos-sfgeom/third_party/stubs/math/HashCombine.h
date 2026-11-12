// Stub: libmath's HashCombine.h is absent from the checkout; std::hash is all ui::Rect/FloatRect need.
#pragma once
#include <cstddef>
#include <functional>
namespace android {
template <typename T>
inline void hashCombineSingle(size_t& seed, const T& value) {
    seed ^= std::hash<T>{}(value) + 0x9e3779b9 + (seed << 6) + (seed >> 2);
}
template <typename... Ts>
inline size_t hashCombine(const Ts&... values) {
    size_t seed = 0;
    (hashCombineSingle(seed, values), ...);
    return seed;
}
} // namespace android
