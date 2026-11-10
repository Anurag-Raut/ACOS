# W120-A — RN bridge under confinement: Landlock traversal + SoLoader reach (2026-11-11)

Worktree: `.cache/wt-w120a` (clean detached HEAD `834d92c`), built to
`.cache/target-w120a` with `CARGO_PROFILE_DEV_DEBUG=0` via `tools/guarded.sh`.
Patch verified with `git apply --check --reverse` in the clean worktree and
already present in `/home/Qui/Projects/acos`. Not committed.

## 1. Result at a glance

| stage | W106-A | W120-A |
|---|---|---|
| confinement | active (Landlock ABI 10, **7 rules**) | active (Landlock ABI 10, **36 rules**) |
| `SoLoader.init` | ok, sources registered | ok, sources registered |
| `File.canonicalize0(<scratch>/lib)` | **EACCES** | **ok** (guest path in VFS) |
| `SoLoader.loadLibrary("hermes")` | not reached | **ok** |
| `SoLoader.loadLibrary("hermestooling")` | not reached | **ok** (merged onload) |
| real RN modules | `ExceptionsManager`/`Timing` NPE | `ExceptionsManager` **ok**, `Timing` blocked on `ReactChoreographer` |
| `HermesExecutorFactory.create()` | not reached | **reached**, aborts on VM opcode |
| `CatalystInstanceImpl.Builder.build` / `runJSBundle` / `runApplication` | not reached | not reached |
| captured `ACOS_RN_OP` | 0 | **0** |

The bridge now gets all the way to constructing the Hermes executor. The
remaining abort is a **VM-owned** opcode gap, not a confinement or harness bug
(§4).

Raw evidence: `results/rn/w120a-bridge.json` (stages, summary, `ops: []`).

## 2. Fix 1 — generic Landlock ancestor traversal (`acos-sandbox`)

`landlock::restrict` now walks the ancestor chain of every read-only and
read-write grant and adds an **`EXECUTE`-only** path-beneath rule for each
ancestor directory (the chain, deduplicated, skipping directly-granted roots).

* `EXECUTE` (search) is what absolute-path resolution — `realpath(3)`,
  `File.canonicalize0`, `openat` after a walk — needs on each ancestor.
* `READ_DIR` is deliberately **not** granted: it would expose the ancestor's
  directory listing, and traversal does not need it. This is the safe form of
  the requested ancestor grant; the alternative (pre-canonicalise everything)
  is no longer necessary.
* Landlock rules apply to a whole subtree, so an ancestor's `EXECUTE` is
  inherited by its descendants. That is unavoidable with path-beneath rules,
  but it grants *search only*: the sibling's `READ_FILE`/`READ_DIR`/write stay
  denied.

Fail-closed posture and denial surface are unchanged. Evidence:
`crates/acos-sandbox/tests/confine.rs`:

* `canonicalize_ro` (granted) now **succeeds** — it failed before, because the
  ancestor chain was not traversable.
* `canonicalize_outside` succeeds too (pure traversal, no content) — recorded
  explicitly rather than hidden.
* `open_outside`, `list_outside`, `write_ro`, `write_outside` remain **denied**:
  `EXECUTE` does not imply `READ_DIR`/`READ_FILE`/write.
* All existing seccomp escape probes (`exec`, `ptrace`, `socket_inet`,
  `io_uring_setup`, `clone_userns`, …) remain denied.

Unit test `ancestor_access_is_search_only` pins the mask to `FS_EXECUTE` and
asserts no `READ_DIR`/`READ_FILE`/`WRITE_FILE`.

### 2.1 Root cause correction for the `EACCES`

W106-A attributed `canonicalize0(<scratch>/lib): EACCES` to Landlock denying
ancestor traversal. Reproducing it showed the immediate cause was the VM's
sandbox **VFS session root not being writable**: `VfsConfig::from_env` picks
`.cache/sessions/<id>/data` *after* confinement, and this profile only granted
`.cache` read-only, so `Vfs::new` poisoned itself with `EACCES` and every VFS
call returned it. The harness now pins the VFS root into the writable scratch
(`ACOS_SESSION_ROOT` in `ConfineProfile::env`, exactly like the product
`acos-cli` launch path). With that fixed the error became `ENOENT`, proving the
ancestor grant was not the (only) issue.

## 3. Fix 2 — guest paths for SoLoader (`acos-jni` harness)

`java.io.File`/`canonicalize0` in the VM operate on **guest** paths in the VFS;
the old harness handed SoLoader the raw host scratch paths, which are unmapped
(`Target::Denied`). The harness now:

* stages the fixture libs at `<vfs>/data/user/0/dev.acos.fixtures.rnhello/lib`
  and the APK at `…/base.apk` (the VFS root is the writable `ACOS_SESSION_ROOT`);
* passes guest `nativeLibraryDir` / `sourceDir` to the probe, while the
  host APK path is still handed to `AssetManager.addAssetPath` (that native is
  host-based).

`BridgeProbe.ProbeContext` gained the `(libDir, sourceDir, assetApkPath)`
split. `ApplicationInfo` now carries the guest `sourceDir`/`nativeLibraryDir`
and `publicSourceDir`, so `SoLoader`'s `CheckBaseApkExists` and
`DirectorySoSource` resolution work.

## 4. Fix 3 — `ExceptionsManager` / `Timing` null dev-support (`acos-jni` probe)

The Kotlin constructors reject `null` (`ReleaseDevSupportManager` required).
The probe now passes the real RN no-op
`com.facebook.react.devsupport.ReleaseDevSupportManager`:

* `ExceptionsManager` → **ok**.
* `Timing` → still throws, now a different and real blocker:
  `IllegalStateException: ReactChoreographer needs to be initialized.` Its
  constructor calls `ReactChoreographer.Companion.getInstance()` before the
  choreographer has been `init()`-ed. Not on the `initializeBridge` path;
  recorded for the bridge owner.

## 5. Open blocker — VM `invoke-polymorphic` (VM-owned)

After both merged libraries load, `HermesExecutorFactory.create()` aborts:

```
JNI_NATIVE_FAULT: Call<Type>Method: opcode invoke-polymorphic
(invoke-polymorphic/custom) is not implemented
(at Ljava/util/concurrent/atomic/AtomicReference;.compareAndSet pc=2)
```

Android 14's real `AtomicReference.compareAndSet` uses `VarHandle`, which
decodes to `invoke-polymorphic`; `acos-vm` only implements `invoke-polymorphic`
for `AtomicInteger`/`AtomicLong` (and, per the concurrent render work,
`AtomicBoolean`). This is **`acos-vm` territory** and is explicitly owned by
another workstream, so it was not patched here. The shared fix is a small
`atomic_reference` override in `crates/acos-vm/src/natives/vmrt.rs`, mirroring
the existing `atomic_integer`/`atomic_long` storage shims.

Consequence: `CatalystInstanceImpl.Builder.build()` (`initializeBridge`),
`loadScriptFromAssets`, `runJSBundle` and `AppRegistry.runApplication` are not
reached, so **no `ACOS_RN_OP` is captured**. `LoggingUIManagerModule` op capture
is already wired (`createView`/`manageChildren`/`updateProps`/`setChildren`)
and will emit ops as soon as the executor constructs.

## 6. Acceptance (clean worktree `.cache/wt-w120a`, HEAD `834d92c` + patch)

| check | result |
|---|---|
| `cargo test -p acos-sandbox` | **pass** (8 lib + 3 integration) |
| `cargo test -p acos-jni --test rn_bridge` | **2 passed** (evidence, blocker recorded) |
| `cargo test -p acos-jni --test rn_hermes` | **2 passed** |
| `cargo test -p acos-jni --test rn_native_stack` | **2 passed** |
| `cargo test -p acos-jni --test flutter_hello` | **2 passed** |
| `cargo clippy -p acos-sandbox -p acos-jni --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported** |

## 7. Files

```
crates/acos-sandbox/src/linux/landlock.rs        ancestor EXECUTE-only rules + unit test
crates/acos-sandbox/src/bin/sandbox_child.rs     canonicalize_ro/outside, list_outside actions
crates/acos-sandbox/tests/confine.rs             traversal allowed; listing/read/write denied
crates/acos-jni/tests/rn_bridge.rs               VFS session root, guest paths, w120a evidence
crates/acos-jni/tests/java/.../BridgeProbe.java  guest sourceDir/nativeLibraryDir, ReleaseDevSupportManager
crates/acos-jni/tests/data/rn-bridge-probe.dex   rebuilt probe
results/rn/w120a-bridge.json                     raw stages/summary/ops
```

Patch: `/tmp/w120a.patch` (binary-safe, reverse-applies against the clean
worktree). The scratch test dirs are removed by the harness.
