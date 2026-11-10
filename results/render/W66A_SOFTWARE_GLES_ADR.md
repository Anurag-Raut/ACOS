# ADR-0023 (proposed, W66-A) — Software GL/EGL substrate for Flutter and React Native modules

**Status:** Proposed. No implementation in W66-A (the task scopes this to a note
plus a minimal plan). Tracked as a follow-up workstream.

**Context.** `crates/acos-render` is a software 2D rasteriser (tiny-skia) driven
by the framework's `android.graphics.*` natives. That is sufficient for the
classic View toolkit and its `HardwareRenderer`/DisplayList path. It is **not**
sufficient for two families of real apps:

* **React Native** (old architecture) renders through
  `ReactRootView` → `UIManagerModule` → view managers, which bottom out in the
  standard `android.view`/`android.graphics` path — and that already works. The
  *new* architecture and several popular modules (`react-native-svg`,
  `@shopify/react-native-skia`, `react-native-video`, `expo-gl`) call
  `GLES20`/`EGL14` directly from JNI.
* **Flutter** does not use the View toolkit at all. `FlutterView` owns a
  `SurfaceView`/`TextureView` and hands a native window to the engine, which
  renders the entire UI through **GLES2/GLES3** (and on some devices
  Vulkan/Impeller). Without a GLES implementation the engine draws nothing.

`AndroidRuntime`/ART always presents a GLES-capable EGL surface to apps even
when the app is pure View code, so a correct Android-compatible substrate also
has to answer the `EGL10`/`EGL14`/`GLES20` JNI surface for *any* app that probes
it.

## Decision

Implement a **CPU-rasterised GLES2 subset behind a real EGL surface**, in two
layers, and only as far as the two engines above need:

1. **`libEGL`/`libGLESv2` shim JNI** — the `android.opengl.*` Java classes
   (`EGL14`, `EGLDisplay`, `GLES20`, `GLUtils`) route their natives to a new
   `gfx_egl.rs` / `gfx_gles.rs` native module backed by an `EglContext` /
   `GlesContext` in a new `acos-gles` crate. Unknown entry points return the
   structured `UNSUPPORTED_JAVA_API` error, never a silent no-op.
2. **A framebuffer → `acos-render` bridge.** The GL context owns an RGBA8
   colour buffer (`eglSwapBuffers` publishes it as a `CompletedFrame` exactly
   like `frame_handoff` does today). `glReadPixels`, `glTexImage2D`,
   `glTexSubImage2D` and the FBO render-target path are the only places the GL
   and 2D worlds meet; no shared shader compiler is required.

The rasteriser is a **scan-line triangle rasteriser with the fixed-function
pipeline semantics GLES2 guarantees**: attribute/varying interpolation,
perspective-correct texture coordinates, the GLES2 default vertex/fragment
source, integer/float uniform arrays, and the full GLES2 blend/depth/stencil
state. Programmable shaders are *interpreted* from the GLSL ES 1.00 source via a
small AST interpreter (no JIT), which is enough for the handful of engines that
upload their own shaders and keeps the security model (no executable guest
memory) intact.

## Minimal subset the first milestone must cover

Enumerated from what the two engines actually call (RN module inventory in
`results/rn/`, Flutter engine's `impeller`/legacy GL backend):

* EGL: `eglGetDisplay/Initialize/ChooseConfig/CreateWindowSurface/
  CreateContext/MakeCurrent/SwapBuffers/QuerySurface/GetConfigAttrib`
  (`EGL14.eglCreateWindowSurface`, `eglCreatePbufferSurface`).
* GLES2 buffers: `gen/delete/bindBuffer`, `bufferData`, `bufferSubData`.
* GLES2 programs: `createShader/ShaderSource/compileShader/getShaderiv`,
  `createProgram/attachShader/linkProgram/getProgramiv/getUniformLocation/
  getAttribLocation`, `useProgram`, `uniform1f/2f/3f/4f/1i/Matrix4fv`,
  `vertexAttribPointer` + `enable/disableVertexAttribArray`.
* GLES2 draw/state: `viewport`, `clear/clearColor`, `enable/disable`
  (`BLEND`, `DEPTH_TEST`, `CULL_FACE`, `SCISSOR_TEST`), `blendFunc`,
  `blendFuncSeparate`, `depthFunc`, `depthMask`, `colorMask`, `scissor`,
  `drawArrays`, `drawElements` (`UNSIGNED_SHORT`/`UNSIGNED_INT`),
  `activeTexture`, `bindTexture`, `texParameteri`, `pixelStorei`.
* GLES2 textures/FBO: `gen/delete/bindTexture`, `texImage2D` (RGBA/RGB/LUMINANCE/
  ALPHA, `UNSIGNED_BYTE`), `texSubImage2D`, `generateMipmap`, `gen/bind/
  framebuffer`, `framebufferTexture2D`, `checkFramebufferStatus`,
  `deleteFramebuffer`, `readPixels`, `glGetError`.
* GLES1 is **out of scope** initially; RN modules that use it will get a
  structured unsupported and can be prioritised from the inventory.

## Non-goals

* No Vulkan, no shader JIT, no GPU passthrough, no `libhwui` substitution.
* Not a general-purpose GL implementation: anything not exercised by the RN
  inventory or the Flutter engine returns structured `UNSUPPORTED`.
* No attempt to make GLES fast enough for 60 fps early; correctness and frame
  identity vs a reference first, then perf.

## Consequences

* Adds a large, security-sensitive interpreter (GLSL) and a new guest-visible
  surface. It must live inside the existing `acos-sandbox` confinement and the
  `guarded.sh` memory/time bounds.
* The frame contract (`frame_seq`, `cause_id`, `content_hash`) is preserved: a
  GL swap produces the same `CompletedFrame` as the View path, so the existing
  WMS compositor and `frame-diff` keep working unchanged.
* Until it lands, any app that requires GL reports a structured failure naming
  the missing entry point; it does **not** render a black frame and call it a
  success. That is the current, honest state.
