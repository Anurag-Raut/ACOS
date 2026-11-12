# Vendored AOSP source — provenance

These files are **verbatim copies** of Google's Android 14 source, compiled into
`acos-sfgeom` (B1, rung 4: "a subset of AOSP compiled in"). Do not edit them; adapt
in `cpp/sfgeom_kernel.cpp` instead.

- Source tree: `/home/Qui/Projects/acos/.cache/aosp/native` (sparse checkout)
- AOSP tag: **android-14.0.0_r75**
- Licence: Apache License 2.0 (per-file headers retained; AOSP `MODULE_LICENSE_APACHE2`)
- Decision: `docs/decisions/2026-11-12-compositor-spikes.md`, "Decision 2026-11-12 ~06:10 UTC: B1 approved"
- Spec: `results/art-runtime/SPIKE-B.md` §8

## Verbatim files

| File here | AOSP path (under `native/`) | sha256 |
|---|---|---|
| `libs/ui/Rect.cpp` | `libs/ui/Rect.cpp` | `a4a75fc5885672247f7910fec0b2f04f8d2c1077da101244578483c9ee0003d1` |
| `libs/ui/Transform.cpp` | `libs/ui/Transform.cpp` | `6e65b05150e733e44e492f7d59cb67b751e3df70811499a5bf3c71e537c8f669` |
| `libs/ui/include/ui/Rect.h` | `libs/ui/include/ui/Rect.h` | `d04244286738daf59a8629ea70a15e5926e33159ce83985d9320898ec92b6bdd` |
| `libs/ui/include/ui/Transform.h` | `libs/ui/include/ui/Transform.h` | `471d4dfa0e1d98ed4ca2e8d9f1177ae841b6ff8727f3595b28b9a11236b8a43e` |
| `libs/ui/include/ui/FloatRect.h` | `libs/ui/include/ui/FloatRect.h` | `87ffbbde4db41b191f10a0aab2e1882c3129ba439b384cd5b01daf0906fb17f8` |
| `libs/ui/include/ui/Size.h` | `libs/ui/include/ui/Size.h` | `b93aac66c61bf4e3d6872ac84decbebead03921da9c2c6b1a1c5b75488de4c22` |
| `libs/ui/include/ui/Point.h` | `libs/ui/include/ui/Point.h` | `5438f5663a08f1753ef45d4712fea3c491e537d4ba50ae0c87611d0aaa51b842` |
| `libs/ui/include/ui/Rotation.h` | `libs/ui/include/ui/Rotation.h` | `bb1e88d388d2664823a65477a8c1f90e0a953e091780dd090c8717e30fe98a7c` |
| `libs/ui/include/ui/Region.h` | `libs/ui/include/ui/Region.h` | `440fcca0c011a304e68473fb3f913450c7fe66e77cc64ccdaeb5ea2d5fc50cdd` |
| `libs/ui/include/ui/FatVector.h` | `libs/ui/include/ui/FatVector.h` | `daa9b482a0a57c253d541db03697dce113007d3906b1171022a1c4d01e907ca2` |

`Region.h` and `FatVector.h` are present because `Transform.cpp` includes
`<ui/Region.h>` for its `Transform transform(const Region&)` overload.

## Not vendored

- `libs/ui/Region.cpp` — **not vendored**: it needs `ui/RegionHelper.h`, which is absent from the
  sparse checkout and whose fetch was not approved. `Transform.cpp`'s Region overload is
  therefore never linked: `build.rs` compiles with `-ffunction-sections`/`-fdata-sections` and
  links with `--gc-sections`, which drops that section (same approach as SPIKE-B §4).
- `ui/RegionHelper.h` — absent upstream in this checkout; see above.
- libsurfaceflinger FrontEnd sources are **not** vendored as files: the five geometry bodies are
  copied into `cpp/sfgeom_kernel.cpp` with a `file:line` provenance comment above each one, as
  required by B1a (the spike did the same). See `cpp/sfgeom_kernel.cpp` for
  `RequestedLayerState.cpp`, `LayerSnapshotBuilder.cpp`, `LayerSnapshot.cpp` and
  `LayerHierarchy.cpp` line references.

## Stubs (`third_party/stubs/`)

None of these are AOSP code. Each file starts with a one-line justification; summary:

| Stub | Why |
|---|---|
| `math/{vec2,vec3,vec4,mat4,HashCombine}.h` | libmath is not in the checkout; `ui::Transform` needs only these small POD/template shapes. |
| `log/log.h` | liblog is not vendored; the kernel never logs (the invalid-transform `ALOGW` is dropped). |
| `android-base/stringprintf.h`, `android-base/macros.h` | libbase is not vendored; debug strings and `WARN_UNUSED` only. |
| `android/rect.h` | bionic's `ARect` POD (`ui::Rect`'s base) is absent from the checkout. |
| `system/graphics.h` | generated header; only the four `HAL_TRANSFORM_*` bits are needed by `Rect.cpp`. |
| `utils/{Log,Flattenable,TypeHelpers,Errors,String8}.h` | libutils is not vendored; only Parcel/log/status_t scaffolding referenced by libui headers. |
| `cutils/compiler.h` | only `CC_LIKELY` / `CC_UNLIKELY`. |

## Adaptation record

The vendored `.cpp` files are unmodified. In `cpp/sfgeom_kernel.cpp` the AOSP bodies are
copied as closely as the type substitution allows. Deviations, all recorded inline:

1. `layer_state_t` / `RequestedLayerState` → POD `LayerGeometry`; only geometry fields kept.
2. `renderengine::ExternalTexture` → `{hasBuffer, bufferWidth, bufferHeight}`; stock reads only
   `getWidth()`, `getHeight()` and `getBounds()` from it (SPIKE-B §3(c)).
3. Invalid-transform `ALOGW` (LayerSnapshotBuilder.cpp:951-973) dropped (no liblog).
4. `transparentRegion` reductions (LayerSnapshotBuilder.cpp:989-995) and the
   `potentialCursor` frame dropped with `ui/Region` (not approved).
5. No incremental `Changes` filtering: every layer is recomputed (forced-update path).
6. Relative z / reparent / layer stack / mirroring not modelled; see the file header of
   `cpp/sfgeom_kernel.cpp`.

One behavioural quirk is preserved verbatim rather than "fixed":
`RequestedLayerState.cpp:316` calls
`requestedTransform.set(matrix.dsdx, matrix.dtdy, matrix.dtdx, matrix.dsdy)`, which places
`dtdy` in `Transform::set`'s second slot and `dtdx` in its third. `cpp/sfgeom_kernel.cpp`
passes the same argument order.
