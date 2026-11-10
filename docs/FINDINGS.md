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

## 2026-11-10 — Owner directive D5: V1 is not the end; dogfood with real agents

The owner requires that ACOS keep being refined after version 1 as a real product. After V1, "dogfood" waves are mandatory: subagents act as Android-developing agents (write app code, build the APK, run `acos verify`, read evidence, fix, repeat) using every ACOS feature, and the friction they hit becomes first-class backlog. Source: owner message, 2026-11-10. Recorded by the lead.

## 2026-11-10 — Measured dynamic work-lists (research/dynamic-trace/REPORT.md)

Real ART method tracing replaced the static estimates: println needs 21 natives (30 across three console programs), not 152-663. Activity launch of hello-views: 7,018 methods, 379 natives (92 core, 287 android.*), 51 outgoing binder transactions over 19 interfaces, 1,128 classes executed of which 95.4% come from zygote's preloaded-classes (ACOS has no zygote: snapshotting must reproduce that preloaded state). Real ART cold launch p50 466 ms / p95 536 ms, warm 17 ms; idle PSS ~29.7 MiB; first draw ~179 ms into the trace. Consequence: the native and service work for the first Activity is hundreds of functions, not thousands; snapshot/restore (ADR-0010) is confirmed as the main lever because most launch work is class init that zygote does ahead of time on Android.

## 2026-11-10 — First execution: interpreter baseline (results/vm/bench-2026-11-10-141049.json)

acos-vm runs all 20 fixture programs byte-identical to real ART (fixtures/expected-art) in normal and GC-stress modes. This used hand-written Rust core-library built-ins, so the 0.68 ms / 17 MiB startup for 01_hello (host JVM: 12 ms / 51 MiB) does NOT include booting the real core library and must not be quoted as a startup result. Interpreter throughput is a gap: 15-93 M DEX instructions/s; vs host `java -Xint` it is 6x slower on BenchFib (2201 vs 338 ms), 15x on BenchLoop (5908 vs 388 ms), 6x on BenchAlloc, equal on BenchString; vs JIT 100-250x slower. The service-layer estimate (research/service-layer/REPORT.md) assumed 50 M units/s for the 3.2 M-unit launch workload (64 ms); at 15-24 M/s that is 130-210 ms, still inside real ART's 466 ms cold launch but too close for the interaction path and for snapshot-less class init. Decision: an interpreter performance track runs in parallel with the real-core-library track (ADR-0013 step 1 pulled forward).
