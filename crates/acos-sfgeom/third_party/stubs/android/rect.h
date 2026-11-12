// Stub: bionic's native_window/rect.h is absent from the checkout; ARect is a 4-int POD (system/core).
#pragma once
#include <cstdint>
struct ARect {
    typedef int32_t value_type;
    int32_t left;
    int32_t top;
    int32_t right;
    int32_t bottom;
};
