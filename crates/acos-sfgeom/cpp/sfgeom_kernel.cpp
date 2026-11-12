/*
 * Copyright 2026 The ACOS Authors
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

/*
 * AOSP SurfaceFlinger geometry kernel, android-14.0.0_r75.
 *
 * Each function body is copied from the AOSP file and line range named in
 * the comment above it. Only the state types are adapted:
 *   - RequestedLayerState            -> LayerGeometry   (POD fields only)
 *   - renderengine::ExternalTexture  -> {bufferWidth, bufferHeight, hasBuffer}
 *   - LayerSnapshot                  -> SnapshotGeometry (geometry subset)
 *   - LayerHierarchy                 -> flat (parent, z, id) records
 *
 * Deliberate omissions, all outside the B1a geometry scope:
 *   - transparentRegion (RequestedLayerState::reduce, LayerSnapshotBuilder.cpp:989-995):
 *     needs ui/Region.cpp, whose ui/RegionHelper.h is absent from the checkout and
 *     was not approved for fetch (docs/decisions/2026-11-12-compositor-spikes.md).
 *   - relative z, reparenting, layer stacks and mirroring (LayerHierarchy.cpp:64-130):
 *     B1a takes an absolute z per layer, as C1 does today (SPIKE-B.md §2).
 *   - incremental change filtering (Changes flags): B1a recomputes every layer, which
 *     is the forced-update path (LayerSnapshotBuilder.cpp:721-724, 863-865).
 */

#include "sfgeom_kernel.h"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <numeric>
#include <unordered_map>
#include <vector>

#include <ui/FloatRect.h>
#include <ui/Rect.h>
#include <ui/Size.h>
#include <ui/Transform.h>

namespace {

/* AOSP bodies live in android::surfaceflinger::frontend; Rect/FloatRect are android::,
 * while Transform/Size/Rotation are android::ui. */
namespace ui = android::ui;
using android::FloatRect;
using android::Rect;

constexpr int32_t kNoParent = -1;

/* Stand-in for RequestedLayerState's geometry fields (RequestedLayerState.h:62-130). */
struct LayerGeometry {
    int32_t id = 0;
    int32_t parentId = kNoParent;
    int32_t z = 0;

    ui::Transform requestedTransform;
    Rect destinationFrame = Rect::INVALID_RECT;
    Rect crop = Rect::INVALID_RECT;
    Rect bufferCrop = Rect::INVALID_RECT;

    bool hasBuffer = false;
    uint32_t bufferWidth = 0;
    uint32_t bufferHeight = 0;
    uint32_t bufferTransform = 0;
    bool transformToDisplayInverse = false;

    bool hidden = false;
    float alpha = 1.0f;

    /* renderengine::ExternalTexture::getBounds() for a {w,h}-only descriptor. */
    Rect getBufferBounds() const { return Rect(0, 0, bufferWidth, bufferHeight); }

    /* RequestedLayerState.cpp:347-360. Body verbatim; externalTexture fields become scalars. */
    ui::Size getUnrotatedBufferSize(uint32_t displayRotationFlags) const {
        uint32_t bufferWidth = this->bufferWidth;
        uint32_t bufferHeight = this->bufferHeight;
        // Undo any transformations on the buffer.
        if (bufferTransform & ui::Transform::ROT_90) {
            std::swap(bufferWidth, bufferHeight);
        }
        if (transformToDisplayInverse) {
            if (displayRotationFlags & ui::Transform::ROT_90) {
                std::swap(bufferWidth, bufferHeight);
            }
        }
        return {bufferWidth, bufferHeight};
    }

    /*
     * RequestedLayerState.cpp:362-398. Body verbatim, except:
     *   - `flags & layer_state_t::eIgnoreDestinationFrame` is not part of the B1a input
     *     (C1 does not decode that flag; documented in the B1b risk list);
     *   - `!externalTexture` becomes `!hasBuffer`.
     */
    ui::Transform getTransform(uint32_t displayRotationFlags) const {
        if (destinationFrame.isEmpty()) {
            // If destination frame is not set, use the requested transform set via
            // Transaction::setPosition and Transaction::setMatrix.
            return requestedTransform;
        }

        Rect destRect = destinationFrame;
        int32_t destW = destRect.width();
        int32_t destH = destRect.height();
        if (destRect.left < 0) {
            destRect.left = 0;
            destRect.right = destW;
        }
        if (destRect.top < 0) {
            destRect.top = 0;
            destRect.bottom = destH;
        }

        if (!hasBuffer) {
            ui::Transform transform;
            transform.set(static_cast<float>(destRect.left), static_cast<float>(destRect.top));
            return transform;
        }

        ui::Size bufferSize = getUnrotatedBufferSize(displayRotationFlags);

        float sx = static_cast<float>(destW) / static_cast<float>(bufferSize.width);
        float sy = static_cast<float>(destH) / static_cast<float>(bufferSize.height);
        ui::Transform transform;
        transform.set(sx, 0, 0, sy);
        transform.set(static_cast<float>(destRect.left), static_cast<float>(destRect.top));
        return transform;
    }

    /* RequestedLayerState.cpp:447-469. Body verbatim; externalTexture becomes hasBuffer. */
    Rect getBufferSize(uint32_t displayRotationFlags) const {
        // for buffer state layers we use the display frame size as the buffer size.
        if (!hasBuffer) {
            return Rect::INVALID_RECT;
        }

        uint32_t bufWidth = bufferWidth;
        uint32_t bufHeight = bufferHeight;

        // Undo any transformations on the buffer and return the result.
        if (bufferTransform & ui::Transform::ROT_90) {
            std::swap(bufWidth, bufHeight);
        }

        if (transformToDisplayInverse) {
            uint32_t invTransform = displayRotationFlags;
            if (invTransform & ui::Transform::ROT_90) {
                std::swap(bufWidth, bufHeight);
            }
        }

        return Rect(0, 0, static_cast<int32_t>(bufWidth), static_cast<int32_t>(bufHeight));
    }

    /* RequestedLayerState.cpp:471-479. Body verbatim. */
    Rect getCroppedBufferSize(const Rect& bufferSize) const {
        Rect size = bufferSize;
        if (!crop.isEmpty() && size.isValid()) {
            size.intersect(crop, &size);
        } else if (!crop.isEmpty()) {
            size = crop;
        }
        return size;
    }

    /* RequestedLayerState.cpp:481-494. Body verbatim; externalTexture becomes hasBuffer. */
    Rect getBufferCrop() const {
        // this is the crop rectangle that applies to the buffer
        // itself (as opposed to the window)
        if (!bufferCrop.isEmpty() && hasBuffer) {
            // if the buffer crop is defined and there's a valid buffer, intersect buffer size and
            // crop since the crop should never exceed the size of the buffer.
            Rect sizeAndCrop;
            getBufferBounds().intersect(bufferCrop, &sizeAndCrop);
            return sizeAndCrop;
        } else if (hasBuffer) {
            // otherwise we use the whole buffer
            return getBufferBounds();
        } else if (!bufferCrop.isEmpty()) {
            // if the buffer crop is defined, we use that
            return bufferCrop;
        } else {
            // if we don't have a buffer yet, we use an empty/invalid crop
            return Rect();
        }
    }
};

/* LayerSnapshot geometry fields (LayerSnapshot.h:52-70, LayerSnapshot.cpp:442-502). */
struct SnapshotGeometry {
    ui::Transform localTransform;
    ui::Transform geomLayerTransform;
    Rect bufferSize = Rect::INVALID_RECT;
    Rect croppedBufferSize = Rect::INVALID_RECT;
    Rect bufferCrop = Rect::INVALID_RECT;
    FloatRect geomLayerBounds;
    FloatRect transformedBounds;
    bool invalidTransform = false;
    bool isHiddenByPolicyFromParent = false;
    float alpha = 1.0f;
};

/* LayerSnapshot.cpp:261-264. Body verbatim. */
bool isTransformValid(const ui::Transform& t) {
    float transformDet = t.det();
    return transformDet != 0 && !std::isinf(transformDet) && !std::isnan(transformDet);
}

/*
 * LayerSnapshotBuilder.cpp:44-62 (getMaxDisplayBounds), inlined for a single display.
 * AOSP takes the max logical size of all displays and expands it 10x so the
 * synthetic root never clips a real layer.
 */
FloatRect getMaxDisplayBounds(const SfDisplay& display) {
    ui::Size maxSize = (display.width == 0 || display.height == 0)
            ? ui::Size{5000, 5000}
            : ui::Size{static_cast<int32_t>(display.width),
                       static_cast<int32_t>(display.height)};

    // Ignore display bounds for now since they will be computed later. Use a large Rect bound
    // to ensure it's bigger than an actual display will be.
    const float xMax = static_cast<float>(maxSize.getWidth()) * 10.f;
    const float yMax = static_cast<float>(maxSize.getHeight()) * 10.f;

    return {-xMax, -yMax, xMax, yMax};
}

/* LayerSnapshotBuilder.cpp:338-365 (getRootSnapshot), geometry subset. */
SnapshotGeometry getRootSnapshot(const SfDisplay& display) {
    SnapshotGeometry snapshot;
    snapshot.geomLayerTransform.reset();
    snapshot.geomLayerBounds = getMaxDisplayBounds(display);
    snapshot.alpha = 1.0f;
    return snapshot;
}

/*
 * LayerSnapshot.cpp:442-502 (LayerSnapshot::merge), the three buffer/transform inputs that
 * updateLayerBounds consumes. Body verbatim in formula order; change flags are not modelled.
 */
void mergeRequestedGeometry(SnapshotGeometry& snapshot, const LayerGeometry& requested,
                            uint32_t displayRotationFlags) {
    snapshot.bufferSize = requested.getBufferSize(displayRotationFlags);
    snapshot.croppedBufferSize = requested.getCroppedBufferSize(snapshot.bufferSize);
    snapshot.bufferCrop = requested.getBufferCrop();
    snapshot.localTransform = requested.getTransform(displayRotationFlags);
}

/*
 * LayerSnapshotBuilder.cpp:944-996 (updateLayerBounds). Body verbatim except:
 *   - the ALOGW invalid-transform diagnostic (lines 951-973) is dropped: no liblog;
 *   - `requested.externalTexture` becomes `requested.hasBuffer`;
 *   - the transparentRegion reductions (lines 989-995 + 997-1001) and potentialCursor
 *     frame are dropped together with ui/Region (see file header).
 *
 * The hidden-by-policy and alpha propagation that stock does before calling this function
 * are at LayerSnapshotBuilder.cpp:717-719 and 756-760 and live in sfgeom_compute().
 */
void updateLayerBounds(SnapshotGeometry& snapshot, const LayerGeometry& requested,
                       const SnapshotGeometry& parentSnapshot,
                       uint32_t primaryDisplayRotationFlags) {
    snapshot.geomLayerTransform = parentSnapshot.geomLayerTransform * snapshot.localTransform;
    const bool transformWasInvalid = snapshot.invalidTransform;
    snapshot.invalidTransform = !isTransformValid(snapshot.geomLayerTransform);
    if (snapshot.invalidTransform) {
        // ALOGW("Resetting transform ...") omitted: no liblog.
        snapshot.geomLayerTransform.reset();
    }
    (void)transformWasInvalid; // only drives mResortSnapshots in stock

    FloatRect parentBounds = parentSnapshot.geomLayerBounds;
    parentBounds = snapshot.localTransform.inverse().transform(parentBounds);
    snapshot.geomLayerBounds =
            (requested.hasBuffer) ? snapshot.bufferSize.toFloatRect() : parentBounds;
    if (!requested.crop.isEmpty()) {
        snapshot.geomLayerBounds =
                snapshot.geomLayerBounds.intersect(requested.crop.toFloatRect());
    }
    snapshot.geomLayerBounds = snapshot.geomLayerBounds.intersect(parentBounds);
    snapshot.transformedBounds = snapshot.geomLayerTransform.transform(snapshot.geomLayerBounds);
}

/* Assigns Rect from the C ABI {left, top, right, bottom} arrays. */
void setRect(Rect& rect, const int32_t (&values)[4]) {
    rect.left = values[0];
    rect.top = values[1];
    rect.right = values[2];
    rect.bottom = values[3];
}

/* LayerHierarchy.cpp:28-38 (layerZCompare), without the layerStack term. */
bool layerZLess(const SfLayerIn& a, const SfLayerIn& b) {
    if (a.z != b.z) {
        return a.z < b.z;
    }
    return a.id < b.id;
}

} // namespace

extern "C" void sfgeom_compute(const SfLayerIn* layers, size_t n, SfDisplay display,
                               SfSnapshotOut* out) {
    if (n == 0) {
        return;
    }

    std::vector<LayerGeometry> requested(n);
    for (size_t i = 0; i < n; i++) {
        const SfLayerIn& in = layers[i];
        LayerGeometry& r = requested[i];
        r.id = in.id;
        r.parentId = in.parent_id;
        r.z = in.z;
        // RequestedLayerState.cpp:311-317: position then matrix, same argument order.
        r.requestedTransform.reset();
        r.requestedTransform.set(in.x, in.y);
        r.requestedTransform.set(in.dsdx, in.dtdy, in.dtdx, in.dsdy);
        setRect(r.destinationFrame, in.destination_frame);
        setRect(r.crop, in.crop);
        setRect(r.bufferCrop, in.buffer_crop);
        r.bufferWidth = in.buffer_width;
        r.bufferHeight = in.buffer_height;
        r.hasBuffer = in.buffer_width != 0 && in.buffer_height != 0;
        r.bufferTransform = in.buffer_transform;
        r.transformToDisplayInverse = in.transform_to_display_inverse != 0;
        r.hidden = in.hidden != 0;
        r.alpha = in.alpha;
    }

    std::unordered_map<int32_t, size_t> idToIndex;
    idToIndex.reserve(n);
    for (size_t i = 0; i < n; i++) {
        idToIndex.emplace(requested[i].id, i);
    }

    std::vector<size_t> parentIndex(n, n); // n == "no parent / unreachable"
    for (size_t i = 0; i < n; i++) {
        if (requested[i].parentId == kNoParent) {
            continue;
        }
        auto it = idToIndex.find(requested[i].parentId);
        if (it != idToIndex.end() && it->second != i) {
            parentIndex[i] = it->second;
        }
    }

    // Parent-before-child order. A cycle (which stock cannot build: LayerHierarchy only
    // detects relative-z loops) is broken by detaching the lowest-index unprocessed
    // layer from its parent, so every child still sees a computed parent.
    std::vector<uint8_t> done(n, 0);
    std::vector<size_t> order;
    order.reserve(n);
    while (order.size() < n) {
        bool progress = false;
        for (size_t i = 0; i < n; i++) {
            if (!done[i] && (parentIndex[i] == n || done[parentIndex[i]])) {
                done[i] = 1;
                order.push_back(i);
                progress = true;
            }
        }
        if (!progress) {
            for (size_t i = 0; i < n; i++) {
                if (!done[i]) {
                    parentIndex[i] = n;
                    break;
                }
            }
        }
    }

    const SnapshotGeometry rootSnapshot = getRootSnapshot(display);
    std::vector<SnapshotGeometry> snapshots(n);
    for (size_t i : order) {
        const LayerGeometry& r = requested[i];
        const bool hasParent = parentIndex[i] != n;
        const SnapshotGeometry& parent =
                hasParent ? snapshots[parentIndex[i]] : rootSnapshot;

        SnapshotGeometry& s = snapshots[i];
        // LayerSnapshotBuilder.cpp:717-719. The last term is the B1a stand-in for stock's
        // "unreachable from a root" snapshots: a missing parent id hides the layer.
        s.isHiddenByPolicyFromParent = parent.isHiddenByPolicyFromParent ||
                parent.invalidTransform || r.hidden ||
                (!hasParent && r.parentId != kNoParent);
        // LayerSnapshotBuilder.cpp:756-760.
        s.alpha = parent.alpha * r.alpha;
        // LayerSnapshotBuilder.cpp:863-865 -> updateLayerBounds.
        mergeRequestedGeometry(s, r, display.rotation_flags);
        updateLayerBounds(s, r, parent, display.rotation_flags);
    }

    // Draw order: LayerHierarchy::traverseInZOrder depends on children sorted with
    // layerZCompare (LayerHierarchy.cpp:26-39). B1a has one flat list, so the same
    // ascending (z, id) order is applied globally.
    std::vector<size_t> drawOrder(n);
    std::iota(drawOrder.begin(), drawOrder.end(), 0);
    std::stable_sort(drawOrder.begin(), drawOrder.end(), [&](size_t a, size_t b) {
        return layerZLess(layers[a], layers[b]);
    });

    for (size_t i = 0; i < n; i++) {
        const SnapshotGeometry& s = snapshots[i];
        SfSnapshotOut& o = out[i];
        std::memset(&o, 0, sizeof(o));
        o.id = requested[i].id;
        for (size_t col = 0; col < 3; col++) {
            for (size_t row = 0; row < 3; row++) {
                o.geom[col * 3 + row] = s.geomLayerTransform[col][row];
            }
        }
        o.transformed_bounds[0] = s.transformedBounds.left;
        o.transformed_bounds[1] = s.transformedBounds.top;
        o.transformed_bounds[2] = s.transformedBounds.right;
        o.transformed_bounds[3] = s.transformedBounds.bottom;
        o.geom_layer_bounds[0] = s.geomLayerBounds.left;
        o.geom_layer_bounds[1] = s.geomLayerBounds.top;
        o.geom_layer_bounds[2] = s.geomLayerBounds.right;
        o.geom_layer_bounds[3] = s.geomLayerBounds.bottom;
        o.cropped_buffer_size[0] = s.croppedBufferSize.left;
        o.cropped_buffer_size[1] = s.croppedBufferSize.top;
        o.cropped_buffer_size[2] = s.croppedBufferSize.right;
        o.cropped_buffer_size[3] = s.croppedBufferSize.bottom;
        o.buffer_crop[0] = s.bufferCrop.left;
        o.buffer_crop[1] = s.bufferCrop.top;
        o.buffer_crop[2] = s.bufferCrop.right;
        o.buffer_crop[3] = s.bufferCrop.bottom;
        o.alpha = s.alpha;
        o.hidden_by_policy = s.isHiddenByPolicyFromParent ? 1 : 0;
        o.invalid_transform = s.invalidTransform ? 1 : 0;
        o.draw_order = -1;
    }
    for (size_t rank = 0; rank < n; rank++) {
        out[drawOrder[rank]].draw_order = static_cast<int32_t>(rank);
    }
}
