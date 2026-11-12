// Stub: libutils is not vendored; ui::Point/ui::Rect only inherit these tag bases for Parcel use.
#pragma once
#include <cstddef>
#include <sys/types.h>
#include <utils/Errors.h>
namespace android {
template <typename T>
class LightFlattenable {
public:
    bool isFixedSize() const { return false; }
    size_t getFlattenedSize() const { return 0; }
    status_t flatten(void*, size_t) const { return INVALID_OPERATION; }
    status_t unflatten(void const*, size_t) { return INVALID_OPERATION; }
protected:
    ~LightFlattenable() = default;
};
template <typename T>
class LightFlattenablePod {
public:
    bool isFixedSize() const { return true; }
    size_t getFlattenedSize() const { return sizeof(T); }
    status_t flatten(void* buffer, size_t size) const {
        if (size < sizeof(T)) return BAD_VALUE;
        *static_cast<T*>(buffer) = *static_cast<const T*>(this);
        return NO_ERROR;
    }
    status_t unflatten(void const* buffer, size_t size) {
        if (size < sizeof(T)) return BAD_VALUE;
        *static_cast<T*>(this) = *static_cast<const T*>(buffer);
        return NO_ERROR;
    }
protected:
    ~LightFlattenablePod() = default;
};
} // namespace android
