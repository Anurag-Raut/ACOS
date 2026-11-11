# W169-A — what a *real* Flutter app needs, and the engine surface the raw `dart:ui` probe can already exercise

**Date:** 2026-11-11 (wave W169-A). **Base:** clean worktree of committed `HEAD`
(`7755818`, W156-A) at `.cache/wt-w169a`. Built with
`CARGO_TARGET_DIR=.cache/target-w169a`, `CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_BUILD_JOBS=2`, `tools/guarded.sh`. **Not committed.** The wave's edits are
confined to the fixture (`flutter_probe_main.dart`, `FlutterProbe.java`, the
rebuilt `flutter-probe.dex`) and the harness (`flutter_hello.rs`); no
production `flutter.rs`/`jni.rs`/bionic shim needed a change.

The current fixture is a hand-built engine probe: `dart:ui` only, no
`package:flutter`. A real Flutter app is a **framework** app (`material.dart`,
`WidgetsBinding`, gestures, `Semantics`, services). This wave answers two
questions: (1) what does a real app need that is not on this box, and (2) using
only what is on this box, how much of the *engine surface* a real app rides on
does the probe already exercise?

---

## 1. Recon — what exists locally (no download)

Commands (read-only, no project outside `~/Projects/acos` modified):

```
which flutter dart
find ~/ -maxdepth 4 -name flutter -type d
find ~/ -maxdepth 6 \( -name libapp.so -o -name app.so -o -name libflutter.so \) -type f
find / -maxdepth 5 -name flutter -type f
ls ~/.pub-cache/hosted/pub.dev
ls -R .cache/flutter-1
```

| artifact | state |
|---|---|
| `flutter` tool / full Flutter SDK | **absent**. No `flutter` executable anywhere on the box; `~/.config/flutter` holds only `tool_state`; `~/.flutter` is an `enabled:false` analytics file |
| Dart SDK | **present**: `/opt/dart-sdk` `3.13.7` (exactly Flutter 3.47.9's pinned `dart_sdk_version`), gives `dartaotruntime` + `frontend_server_aot.dart.snapshot` |
| `~/.pub-cache` | third-party pub packages only; **no `flutter` SDK package**, no `package:flutter` / `material.dart` anywhere on disk |
| `package:flutter` (framework Dart sources) | **absent**. The engine's `flutter_patched_sdk_product` is only `platform_strong.dill` (dart:ui + core), and the shipped `sky_engine` tree is `lib/ui` + core — neither contains Widgets/Material/Rendering/Gestures/Services |
| `gen_snapshot` (AOT compiler) | **present** in `.cache/flutter-1/engine/linux64/gen_snapshot` (x86_64) and `.cache/flutter-1/engine-arm64/linux64/gen_snapshot` (arm64), Flutter 3.47.9 / engine `89c39edf…` |
| engine `libflutter.so` | **present**: `.cache/flutter-1/lib-x86_64/libflutter.so` (166 MB, `.symtab`+DWARF) |
| AOT snapshot | **present**: `.cache/flutter-1/lib-x86_64/libapp.so` (shipped fixture) and `libapp_probe.so` (instrumented, rebuilt this wave) |
| `flutter.jar` (Java embedding) | **present**: `FlutterJNI`, `AccessibilityBridge`, `DartMessenger`, `PlatformPlugin`, `TextInputPlugin`, `LocalizationPlugin`, `NavigationChannel`, `SettingsChannel` all in the jar |
| fonts | engine reads the extracted boot tree `.cache/boot/android-34/fonts` (`Roboto-Regular.ttf`); the fixture's `FontManifest.json` is `[]`, so no asset font is registered |
| other Flutter APKs / projects | none usable. The moto-g35 backup dir named `app.revanced.manager.flutter` contains **no `libflutter.so`** (not a Flutter APK); the `~/Projects/gaten/*.apk` are React Native/Hermes. No `libapp.so`/`app.so` exists in any other project |
| `dart:ui` 3.47 API check | `PlatformDispatcher.scheduleFrameCallback` **does not exist** (it is framework `SchedulerBinding`); the accessibility callback is `onSemanticsActionEvent(SemanticsActionEvent)`, **not** `onSemanticsAction` |

### What a real framework app would additionally need

1. **The Flutter SDK's `package:flutter`** (Material/Widgets/Rendering/Gestures/
   Services). Not present; it is the single hard prerequisite. The SDK tarball
   (`flutter_linux_3.47.9-stable.tar.xz`) is large; **downloading is not
   authorised** and was not attempted. Everything below was done with what is
   local.
2. **The Android embedding plugin set**: `PlatformPlugin` (SystemChrome),
   `TextInputPlugin` (IME), `NavigationChannel` (back button),
   `LocalizationPlugin`, `SettingsChannel`, `PlatformViewsController`. The
   classes are in `flutter.jar`, but the raw `FlutterJNI`-only driver does not
   instantiate `FlutterEngine`/`FlutterActivity`, so they are unused. A real app
   registers them against a `Context`/`Activity`.
3. **Framework-side services**: `Ticker`/`SchedulerBinding`,
   `WidgetsBinding`'s `popRoute` handler, `Semantics` compilation,
   `SystemChannels` codecs. These are Dart, not engine.

So: ACOS has the **engine and its AOT toolchain** locally, and can therefore
probe the engine surface a real app uses; it does **not** have the framework
that a real Material app is written against.

---

## 2. Extended fixture — engine surface exercised (Step 2)

The probe (`crates/acos-jni/tests/data/flutter_probe_main.dart`) and the Java
driver (`FlutterProbe.java`) were extended, rebuilt with
`tools/flutter-hello-snapshot.sh` + `tools/flutter-hello-probe.sh`, and driven
by `crates/acos-jni/tests/flutter_hello.rs`. New machine-readable stages:
`text-pixels` and `surface`.

| surface | status | evidence (this run) |
|---|---|---|
| **multiple animation frames** (`onBeginFrame` + reschedule; raw-`dart:ui` substitute for `Ticker`) | **ok** | `ACOS_DART_BEGINFRAME n=1..12` with monotonic `at=`, engine deltas ~5–15 ms while animating; `ACOS_DART_ANIM step=0..5`; `w130a-frame-2..7.png` carry the moving element |
| **platform channel round trip (Dart→embedder→Dart)** | **ok** | Dart `sendPlatformMessage('acos/probe')` → Java `handleMessageFromDart` → `invokePlatformMessageResponseCallback` → Dart callback logs `ACOS_DART_CHANNEL_REPLY data=pong-from-java` |
| **`flutter/platform` (SystemChrome) transport** | **ok (transport) / blocked (handler)** | `surface` stage: `platform_messages=3`, `last_platform_channel="flutter/platform"`, `last_platform_payload="SystemChrome.setApplicationSwitcherDescription"`, Java logs `phase=systemChromeSeen`. The engine delivers the real channel to the embedder; no `PlatformPlugin` is registered to apply it |
| **embedder→Dart message** | **ok** | Java `dispatchPlatformMessage('acos/dart_in')` → Dart `channelBuffers` listener logs `ACOS_DART_CHANNEL_IN channel=acos/dart_in data=push-from-java`; Dart acks → Java `handlePlatformMessageResponse` (`platform_responses=3`) |
| **keyboard key data (`flutter/keydata`)** | **ok** | Java `dispatchPlatformMessage(flutter/keydata, <6×u64 KeyData>)` → `PlatformDispatcher.onKeyData` logs `ACOS_DART_KEY type=KeyEventType.down physical=0 logical=0 character=null synthesized=false`; Dart returns handled, Java sees the response |
| **back button (`flutter/navigation` popRoute)** | **transport ok / framework blocked** | Java `dispatchPlatformMessage('flutter/navigation')` → Dart listener logs `ACOS_DART_NAVIGATION bytes=1`. There is no engine-level popRoute: a real app's `WidgetsBinding` must answer it, and no framework is present. Honest: the channel is proven, the behaviour is not |
| **text rendering via `dart:ui` Paragraph** | **ok** | `ACOS_DART_TEXT text=Hello ACOS frame=0..5 width=300.0 height=23.0 longest=102.55859375`; `text-pixels` stage `max=374` amber glyph pixels, `per_frame=[0,0,374,374,374,374,374,374,0,0,0,0]` (rest frames 0/1 and 8..11 have none) |
| **fonts** | **ok (system) / blocked (asset)** | glyphs come from the boot-tree Roboto (`SkFontMgr` reads `/system/etc/fonts.xml` → `.cache/boot/android-34`); the fixture bundles no font (`FontManifest.json=[]`). Asset fonts would need the framework's `FontLoader`; the engine's asset manager does work for the manifests (`AAssetManager_open NativeAssetsManifest.json` returns the documented `RES_ASSET_NOT_FOUND`, not a fake success) |
| **`flutter/textinput` (IME)** | **blocked** | no `TextInputPlugin`/IME is wired in the `FlutterJNI`-only driver; there is no `FlutterJNI.dispatchKeyEvent` at all (that is `TextInputPlugin`/`KeyboardChannel` in the embedding). Only the generic transport is proven |
| **semantics tap action (W156-A's stated boundary)** | **ok — the boundary is closed** | after two fixture fixes (below), `FlutterJNI.dispatchSemanticsAction` reaches the Dart handler: `ACOS_DART_SEMANTICS_ACTION id=1 action=SemanticsAction.tap view=0` → `ACOS_DART_COUNTER counter=1 via=semantics` (product e2e) |

### Two principled fixture fixes the wave exposed

1. **The committed probe did not compile against the pinned `dart:ui`.**
   `HEAD`'s `flutter_probe_main.dart` used `PlatformDispatcher.onSemanticsAction`
   and `_onSemanticsAction(int, SemanticsAction, ByteData?)`. That API does not
   exist in Flutter 3.47 (`onSemanticsActionEvent(SemanticsActionEvent)` does).
   The snapshot in `.cache/flutter-1/lib-x86_64/libapp_probe.so` predated the
   W156-A edit, so the W156-A test never rebuilt it and the broken code was
   never compiled. Rebuilding the snapshot from `HEAD` fails with
   `The setter 'onSemanticsAction' isn't defined for the type 'PlatformDispatcher'`.
   Fixed to `onSemanticsActionEvent`; with the fix the semantics action path
   that W156-A recorded as unreachable now fires (`via=semantics`).
2. **A Dart→embedder reply is not a new platform message.** The first cut
   replied with `FlutterJNI.dispatchPlatformMessage(...)`, which sends a brand
   new embedder→Dart message (dropped, since the probe registers no listener on
   its own request channel). The correct API — the one `DartMessenger$Reply`
   uses — is `invokePlatformMessageResponseCallback(replyId, buffer, position)`.
   `DartMessenger` bytecode confirms it.

---

## 3. Acceptance

| check | result |
|---|---|
| `cargo test -p acos-jni --test flutter_hello` | **2 passed / 0 failed** (15.9 s): animation (`ACOS_DART_BEGINFRAME n=12`, `ANIM step=0..5`), channel round trip, key data, navigation, `text-pixels max=374`, `surface platform_messages=3 responses=3`; masked parity unchanged (`global 0.9987 / 0.9968`, tap delta 160 px @ `[134,231,146,248]`) |
| `cargo test -p acos-mcp --test flutter_session` (`ACOS_REQUIRE_FIXTURES=1`) | **1 passed / 0 failed** (3.6 s): launch `wall_ms=2033`; `tap --text Increment` now resolves **and** fires the semantics action (`via=semantics`); post-tap frame seq 1→2 |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | **clean** (0 errors / 0 warnings) |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** |

Reproduce (clean worktree of `HEAD`):

```bash
export CARGO_TARGET_DIR=$PWD/.cache/target-w169a CARGO_PROFILE_DEV_DEBUG=0
tools/flutter-hello-probe.sh        # rebuild flutter-probe.dex after Java edits
tools/flutter-hello-snapshot.sh     # rebuild libapp_probe.so after Dart edits
tools/guarded.sh --mem 12G --time 900 -- cargo test -p acos-jni --test flutter_hello -- --nocapture
ACOS_REQUIRE_FIXTURES=1 tools/guarded.sh --mem 12G --time 900 -- \
  cargo test -p acos-mcp --test flutter_session -- --nocapture
```

## 4. Honest boundary (what this does **not** prove)

* It is still a `dart:ui` probe, **not a framework app**. "Animation", "channel",
  "key data" and "popRoute" are proven at the **engine/embedder boundary**; the
  framework/widget layer that a real app actually uses (Ticker, MethodChannel
  codecs, EditableText, Navigator) is not present and cannot be exercised
  without the Flutter SDK.
* `flutter/platform`, `flutter/textinput` and `flutter/navigation` were sent to
  the embedder as raw bytes; the StandardMethodCodec call envelopes and the
  `PlatformPlugin`/`TextInputPlugin` handlers are not present.
* No real third-party Flutter APK exists on this box to drive, so the fixture is
  the only Flutter sample.

### Files

* `crates/acos-jni/tests/data/flutter_probe_main.dart` — animation, platform
  channel listeners/send, key data, navigation, `Hello ACOS` Paragraph;
  `onSemanticsActionEvent` fix.
* `crates/acos-jni/tests/java/dev/acos/fixtures/flutterhello/FlutterProbe.java`
  — `PlatformMessageHandler`, `sendDartIn`/`sendKeyData`/`sendNavigationPopRoute`,
  correct `invokePlatformMessageResponseCallback` reply.
* `crates/acos-jni/tests/data/flutter-probe.dex` — rebuilt.
* `crates/acos-jni/tests/flutter_hello.rs` — probe drive stage, `surface` and
  `text-pixels` stages, parent assertions.
* Evidence stages: `W93A_STAGE {"phase":"surface",…}`,
  `W93A_STAGE {"phase":"text-pixels",…}` in the child stdout; Dart logs in
  child stderr.
