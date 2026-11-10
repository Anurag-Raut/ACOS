# ACOS — Findings log

**Append-only.** Dated findings that changed or confirmed the design. Each entry
cites the file/command that verifies it. Do not rewrite history; add a new entry
if a finding is superseded.

Format: `### F-NNNN — <title> — <date>` then **Finding**, **Evidence**,
**Design impact**.

---

### F-0001 — The legacy ACVM AOSP track is frozen; no reusable app corpus — 2026-11-10

**Finding.** The legacy engine `/home/Qui/Work/android-container` is frozen at
`main` = `d005a0d` ("docs: ACVM Lite direction (own runtime), AOSP track
archived"); tag `aosp-track-final` = `31da75a`. The handoff
(`docs/handoff/05`) is numerically stale: test counts (272/281 vs latest 308 +
138), and U2/R2/W2b/D3/D4 are **not merged to `main`** (branch-only, verified by
`git merge-base --is-ancestor`). No committed third-party app corpus exists (the
P2b 23-app set is absent; only a 10-app result JSON survives); the helper APKs
use the research AOSP platform test key. Legacy per-session memory is dominated
by the app renderer's native heap (~88 MiB/session; one fixed 75,296,768 B
SwiftShader/Vulkan mapping), not by Android user bookkeeping.

**Evidence.** `docs/reference/ACVM_INVENTORY.md` §1 (reconciliation table),
§2 (measured numbers), §3 (fixture inventory); `git -C
/home/Qui/Work/android-container log --oneline -1` → `d005a0d`;
`git tag -l` → `aosp-track-final`.

**Design impact.** The legacy engine is a frozen oracle/baseline, not a live
track. ACOS must author its own licensed fixtures. The renderer is the memory
lever, confirming ADR-0008/ADR-0011.

---

### F-0002 — The real boot-classpath DEX is obtainable unprivileged; all v039, unstripped — 2026-11-10

**Finding.** The installed SDK image
(`~/Android/Sdk/system-images/android-34/google_apis/x86_64`) contains a GPT disk
whose `super` partition holds the ext4 logical partitions. Using only installed,
unprivileged tools (`dd`, `lpunpack`, `debugfs`, `unzip`, `python3`), the full
boot classpath can be extracted with no downloads, no mounts and no sudo. All 24
extracted dex files are `dex\n039` except the fixture app (`038`); **no cdex /
compact-dex**; the jars are **not dex-stripped** (the image also ships vdex/odex,
not needed). Total real boot classpath: **17 dex / 55.5 MiB, 41,829 classes,
328,352 methods, 5,607 native methods**.

**Evidence.** `research/framework-dex/REPORT.md` §Q1–Q2; `tools/extract-bootclasspath.sh`;
`.cache/boot/android-34/{BOOTCLASSPATH,manifest.json,jars/}` (12-jar boot order
`core-oj … core-icu4j`).

**Design impact.** ADR-0007 is feasible without a download. ADR-0016's
runtime-only, user-provided-image posture is available today. The prototype
(`userdebug`) caveat is recorded as an open licence/feasibility item.

---

### F-0003 — Native work-list for the first milestones is finite — 2026-11-10

**Finding.** Static reachability over the real API-34 DEX gives a bounded Rust
native work-list: `System.out.println("x")` needs **152 natives / 39 classes**
(lower bound) to **663 / 79** (CHA over-approx); a minimal Activity
(`<init>+attach+performCreate+setContentView(inflate LinearLayout/TextView/
EditText/Button)+measure/layout/draw`) needs **612 natives / 105 classes** lower
to **2,270 / 300** upper. Of the 612, **175 core, 419 `android.*`, 18 other** —
about two-thirds is Android graphics/widget/resource natives. A passive Activity
launch reaches ~10 AIDL interfaces (`IActivityManager`, `IPackageManager`,
`IWindowSession`, `IWindowManager`, `IDisplayManager`, `IAccessibilityManager`,
`IInputMethodManager`, `IGraphicsStats`, `IServiceManager`, …), all satisfiable
by an in-process Rust stub for a deterministic task.

**Evidence.** `research/framework-dex/REPORT.md` §Q3–Q4;
`research/framework-dex/data/{println_lower,println_upper,activity_lower,activity_upper,aidl}.txt`.

**Design impact.** ADR-0007/0008 have a credible first target. The honest
progress metric is the **first `unsupported` native per fixture**, not class
counts. Graphics/widget/resource natives dominate the real work (ADR-0011).

---

### F-0004 — `acos-dex` disassembly matches `dexdump` exactly on the committed fixtures — 2026-11-10

**Finding.** `acos-dex`'s per-method instruction counts and opcode mnemonic
sequences match the SDK `dexdump -d` on all three committed fixture DEX files:
**3 files, 380 methods, 4,253 instructions, 0 mismatches**.

**Evidence.** `crates/acos-dex/tests/diff_dexdump.sh`; re-run on 2026-11-10 with
the prebuilt `target/debug/examples/dexdump`
(`DEXDUMP_BIN=… bash crates/acos-dex/tests/diff_dexdump.sh`) →
`files=3 methods=380 instructions=4253 mismatches=0`. Also asserted by the
commit message of `5945a57`.

**Design impact.** The DEX decoder is trustworthy at instruction level for the
fixture corpus. Boot-classpath coverage is env-gated
(`ACOS_BOOT_JARS=…`, `crates/acos-dex/tests/boot_jars.rs`) and pending.

---

### F-0005 — Boot-classpath DEX has SHA-1 signatures that do not validate — 2026-11-10

**Finding.** Every boot-classpath dex initially failed the DEX SHA-1 signature
check with `DEX_BAD_SIGNATURE`. Android does not verify the DEX SHA-1 signature
at load time (platform hiddenapi tooling rewrites boot dex without updating it),
so rejecting on mismatch is stricter than Android and blocks the real boot
classpath. `acos-dex` now follows Android semantics: **Adler-32 enforced,
SHA-1 recorded (`signature_valid`) but accepted by default**, with
`ParseOptions::STRICT` to enforce it.

**Evidence.** `crates/acos-dex/src/lib.rs` (`ParseOptions`,
`signature_valid`, `BadSignature`); `crates/acos-dex/tests/boot_jars.rs`
(`signature_mismatches_accepted`). Recorded as ADR-0017.

**Design impact.** ADR-0017. Any strict/lenient policy must be explicit in
results; `signature_valid=false` is reported, never hidden.

---

### F-0006 — `/usr/bin/time` is not installed; measure via `/proc` — 2026-11-10

**Finding.** The host has no `/usr/bin/time` (`ls` → "No such file or
directory"). Any benchmark or tool that shells out to `/usr/bin/time` will fail.
Time/CPU/memory must be measured from `/proc` (e.g. `/proc/<pid>/status`,
`/proc/<pid>/stat`, cgroup files) or shell builtins.

**Evidence.** `ls -la /usr/bin/time` → absent (2026-11-10).

**Design impact.** Benchmark tooling must not depend on `/usr/bin/time`
(`BENCHMARKS.md` §5). Matches the legacy `bench/lib/harness.py` approach.

---

### F-0007 — `/tmp` is RAM-backed; large artifacts go to `.cache/` — 2026-11-10

**Finding.** `/tmp` is a **31 GiB tmpfs** (`findmnt -no FSTYPE /tmp` → `tmpfs`).
Large transient artifacts (extracted boot jars, dex, APK build intermediates)
stored under `/tmp` consume RAM and can trip the 85 % host guard.

**Evidence.** `findmnt -no FSTYPE,SOURCE /tmp` → `tmpfs tmpfs`;
`df -h /tmp` → 31 GiB, 2 % used (2026-11-10).

**Design impact.** Repo policy: large/generated artifacts live under
`.cache/` (gitignored) or `fixtures/build/` (gitignored), not `/tmp`. The
boot-classpath extractor writes `.cache/boot/android-34/` and cleans its
`.work/` intermediates; the DEX differential uses a small `mktemp` under `/tmp`
for tiny dex only.

### F-0008 — `core.os` natives: `Build`'s identity comes from init-derived properties — 2026-11-10

**Finding.** The extracted `.cache/boot/android-34/build.prop` is the **system
partition's** file, so it stores no `ro.build.fingerprint` and no
`ro.product.cpu.abilist*`: init derives the fingerprint from
`ro.system.build.fingerprint` and the abi lists from
`ro.system.product.cpu.abilist{,32,64}` (the legacy `ro.product.*` aliases come
from `ro.product.system.*` the same way). Serving the file verbatim yields
`Build.FINGERPRINT == ""` and `Build.SUPPORTED_ABIS == [""]`, while the reference
emulator reports the `device-config.json` fingerprint and `[x86_64,
arm64-v8a]`. The property seed (`crates/acos-vm/src/natives/os_props.rs`)
reproduces init's derivations and records per-key provenance (`BuildProp` /
`Derived` / `ArtCapture`), so `Build.SUPPORTED_32_BIT_ABIS` is `[]` (abilist32
is empty) exactly like the ART path.

Two related non-obvious points from the same task:

1. `research/service-layer/natives_plan.tsv` suggests `nativeGetEnabledTags`
   return `0x2a` "for compat". That makes `Trace.isTagEnabled` true for tags
   1/3/5 and sends framework code down trace paths no sink records. ACOS
   returns `0` (nothing traced) — honest for a VM without systrace/Perfetto.
2. `Process.myPid/myTid/myUid` are **not** natives: their bytecode calls
   `android.system.Os.getpid/gettid/getuid` → `libcore.io.Linux`. The app uid
   (10192) and the kernel tid therefore have to come from the `Linux` natives
   (`natives/os_misc.rs` re-registers them), not from `Process`.

**Evidence.** `grep -n '^ro.build.fingerprint\|^ro.product.cpu.abilist'
.cache/boot/android-34/build.prop` → no match; `grep -n
'ro.system.build.fingerprint\|ro.system.product.cpu.abilist'
.cache/boot/android-34/build.prop` → present;
`fixtures/reference/hello-views/art-api34-x86_64/device-config.json`;
`dexdump -d .cache/boot/android-34/dex/framework/classes3.dex` on
`android.os.Build$VERSION.<clinit>` / `android.os.Process.myUid`;
`cargo test -p acos-vm --test os_natives` (java-level assertions).

**Design impact.** Property seeding is not a flat `build.prop` parse: init's
derivation rules are part of the compatibility surface for `Build.*` and
`android.sysprop.*`. `ro.product.*` still comes from the system partition here
(`MODEL = mainline`, `DEVICE = generic`); a full image would take it from
`vendor`/`odm`, which the boot cache does not extract.

### F-0009 — The DEX class loader must publish a class only after it is complete — 2026-11-10

**Finding.** `load_dex_class` inserted the class descriptor into
`class_by_desc` at the very start of loading, before the superclass/interfaces,
fields, methods and vtable existed. The W8-A note recorded one non-recurring
`UNSUPPORTED_JAVA_API: Landroid/os/Build;->-$$Nest$smgetString` at
`Build$VERSION.<clinit> pc=3`: the nest-bridge is a d8 synthetic in
`android.os.Build`, so an `invoke-static` during `Build$VERSION.<clinit>`
resolves the host class on demand. If that load had failed part-way (or been
re-entered), the partial `Build` stayed visible and `find_method` failed,
because `invoke` maps any `load_class`/`find_method` error to
`UNSUPPORTED_JAVA_API` naming the call site. The loader is now atomic:
`load_dex_class` builds through `create_class` (no publication), guards
re-entry with a `loading_classes` descriptor set, and inserts into
`class_by_desc` only after `link_class` succeeds. A re-entrant load of a class
that is still being built is reported as `cyclic class load: <desc>` instead
of returning a half-linked class; DEX class hierarchies are acyclic, so this
is always a bug.

**Evidence.** `crates/acos-vm/src/vm.rs` `create_class` / `load_dex_class` /
`load_dex_class_body` (publication moved past `link_class`);
`crates/acos-vm/tests/loader_stress.rs` loads and initialises `Build`,
`Build$VERSION`, `Build$VERSION_CODES`, `Build$Partition` and cross-jar
framework classes in every rotation, asserting the nest bridge and the direct
method map after each step, and repeats the nested initialisation 32x under
`ACOS_GC_STRESS`; `cargo test -p acos-vm` (all suites pass);
`tools/diff-dexprogs.sh --corelib real` 20/20 normal and 20/20 `--gc-stress`.
The original transient was not reproduced in the stress runs (it needed
fresh-VM, varied-order, GC-stress conditions), so the fix is a code invariant,
not a captured interleaving.

**Design impact.** Lazy class loading is transactional with the descriptor
map as the single publication barrier: nothing may observe a class whose
members are still being added. Future lazy providers (snapshot restore,
additional jars) must preserve this — build first, publish last.

## 2026-11-10 — Owner directive D5: V1 is not the end; dogfood with real agents

The owner requires that ACOS keep being refined after version 1 as a real product. After V1, "dogfood" waves are mandatory: subagents act as Android-developing agents (write app code, build the APK, run `acos verify`, read evidence, fix, repeat) using every ACOS feature, and the friction they hit becomes first-class backlog. Source: owner message, 2026-11-10. Recorded by the lead.

## 2026-11-10 — Measured dynamic work-lists (research/dynamic-trace/REPORT.md)

Real ART method tracing replaced the static estimates: println needs 21 natives (30 across three console programs), not 152-663. Activity launch of hello-views: 7,018 methods, 379 natives (92 core, 287 android.*), 51 outgoing binder transactions over 19 interfaces, 1,128 classes executed of which 95.4% come from zygote's preloaded-classes (ACOS has no zygote: snapshotting must reproduce that preloaded state). Real ART cold launch p50 466 ms / p95 536 ms, warm 17 ms; idle PSS ~29.7 MiB; first draw ~179 ms into the trace. Consequence: the native and service work for the first Activity is hundreds of functions, not thousands; snapshot/restore (ADR-0010) is confirmed as the main lever because most launch work is class init that zygote does ahead of time on Android.

## 2026-11-10 — First execution: interpreter baseline (results/vm/bench-2026-11-10-141049.json)

acos-vm runs all 20 fixture programs byte-identical to real ART (fixtures/expected-art) in normal and GC-stress modes. This used hand-written Rust core-library built-ins, so the 0.68 ms / 17 MiB startup for 01_hello (host JVM: 12 ms / 51 MiB) does NOT include booting the real core library and must not be quoted as a startup result. Interpreter throughput is a gap: 15-93 M DEX instructions/s; vs host `java -Xint` it is 6x slower on BenchFib (2201 vs 338 ms), 15x on BenchLoop (5908 vs 388 ms), 6x on BenchAlloc, equal on BenchString; vs JIT 100-250x slower. The service-layer estimate (research/service-layer/REPORT.md) assumed 50 M units/s for the 3.2 M-unit launch workload (64 ms); at 15-24 M/s that is 130-210 ms, still inside real ART's 466 ms cold launch but too close for the interaction path and for snapshot-less class init. Decision: an interpreter performance track runs in parallel with the real-core-library track (ADR-0013 step 1 pulled forward).

## 2026-11-10 — M1 reached: real Android 14 core library on the Rust VM

All 20 console programs run on the real core-oj/core-libart/core-icu4j bytecode (439 Rust natives, 8 modules) and match real-ART stdout and exit code byte for byte, normally and under GC stress. 01_hello: 23 ms wall (14 ms VM bring-up incl. boot-classpath parse, 8 ms run), 36 MiB peak RSS, 196 classes loaded, 46 <clinit>, 42,550 instructions, 8,227 invokes; real ART dalvikvm cold hello was 71 ms (different host, ART has a boot image: indicative only). A real String/Thread bridge was needed because the real String's constructors and many methods are VM-implemented in ART. Next critical path: boot the framework classes and the Binder transport (T-18..T-27).

## 2026-11-10 — Runaway acos process reached 45 GB RSS (host-safety incident)

During the parallel W9/W10 waves a debug `acos run-dex .../01_hello/classes.dex --main Main` was found after ~2.8 h with 45 GB RSS (host 62 GB, no swap, ~91% used). It was killed manually; no crash. There was no host-side memory or time ceiling in the VM (only the guest ACOS_HEAP_MAX). Actions: temporary watchdog (kills acos processes >8 GB or when MemAvailable <9 GB, log in /tmp/acos-ramguard.log), W12 to find the root cause and add VM-side limits and tools/guarded.sh. Policy recorded in DECISIONS.md ADR-0018.

**Root cause (W12, 2026-11-10).** A semantic regression from the vm-perf merge
(`2563a86`). The rewrite of `crates/acos-vm/src/interp.rs::run_batch` omitted
`ThreadStatus::Parked` from its scheduler-yield condition, and `interp::run`
unconditionally set the current thread back to `Runnable` on its periodic
pre-emption (`next_yield`). A looper thread parked in
`MessageQueue.nativePollOnce(-1)` was therefore picked up again every 8192
instructions and never reached the blocking `pump` in `vm::run_inner` — a 100%
host-CPU spin with unbounded execution. It only manifests when the VM holds more
than one thread (`vm.threads.len() > 1`); `LooperBlockMain` (one thread) blocks
correctly, which is why the existing `looper_cpu` test was green. Because the VM
had no host-side ceiling, the spin's execution (plus any guest/heap work) could
grow the host process far past the 256 MiB guest heap without ever tripping
anything.

**Evidence.** Before the fix, `target/debug/acos run-dex LooperBlockMT
--corelib real` consumed 997 cs of CPU in 10 s (100% of a core) with flat RSS;
the regression test `crates/acos-vm/tests/looper_cpu_mt.rs` fails `308 -> 808`
CPU ticks when the original `run_batch`/`run` code is restored and passes after
the fix. `01_hello` itself exits normally, so the reported command alone does
not reproduce; a stuck W10 agent/test process that parked a looper was the
unbounded one.

**Fix.** Yield on `Parked` in `run_batch`; only reset the current thread to
`Runnable` when it is still `Running`. Product-side `limits::HostGuard` now
enforces `ACOS_HOST_RSS_MAX` (`HOST_MEMORY_LIMIT`) and `ACOS_WALL_MAX_SECS`
(`TIMEOUT`) independently of the guest heap (ADR-0018, `docs/AGENT_API.md`
§16), and `tools/guarded.sh` / `tools/ramguard.sh` bound and watch every run.
