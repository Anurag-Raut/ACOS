# E1B — Approach B: real ART's `libart.so` inside the ACOS bionic loader

**Experiment:** load the Android 14 (API 34, x86_64, `google_apis`) platform ART
— `libart.so` plus its 16-library `DT_NEEDED` closure (17 libraries in the global scope) — through
`acos-native-loader`, call the exported `JNI_CreateJavaVM`, and run a DEX,
without booting Android.

**Verdict (short):** *mapping, relocating and initialising real ART inside our
loader works, under the unmodified ACOS sandbox.* `JNI_CreateJavaVM` then runs
`Runtime::Init` through GC selection, boot-classpath setup and libnativeloader
init, and stops at a blocker that is **not** a bionic-symbol problem: ART's
`libnativeloader`/`libnativebridge` require the **dynamic linker's namespace
implementation** (`android_namespace_t`, `ld.config.txt`), whose only
implementation on Android is the `/system/bin/linker64` binary. That is a
bounded but non-trivial piece of work (and `ld-android.so` is a 2-byte `ud2`
stub, so it cannot be borrowed).

Everything in this experiment lives in `research/art-feasibility/b-loader/`
(own `[workspace]`, path-deps read-only into `crates/`) — no ACOS source was
modified. Loader changes were made in a **vendored copy**
(`research/art-feasibility/b-loader/vendor/acos-native-loader/`, 10 documented
`B-LOADER PATCH` hunks). Artifacts: `artifacts/gapscan-libart.{json,md}`,
`artifacts/load-report.txt`.

---

## 1. What actually ran

| step | result |
|---|---|
| `gapscan` over the extracted sysroot (1162 `.so`) | closure from `libart.so` = **17 libs**, all `DT_NEEDED` resolved; **101 distinct missing imports** before patches |
| map + relocate + constructors, unconfined | **OK**, 16,581 relocations (8,414 in libart alone) |
| map + relocate + constructors, **confined** (`no_new_privs`+Landlock+seccomp, 24 GiB AS) | **OK** — same result, unchanged filter |
| `JNI_CreateJavaVM` | runs to `InitDefaultPublicLibraries`, then aborts (blocker 9 below) |
| DEX `main` via `FindClass`/`CallStaticVoidMethodA` | **not reached** (needs a VM) |
| `RegisterNatives` with a Rust function (step 4) | **not reached** |
| 27-program correctness corpus (step 3) | **not run** (needs a VM); no ART-vs-Rust-VM timing/RSS comparison possible |

Measured (release build, `.cache/target-artexp-b/release/artspike --step load`):
**2.7 s wall, 37 MiB max RSS** to map + relocate + init the whole ART closure
from cold page cache (debug build with debuginfo: 17–19 s, 38 MiB — the cost is
relocation, not memory). The sandboxed run shows the same numbers.

Per-library binding after the patches (`artifacts/load-report.txt`): 6,189
import slots across the 17 libraries — 594 forwarded to host libc/libgcc/zlib,
305 bound to shims, **0 missing**; bionic table grew 728 → 894 entries.

## 2. Gap table

`status`: **fixed** = done in the vendored copy this session; **needed** = must
be built for production; **partial** = works for the paths exercised here.

| # | gap (symbol / feature) | evidence | status | effort |
|---|---|---|---|---|
| 1 | **APS2 packed relocations, real format** — group flags `GROUPED_BY_INFO/OFFSET_DELTA/ADDEND/HAS_ADDEND`, offsets cumulative across groups | `ELF_MALFORMED: Android packed relocation group flags 0x0 are unsupported`; libart's `.rela.dyn` is 8 groups / 527 relocs | fixed (decoder rewritten; byte-exact parse of all 527 relocs, ends exactly at section end) | 0.5 d |
| 2 | **TLS GD/LD GOT-pair relocations** (`R_X86_64_TLSGD` 19, `TLSLD` 20, `TPOFF32` 23, `GOTTPOFF` 22) | 192 TLSGD in libart + 184 TLSLD in libartbase; upstream rejects 19/20 as `UNSUPPORTED_RELOCATION` | fixed (write `{modid, offset}` into the GOT pair, which our `__tls_get_addr` already consumes) | 0.5 d |
| 3 | **101 missing bionic imports** (libc++ `_Unwind_*`+`isw*`+`strtod_l`, libbase `__strlcpy_chk`/`__pwrite_chk`/`mkdtemp`/properties, libsigchain `*64` signal calls, libunwindstack `ptrace`/`process_vm_readv`, libart `pidfd_open`/`mallinfo`/`mincore`/`fallocate`/`adler32`, libprofile zlib, `android_fdsan_*`, `android_get_device_api_level`, liblog event API, `atrace_*`, `ashmem_*`, `_ctype_`) | `UNSUPPORTED_NATIVE_SYMBOL` listing all 101 | fixed as 166 table entries (87 host forwards + 79 shims); see `art_gaps.rs` | 2–3 d to harden (exact `mallinfo`/`__assert`/`_ctype_` semantics) |
| 4 | **`dlsym(RTLD_NEXT)`** + `sigaction64`/`sigprocmask64`/`sigwait64` | `libc: Unable to find next sigaction64 in signal chain` → broken ART signal chain | fixed (RTLD_NEXT resolves to the bionic table; `*64` spellings map onto the 8-byte bionic `sigset_t`) | 0.5 d |
| 5 | **`ld-android.so` is a `ud2` stub**; every `__loader_*` symbol points at a 2-byte trap. Mapping it shadows the bionic table (`resolve_global` prefers loaded objects) → SIGILL | `SIGILL at 0x…18a0` in ld-android, reached from libnativebridge | fixed (treated as a shim system lib, never mapped) | 0.5 d |
| 6 | **Global-group preload** (bionic namespaces pre-populate `libcutils`/`libselinux`; `libtombstoned_client` imports `socket_local_client` from libcutils without a `DT_NEEDED`) | `libartpalette-system.so` dlopen failed: `socket_local_client`, `atrace_*`, `ashmem_*`, `_ctype_`, `fts_*`, `getpwuid` … | partial (spike preloads libcutils+libselinux; production needs a real namespace model) | 1–2 d |
| 7 | **`sysconf` numbering differs** (bionic `_SC_PAGE_SIZE`=0x28, glibc `_SC_PAGESIZE`=30; 146 of 148 names differ) | `Check failed: sysconf(0x28) == kPageSize (255 vs 4096)` | fixed (generated 146-entry map from NDK 27 + compiled glibc values) | 0.5 d |
| 8 | **Android absolute VFS paths** (`/system/etc/public.libraries.txt`, `/apex/...`, `/system/lib64/...`) do not exist on the host | `Error reading public native library list from "/system/etc/public.libraries.txt": No such file or directory` (libnativeloader `LOG(FATAL)`) | fixed for `open/openat/fopen/access/stat/realpath/opendir` (root remap + per-path overrides); production wants a real mount/chroot-like view | 1–2 d |
| 9 | **Linker namespace ABI** — `android_namespace_t` internals, `android_create_namespace` / `link_namespaces` / `get_exported_namespace` / `init_anonymous_namespace`, `ld.config.txt` semantics, per-namespace search paths, `public.libraries.txt` preload | `nativebridge: system_ns == nullptr: Failed to get system namespace for loading libz.so`; returning a synthetic handle gets further but crashes inside libnativeloader (`strdup` on a namespace field) | **needed — hard blocker** | **8–15 d** |
| 10 | Boot image: the system image ships **no `.art`/`.oat`/`.vdex`** (only `core-oj.jar`, `core-libart.jar`, `core-icu4j.jar`) → `-Xnoimage-dex2oat` + runtime verification of two boot jars | `DexFile … is in boot class path but is not in a known location` (warning) | partial: works as a fallback, cost unmeasured (VM never came up) | 3–5 d (ship/construct a boot image, or measure pure-verify startup) |
| 11 | `dex2oat` subprocess / AOT / JIT / `-Ximage` generation require `execve` | seccomp denies `execve`; `-Xint` avoided it | needed only for AOT/JIT modes | 2–4 d (in-process dex2oat or prebuilt artifacts) |
| 12 | ART thread/signal internals at scale: `pthread_getattr_np`, `pthread_getcpuclockid`, `pthread_gettid_np`, TLS dtors, `sigaltstack`, sigchain fidelity, futex-based suspension | not reached (needs a live VM: threads start after `Runtime::Init`) | unknown / expected | 5–8 d |

## 3. Blocker chain (exact errors, in order)

```
1  ELF_MALFORMED  ELF is malformed: Android packed relocation group flags 0x0 are unsupported
2  UNSUPPORTED_RELOCATION  (TLSGD/TLSLD, would follow)
3  UNSUPPORTED_NATIVE_SYMBOL  101 symbols across 13 libraries
4  libc: Unable to find next sigaction64 in signal chain      -> SIGSEGV
   libc: Unable to find next sigprocmask64 in signal chain    -> SIGSEGV
5  [android-assert] libartpalette: (libartpalette-system.so dlopen failed: atrace_*/ashmem_*/socket_local_client)
6  Check failed: static_cast<size_t>(sysconf(0x0028)) == kPageSize (255 vs 4096)
7  [android-assert] nativeloader: Error reading public native library list from
   "/system/etc/public.libraries.txt": No such file or directory
8  [android-assert] nativeloader: handle == nullptr: Error preloading public library libaaudio.so
9  [android-assert] nativebridge: system_ns == nullptr: Failed to get system namespace for loading libz.so
   (with a synthetic namespace handle: SIGSEGV inside libnativeloader -> strdup)
   [art 7] art: runtime_common.cc:466 HandleUnexpectedSignal reenter
```

Steps 1–8 were fixed inside the research project; step 9 is where this
experiment stops.

## 4. Sandbox: what ART needs, what the current filter allows

Verified: the **whole ART closure loads and runs constructors** under
`confine()` with the *unmodified* policy (no_new_privs, fd hygiene, sanitised
env, rlimits, Landlock, seccomp; user/net/mount namespaces skipped).

Required widenings / confirmations for a live VM:

| need | current filter | action |
|---|---|---|
| `RLIMIT_AS` | default **4 GiB** — too small for ART heap + boot dex + 16 libs | raise to **16–24 GiB** (this spike: 24 GiB). `confine()`'s default must be per-profile |
| Landlock read set | sysroot, `/proc`, `/sys`, `/usr/lib` read-only; data dir rw | keep; ART reads `/proc/self/maps`, `/proc/self/status`, `/proc/cpuinfo`, `/sys/devices/system/cpu`, `/proc/meminfo` |
| `sigaltstack`, `rt_sigaction`, `mprotect`, `madvise`, `mmap(MAP_FIXED|MAP_NORESERVE)` | allowed | no change (verified by the load step; ART's SIGSEGV-null-check/stack-overflow handlers install fine) |
| `tgkill` | allowed iff `tgid == own pid` | OK — ART suspends only its own threads. (`kill`/`tkill` denied is correct.) |
| `memfd_create`/ashmem, `prctl`, `futex`, `membarrier`, `sched_*` | allowed (deny-list model) | no change |
| `userfaultfd` | **denied** | ART 14 CC GC only uses UFFD with an explicit flag (`enable_uffd_gc`); keep denied, set the property false |
| `pidfd_open` | **denied** (libart imports it) | only on process-watch/crash paths; allow per-profile if ART asserts |
| `process_vm_readv`, `ptrace` (libunwindstack) | **denied** | crash-dump/unwinder only; keep denied, expect degraded tombstones |
| `mount`, `unshare`, `setns` (libnativebridge) | **denied** | correct to deny; the namespace API must be *implemented*, not forwarded |
| `execve` | **denied** | fine for `-Xint`/`-Xnoimage-dex2oat`; AOT/JIT/dex2oat needs in-process compilation or a brokered helper |

## 5. Step 3 and step 4 (not reached)

* The 27-program `dexprogs` corpus and the ART-vs-Rust-VM startup/RSS/p95
  comparison need a live VM; nothing to report beyond the loader-side numbers in §1.
* `RegisterNatives`-style Rust natives (step 4) likewise: the JNI slot indices
  and the call path are wired in `artspike` (`--natives`, `nativeEcho()I`), but
  the VM never came up, so no call-overhead measurement. *Design* answer for the
  report: once ART is up, a Rust native registered via `RegisterNatives` is
  entered directly by ART with a real `JNIEnv*`, so fbjni's local-reference
  rules/exceptions come from ART for free — but ACOS's `acos-jni` object model
  (which owns refs, exceptions and the JNI vtable today) is then bypassed.

## 6. Estimate to make "real ART inside our loader" production-viable

Agent-team person-days (an agent ≈ 1 dev-day of throughput; ranges, 90% conf):

| work package | days |
|---|---|
| Loader core: APS2/TLS/ld-android policy/preload/path-remap hardening + regression tests (mostly prototyped here) | 3–5 |
| Bionic surface: the 166 entries here + the next few hundred that execute only at runtime (jemalloc/`mallinfo`, ICU data, `__cxa_*`, locale, `_ctype_` semantics, fortify, fd-based APIs), plus `sysconf`/`syscall` numbering audits | 5–10 |
| **Linker namespace implementation** (`android_namespace_t` layout, `ld.config.txt` subset, create/link/get_exported, `public.libraries.txt`, per-namespace `LD_LIBRARY_PATH`, `android_dlopen_ext` `android_dlopen_ext_ExtInfo`) | **8–15** |
| Threads/TLS/signals at ART scale (pthread internals, sigchain fidelity, alt stacks, futex suspension, thread-local dtors) | 5–8 |
| Boot image / dex plumbing without `execve` (ship or generate `boot.art`+`boot.oat`, checksums, `/data/dalvik-cache`, in-process dex2oat) | 5–10 |
| Sandbox profile widening + adversarial tests + 27-program corpus parity + startup/RSS benchmarks | 3–5 |
| Unknown-unknowns buffer (ART `CHECK`s, SELinux stubs, property surface, zygote assumptions; we hit ~9 such blockers in 3 h) | 5–10 |
| **total** | **≈ 34–63 agent-days (7–13 weeks for a 1-agent team, ~2–3 weeks for a 3-agent team)** |

## 7. Architectural read (why the estimate is not the whole story)

1. **It replaces the VM.** A working ART makes `acos-vm`, `acos-jni` and the
   framework-dex work redundant for the *execution* path — the strategic
   opposite of Approach A. The 8k-line Rust loader becomes a shim under a
   30 MB C++ VM.
2. **Version lock-in.** ART's `libart.so` is welded to one bionic + one APEX
   layout + one `ld.config.txt`. Every Android release becomes a porting cycle
   (this experiment already needed an ART-14-specific APS2 parser and a
   sysconf table).
3. **The hard 30% is not bionic symbols; it is the linker.** Namespaces,
   `ld.config.txt`, APEX mount layout and `public.libraries.txt` are the
   "Android-ness" that a flat `search_paths` loader deliberately does not have.
   Steps 1–8 were ~3 hours; step 9 is the multi-week item.
4. **Cheap, valuable fallbacks discovered on the way** (worth landing in ACOS
   regardless of the approach chosen): the real APS2 decoder (upstream is wrong
   for multi-group sections), TLSGD/TLSLD support, the `sysconf` numbering
   table, `dlsym(RTLD_NEXT)`, `_Unwind_*`/`isw*`/zlib/libgcc forwarding, and the
   `ld-android.so`-is-a-stub rule.
