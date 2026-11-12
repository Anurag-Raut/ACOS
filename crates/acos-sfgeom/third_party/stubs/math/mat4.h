// Stub: libmath's mat4 is not in the AOSP sparse checkout; only Transform::asMatrix4 needs it.
#pragma once
#include <cstddef>
#include <math/vec4.h>
namespace android {
struct mat4 {
    enum Init { NO_INIT };
    vec4 v[4];
    mat4() = default;
    explicit mat4(Init) {}
    vec4& operator[](size_t i) { return v[i]; }
    const vec4& operator[](size_t i) const { return v[i]; }
};
} // namespace android
