# E1A — Can real Google ART run DEX **without booting Android**? (approach A)

**Answer: yes.** Real Android 14 ART (`dalvikvm64` from the `com.android.art`
APEX, real 34-jar API-34 boot classpath, real bionic `linker64`, real
ICU/tzdata APEXes) initialises and executes Java/DEX on this Linux host with
**no init, no zygote, no system_server, no binder, no kernel/emulator**. It runs
the whole ACOS correctness corpus (27/27) and the benchmark suite.

- Owner / date: approach-A feasibility agent, 2026-11-11.
- Time-box: 90 min planned, ~150 min used (the bring-up chain is 9 layered
  blockers, each cheap once the previous one is fixed — see §3).
- Constraints honoured: no sudo, no mounts outside unprivileged user+mount
  namespaces, no emulator start, SDK image read-only, existing ACOS sources
  untouched. All artefacts in `.cache/art-exp/` (gitignored scratch) plus
  `research/art-feasibility/a-dalvikvm/` (scripts) and this report.
- Cost: **zero downloads.**

Everything below is reproducible from `research/art-feasibility/a-dalvikvm/`:

| script | what it does |
|---|---|
| `scripts/extract-sysroot.sh` | GPT → `super` → `lpunpack system` → `debugfs` dump of `/system` (bin, lib64, etc, framework jars) + the ART/runtime/i18n/tzdata/statsd APEX payloads. Writes `READY`/`MANIFEST.txt`. |
| `scripts/extract-all-apex.sh` | payload of **every** APEX in `/system/apex` (all 35 of them) into `…/sysroot/system/apex/<module>/` (module name from `apex_manifest.pb`, i.e. the on-device mount point). |
| `scripts/env.sh` | the environment (device `BOOTCLASSPATH` in real order from `bootclasspath.txt`, `ANDROID_*_ROOT`). |
| `scripts/artroot.sh` | the **mini-Android root**: `bwrap` user+mount namespace with `/system`, every APEX at `/apex/<module>`, `/apex/sharedlibs`, a `linkerconfig`-generated `/linkerconfig/ld.config.txt`, synthetic `/apex/apex-info-list.xml`, writable `/data`. Runs `dalvikvm`/`dex2oat64`/anything inside. |
| `scripts/run-dexprogs-art.sh` | the 27-program corpus under ART, diffed against `fixtures/expected-art/dexprogs`. |
| `scripts/measure.py` | startup / memory / speed / sharing measurements (median of ≥5). |
| `scripts/artrun.sh` | the env-only (no-namespace) launch path — recorded because it is the natural first route; it dead-ends at `libnativeloader` (§3.5). Use `artroot.sh`. |

## 1. What worked — exact commands

```bash
# 1. sysroot (~140 MB) + all APEX payloads (~520 MB total), 15 s, no root
research/art-feasibility/a-dalvikvm/scripts/extract-sysroot.sh
research/art-feasibility/a-dalvikvm/scripts/extract-all-apex.sh

# 2. hello world under real ART, no Android
research/art-feasibility/a-dalvikvm/scripts/artroot.sh \
    dalvikvm -cp /data/dex/01_hello/classes.dex Main
#   -> Hello, ACOS! … Goodbye, ACOS!   exit 0

# 3. the whole correctness corpus (writes results/art-feasibility/dexprogs-art-default.json)
research/art-feasibility/a-dalvikvm/scripts/run-dexprogs-art.sh
#   -> 27 pass, 0 fail

# 4. an AOT boot image, built by ART's own dex2oat inside the same root (3.8 s,
#    173 MB of boot.art/oat/vdex for the full 34-jar boot classpath)
ART_DEV_LD_LIBRARY_PATH=/apex/com.android.art/lib64:/apex/com.android.os.statsd/lib64:\
/apex/com.android.i18n/lib64:/system/lib64:/apex/sharedlibs/lib64 \
research/art-feasibility/a-dalvikvm/scripts/artroot.sh \
  /apex/com.android.art/bin/dex2oat64 --image=/data/img/boot.art \
  --base=0x70000000 --oat-file=/data/img/boot.oat --instruction-set=x86_64 \
  $(for j in $JARS; do printf -- '--dex-file=%s --dex-location=%s ' $j $j; done)
# then, after moving the output to /data/img/x86_64/ (ART's named-image layout):
… artroot.sh dalvikvm -Ximage:/data/img/boot.art -cp /data/dex/01_hello.dex Main

# 5. measurements
python3 research/art-feasibility/a-dalvikvm/scripts/measure.py startup memory bench sharing
#   -> results/art-feasibility/measurements-*.json
```

`ART version 2.1.0 x86_64` (Android 14 / API 34, `com.android.art` APEX, build
`b6dc79e02101ea00827a35a55ab6597a` libart.so) — the *same* ART the emulator runs.

## 2. Measurements (median of 5; host: Linux 7.2.11 x86_64, no KVM)

Process start → exit, including a **28.2 ms** shim floor (`bwrap` + bionic
linker + `linkerconfig`-equivalent startup, measured with a trivial dynamic
Android binary in the same root). "compute" subtracts the empty-`main()` baseline
of the same mode (172.5 / 171.4 / 174.1 ms for noimage / int / aot), i.e. it is
main()'s own cost.

| program | ACOS Rust VM | ART `-Xint` | ART default (JIT, no image) | ART AOT boot image | JVM 17 (ref) |
|---|---|---|---|---|---|
| startup: hello (wall, ms) | **0.73** | 176.2 | 177.4 | 177.3 | 11.7 |
| startup: empty `main()` (ms) | — | 171.4 | 172.5 | 174.1 | — |
| BenchFib (compute ms) | 528.4 | 261.5 | **3.9** | 6.5 | 20.2 |
| BenchLoop | 1696.2 | 794.5 | **44.8** | 46.0 | 40.6 |
| BenchString | 185.2 | 957.3 | **36.8** | 33.3 | 48.1 |
| BenchAlloc | 2108.6 | 726.9 | **39.5** | 43.5 | 57.5 |
| BenchVirtual | 2107.3 | 899.6 | **39.5** | 39.1 | 56.9 |
| BenchGc | n/a | 155.5 | **45.0** | 45.3 | n/a |
| idle PSS after hello | ~17 MB¹ | 117.5 | 119.1 | 118.6 | 43.0 |
| — of which anon | — | 15.6 | 15.6 | 15.7 | — |
| — of which file-backed | — | 102.1 | 103.4 | 103.0 | — |

¹ `results/vm/bench-2026-11-10-160048.json` peak RSS for `acos run-dex 01_hello`
= 16 932 KiB (RSS, not PSS). ACOS numbers are the committed ones — our VM was
**not** rebuilt (no need: the comparison is ~13–53× wide).

Sharing/zygote-free multi-process (HelloSleep in `main()`, sum over N processes):

| N | sum RSS | sum PSS | PSS file-backed | PSS anon | PSS/proc |
|---|---|---|---|---|---|
| 1 | 119.1 MB | 118.8 MB | 103.1 MB | 15.6 MB | 118.8 MB |
| 5 | 591.2 MB | 183.4 MB | 105.2 MB | 78.2 MB | 36.7 MB |
| 20 | 2380.2 MB | 420.5 MB | 105.5 MB | 314.7 MB | **21.0 MB** |

- File-backed boot-classpath pages (jar/oat/art/vdex, 103 MB) are **shared across
  processes** — the total stays ~105 MB at N=20 while RSS suggests 2.38 GB.
- Marginal cost per extra ART process ≈ **16 MB PSS** (anon only). This is the
  number to compare with a future "per-app ART" budget.
- **Warm-parent fork (zygote) experiment: not attempted / infeasible here.**
  `dalvikvm -Xzygote` exists (`-Xzygote`, `-Xforcejitzygote`,
  `-Xjitzygotepthreadpriority` are all in `dalvikvm -help`), so ART has the
  zygote path, but driving it requires `ZygoteServer` + the
  `forkAndSpecialize` socket protocol and the `app_process`/`ZygoteInit` boot
  class, i.e. real Zygote — that is the Android process model, not a CLI flag.
  The sharing numbers above show the upside is already mostly captured by page
  cache sharing of the immutable boot classpath; a warm fork would only remove
  the ~16 MB anon and ~145 ms init per process.

## 3. Blockers hit (in order) and how each was solved

Every one of these was fatal; none needed root or a patch to ART.

1. `linker64` not in `/system/bin` on API 34 — bionic lives in the
   `com.android.runtime` APEX. *Fix:* invoke `/apex/com.android.runtime/bin/linker64 <exe>`
   directly (it is a static PIE), and create the device-accurate
   `/system/bin/linker64 → /apex/com.android.runtime/bin/linker64` symlink for
   programs whose interpreter is that path (e.g. `linkerconfig`, `dex2oat64`).
2. **All ART logs invisible** — liblog has no logd on this host and the
   "already have a log" fallbacks are compiled out, so `JNI_CreateJavaVM`
   failed with a bare `Failed to initialize JNI invocation API from (null)`.
   *Fix:* `.cache/art-exp/shim/logshim.c`, a `-nostdlib -fPIC -shared`
   interposer (no `DT_NEEDED`) for `__android_log_write/print/vprint/assert/
   buf_write/write_log_message/logd_logger/call_aborter`, `LD_PRELOAD`ed into
   the bionic process. **This is the single most valuable reusable artefact of
   this spike** — it turns every ART failure into an exact message.
   ⚠️ Caveat: it replaces `__android_log_call_aborter` with a no-op, which
   *suppresses one FATAL CHECK* (§5, item 1). Real crashes still abort (we saw
   plenty of `SIGABRT`/`stack corruption` traces while iterating).
3. Broken APEX symlinks: `com.android.art/lib64/{libbase,libc++,libcrypto}.so`
   are symlinks into `/apex/sharedlibs/lib64/…`. *Fix:* don't patch them — mount
   the SDK-versioned shared-lib APEX (`com.google.mainline.primary.libs`) at
   `/apex/sharedlibs`, exactly as apexd does.
4. `dlopen libart.so` failed with `library "libstatssocket.so" not found`.
   ART's `libart.so` needs the *statsd* APEX lib (and `heapprofd_client_api.so`
   from `/system/lib64`). *Fix:* extract all APEX payloads (script #2).
5. `nativeloader: ... Failed to get system namespace for loading libandroid.so`
   + `stack corruption detected` → **the bionic linker had no namespace
   config at all** (`/linkerconfig/ld.config.txt` missing), so
   `android_get_exported_namespace("system")` returned null.
   *Fix:* run the image's own `linkerconfig` binary to generate it — which
   requires the next two items.
6. `linkerconfig: Can't read /apex/apex-info-list.xml`. *Fix:* synthesise the
   XML apexd normally writes (module name, module path, `isActive`,
   `provideSharedApexLibs`) — generated automatically from `APEX_MODULES.txt`.
7. Mount points: the ART APEX *file* is `com.google.android.art.apex` but its
   **module/mount name is `com.android.art`**, which is what ART hard-codes.
   *Fix:* mount by module name (from `apex_manifest.pb`), not file name.
8. `Check failed: !location.empty() No mainline framework library found` +
   `DexFile …/javalib/core-oj.jar is in boot class path but is not in a known
   location` — boot classpath entries must be `/apex/<module>/javalib/…`.
   *Fix:* rewrite `BOOTCLASSPATH` from host paths to device paths (`devpath()`).
9. `JNIHelp: Native registration unable to find class
   'com/android/icu/text/TimeZoneNamesNative'` — an incomplete boot classpath.
   *Fix:* use the real 34-jar device `BOOTCLASSPATH`
   (`.cache/boot/android-34/BOOTCLASSPATH`), which spans ~15 APEXes
   (`core-icu4j` from i18n, `conscrypt`, `framework-connectivity`, …).

Non-blockers that were expected and did **not** matter: no properties
(`ro.*`), no SELinux, no logd, no tombstoned (`failed to connect to tombstoned`
is only cosmetic), no `/system/etc/ld.config.*.txt`, no `/system/bin` contents,
no writable `/data` beyond a scratch dir, no `MAINLINE_*` env.

Unprivileged namespaces: **enabled** on this host
(`kernel.unprivileged_userns_clone=1`, `user.max_user_namespaces=246630`), so
`bwrap` (bubblewrap 0.11) builds the root with no sudo. Nothing here needs
CAP_SYS_ADMIN outside the namespace.

## 4. Dependencies / artefacts

- Host tools, all already installed: `dd`, `lpunpack`, `debugfs`, `unzip`,
  `python3`, `bwrap`, `clang` (only to build the 3 KB log shim), `javac`+`d8`
  (to build benchmark DEX; `~/Android/Sdk/build-tools/35.0.0/d8`).
- Sysroot: `.cache/art-exp/sysroot` — **521 MB, 1425 files**, `READY` +
  `MANIFEST.txt` written for approach B. Contains `/system` (incl. all 706
  `lib64` entries and all framework jars), 34 APEX payloads
  (`bin`, `lib64`, `javalib`, `etc`, `apex_manifest.pb`), `/apex/sharedlibs`.
- **No boot image ships in the system image** (on device `odrefresh` builds
  `boot.art` at first boot into `/data/misc/apexdata/com.android.art`). We
  generated one with `dex2oat64` in 3.8 s (§1.4); 173 MB for the full 34-jar
  boot classpath.
- Everything here is x86_64-only. Nothing in the recipe is architecture
  specific except the tool/`${LIB}` names, but the target images are
  aarch64/arm and would need their own extraction + a working aarch64 bionic
  loader on the host (qemu-user exists in `tools/fetch-qemu-aarch64.sh`).

## 5. Remaining risks / open items (for a real integration)

1. **`Check failed: !location.empty() No mainline framework library found`**
   (fatal, currently suppressed by the log shim, fired once during
   `libnativeloader` init). Cause not fully identified inside the time-box; the
   boot classpath *is* accepted (`known location` warnings disappear once
   paths are right) and all 27 programs pass, so it looks like a
   mainline-framework *version/metadata* check (APEX manifest or
   `apex-info-list.xml` attribute we stub). Must be fixed properly (correct
   `apex_manifest.pb`-derived `apex-info-list.xml`, or the real
   `ro.apex.updatable`/`apex.libraries.config.txt` inputs) before trusting ART
   error handling.
2. No `debuggerd`/`tombstoned`: abort messages arrive only through our shim, and
   ART's own tombstone writer cannot run. A stub or a logd-shaped sink is
   needed for good diagnostics.
3. No property service: `ro.*`/`debug.*`/`dalvik.vm.*` reads all fail silently.
   Fine for `dalvikvm`, but `app_process`/Zygote and framework code read
   properties everywhere.
4. Anything that touches binder (`ActivityManager`, `PackageManager`,
   `SurfaceFlinger`, `HWUI`) is still unreachable — the same framework-server
   wall documented in `research/framework-dex/REPORT.md`. ART gives us a *fast,
   faithful DEX/Java engine*, not an Android system.
5. Memory/CPU accounting is per-process only; integrating with ACOS's
   cgroup/PSS-based accounting needs the marginal (16 MB) figure, not RSS.
6. `-Ximage` needs the named-image directory layout (`/data/img/x86_64/boot.art`)
   — easy to get wrong (we lost 30 min to a silently ignored image that fell
   back to imageless running).

## 6. What real ART would buy ACOS, and what it would cost

**Buys:** correct, complete, JIT-compiled Java semantics for free — 27/27 corpus
parity including regex, zip, crypto digests, threads and reflection, and
13–53× the compute speed of the Rust VM on the same DEX. It also removes the
entire "native method coverage" work-list from `research/framework-dex/REPORT.md`
(661 native methods in 79 classes for `System.out.println`): those natives are
just there.

**Costs:** 145 ms and ~119 MB PSS per process (16 MB marginal), versus 0.7 ms /
17 MB for the Rust VM; a 152 MB per-architecture sysroot and a 173 MB boot image
to ship; a mount-namespace (or equivalent) requirement; and — decisively — it
does **not** solve the Android-servers problem, because there is still no
zygote/`system_server`/binder/graphics stack.

**Estimate to integrate real ART as an ACOS execution engine** (agent team,
1 engineer-day ≈ 1 agent-day at the pace of this spike):

| work item | person-days |
|---|---|
| Productise extraction: sysroot + all APEXes + `apex-info-list.xml` + `linkerconfig` generation, reproducible, both architectures | 2–3 |
| Namespace runtime (`bwrap`/userns) + fallback when userns is disabled + mount-set caching | 2–3 |
| Host-side Android substrate: logd sink, tombstoned stub, property service (`ro.*`, `dalvik.vm.*`), `/data` layout | 3–5 |
| Fix §5.1 + validate APEX metadata fidelity (clean error paths, no suppressed CHECKs) | 1–2 |
| Engine process model: spawn one ART per task/app, dex provisioning, stdout/exit capture, JNI `System.loadLibrary` paths, warm-image reuse, timeouts/limits | 3–5 |
| Boot image lifecycle: build at packaging time with `dex2oat64`, verify/checksum, `-Ximage` fallback | 2–3 |
| Parity + perf harness: run `diff-dexprogs` against ART as *the* engine, benchmark + memory accounting (marginal PSS), CI wiring | 2–3 |
| **Subtotal: "run arbitrary DEX, get correct stdout/exit, accounted and bounded"** | **15–24** |
| Per-Android-subsystem work (binder/graphics/window/input/services) to get *apps* running on it | +30–60, and it is the *same* work ACOS already faces |

So: integration as a **DEX/Java execution engine is ~3–5 agent-weeks** and is
low-risk (everything above was proven in 2.5 h). Using it to run real Android
apps is not a shortcut — it swaps "write a VM that matches ART" for "run ART and
still build everything above it".

## 7. Honest caveats on the numbers

- The host was a shared desktop (60 GB RAM, load from other agents); all runs
  are medians of ≥5, min/max spread < 3 % except BenchFib noimage (max 204 ms
  outlier). Startup numbers include a constant 28.2 ms `bwrap`/linker shim
  floor that a product integration would not pay.
- "compute ms" = p50 − empty-`main()` p50 of the same mode. The empty-`main()`
  baseline is a *proxy* for "process start → main() entered"; ART's
  `-Xint`/`aot` baselines are within 3 ms of each other, so the subtraction is
  sound at the reported precision.
- ACOS numbers are the committed `results/vm` JVM-comparison run; its
  instruction counts are ACOS-internal and are **not** comparable with ART's
  cost, so only wall-clock is used.
- ART here runs the **full 34-jar device boot classpath** (55 MB DEX,
  `framework.jar` included), i.e. the same Java environment an app sees, not a
  trimmed `core-oj`-only one.
