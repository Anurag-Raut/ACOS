# ACOS — ARM64 execution via qemu-user (design)

**Status:** design + spike (W190-A, 2026-11-11). The spike is recorded in
`results/arm64/W190A.md`; nothing here is committed and no product code is
changed. Owner decision recorded: **ARM64 via qemu-user is approved**
(user-mode only; no `sudo`, no system install).

**Goal:** an APK whose only native ABI is `arm64-v8a` (e.g. the on-device Gaten
v12 artifact, `results/gaten/W177A.md` §4.4) must be *runnable* rather than
rejected with `UNSUPPORTED_ABI`. Today `HOST_ABI = "x86_64"`
(`crates/acos-apk/src/apk.rs:20`) is a single constant used by every ABI gate.

This document covers **Step 1** (options + JNI/bridge architecture + which
parts need an aarch64 bionic libc/linker) and summarises the **Step 2** spike.

---

## 1. Constraints

* **No `sudo`, no system install.** Host is Arch; `qemu-user` is not installed
  and `pacman` needs root. The spike downloads the Arch `qemu-user` package
  (14 MB) and extracts it **user-locally** under `.cache/`; nothing is installed.
* **Landlock/seccomp must still apply.** The arm64 path must not be a hole. The
  existing `acos-sandbox` (`confine`) stays in force; only explicit, documented
  widening is allowed.
* **Do not weaken the x86_64 path.** `HOST_ABI` and the current in-process
  loader stay byte-for-byte as they are for x86_64.
* **Fall back honestly.** A missing qemu, a missing bionic runtime, or an
  unsupported op stays a structured `UNSUPPORTED_*`, never a fake pass.
* Fidelity label on every result; read-only on `~/Projects/gaten`.

---

## 2. Options

### A. `qemu-aarch64` (user-mode) + aarch64 bionic sysroot

Run an aarch64 guest process under `qemu-aarch64` with `-L <bionic-sysroot>`,
so the guest's `PT_INTERP` (`/system/bin/linker64`) and its `libc.so`/`libm.so`/
`libdl.so`/`liblog.so` imports resolve inside the sysroot.

* **Pros:** small (~14 MB toolchain), no kernel changes, fast to adopt; already
  proven as the transport by the W190-A spike (`results/arm64/W190A.md`).
* **Cons:** qemu-user TCG is ~10–50× slower than native; bionic's
  `linker64` expects Android properties/`ld.config` and may need a shim sysroot;
  the APK's own `System.loadLibrary` expectations only hold when the *product
  runtime* runs in this process (see option B/C).
* **Reusable pieces:** `acos-native-loader` already implements its own ELF
  loader + bionic shim table; if the whole runtime is cross-built it does not
  even need the bionic linker (see option B).

### B. Cross-build the ACOS runtime for aarch64, run it under `qemu-aarch64`

Compile `acos-vm` + `acos-jni` + `acos-native-loader` (+ `acos-services`) for
`aarch64` and run **that** under `qemu-aarch64`. The VM, JNIEnv and the guest
`.so` are then all in the same address space and the same instruction set, so
**JNI stays in-process** exactly as it is on x86_64 today; `acos-native-loader`
maps the APK's `lib/arm64-v8a/*.so` and resolves their bionic imports through
its existing shim table.

* **Pros:** no cross-architecture JNI bridge; reuses the whole existing runtime
  and the W181-A `_setjmp`-style bionic shims (which are currently x86_64-only
  and would need an aarch64 implementation); the bionic linker is optional.
* **Cons:** a real cross-compilation effort for the workspace
  (`acos-sandbox` hard-codes `AUDIT_ARCH_X86_64`; `acos-native-loader`'s
  `engine_gaps.rs` inline asm is x86_64-only and otherwise traps); TCG speed.
* **Decision input:** how much of the workspace is arch-conditional (see §5).

### C. Host x86_64 VM + emulated aarch64 **native child**, bridged (the Flutter/RN confined-child model)

Keep the VM x86_64 and execute only the APK's native libraries in a confined
`qemu-aarch64` child, bridged with the existing daemon protocol
(`acos-cli/src/serve.rs`, `--confined-child`) that already fronts the Flutter
and React Native engines.

* **Pros:** the VM/renderer/service layer are untouched; the model and the
  `--confined-child` plumbing already exist.
* **Cons:** JNI is a *synchronous, pointer-heavy, bidirectional* ABI. A true
  cross-arch `JNIEnv`/`jobject` bridge (registers, local/global refs, strings,
  arrays, callbacks into Java from native threads) is a remote-JNI runtime, not
  a message boundary. W181-A already hit the reference-identity side of this
  (`fbjni alias_ref<jclass>` assertion) **in-process on x86_64**; doing it
  across processes and instruction sets is strictly worse. Only coarse engines
  (e.g. "run Hermes as a whole") are tractable this way, and even Hermes is
  driven through fbjni/JSI.

### D. Rejected

* **`qemu-system-aarch64` / full Android emulator:** heavyweight, boots an
  Android guest; already the `art-reference` oracle, not the product path.
* **`binfmt_misc` + `qemu-aarch64-static`:** needs root (kernel registration);
  the execution model is otherwise the same as A.
* **AOT binary translation of arm64 `.so` to x86_64:** out of scope, very large.

### Recommendation

* **Transport: A** (`qemu-aarch64`, user-mode). The spike proves it works under
  the existing sandbox.
* **Runtime topology: B over C.** Cross-build the runtime and run it under the
  qemu transport so JNI remains in-process. C should be reserved for
  coarse-grained engines if B's cross-build or TCG speed proves unacceptable.
* **Minimal interim step:** keep rejecting arm64 with `UNSUPPORTED_ABI` until B's
  aarch64 target builds; surface a new structured code (e.g.
  `UNSUPPORTED_ABI_EMULATED` / `QEMU_UNAVAILABLE`) the moment the transport is
  wired but the runtime is not.

---

## 3. Which parts need an aarch64 bionic libc/linker

| Component | Needs bionic `linker64`? | Needs bionic `libc.so`/`libm.so`/`libdl.so`/`liblog.so`? | Source |
|---|---|---|---|
| Static NDK probe binary (spike) | no | no (static bionic libc linked in) | NDK `aarch64-linux-android29-clang -static` |
| APK `lib/arm64-v8a/*.so` run through `acos-native-loader` | **no** — the loader implements its own ELF mapping/resolution and bionic shims | no, if hostile libc imports resolve through the shim table; **yes** only if we choose to run the *bionic* linker | APK itself + `acos-native-loader/src/bionic/` |
| APK `lib/arm64-v8a/*.so` run through the real Android loader (`System.loadLibrary` fidelity, dlopen namespaces, `libc++_shared.so`/`libjnigraphics.so`/`libGLESv2.so`) | **yes** | **yes** | Android arm64 system image or a connected arm64 device |
| The cross-built ACOS runtime (option B) | no (its host libc is whatever target we build for) | only for the guest `.so` resolution path (shims) | Rust target `aarch64-linux-android` (NDK) or `aarch64-unknown-linux-gnu` |

**Finding (spike):** the NDK sysroot in this environment
(`~/Android/Sdk/ndk/27.1.12297006/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/lib/aarch64-linux-android/`)
ships **only static `libc.a`/`libm.a`/`libdl.a`** plus `libc++_shared.so` — it
contains **no** `libc.so`, `libm.so`, `libdl.so`, `liblog.so`, and **no
`linker64`**. Confirmed by direct listing and by running a dynamically-linked
NDK binary under qemu: `qemu-aarch64: Could not open '/system/bin/linker64'`.
There is no NDK/sysroot/bionic tree under the repo `.cache` either.

So a bionic sysroot, if wanted, must come from one of:

1. **A connected arm64 device** (cheapest): `ZA222WGJPP` is attached. `/system`
   is world-readable, so `adb pull /system/bin/linker64` and
   `/system/lib64/{libc,libm,libdl,liblog,libandroid}.so` needs no root. This is
   the recommended source; it is a read of the device, not a modification.
2. **An arm64 system image** via `sdkmanager "system-images;android-34;google_apis;arm64-v8a"`
   (~1.5 GB download → needs authorisation per the host-safety rule), then
   extract `/system/{bin/linker64,lib64/*}` from `system.img`.
3. The **APK's own** `lib/arm64-v8a/libc++_shared.so` (when shipped) for C++
   runtime only; never libc/linker.

---

## 4. Sandboxing and the widened policy

### 4.1 Why seccomp must be deferred past `execve`

`acos-sandbox::confine` installs seccomp **before** the session worker starts
(`crates/acos-cli/src/native_launch.rs`) and the filter **denies `execve` /
`execveat` outright** (`linux/seccomp.rs`). The arm64 transport must `execve`
`qemu-aarch64`. It is impossible to `confine()`-then-`exec` qemu. So the arm64
profile splits the layers:

1. `prctl(PR_SET_NO_NEW_PRIVS)` — persists across `execve`;
2. fd hygiene, sanitised env, rlimits — persist;
3. **Landlock** ruleset — persists across `execve`;
4. `execve(qemu-aarch64)`;
5. **seccomp installed inside the qemu process** by an `LD_PRELOAD` constructor
   (`libacos-qemu-guard.so`), after the dynamic linker has run, before `main`.

The post-exec window contains only the *trusted* host dynamic linker for
`qemu-aarch64`; the guest is not running yet. The filter contents are unchanged
by this reordering.

> **Product change:** add a mode to `ConfineProfile`, e.g.
> `seccomp: SeccompMode::{Inline, DeferredPreload(PathBuf)}`, and expose the
> audited filter builder so the preload guard does not carry a second copy of
> the policy. `confinement_active()` must remain `false` until the guard reports
> the filter installed (a post-exec status pipe), so a failed preload can never
> look confined.

### 4.2 Landlock roots the qemu process needs (measured)

| Path | Access | Why |
|---|---|---|
| aarch64 sysroot (NDK/device) | RO | guest `linker64` + bionic libs |
| extracted APK `lib/<abi>` + assets | RO | guest `.so` and data |
| `/usr`, `/lib`, `/lib64` | RO | host `qemu-aarch64` + its host libraries + host `ld.so` |
| `/proc` | RO | qemu reads `/proc/self/{maps,exe}`, CPU info |
| `/dev` | RO | `/dev/urandom`, `/dev/null`, stdio fallbacks |
| session root | RW | app data only |

Without `/proc` qemu-user may fail to self-inspect; with the profile above the
spike ran cleanly. `/sys` RO may be needed for `-cpu max` cache topology; add it
only if a run shows it.

### 4.3 seccomp allowances (measured: **none added**)

The spike ran a threaded static aarch64 binary under `qemu-aarch64` with the
full ACOS deny list active and **needed no new syscall allowance**:

* qemu TCG needs `mmap`/`mprotect` with `PROT_EXEC` — **already allowed** by the
  native profile and required there too (`rustc`/engines);
* qemu threads use `clone` (the `clone3` → `ENOSYS` rule makes libc fall back);
* `futex`, `rt_sigprocmask`, `sched_*`, `prctl`, `getrandom` are default-allow.

The only *widening* is structural: the seccomp install is deferred past
`execve`, and Landlock gains the RO roots above. If a future qemu build needs a
presently-denied syscall, add it explicitly with a code comment and an updated
`DECISIONS.md` ADR entry — never flip the filter to default-deny-by-omission.

### 4.4 x86_64 path is untouched

`HOST_ABI`, `confine()` ordering, the filter and `acos-native-loader` are
unchanged for x86_64. The arm64 profile is a **new** selection; it is never
chosen when the APK ships an x86_64 `lib/` directory.

---

## 5. Code seams (for the implementation estimate)

* `crates/acos-apk/src/apk.rs`: `HOST_ABI` → add `GuestAbi { X86_64, Arm64 }`
  and `check_native_abi_for(abi)`; keep `HOST_ABI` and the existing method
  behaviour for x86_64.
* `crates/acos-cli/src/native_launch.rs`: select the transport from the APK's
  ABIs — x86_64 → current in-process path; arm64-only → qemu transport.
* `crates/acos-runtime/src/native_bridge.rs`: `NativeBridge::prepare` is
  `HOST_ABI`-specific; either (B) make the whole runtime's ABI a build/launch
  variable, or (C) route to a new `acos-arm64` child crate.
* `crates/acos-sandbox`: `SeccompMode`, an exported filter builder, and a
  `ConfinementReport` check that the deferred layer actually loaded.
* `crates/acos-native-loader/src/bionic/engine_gaps.rs`: the bionic shims have
  x86_64 inline asm (setjmp) and structured traps elsewhere; aarch64 needs real
  implementations when the runtime is cross-built.
* New crate `acos-arm64` (name TBD): qemu discovery (env/`PATH`/extracted
  `.cache/qemu-user`), bionic-sysroot assembly, arg building (`-L`, `-cpu`,
  `-R`), child spawn, and structured errors (`QEMU_UNAVAILABLE`,
  `BIONIC_SYSROOT_MISSING`).

---

## 6. Roadmap

| Step | Deliverable | Estimate |
|---|---|---|
| **W190-A (this task)** | design doc + confined qemu spike (static bionic hello) | **~45 min** ✅ |
| W191-A | qemu discovery + `acos-arm64` transport crate; `SeccompMode::DeferredPreload` in `acos-sandbox`; guard `.so` from the audited filter | 1–1.5 days |
| W192-A | bionic sysroot from the attached arm64 device; run a **dynamic** aarch64 `.so` (e.g. a tiny NDK `.so` via a host aarch64 driver) under `-L`; document the `ld.config`/property gaps | 1 day |
| W193-A | cross-build spike: `cargo build -p acos-sandbox -p acos-native-loader -p acos-jni` for `aarch64-unknown-linux-gnu` (or `-linux-android`); enumerate and fix `#[cfg(target_arch)]` sites | 1–2 days |
| W194-A | run an APK's real `lib/arm64-v8a/*.so` through `acos-native-loader` inside the aarch64 runtime under qemu (start with a small fixture, then Gaten's `libhermesvm.so`) | 2–4 days |
| W195-A | lift `UNSUPPORTED_ABI` for arm64-only APKs; full `acos launch` of the Gaten v12 arm64 artifact; performance + fidelity report | 2–4 days |

**Risks:** qemu TCG startup/throughput on a 250 k-method RN app; bionic
`linker64` property/namespace assumptions; aarch64 bionic shim coverage in
`acos-native-loader`; and the pre-existing (arch-independent) W181-A blockers
(fbjni reference identity, missing ICU/gcov symbols) which arm64 does **not**
fix.

---

## 7. Spike summary (details in `results/arm64/W190A.md`)

* `qemu-aarch64 11.1.3` extracted user-locally from the Arch `qemu-user`
  package; no install, no root.
* NDK-built **static** aarch64 hello (threads, `getpid`, allowed + outside
  reads, `unshare`/`ptrace`/`setuid` probes) runs under `qemu-aarch64`.
* Same binary under the spike **Landlock + seccomp** launcher + `LD_PRELOAD`
  guard: Landlock denies `/etc/hostname` and the out-of-root secret; seccomp
  denies `ptrace`/`unshare`/`setuid`; the guest's allowed read still succeeds,
  its thread still joins. No new seccomp allowance was required.
* Dynamic NDK binary fails with `Could not open '/system/bin/linker64'` —
  confirming the bionic-sysroot requirement in §3.
