# Flutter hello fixture — build notes (local)

Generated 2026-11-11. This directory is local evidence for the ACOS Flutter fixture;
it is not committed (large binaries / caches are gitignored).

## Versions

| Component | Value |
| --- | --- |
| Flutter framework | 3.47.9 (stable, release 2026-10-29) |
| Engine hash | `89c39edfdfae2949f6806e4a315c0c08522238b7` |
| Dart SDK / AOT compiler | 3.13.7 (stable, linux_x64) |
| `gen_snapshot` | 3.13.7 (from engine `linux-x64.zip`) |
| Android build tools | 35.0.0 (`aapt2`, `d8`, `zipalign`, `apksigner`) |
| Android platform jar | android-34 |
| Host JDK | `javac` from PATH (used only for the tiny `MainActivity`) |
| AOT mode | release / AOT (`--snapshot_kind=app-aot-elf --strip`) |
| Embedding | prebuilt `flutter.jar` from engine `artifacts.zip` |

## Artifacts

| APK | ABI | Size (bytes) | SHA-256 |
| --- | --- | ---: | --- |
| `fixtures/build/apps/flutter-hello.apk` | x86_64 | 45,568,784 | `bb40a9c2586c04d23faab0716c0b2bdd3c19c1096534117dd998ddb375ded528` |
| `fixtures/build/apps/flutter-hello-arm64.apk` | arm64-v8a | 45,245,206 | `8d364875006a70640cb2937f96883500ed68f1356858e7a18929689ccd8555e4` |

Package id: `dev.acos.fixtures.flutterhello`. Launcher: `.MainActivity` (`io.flutter.embedding.android.FlutterActivity`).

## Native libraries in the x86_64 APK

| Entry | Uncompressed bytes | Notes |
| --- | ---: | --- |
| `classes.dex` | 8,046,760 | app + `flutter.jar` + AndroidX + Kotlin stdlib + ReLinker |
| `lib/x86_64/libapp.so` | 721,800 | AOT snapshot; exports `_kDartSnapshotText/Data/BuildId`; **0 imports** |
| `lib/x86_64/libflutter.so` | 166,186,240 | engine; `needed` = libc, libdl, libm, libandroid, libEGL, libGLESv2, liblog, libjnigraphics |
| `assets/flutter_assets/AssetManifest.bin` | 2 | empty StandardMessageCodec map |
| `assets/flutter_assets/FontManifest.json` | 2 | `[]` |
| `assets/flutter_assets/NOTICES` | 0 | empty |

The arm64 APK has the same `classes.dex` and assets; `libapp.so` is also 721,800 bytes and `libflutter.so` is 165,324,816 bytes.

## `acos native-inventory` (x86_64 APK)

Command: `target/release/acos native-inventory fixtures/build/apps/flutter-hello.apk`.
Raw JSON + table: `results/flutter/native-inventory.json`.

Totals: **2 libraries, 485 distinct imports: 300 provided, 49 partial, 136 missing (73 defined in corpus)**.

`libapp.so` contributes 0 imports. All 485 imports are from `libflutter.so`.

### Missing imports vs the loader corpus (136)

**libc/other** (48): `__cmsg_nxthdr`, `__openat_2`, `__poll_chk`, `__pread_chk`, `__read_chk`, `__readlink_chk`, `close_range`, `environ`, `epoll_create1`, `epoll_ctl`, `epoll_wait`, `execvp`, `feof`, `fma`, `fork`, `freeaddrinfo`, `freeifaddrs`, `ftruncate64`, `gai_strerror`, `getaddrinfo`, `getifaddrs`, `getnameinfo`, `if_nametoindex`, `inet_aton`, `inet_ntop`, `inet_pton`, `inotify_add_watch`, `inotify_init1`, `inotify_rm_watch`, `llround`, `llroundf`, `longjmp`, `malloc_usable_size`, `mkdirat`, `posix_madvise`, `pthread_condattr_setclock`, `readlinkat`, `renameat`, `rewinddir`, `scalbnf`, `sendfile64`, `setbuf`, `setjmp`, `sigprocmask`, `symlinkat`, `timerfd_create`, `timerfd_settime`, `wait`

**EGL** (3): `eglCreateImageKHR`, `eglDestroyImageKHR`, `glEGLImageTargetTexture2DOES`

**GLESv2** (82): `glActiveTexture`, `glAttachShader`, `glBindAttribLocation`, `glBindBuffer`, `glBindRenderbuffer`, `glBlendColor`, `glBlendEquation`, `glBlendFunc`, `glBufferData`, `glBufferSubData`, `glCheckFramebufferStatus`, `glClearStencil`, `glColorMask`, `glCompileShader`, `glCompressedTexImage2D`, `glCompressedTexSubImage2D`, `glCopyTexSubImage2D`, `glCreateProgram`, `glCreateShader`, `glCullFace`, `glDeleteBuffers`, `glDeleteProgram`, `glDeleteRenderbuffers`, `glDeleteShader`, `glDepthMask`, `glDisableVertexAttribArray`, `glDrawArrays`, `glDrawElements`, `glEnableVertexAttribArray`, `glFramebufferRenderbuffer`, `glFrontFace`, `glGenBuffers`, `glGenRenderbuffers`, `glGenerateMipmap`, `glGetBufferParameteriv`, `glGetProgramInfoLog`, `glGetProgramiv`, `glGetRenderbufferParameteriv`, `glGetShaderInfoLog`, `glGetShaderPrecisionFormat`, `glGetShaderiv`, `glGetUniformLocation`, `glIsTexture`, `glLineWidth`, `glLinkProgram`, `glRenderbufferStorage`, `glScissor`, `glShaderSource`, `glStencilFunc`, `glStencilFuncSeparate`, `glStencilMask`, `glStencilMaskSeparate`, `glStencilOp`, `glStencilOpSeparate`, `glTexParameterf`, `glTexParameterfv`, `glTexParameteriv`, `glTexSubImage2D`, `glUniform1f`, `glUniform1fv`, `glUniform1i`, `glUniform1iv`, `glUniform2f`, `glUniform2fv`, `glUniform2i`, `glUniform2iv`, `glUniform3f`, `glUniform3fv`, `glUniform3i`, `glUniform3iv`, `glUniform4f`, `glUniform4fv`, `glUniform4i`, `glUniform4iv`, `glUniformMatrix2fv`, `glUniformMatrix3fv`, `glUniformMatrix4fv`, `glVertexAttrib1f`, `glVertexAttrib2fv`, `glVertexAttrib3fv`, `glVertexAttrib4fv`, `glVertexAttribPointer`

**OpenSSL** (3): `OPENSSL_memory_alloc`, `OPENSSL_memory_free`, `OPENSSL_memory_get_size`

### Partial imports (49)

The loader provides these but the inventory marks them partial (usually versioned libc/EGL symbols): `__cxa_thread_atexit_impl`, `__system_property_find`, `dladdr`, `dlclose`, `dlerror`, `dlopen`, `dlsym`, `eglChooseConfig`, `eglCreateContext`, `eglCreatePbufferSurface`, `eglCreateWindowSurface`, `eglDestroyContext`, `eglDestroySurface`, `eglGetCurrentContext`, `eglGetCurrentDisplay`, `eglGetCurrentSurface`, `eglGetDisplay`, `eglGetError`, `eglGetProcAddress`, `eglInitialize`, `eglMakeCurrent`, `eglQueryString`, `eglQuerySurface`, `eglSwapBuffers`, `eglTerminate`, `glBindFramebuffer`, `glBindTexture`, `glClear`, `glClearColor`, `glDeleteFramebuffers`, `glDeleteTextures`, `glDisable`, `glEnable`, `glFinish`, `glFlush`, `glFramebufferTexture2D`, `glGenFramebuffers`, `glGenTextures`, `glGetError`, `glGetFramebufferAttachmentParameteriv`, `glGetIntegerv`, `glGetString`, `glPixelStorei`, `glReadPixels`, `glTexImage2D`, `glTexParameteri`, `glUseProgram`, `glViewport`, `pthread_getattr_np`

## What an ACOS runtime must supply

1. **Native loader / linker:** load `libflutter.so` and the AOT `libapp.so` from the APK (`lib/x86_64/` or `lib/arm64-v8a/`), resolve the 8 `DT_NEEDED` libraries, and provide the 136 missing imports above (plus the 49 partials).
2. **Android/JNI surface:** `libandroid.so` (`AAssetManager_*`, `ANativeWindow_*`, `AConfiguration_*`), `libjnigraphics.so` (`AndroidBitmap_*`), `liblog.so` (`__android_log_*`), and the JNI environment for `FlutterActivity`/`FlutterView`/`FlutterJNI`.
3. **EGL/GLESv2 (or fallback):** the engine was built with Impeller and selects Vulkan when available; on the phone it logged `Using the Impeller rendering backend (Vulkan)`. The EGL/GLESv2 imports above are still referenced. If the ACOS runtime cannot supply a working EGL/GLESv2 surface, use the software-rendering fallback: `FlutterShellArgs --enable-software-rendering` (documented in `MainActivity.java`; override `getFlutterShellArgs()` to return `new FlutterShellArgs(new String[]{"--enable-software-rendering"})`).
4. **Assets:** mount the APK's `assets/flutter_assets/` (the engine reads `AssetManifest.bin`, `FontManifest.json`, `NOTICES`; `kernel_blob.bin` is only needed in debug/JIT, and this fixture is AOT so it is absent).
5. **Dart AOT snapshot:** `libapp.so` is self-contained (0 imports, 3 exports). The engine loads it as the app snapshot; no JIT/kernel is needed.

## Phone reference (attached device ZA222WGJPP)

- Installed `dev.acos.fixtures.flutterhello` from `fixtures/build/apps/flutter-hello-arm64.apk` (arm64 phone).
- The app launched, the engine loaded `libflutter.so` and `libapp.so`, and the Dart `main` rendered.
- Reference screenshot: `fixtures/reference-device/flutter-hello/screenshot_initial.png` (1080x2400). It shows the `hello acos` title and the `counter: 0` button in the top-left of the window.
- The raw `dart:ui` app lays out in logical coordinates; on this 432x960 dp window the scene occupies the top-left 432x960 px region of the 1080x2400 surface. The remainder is the engine surface background.
- **uiautomator dump: not captured.** `uiautomator dump` was killed by the device (`RC:137`/`SIGKILL`) on repeated attempts; the device was also low on free memory (~376 MB free of 3.4 GB) and another accessibility client had already registered (`UiAutomationService ... already registered!`). No phone settings were changed.

## Build / reproduce

```bash
tools/build-flutter-hello.sh x86_64   # -> fixtures/build/apps/flutter-hello.apk
tools/build-flutter-hello.sh arm64    # -> fixtures/build/apps/flutter-hello-arm64.apk
```

The script downloads nothing at build time: it uses `.cache/flutter-1/engine` (x86_64) and `.cache/flutter-1/engine-arm64` (arm64), `.cache/flutter-1/androidx.jar`, and `.cache/flutter-1/relinker-maven/`. It runs under `tools/guarded.sh --mem 6G --time 1800` and caps the JVM heap at `-Xmx2g`.

The AndroidX closure is generated by `tools/flutter_androidx.py` from the read-only Gradle module cache, plus Kotlin stdlib, KotlinX coroutines, and ReLinker. The ReLinker AAR (27 KB) was fetched once from Maven Central into `.cache/flutter-1/relinker-maven/`; without it `FlutterLoader` crashes with `NoClassDefFoundError: Lcom/getkeepsafe/relinker/ReLinker$Logger;`. Kotlin stdlib is likewise required by `androidx.lifecycle.LifecycleRegistry` (`kotlin.jvm.internal.Intrinsics`).

Cache footprint: `.cache/flutter-1` ≈ 216 MB (engine artifacts + generated `androidx.jar` + ReLinker). The downloaded engine archives are kept so the fixture can be rebuilt without re-downloading.
