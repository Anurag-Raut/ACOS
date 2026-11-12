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
 * C ABI of the ACOS SurfaceFlinger geometry kernel.
 *
 * The geometry is Google's Android 14 `ui::Transform` / `ui::Rect` algebra
 * compiled in (see third_party/aosp/PROVENANCE.md). Only the surrounding
 * state types are adapted: a POD layer record replaces `layer_state_t`, and
 * `{buffer_width, buffer_height}` replaces
 * `renderengine::ExternalTexture` (stock geometry only ever reads width,
 * height and bounds from it; see SPIKE-B.md §3(c)).
 *
 * Rect encoding: {left, top, right, bottom}. An empty rect is {0,0,-1,-1}
 * (ui::Rect::INVALID_RECT, Rect.cpp:20). `destination_frame` empty selects
 * the requested position/matrix path (RequestedLayerState.cpp:363).
 *
 * geom[9] is ui::Transform's internal mat33 in column-major order:
 * geom[col * 3 + row]. geom[0] = dsdx, geom[1] = dtdx, geom[3] = dtdy,
 * geom[4] = dsdy, geom[6] = tx, geom[7] = ty (Transform.cpp:103-108, 229-236).
 * Note the dtdx/dtdy placement comes from RequestedLayerState.cpp:316's argument
 * order; see PROVENANCE.md "Adaptation record".
 */

#ifndef ACOS_SFGEOM_KERNEL_H
#define ACOS_SFGEOM_KERNEL_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SfDisplay {
    uint32_t width;
    uint32_t height;
    /* ui::Transform::RotationFlags (ROT_0 = 0, HAL_TRANSFORM_* bits). */
    uint32_t rotation_flags;
} SfDisplay;

typedef struct SfLayerIn {
    int32_t id;
    /* -1 means "no parent"; the layer then hangs off the synthetic root. */
    int32_t parent_id;
    int32_t z;

    /* Transaction::setPosition / setMatrix, same meaning as layer_state_t. */
    float x;
    float y;
    float dsdx;
    float dtdx;
    float dtdy;
    float dsdy;

    int32_t destination_frame[4];
    int32_t crop[4];
    int32_t buffer_crop[4];

    /* 0 x 0 (or either dimension 0) means "no buffer". */
    uint32_t buffer_width;
    uint32_t buffer_height;
    uint32_t buffer_transform;
    uint8_t transform_to_display_inverse;

    float alpha;
    uint8_t hidden;
} SfLayerIn;

typedef struct SfSnapshotOut {
    int32_t id;
    float geom[9];
    float transformed_bounds[4];
    float geom_layer_bounds[4];
    int32_t cropped_buffer_size[4];
    int32_t buffer_crop[4];
    float alpha;
    uint8_t hidden_by_policy;
    uint8_t invalid_transform;
    /* Position of this layer in ascending (z, id) draw order. */
    int32_t draw_order;
} SfSnapshotOut;

/*
 * Computes one snapshot per input layer, written to out[i] for layers[i].
 * `out` must point to at least n elements. Layers may be passed in any order;
 * parent ids must name a layer in the same call (or -1).
 */
void sfgeom_compute(const SfLayerIn* layers, size_t n, SfDisplay display, SfSnapshotOut* out);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* ACOS_SFGEOM_KERNEL_H */
