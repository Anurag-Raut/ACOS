# 6 — Technical references, research baseline and compatibility constraints

## Authoritative specifications and research inputs

These are **reading references, not instructions to import entire repositories**.

1. AOSP platform architecture: https://source.android.com/docs/core/architecture
2. Android platform architecture / ART description: https://developer.android.com/guide/platform
3. AOSP DEX file format: https://source.android.com/docs/core/runtime/dex-format
4. AOSP DEX instruction formats: https://source.android.com/docs/core/runtime/instruction-formats
5. AOSP ART runtime and compilation configuration: https://source.android.com/docs/core/runtime/configure
6. AOSP Zygote process: https://source.android.com/docs/core/runtime/zygote
7. Android NDK JNI tips and ABI requirements: https://developer.android.com/ndk/guides/jni-tips
8. Android NDK JNI reference: https://docs.oracle.com/javase/8/docs/technotes/guides/jni/spec/jniTOC.html
9. Android compatibility program and meaning of compatibility: https://source.android.com/docs/compatibility/overview
10. Android Compatibility Definition Document (CDD): https://source.android.com/docs/compatibility/cdd
11. CTS overview: https://source.android.com/docs/compatibility/cts
12. Rust support in AOSP: https://source.android.com/docs/setup/build/rust/building-rust-modules/overview
13. Cranelift Rust compiler backend: https://cranelift.dev/
14. Android Translation Layer (ATL): https://gitlab.com/android_translation_layer/android_translation_layer
15. ATL standalone ART research: https://gitlab.com/android_translation_layer/art_standalone
16. Existing user's ACVM project: `/home/Qui/Work/android-container` (local, read only).

Verify links/license status as part of the research and use a pinned version when benchmarking or importing anything. The existence of ATL's standalone ART demonstrates Linux execution is possible, but the user explicitly prefers our own Rust runtime rather than embedding ART from that project. ATL is a competitor/reference for design and functionality, not the new codebase.

## Required compatibility dimensions

Each app must be classified by:
- APK format (base APK, split APK collection, manifest/resource/DEX variants);
- target and minimum API levels; installed Android framework API level;
- CPU ABI and native library requirements (`arm64-v8a`, `armeabi-v7a`, `x86_64`, etc.);
- Java/DEX/native ratio and packaging/toolchain (Java/Kotlin/Compose/Flutter/RN/Unity);
- activity task/launch mode, content providers, broadcast receivers, background services;
- WebView, rendering, codecs, OpenGL/Vulkan, font and locale dependencies;
- permissions, Play Services, GMS, Google sign-in, push, integrity attestation, DRM, OEM/system APIs;
- independent storage, package identity, system service and native process needs;
- whether output requires device fidelity, stable determinism or mere preview.

Every compatibility test records `target-api`, `runtime-api`, `abi`, `apk-sha256`, `app-version`, `backend`, `fidelity`, `native-coverage`, `assertion-result`, `failure-reason`, `resource-metrics` and `fallback-used`.

## Licensing and restrictions

- AOSP is not a single-license blob; check each component. Linux kernel/drivers and other GPL programs have separate obligations; most platform code uses other permissive/copyleft licenses as applicable.
- ATL and related compatibility projects may have copyleft conditions. Perform legal and dependency checks before copying code.
- Do not bundle proprietary Google Mobile Services or DRM/attestation implementations without appropriate rights. Never claim Play Integrity support by faking a signed device attestation.
- Native APK libraries may include copyrighted/proprietary content; don't redistribute someone else's APKs or `.so` in the public benchmark corpus without rights.
- Security: user-owned debug builds and authorized third-party APKs are allowed test inputs; isolate untrusted code and keep controls private.

## Compatibility is defined by behavior, not 'it launches'

A successful app must:
1. Install/load valid code and assets without unauthorized transformations.
2. Execute meaningful business and UI behavior of the intended screen/task.
3. Render correct contents and receive input with proper lifecycle/permissions.
4. Produce correct Java/native exception behavior and failure events.
5. Match selected real Android reference behavior within stated API/device configuration.
6. Remain correct under repeat, concurrency, resets and independent state when claimed.

Compatibility coverage should be a quantitative matrix, not an unsupported headline that 'all APKs run'. As work proceeds, prioritize full fidelity for the most common SDK/NDK APIs and app frameworks, then expand to the difficult cases systematically.

## Minimal 'why' behind runtime complexity

Android apps are not portable Linux ELF programs with a launcher. `classes.dex` runs under an Android-specific managed runtime; app-facing Java classes communicate with Android framework services; the app uses Android resources, permissions, Binder IPC and lifecycle callbacks, and may call native NDK libraries via a strict JNI ABI. Removing `system_server` is possible only by implementing or replacing the app-visible behavior it supplies. A Rust runtime can implement those semantics, but it is still substantial work. This is why ACOS must be built with a compatibility ledger, differential test harness and rigorous stop/escalation rules.
