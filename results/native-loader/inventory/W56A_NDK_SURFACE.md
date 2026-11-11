# W56-A — NDK platform surface (`libandroid` / `libjnigraphics` / EGL)

Re-ran the inventory on the same Gaten corpus after adding the NDK surface:

```
acos native-inventory ~/Projects/gaten/app/build/outputs/apk/debug/app-x86_64-debug.apk \
    --abi x86_64 --json results/native-loader/inventory/gaten-after.json
```

| metric | before (W55) | after (W56-A) |
|---|---:|---:|
| distinct external imports | 1458 | 1458 |
| provided | 1359 | **1370** |
| partial | 8 | **56** |
| **missing** | **91** | **32** |

+11 provided are the real NDK entry points the corpus imports:
`AAssetManager_fromJava`, `AAssetManager_open`, `AAsset_read`, `AAsset_getBuffer`,
`AAsset_getLength`, `AAsset_close`, `AndroidBitmap_getInfo`,
`AndroidBitmap_lockPixels`, `AndroidBitmap_unlockPixels` (plus the `libjnigraphics`
pair on the second/third consumer library).

+48 partial are EGL/GLES (and `feclearexcept`/`fetestexcept` are now forwarded):
those symbols **resolve**, but only to a function that reports
`UNSUPPORTED_NATIVE_SYMBOL` and returns `EGL_NO_DISPLAY` / `EGL_NOT_INITIALIZED` /
`GL_INVALID_OPERATION`. Software GL is a later task; classifying them as
"provided" would overstate the loader.

The remaining 32 missing are unchanged and expected from sibling objects or
deliberate refusals: `__gcov_*`, `MallocExtension_*`, `absl::leak_check_internal`,
`tflite::UseGemmlowpOnX86`, `_ZTH*`, `uprv_getICUData_*`, `x_cgo_init`,
`stats_census_*` and the bionic `jmp_buf`/`__pthread_cleanup_*` entries.

## Where the implementations live

* `crates/acos-native-loader/src/bionic/ndk.rs` — `NdkHost` seam + registry. A
  call with no host (or behaviour a host cannot honour) reports structurally and
  returns the API's documented error; it is never a success stub.
* `bionic/ndk/ndk_asset.rs` — `AAssetManager_*`, `AAsset_*`, `AAssetDir_*`.
* `bionic/ndk/ndk_bitmap.rs` — `AndroidBitmap_*` (RGBA_8888 storage is the
  bitmap's own buffer; other configs report `BAD_PARAMETER`).
* `bionic/ndk/ndk_window.rs` — `ANativeWindow_*` (software buffer + post).
* `bionic/ndk/ndk_looper.rs` — real `ALooper` over `epoll` + `eventfd`.
* `bionic/ndk/ndk_misc.rs` — `AChoreographer` (host vsync), `AConfiguration`
  (real system config), `ASensor*`/`AInputQueue` as structured unsupported.
* `bionic/ndk/ndk_gl.rs` — EGL/GLES reported traps.
* `crates/acos-native-loader/tests/ndk.rs` + `fixtures/jni/acos_ndk_surface.c` —
  NDK-compiled fixture driven through the loader with a host backed by a real
  APK zip, a real `acos_render::Bitmap` pixel store, a real frame store and the
  system `Configuration` (test-only native execution, ADR-0018).
