# W86-A — software GLES2/EGL substrate (`crates/acos-gles`)

Implements the first milestone of **ADR-0023** (`results/render/W66A_SOFTWARE_GLES_ADR.md`):
a CPU-rasterised GLES2 subset behind a real EGL surface, wired so an
`ANativeWindow`-backed EGL surface publishes frames into the compositor's
`FrameStore` on `eglSwapBuffers`.

Nothing was committed. Work was done in a clean worktree of `HEAD` (27c725e)
with `CARGO_TARGET_DIR=.cache/target-w86a`, then applied to the working tree
after `git apply --check`.

## What landed

| file | role |
|---|---|
| `crates/acos-gles/src/lib.rs` | crate docs, `#![deny(unsafe_code)]` for everything except `ffi`, `install_loader_table()` |
| `crates/acos-gles/src/glsl.rs` | GLSL ES 1.00 **interpreter** (lexer → AST → evaluator). Structured accept/reject, no JIT, no guest memory execution |
| `crates/acos-gles/src/raster.rs` | RGBA8888 framebuffer + edge-function triangle rasteriser: viewport, scissor, perspective-correct varyings, depth test/mask, GLES2 blend factors, colour mask, cull |
| `crates/acos-gles/src/gl.rs` | GLES2 object model + state machine (buffers, textures, shaders/programs, FBO/RBO, uniforms, attributes, draws, clear, readpixels) |
| `crates/acos-gles/src/egl.rs` | display/config/context/surface objects; `WindowHost` seam; `eglSwapBuffers` |
| `crates/acos-gles/src/ffi.rs` | the `libEGL`/`libGLESv2` C entry points + `table()`/`lookup()` |
| `crates/acos-native-loader/src/bionic/gles.rs` | installed-provider slot; `bionic::resolve` prefers it over the EGL/GLES traps |
| `crates/acos-jni/src/gles_host.rs` | `ANativeWindow` → `substrate::submit_frame` host + install/uninstall |
| `crates/acos-jni/src/ndk_host.rs` | `install()`/`install_forced()` now install the substrate (native path only); `window_geometry`/`window_present`/`register_native_window` |
| `fixtures/jni/acos_gles_surface.c` | NDK fixture: clear + triangle through EGL, read-back, swap |
| `fixtures/jni/build.sh` | builds `crates/acos-jni/tests/data/libacosgles.so` |
| `crates/acos-runtime/tests/egl_swap_frame.rs` | swap → real `FrameStore` |
| `crates/acos-jni/tests/gles_fixture.rs` | confined child: load fixture with `require_confinement`, draw, publish |
| `crates/acos-gles/tests/egl_swap.rs` | EGL context + draw + swap pixel goldens; textured draw; unsupported call |

### Fail-closed / honesty rules

* The loader's EGL/GLES **traps remain the default**. The substrate table is
  installed only from `acos-jni`'s native load path (`load_named`), i.e. only
  when `--native` / `ACOS_NATIVE=1` allowed native code, which the CLI already
  gates behind confinement.
* An entry point outside the subset logs exactly one
  `UNSUPPORTED_GLES call=<name> reason=<...>` line and sets
  `GL_INVALID_OPERATION`; a shader outside the interpreted GLSL subset fails
  `glGetShaderiv(GL_COMPILE_STATUS)` with a positioned reason in the info log.
* `eglCreateWindowSurface` on an unknown `ANativeWindow` → `EGL_BAD_NATIVE_WINDOW`;
  `eglSwapBuffers` with no host → `EGL_BAD_SURFACE` + structured log. No black
  frame is ever reported as success.
* A frame is published only by a real swap (and only through the compositor
  sink), never by a draw call.

## Coverage vs. the two engine inventories

Import lists were taken from `results/native-loader/inventory/gaten.json`
(React Native corpus, 17 EGL + 31 GL imports) and
`results/tmp/flutter-native-inventory.json` (`libflutter.so`, 20 EGL + 106 GL
imports). Union = 25 EGL + 106 GL symbols; all of them are in `ffi::table()`,
so no guest import falls through to the loader's `LazyTrap` abort.

| area | implemented | notes |
|---|---|---|
| EGL | getDisplay, initialize, terminate, chooseConfig, getConfigs, getConfigAttrib, create/destroy Window+Pbuffer surface, create/destroy context, makeCurrent, swapBuffers, swapInterval, querySurface, queryString, getCurrent{Display,Context,Surface}, bindAPI, releaseThread, getProcAddress | `EGL_CONTEXT_CLIENT_VERSION` must be 2; share contexts accepted but share no objects (diagnostic) |
| GLES2 buffers | gen/delete/bindBuffer, bufferData, bufferSubData, getBufferParameteriv | client-side vertex arrays rejected with a structured log |
| GLES2 shaders | create/delete, ShaderSource, CompileShader, GetShaderiv/InfoLog/PrecisionFormat, create/delete/attach/link program, BindAttribLocation, GetAttrib/UniformLocation, GetProgramiv/InfoLog, UseProgram, ValidateProgram | GLSL subset: attributes/uniforms/varyings/const, scalars/vectors/matrices, constructors, swizzles, indexing, arithmetic/comparison/logical ops, `if/else`, `return`, `discard`, ~35 builtins incl. `texture2D` |
| GLES2 uniforms | 1/2/3/4 f+i, *fv/*iv, Matrix2/3/4fv | arrays bind element 0 and log `UNSUPPORTED_GLES` for the rest |
| GLES2 attributes/draw | VertexAttribPointer + enable/disable, VertexAttrib{1-4}f[v], DrawArrays, DrawElements (UBYTE/USHORT/UINT), DrawBuffers | TRIANGLES/STRIP/FAN; POINTS/LINES rejected structurally |
| GLES2 textures | gen/delete/bind, activeTexture (8 units), texParameter{i,f,fv,iv}, texImage2D/texSubImage2D (RGBA/RGB/LUMINANCE[_ALPHA]/ALPHA, UNSIGNED_BYTE), generateMipmap, isTexture, pixelStorei | everything is normalised to RGBA8888; mipmaps are level-0 only (diagnostic) |
| GLES2 FBO/RBO | gen/delete/bind framebuffer, framebufferTexture2D, framebufferRenderbuffer, checkFramebufferStatus, getFramebufferAttachmentParameteriv, gen/delete/bind renderbuffer, renderbufferStorage, getRenderbufferParameteriv, readPixels | single colour attachment; `readPixels` is RGBA/UNSIGNED_BYTE with GL's bottom-left origin |
| GLES2 state | viewport, scissor, enable/disable (BLEND/DEPTH_TEST/CULL_FACE/SCISSOR_TEST/DITHER), blendFunc[Separate], blendEquation[Separate] (FUNC_ADD), blendColor (diagnostic), depthFunc/Mask, clear{Color,Depthf,Stencil}, colorMask, cullFace, frontFace, lineWidth, stencil* (recorded only) | stencil state/`clearStencil` accepted with a diagnostic: there is no stencil buffer |
| sync | FenceSync, ClientWaitSync (ALREADY_SIGNALED), WaitSync, DeleteSync | a CPU rasteriser has already completed every accepted command |
| structured unsupported | CompressedTexImage2D/SubImage2D, CopyTexSubImage2D, EGLImageTargetTexture2DOES, eglCreate/DestroyImageKHR, TexStorage2D, GetStringi, gen/delete/getQuery* (occlusion queries) | each logs `UNSUPPORTED_GLES` + `GL_INVALID_OPERATION` |

## Verification

| check | result |
|---|---|
| `cargo test -p acos-gles` | 10 unit + 3 integration tests pass (rasteriser goldens: 36-pixel triangle, scissor+blend, depth, varyings; GLSL accept/reject/eval; EGL fail-closed; EGL swap pixel goldens incl. texture; unsupported-call event) |
| `cargo test -p acos-native-loader` | pass (incl. `tests/ndk.rs` unchanged: traps still fail closed without an install) |
| `cargo test -p acos-jni` | pass, incl. `gles_fixture` — NDK fixture clears + draws a triangle through EGL **inside the ADR-0022 confinement** (`confined: true`, `failures: 0`, frame 32×32 RGBA published) |
| `cargo test -p acos-runtime --test egl_swap_frame` | pass — `eglSwapBuffers` → `FrameStore`: `published_count` 0 before the swap, 1 after; frame hashes set; 528 blue / 496 red pixels in the 32×32 layer exactly matching the rasteriser |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | clean |
| `tools/diff-dexprogs.sh --corelib real` | 27 pass / 0 fail / 0 unsupported of 27 |

## Known limits (next milestones)

* No stencil buffer; `glStencil*` is recorded only.
* Mipmaps are not generated (`glGenerateMipmap` accepts, level 0 is sampled).
* `glCopyTexSubImage2D`, compressed textures, EGL images and occlusion queries
  are structured unsupported — none of the RN corpus nor the Flutter GL path
  needs them for the first frame.
* Client-side vertex arrays (no bound `ARRAY_BUFFER`) are refused.
* GLES1 is out of scope, as the ADR says.
* Performance is untested against a frame budget; the ADR explicitly defers
  that until correctness/frame-identity work is done.
