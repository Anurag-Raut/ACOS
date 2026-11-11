# W153-A — Real JNI `<clinit>` + reflected method/field IDs; RN bridge still captures 19 ops

Worktree `.cache/wt-w153a` (detached HEAD `fc18b10`, the W143-A commit) built with
`CARGO_TARGET_DIR=.cache/target-w153a`, `CARGO_PROFILE_DEV_DEBUG=0` through
`tools/guarded.sh`. Changes are uncommitted; nothing was committed. This work
fixes the two principled JNI blockers W143-A documented and removes the two
probe workarounds that papered over them.

## Result at a glance

| | W143-A | W153-A |
|---|---|---|
| `GetStaticFieldID`/`GetStaticMethodID` | never ran `<clinit>` | run `<clinit>` per JNI spec |
| `ReadableType` enum priming | probe hack: `ReadableType.values()` early | **removed** |
| `FromReflectedMethod`/`FromReflectedField` | `JNI_UNSUPPORTED` trap | **real** (pointer-identity `jmethodID`/`jfieldID`) |
| `ToReflectedMethod`/`ToReflectedField` | `JNI_UNSUPPORTED` trap | **real** (`java.lang.reflect.*` mirror) |
| sync `@ReactMethod` (`getAndroidID`) | omitted from the probe | **registered**, exercises `MethodInvoker` |
| captured `ACOS_RN_OP` | 19 | **19** (asserted in the test) |
| registered natives | 85 | 85 |

Evidence: `results/rn/w153a-ops.json` — `ops` (19) plus the full
`ACOS_RN_BRIDGE` stage trail. The `readabletype-init` stage is gone and no
`js-exception` for `ReadableType` appears.

## 1. `GetStaticFieldID` / `GetStaticMethodID` run `<clinit>`

JNI requires both static ID lookups to cause class initialisation. fbjni's
`ReadableNativeMap.importTypes` reads the `ReadableType` enum constants with
`GetStaticObjectField`; without `<clinit>` the constants are null and
`ReadableNativeMap.localTypeMap` throws
`null cannot be cast to non-null type com.facebook.react.bridge.ReadableType`.

`Vm::initialize_class_for_jni` (new, `crates/acos-vm/src/javabridge.rs`) mirrors
the `call_static`/`push_static_call` prologue: ensure the host sentinel frame,
then `initialize_class`, mapping a Java throw to a pending JNI exception.
`jni_GetStaticMethodID` / `jni_GetStaticFieldID` call
`ensure_static_init` before building the ID. The `ReadableType.values()` probe
hack is deleted.

## 2. `FromReflectedMethod` / `FromReflectedField` / `ToReflected*`

fbjni's `JReflectMethod::getMethodID()` calls
`JNIEnv::FromReflectedMethod` for every **sync** `@ReactMethod`; the returned
`jmethodID` is then used with `Call<Type>MethodA`. The four slots moved from
`traps.rs` to `jni.rs` (and `scripts/gen_jni_table.py`'s `RUST_IMPL`), so
`table.rs`/`traps.rs` are regenerated.

* `FromReflectedMethod` reads the mirror's `artMethod` (`MethodId`) and
  re-materialises the `MethodInfo` from the VM's `Method` (name, signature,
  `is_static`).
* `FromReflectedField` reads `declaringClass`/`artFieldIndex`/`accessFlags`
  and rebuilds the `FieldInfo`.
* `ToReflectedMethod` / `ToReflectedField` resolve the ID back to a `MethodId`
  / declared field and call the existing `fwk_annotations::make_method` /
  `fwk_reflect::make_field` mirrors.

The probe's `getAndroidID` is now a real `@ReactMethod(isBlockingSynchronousMethod
= true)`, so the `JavaNativeModule::getMethods()` → `MethodInvoker` path runs.

## 3. Still open (real, non-probe work)

* **Display service (`Choreographer.<init>`)**: `DisplayManagerGlobal.getInstance()`
  → `ServiceManager.rawGetService("display")` needs the `ServiceHost` +
  `BinderTransport` that `acos-runtime` installs. The raw `acos-jni` test VM has
  no transport and cannot depend on `acos-runtime` (dependency cycle), so the
  probe keeps `VirtualChoreographerProvider`; the real frame path is already
  covered by `acos-runtime`'s `vsync_natives` test.
* **`ProbeContext.getSystemService`**: the probe still uses
  `SimplePlatformConstantsModule` (real `Build` constants, `uiMode="normal"`)
  rather than the real `AndroidInfoModule`, which needs a real `UiModeManager`
  from the service registry.

## 4. Files touched (worktree, uncommitted)

* `crates/acos-vm/src/javabridge.rs` — `initialize_class_for_jni`,
  `reflected_method_id`, `make_reflected_method`, `reflected_field_parts`,
  `make_reflected_field`.
* `crates/acos-vm/src/natives/mod.rs` — `fwk_annotations`/`fwk_reflect` visible
  to the bridge.
* `crates/acos-vm/src/natives/fwk_reflect.rs` — `make_field` is `pub(crate)`.
* `crates/acos-jni/src/jni.rs` — `<clinit>` on static IDs; the four
  `*Reflected*` slots; `record_bridge`/`ensure_static_init` helpers.
* `crates/acos-jni/scripts/gen_jni_table.py`,
  `crates/acos-jni/src/table.rs`, `crates/acos-jni/src/traps.rs` — regenerated.
* `crates/acos-jni/tests/java/.../BridgeProbe.java` — remove `ReadableType`
  priming; add sync `getAndroidID`.
* `crates/acos-jni/tests/data/rn-bridge-probe.dex` — rebuilt.
* `crates/acos-jni/tests/rn_bridge.rs` — assert 19 ops; write
  `results/rn/w153a-ops.json`.
* `results/rn/w153a-ops.json` — new evidence.

## 5. Acceptance

| check | result |
|---|---|
| `rn_bridge` | **2 passed** (19 ops asserted; `results/rn/w153a-ops.json`) |
| `rn_hermes` | **2 passed** |
| `rn_native_stack` | **2 passed** |
| `flutter_hello` | **2 passed** |
| `cargo clippy --workspace --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 pass / 0 fail / 0 unsupported** |
