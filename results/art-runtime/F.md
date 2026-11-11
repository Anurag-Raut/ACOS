# F — ART substrate productised: `tools/art-sysroot.sh` + `crates/acos-art`

**Status: the launcher runs the whole DEX correctness corpus on real Android 14
ART — 27/27, with structured results, a property service and a logd sink. Two of
the five work items are partial and one is documented-not-fixed (see the table in
§1).** Owner: implementer, 2026-11-11. Basis: `results/art-feasibility/E1A.md`
(route A), `E1C.md`, `docs/DECISION_ART_RUNTIME.md`.

Everything below is reproducible with no downloads, no sudo and no mounts outside
unprivileged user+mount namespaces.

## 0. What exists now

| path | what it is |
|---|---|
| `tools/art-sysroot.sh` | idempotent extraction: GPT → `super` → `lpunpack system` → `debugfs` `/system` → every APEX payload (module-name dirs) → device `BOOTCLASSPATH` derived from the image's classpath fragments → **AOT boot image built by the image's own `dex2oat64`** → `MANIFEST.txt` + `READY`. `--force`, `--no-boot-image`, `--keep-work`, `--image REL`. |
| `crates/acos-art/src/lib.rs` | `ArtRoot` (sysroot/module table/`apex-info-list.xml`/`linkerconfig`/`BOOTCLASSPATH` mapping) + `Sandbox` (bwrap argv + env builder), `build_shim()`. |
| `crates/acos-art/src/launcher.rs` | `RunOpts`/`run()`/`RunResult`/`Outcome`/`classify()`: rlimits in the child, wall-clock timeout, process-group kill, `128+signo` unwrapping, stable codes. |
| `crates/acos-art/src/props.rs` | the property table (`/data/acos.properties`) the shim serves. |
| `crates/acos-art/src/logd.rs` | Rust `logd` sink on `/dev/socket/logdw` → structured `LogEntry {prio, tag, message}`. |
| `crates/acos-art/shim/artshim.c` | freestanding (`-nostdlib`, no `DT_NEEDED`) `LD_PRELOAD`: liblog → logd socket (+ stderr fallback) and `__system_property_*` over the table. Compiled on demand, cached by source hash. |
| `crates/acos-art/src/main.rs` | `run`, `verify` (the 27-corpus harness), `build-image`, `build-shim`, `props`, `doctor`. |

```bash
# extraction (fresh dir; ~4 min, ~570 MB, +140 MB boot image)
tools/art-sysroot.sh --out .cache/art/sysroot

# one DEX, structured result
cargo run -q -p acos-art -- run --dex fixtures/build/dexprogs/01_hello/classes.dex --class Main --json

# the corpus
cargo run -q -p acos-art -- verify --json-out results/art-runtime/verify-27.json
```

## 1. Task table

| task | pass | numbers | blockers / notes |
|---|---|---|---|
| 1. `tools/art-sysroot.sh`, idempotent, boot image from `dex2oat64` | **yes** (end-to-end run from a clean `--out`) | 33 APEX modules + full `/system` = **782 MB** (642 MB without the boot image), **35/35** boot-classpath jars resolved, `dex2oat64` boot image 140 MB in **3.6 s**; re-running reports "already extracted" in <1 s | `debugfs rdump` semantics cost two iterations (dest parent must exist, it creates `dest/<basename>`); APEX module dir names must come from `apex_manifest.pb`, not the `.apex` file name (`com.android.art` vs `com.google.android.art.apex`) — the SDK image also carries a couple of module names with stray whitespace (` com.android.configinfrastructure`), which the launcher now tolerates. |
| 2. `ArtLauncher`: bwrap argv/env, `run --dex … --class …`, structured result, rlimits, timeout, process-group kill, crash codes | **yes** | 27/27 from both sysroots (E1A scratch `.cache/art-exp/sysroot` and `tools/art-sysroot.sh` output `.cache/art/sysroot`; `verify-27.json`/`verify-27-fresh.json` are the committed runs), wall **146–187 ms**, median **152 ms**; codes `ART_CRASH`, `ART_FATAL_CHECK`, `ART_FATAL_LOG`, `ART_NO_CLASS`, `HOST_MEMORY_LIMIT`, `UNSUPPORTED_SYSCALL`, `TIMEOUT`, `NONZERO_EXIT` | `RLIMIT_AS` is **unusable for ART**: Scudo reserves ~8.6 TB of virtual address space at start-up, so any cap aborts with `Scudo ERROR: internal map failure (NO MEMORY)`. Default is now 0 (unlimited) and RSS/cgroup limits (`tools/guarded.sh`, `ACOS_HOST_RSS_MAX`) remain the budget mechanism. Also `dalvikvm64` must be launched via **`/system/apex/com.android.art/bin/dalvikvm64`**, not `/apex/com.android.art/...`: bionic keys the per-APEX namespace lookup off that path and otherwise falls back to the default namespace ("CANNOT LINK EXECUTABLE … libnativehelper.so not found"). |
| 3. properties so bionic `__system_property_get` works; Java-visible read | **yes** (interposer route) | `/data/acos.properties` (55 device properties: `ro.build.version.sdk=34`, `ro.debuggable=1`, `dalvik.vm.*`, …); NDK probe inside the same root prints `ro.build.version.sdk = "34"`, `ro.debuggable = "1"`, `dalvik.vm.heapgrowthlimit = "192m"`, `find(dalvik.vm.heapsize)` non-null, `read_callback` → `name=dalvik.vm.heapsize value=512m serial=0x1` | bionic 34 has **no** property-area creator: `/dev/__properties__/property_info` is a serialised context trie written by `init` (`ContextsSplit`/`PropertyInfoAreaFile`), plus one `u:object_r:<ctx>:s0` area per SELinux context. We therefore interpose the stable public API (allowed by the task: "area file writer *or* an interposer, whichever is simpler") and keep bionic's in-memory `prop_info` layout, so `__system_property_find` pointers, `->value` and `__system_property_serial` stay valid. Bit 24 of `serial` must stay clear (it is bionic's long-property flag) — getting this wrong makes every read return "". |
| 4. tiny Rust logd on `/dev/socket/logdw`, structured entries, replacing the 3 KB interposer | **partial** | every run now yields structured entries: 7 baseline entries, 12/26/68 for the regex programs, and a **38-entry debuggerd tombstone** (abort message, registers, backtrace) on SIGABRT | The *transport* is real (`SOCK_DGRAM` unix socket at `/dev/socket/logdw`, bound from the data dir, Rust reader). The *writer* is still a shim: liblog's `logd_log_msg` header is private/versioned, so instead of guessing it we format-and-frame ourselves (documented in `logd.rs`). Bonus: the sink also recovers messages written by **bionic's native `LogdLogger`** (scudo/libc/tombstoned write logd's binary format directly) by salvaging the longest printable run — that is how `Scudo ERROR` and the tombstone reach `RunResult::logs`. The shim no longer needs a hand-written `printf` for messages that arrive via `__android_log_buf_write`, but it still formats `__android_log_print` varargs. |
| 5. suppressed `CHECK "No mainline framework library found"` | **documented, not fixed** | fires exactly once per process during JNI/nativeloader init (recorded as `art_aborter`, prio 7), **before** `nativeloader InitDefaultPublicLibraries`; all 27 programs still pass, i.e. it is non-fatal on this path | The string lives in **`libartbase.so`** (not `libnativeloader.so` as E1A guessed), next to `/data` and `default_value` → it is an ART-side "where is my mainline framework library" property/path check, most likely reading a property or an `apex-info-list` attribute we stub. Our `apex-info-list.xml` is synthesised from the real module list with `provideSharedApexLibs` set only for `com.google.mainline.primary.libs`, and the boot classpath is device-accurate (33 of 35 reference jars; `framework-configinfrastructure.jar` and one more come from APEXes the extraction does not carry — the E1A harness silently skipped them, we report them in `BOOTCLASSPATH.missing`). Next step is a bounded spike: `objdump -d libartbase.so` xref on the string to find the caller, then flip the input it needs. **Until then ART error handling on that one path is not device-faithful**: the shim records the abort instead of aborting. |

### 1.1 The 27-program corpus (`cargo run -p acos-art -- verify`)

Imageless JIT, cold data dir, `results/art-runtime/verify-27.json`:

| program | pass | exit/exp | wall ms | logs |
|---|---|---|---|---|
| 01_hello | PASS | 0/0 | 187 | 7 |
| 02_int_arith | PASS | 0/0 | 162 | 7 |
| 03_long_arith | PASS | 0/0 | 152 | 7 |
| 04_float_double | PASS | 0/0 | 152 | 7 |
| 05_branches_loops | PASS | 0/0 | 151 | 7 |
| 06_switch | PASS | 0/0 | 152 | 7 |
| 07_static_methods_recursion | PASS | 0/0 | 146 | 7 |
| 08_arrays | PASS | 0/0 | 152 | 7 |
| 09_strings | PASS | 0/0 | 157 | 7 |
| 10_objects_fields | PASS | 0/0 | 152 | 7 |
| 11_inheritance_virtual | PASS | 0/0 | 152 | 7 |
| 12_interfaces | PASS | 0/0 | 152 | 7 |
| 13_exceptions | PASS | 0/0 | 151 | 7 |
| 14_instanceof_casts | PASS | 0/0 | 152 | 7 |
| 15_inner_lambda | PASS | 0/0 | 152 | 7 |
| 16_enums | PASS | 0/0 | 152 | 7 |
| 17_generics_collections | PASS | 0/0 | 151 | 7 |
| 18_string_format_misc | PASS | 0/0 | 152 | 7 |
| 19_threads_sync | PASS | 0/0 | 172 | 7 |
| 20_uncaught | PASS | 1/1 | 152 | 7 |
| 21_regex_basic | PASS | 0/0 | 152 | 12 |
| 22_regex_advanced | PASS | 0/0 | 152 | 26 |
| 23_regex_split_replace | PASS | 0/0 | 151 | 26 |
| 24_zip_roundtrip | PASS | 0/0 | 157 | 7 |
| 25_security_digest | PASS | 0/0 | 172 | 7 |
| 26_regex_matrix | PASS | 0/0 | 162 | 68 |
| 27_annotations_reflection | PASS | 0/0 | 152 | 7 |

Pass criterion is the E1A one — byte-identical **stdout** and identical **exit
code** against `fixtures/expected-art/dexprogs`. `stderr` is checked as a soft
signal (every expected `stderr` line must appear in the real stderr) and is
reported in the JSON, not used to fail a program: ART's own stderr carries log
noise that the device fixtures do not have. `20_uncaught` is exit 1 with a real
uncaught `main` exception, which is the program's verdict, not an engine crash —
`classify()` checks that before it looks at fatal ART logs.

`28_sqlite_basic` has a fixture but no built DEX; it is reported as `skip` and
excluded from the totals (27 programs, 27 pass).

### 1.2 Boot image (AOT) works and is measurably faster

`acos-art build-image` runs the image's own `dex2oat64` inside the same sandbox
(3.6 s, 33 boot jars, 140 MB of `boot.{art,oat,vdex}` + per-jar `boot-*.art`
images), and `run --image <dir>` uses it via `-Ximage:/data/img/boot.art` with
the named-image layout ART requires. Effect on the same program, same host:

| mode | 01_hello wall | 02_int_arith wall |
|---|---|---|
| imageless JIT (default, no image present) | 187 ms | 162 ms |
| AOT boot image | **76 ms** | **76 ms** |

Re-measured against the freshly extracted sysroot (35 boot jars):
`26_regex_matrix` 161 ms imageless vs **71 ms** with the image; `01_hello` 182 ms
vs 76 ms.

`RunResult::image` records the image that was actually used, and a missing
`x86_64/boot.art` produces an explicit `boot image ignored (…)` diagnostic rather
than ART's silent fallback (E1A §5.6 cost 30 min to that silence).

## 2. Structured outcomes, demonstrated

| invocation | outcome | code | evidence |
|---|---|---|---|
| `run --dex 01_hello/classes.dex` | `ok` | – | exit 0, 7 log entries |
| `run --dex 20_uncaught/classes.dex` | `ok` | – | exit 1 + `Exception in thread "main"`, matches the fixture |
| `run --class NoSuchClass --dex …` | `crash` | `ART_NO_CLASS` | `ClassNotFoundException` |
| `run --timeout 0 --dex …` | `timeout` | `TIMEOUT` | process group SIGKILLed at 0 ms |
| `run --rlimit-as-mb 1024 --dex …` | `unsupported` | `HOST_MEMORY_LIMIT` | `Scudo ERROR: internal map failure (NO MEMORY) requesting 8650752KB` + tombstone |
| corrupt/absent sysroot | `Layout`/`NoSysroot` error | – | `tools/art-sysroot.sh` hint in the message |

`RunResult` (JSON): `exit`, `signal`, `outcome`, `code`, `wall_ms`, `stdout`,
`stderr`, `logs[]`, `diagnostics[]`, `class`, `dex`, `image`, `argv` — the `argv`
field makes every run auditable (it is the exact `bwrap` command line that ran).

## 3. Findings worth carrying forward

1. **`/system/apex/...` vs `/apex/...` is load-bearing.** The same binary launched
   by the two paths links differently: only the `/system/apex/` spelling gets the
   per-APEX linker namespace. This is a one-line bug with a 5-minute-to-find,
   silent-wrong-config failure mode; it is now a comment in `launcher.rs`.
2. **`RLIMIT_AS` cannot bound ART.** Scudo's 8.6 TB reservation means the only
   host-side memory bound is RSS/cgroup (`tools/guarded.sh`), which matches
   ADR-0018's cgroup-first policy. A 1 GB cap yields a SIGABRT 45 ms in, not a
   clean OOM.
3. **`linkerconfig` needs the shim.** It is a bionic binary; without the
   `LD_PRELOAD` sink its `CHECK` failures are invisible and it simply aborts
   (`status 134`, empty log). `ArtRoot::ensure_linkerconfig` now builds and loads
   the shim for that step, which is also how the remaining linkerconfig failures
   became readable.
4. **Two log writers, two formats.** Our shim sends framed text; bionic's native
   `LogdLogger` (used by scudo/libc/debuggerd) sends logd's private header. The
   Rust sink accepts both, which is why crash diagnostics now include a full
   tombstone.
5. **Property `serial` bit 24 is a trap.** `serial = 1 << 24` marks a *long*
   property in bionic; values then read as "" through libc's fast path even though
   `read_callback` shows them.
6. **The sysroot is not the image.** Two boot-classpath jars named by the image's
   classpath fragments (`framework-configinfrastructure.jar`,
   `framework-ondevicepersonalization.jar`) were missing from E1A's scratch
   sysroot, and `env.sh`'s `[ -f ]` test dropped them silently (33/35 jars).
   `tools/art-sysroot.sh` extracts them (35/35, `BOOTCLASSPATH.missing` is empty
   for the fresh sysroot), and the launcher reports anything still missing.

## 4. Open items / next

1. §1 task 5: find the `libartbase.so` caller of "No mainline framework library
   found" (objdump xref; then check whether it wants a property such as
   `ro.vndk.version`, the APEX `provideSharedApexLibs` flag, or
   `/apex/apex.libraries.config.txt` as an *input*). Until then this one CHECK is
   recorded, not enforced.
2. Re-run the corpus against `.cache/art/sysroot` on every extraction (it is the
   artefact a product build would ship; the E1A scratch sysroot is kept only for
   differential comparison). Both give 27/27.
3. Task 4: replace the framed shim transport with liblog's real datagram writer
   (`logd_log_msg`) once the header is pinned down against `liblog.so` — the Rust
   sink side already tolerates it.
4. Properties: add long-property support (>91 bytes) if framework code starts
   reading them, and consider a real `property_info` writer once a consumer needs
   `__system_property_foreach` ordering/serial semantics.
5. `ArtRoot::ensure_data` writes `apex-info-list.xml` and the property table into
   the data dir shared by all runs; concurrent runs against one data dir would
   race on the `linkerconfig` generation. Serialise per data dir (lock file) or
   give each run its own (`--data`).
6. Marginal cost: E1A measured ~16 MB PSS/proc with page-cache sharing of the
   boot classpath; the launcher does not yet *report* per-run PSS. Wire it to the
   ACOS accounting before using ART as a session engine.
