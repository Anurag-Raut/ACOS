# W92-A — software GLES2 substrate: engine-feature closure

Extends the W86-A substrate (`crates/acos-gles`) toward what the two engines
Flutter (Skia GL / Impeller GLES) and React Native (Fabric / Skia-less) actually
call. Built in a clean worktree of `HEAD` (5cc8b70) with
`CARGO_TARGET_DIR=.cache/target-w92a`, applied after `git apply --check`.

## Closed limits (priority order)

| # | W86-A limit | W92-A state | notes |
|---|---|---|---|
| 1 | no stencil buffer; `glStencil*` recorded only | **done** | 8-bit stencil in every `Framebuffer`; full `glStencilFunc[Separate]`, `glStencilMask[Separate]`, `glStencilOp[Separate]`, `glClearStencil`; front/back face state; stencil test runs before depth, ops on `sfail`/`dpfail`/`dppass`; `STENCIL_BUFFER_BIT` clears with the write mask; EGL config advertises `STENCIL_SIZE 8` |
| 2 | mipmaps not generated; sampler reads level 0 | **done** | `glGenerateMipmap` builds a box-filtered 2×2 chain; `glTexImage2D level>0` uploads levels; `LINEAR_MIPMAP_*`/`NEAREST_MIPMAP_*` sampling with a per-draw screen-space LOD estimate and trilinear blend; base-level upload invalidates the chain |
| 3 | `glCopyTexSubImage2D` structured unsupported | **done** | reads the current draw buffer (default or FBO) in GL bottom-left order into the bound texture level; `glCopyTexImage2D` added to the table (was absent) |
| 4 | client-side vertex arrays refused | **done** | `glVertexAttribPointer` with no `ARRAY_BUFFER` stores the guest pointer; at draw time the FFI layer copies the required byte range through the checked `bytes()` reader into `Gl::client_arrays`. No guest pointer is dereferenced inside `gl.rs` (still `#![deny(unsafe_code)]`) |
| 5 | `glBlitFramebuffer` | **not needed** | not imported by `libflutter.so` (checked `results/tmp/flutter-native-inventory.json`); left out |
| 6 | extension strings inaccurate | **done** | `GL_EXTENSIONS` now lists only implemented extensions (`GL_OES_rgb8_rgba8`, `GL_OES_depth24`, `GL_OES_element_index_uint`, `GL_OES_texture_npot`); `eglQueryString(EGL_EXTENSIONS)` no longer claims `EGL_KHR_surfaceless_context` / `EGL_ANDROID_native_fence_sync`, which this substrate does not implement |
| 7 | EGL config values | **done** | RGBA8888 + depth24 + stencil8 config; `eglGetConfigAttrib`/`eglQuerySurface`/`eglChooseConfig` honour `EGL_STENCIL_SIZE` |
| 8 | GLSL subset | **done / partial** | bounded `for` loops (iteration cap 4096), `++`/`--` prefix+postfix, `dFdx`/`dFdy`/`fwidth` → structured compile rejection, scalar-broadcast vs componentwise constructors fixed (`vec4(a,b,c,d)` was collapsing to `(a,a,a,a)`). Arrays and struct uniforms remain structured rejections |
| 9 | performance untested | **done** | `benches/frame.rs`: 1080×2400, 130 triangles (~2× overdraw) → **47 ms/frame** release (target < 100 ms); scanline-incremental edge functions and allocation-free varying interpolation |

## Verification

| check | result |
|---|---|
| `cargo test -p acos-gles` | 13 unit + 5 integration tests (stencil mask/invert, bounded for-loop, client arrays, copyTex+mipmap, existing goldens) |
| `cargo test -p acos-jni --test gles_fixture` | pass under ADR-0022 confinement; extended NDK fixture now also exercises client arrays, stencil test, `glCopyTexImage2D` + `glGenerateMipmap` |
| `cargo bench -p acos-gles` | 47 ms/frame (1080×2400, release) |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |
| `tools/diff-dexprogs.sh --corelib real` | 27 pass / 0 fail / 0 unsupported of 27 |

## Remaining limits (honest)

* GLSL arrays and `struct` uniforms are still structured rejections.
* `GL_KHR_debug`, compressed textures, EGL images and occlusion queries remain
  structured unsupported (none are on the first-frame Flutter/RN GL path).
* Client-side attribute copies use the FFI module's checked reader
  (non-null + 64 MiB ceiling + C ABI range contract); there is no per-region
  guest memory map to validate against in this loader.
* Mip LOD uses a per-draw affine gradient estimate rather than per-fragment
  derivatives; correct for the flat/axis-aligned quads the engines draw, and
  base-level LOD 0 for a 1:1 texture.
