# M1 — the 20 console programs on the real Android 14 core library

**Status: PASS.** All 20 `fixtures/build/dexprogs` programs produce byte-identical
`stdout` and exit codes to `fixtures/expected-art/dexprogs` (the real ART oracle)
with `--corelib=real`, both normally and under `ACOS_GC_STRESS=1`.
For `20_uncaught` the first `stderr` line is also pinned (`Exception in thread
"main" Main$Fatal: fatal without args`); later stack-frame text is VM-specific
and intentionally not compared.

```
tools/diff-dexprogs.sh --corelib real              # 20 pass / 0 fail
tools/diff-dexprogs.sh --corelib real --gc-stress  # 20 pass / 0 fail
tools/diff-dexprogs.sh                             # bootstrap still 20/20
```

Reports: `results/vm/dexprogs-real-normal.json`,
`results/vm/dexprogs-real-gc-stress.json`,
`results/vm/dexprogs-bootstrap-normal.json`,
`results/vm/dexprogs-bootstrap-gc-stress.json`.

## Contents

* [1. What "real core library" means here](#1-what-real-core-library-means-here)
* [2. Design notes](#2-design-notes)
* [3. Counts](#3-counts)
* [4. Startup latency and memory](#4-startup-latency-and-memory)
* [5. Native-diff report](#5-native-diff-report)
* [6. Deviations and gaps](#6-deviations-and-gaps)
* [7. `interp.rs` merge notes](#7-interprs-merge-notes)
* [8. Reproduction](#8-reproduction)

---

## 1. What "real core library" means here

`--corelib=real` replaces the Rust `java.lang`/`java.util` built-ins with the
real boot-classpath DEX extracted from the Android 14 (API 34) system image:

* `BOOTCLASSPATH` and `dex/<jar>/classes*.dex` under `$ACOS_BOOT_DIR`
  (default `.cache/boot/android-34`, falling back to `~/.cache/acos/boot/android-34`).
* For M1 the loader eagerly parses `core-oj`, `core-libart` and `core-icu4j`
  (the three jars the console tier needs); the remaining 9 boot jars are on
  disk and are parsed lazily when a class needs them.
* The real boot DEX is the **class provider**; the VM executes its bytecode
  (`<clinit>`, `PrintStream`, `Charset`, `HashMap`, `Thread`, `Throwable`, …) and
  supplies only methods declared `native` in that DEX, plus a small, documented
  set of VM-internal overrides.

Missing cache:
`BOOT_CLASSPATH_MISSING` is a structured error that names the exact command to
create it (`tools/extract-bootclasspath.sh`).

## 2. Design notes

### 2.1 `ClassProvider` stack

`Vm::dexs` is an ordered provider list. `--corelib=real` calls
`Vm::load_boot_classpath(dir)` which reads `BOOTCLASSPATH`, parses the core trio
with `acos-dex`, and records their DEX indices in `Vm::boot_dexs`. The app DEX is
appended afterwards. `load_class` searches providers in order; builtin classes
are only installed in `--corelib=bootstrap`. The 9 non-M1 boot jars are kept
in `Vm::boot_lazy_jars` and parsed one at a time by `try_load_lazy_boot_jar`
only when a class miss reaches them (the 20 console programs never do).

`acos-dex` parses DEX lazily and keeps strings lazy; the measured cost of the
three M1 jars is in §4 (`vm_new_ms` = 15–16 ms p50, of which ~13 ms is reading
`BOOTCLASSPATH` and parsing `core-oj`/`core-libart`/`core-icu4j`).

### 2.2 How fundamental classes are bridged

The VM keeps three internal representations that the DEX-visible fields are
synthesised from in `src/bridge.rs`:

| class | ACOS representation | DEX-visible synthesised fields |
|---|---|---|
| `java.lang.String` | `Object::String(Vec<u16>)` | `count = (len<<1) \| coder`, `hash` (shadow cache), `coder`, `value` (latin1 or UTF-16LE byte[]) |
| `java.lang.Class` | `Object::Class(ClassId)` | `accessFlags`, `primitiveType`, `componentType`, `superClass`, `classLoader` |
| `java.lang.Object` | `Object::Instance { class, fields }` | `shadow$_klass_` → the class mirror; `shadow$_monitor_` → 0 |

The real `String` DEX has no `value`/`coder` fields and routes content methods
through `StringLatin1`/`StringUTF16`; ACOS instead provides the backing `value`
array on demand so those helpers read correct data, and additionally overrides
the most common `String` content methods (`substring`, `indexOf`,
`lastIndexOf`, `startsWith`, `endsWith`, `contains`, `trim`, `toUpperCase`,
`toLowerCase`, `equalsIgnoreCase`, `isEmpty`, `replace`) plus all the `native`
ones. `StringFactory.*` and every `String` `<init>` are implemented natively
because the real DEX bodies are `throw new UnsupportedOperationException("Use
StringFactory instead.")` stubs.

`Thread`: the main `Thread` object and every spawned thread are real
`java.lang.Thread` instances. A process `ThreadGroup` is created at boot; green
threads are created by `Thread.nativeCreate`, parked by `Thread.sleep`/`yield`,
and finalised by `bridge::finish_thread` (clears `nativePeer`/`tid`, wakes
`Thread.join` waiters on the thread's `lock`). `ThreadGroup.add*` bookkeeping is
a VM no-op because the runtime owns thread membership.

`System`: no separate `initPhase1/2/3` is run — the real `System.<clinit>` in
`core-oj` builds `in`/`out`/`err` itself from `FileDescriptor.in/out/err`,
`FileOutputStream`, `BufferedOutputStream`/`BufferedInputStream` and
`PrintStream`. The only boot work ACOS adds is the `VMRuntime`/`ICU`/`Linux`
natives (`getenv`, `uname`, `getpwuid`, `getuid`) and the `Thread`/`ThreadGroup`
objects, after which the real `<clinit>` chain runs to completion.

### 2.3 Native registry

`src/natives/` registers `(class descriptor, method, signature) -> fn` in
`Vm::natives`; `bind_dex_native` binds a loaded DEX method to its Rust
implementation. Anything unbound becomes a structured
`UNSUPPORTED_JAVA_API` naming `Lclass;->name(sig)`. Modules: `lang`
(Object/String/System/Thread/Throwable/Reference/VMStack), `character`, `icu`
(NativeConverter), `io` (libcore.io.Linux + ICU versions), `vmrt`
(VMRuntime/VMRuntime/AtomicInteger), `unsafe` (jdk.internal.misc.Unsafe /
sun.misc.Unsafe), `reflect` (Class/Field/Array/Enum), `math`
(Math/StrictMath/Float/Double).

ICU: converters for UTF-8, UTF-16LE/BE, ISO-8859-1 and US-ASCII are implemented
in Rust; no `icudt*.dat` is opened. `NativeConverter.charsetForName` materialises
a real `CharsetICU` so the framework's `CharsetEncoderICU` path works unchanged.

### 2.4 GC

The existing precise mark-sweep GC was extended to root the process
`ThreadGroup` and each thread's `wait_monitor`, and to drop shadow `String.hash`
entries when a slot is collected. Multi-dimensional arrays built by
`reflect.Array.createMultiArray` root each child across further allocations, so
`ACOS_GC_STRESS=1` (a collection before every allocation) passes all 20.

## 3. Counts

Measured with the release binary, `--corelib=real`:

| metric | value |
|---|---:|
| native method bindings registered | 439 |
| `source` modules | 8 (`lang`, `character`, `icu`, `io`, `vmrt`, `unsafe`, `reflect`, `math`) |
| classes loaded, 01_hello | 196 |
| `<clinit>` bodies executed, 01_hello | 46 |
| classes loaded, range over the 20 programs | 196–221 |
| `<clinit>` executed, range | 46–54 |

Per-program numbers: `results/m1/dexprogs-metrics.json`.

## 4. Startup latency and memory

`tools/oracle/startup-bench.sh 20` — 01_hello, release build, same host, no
emulator. Per-stage p50/p95 in ms; RSS from `/proc/self/status` `VmHWM`.

| stage | real p50 | real p95 | bootstrap p50 | bootstrap p95 |
|---|---:|---:|---:|---:|
| total wall (CLI) | 25 | 26 | 0 | 0 |
| boot-classpath open + VM bring-up (`Vm::new`) | 16 | 17 | 0 | 0 |
| first user bytecode → exit (`run`) | 8 | 9 | 0 | 0 |
| peak RSS (KiB) | 36 016 | 36 100 | 5 024 | 5 080 |

| comparison | wall p50 (ms) | ratio vs ART |
|---|---:|---:|
| real Android 14 ART (`dalvikvm64` hello, reference emulator) | 71 | 1.00× |
| ACOS `--corelib=real` (this host) | 25 | 0.35× |
| ACOS `--corelib=bootstrap` (this host) | <1 | — |

Caveat: the ART figure is a cold `dalvikvm64` hello on an x86_64 KVM emulator
with a pre-initialised boot image; ACOS runs on the host and starts from no
image, executing the `<clinit>` chain itself. They are different machines, so
the ratio is indicative, not a like-for-like benchmark. Full data:
`results/m1/startup.json`.

## 5. Native-diff report

`tools/oracle/native-diff.sh` records the natives ACOS invokes
(`--trace-natives-file`, same TSV columns as the ART traces) and diffs them
against `research/dynamic-trace/data/natives_*.tsv`. Reports in
`results/m1/native-diff/`.

| program | ACOS | ART | both | acos-only | art-only |
|---|---:|---:|---:|---:|---:|
| 01_hello | 46 | 21 | 16 | 30 | 5 |
| 17_generics_collections | 50 | 25 | 18 | 32 | 7 |
| 19_threads_sync | 55 | 28 | 21 | 34 | 7 |

The **art-only** sets are the fidelity gaps caused by ACOS short-circuiting a
lower layer; they are small and stable:

* `VMRuntime.stopHeapTaskProcessor`, `System.nanoTime`,
  `Thread.interrupt0`, `Thread.sleep(Object,J,I)`,
  `Throwable.nativeFillInStackTrace`, `Object.wait(JI)` — ACOS reaches these
  through its own scheduler/override paths rather than the ART entry points.
* `StringFactory.newStringFromBytes` / `reflect.Array.createObjectArray` — the
  real `String.<init>` and `Array.newInstance` paths ACOS executes use the
  `String` constructor override and `createMultiArray` instead.

The **acos-only** sets are dominated by boot work ART hides in its boot image
(`VMRuntime.bootClassPath/classPath/properties/vmVersion`, `ICU.get*Version`,
`Linux.getenv/uname/getpwuid/getuid`, `Unsafe.arrayBaseOffset/arrayIndexScale`,
`Class.getNameNative/getPrimitiveClass/getDeclaredField`,
`VMStack.getStackClass2`). These are exactly the "natives ART never needs
because the image is pre-initialised" case the report is meant to surface.

Pass `--strict` to fail if any ART native is never invoked.

## 6. Deviations and gaps

All 20 programs pass, so there is no program that "cannot pass yet". The
following are deliberate, documented deviations from real ART; none change the
fixture output:

1. **`String` content methods and constructors are VM-implemented** rather than
   running the DEX bodies. The real bodies delegate to `StringLatin1`/
   `StringUTF16` (which need the hidden `value` array) and the constructors are
   stubs. ACOS supplies `value` on demand *and* overrides the common content
   methods for correctness.
2. **`AtomicInteger`, `ThreadGroup` and `Thread.sleep` are VM-backed overrides.**
   `AtomicInteger.<clinit>` builds a `VarHandle` through `MethodHandles`/
   reflection; ACOS implements the class's `int` storage directly and no-ops its
   `<clinit>`. `ThreadGroup.add*` is a no-op. `Thread.sleep(JI)` parks the green
   thread directly instead of taking `Thread.currentThread().lock`.
3. **`Character` properties** use Rust's Unicode tables, not ICU `UCharacter`.
   `getTypeImpl`/`getDirectionalityImpl` are honest approximations; the console
   tier only needs `isDigit`/`isLetter`/`isWhitespace`/case conversion, which are
   exact for the ASCII repertoire the fixtures use.
4. **ICU data is never opened.** `libcore.icu`/`android.icu` rule/text data
   (`icudt*.dat`) is not in `.cache/boot/android-34`; program 18
   (`String.format`/`Formatter`) passes because the needed number/date/format
   paths in `core-icu4j` do not require the data file for its inputs. If a future
   fixture exercises locale-sensitive collation or line breaking, the ICU data
   file must be extracted (not present in the cache) — this is the one blocking
   data dependency for a wider ICU surface.
5. **`Object.wait` does not release the Java monitor** it holds; because ACOS's
   green threads are cooperative and the fixtures never require another thread
   to acquire that monitor while the waiter sleeps, this is not observable here.
   `notify`/`notifyAll` wake the wait set.

`results/m1/INTERP_EDITS.md` records the minimal `interp.rs` hook points the
speed agent's `vm-perf` branch must reconcile (see §7).

## 7. `interp.rs` merge notes

`git diff --stat crates/acos-vm/src/interp.rs` is **+74/−68** (down from
+244/−48 before the extraction). Only seven tiny hook sites in
`crates/acos-vm/src/interp.rs` remain; the body of the work is in
`src/bridge.rs` (new), `vm.rs` and `natives/`. See
`results/m1/INTERP_EDITS.md` for the mechanical list. In brief:

1. `run()` — `finish_thread` → `bridge::finish_thread`.
2. `step()` — `bridge::record_native` for `--trace-natives`.
3. `step()` — unbound `native` DEX method → `bridge::unbound_native_error`.
4. `execute()` `iget*`/`iput*` — `bridge::internal_field_get/put` hooks.
5. `execute()` — `self.note_clinit()` for the `Metrics.clinit_count`.
6. `arith3`/`arith2addr` — long shift operand width fix (a genuine interpreter
   bug: `shl-long`/`shr-long`/`ushr-long` take an *int* shift amount; the old
   code read a wide register and panicked on `Long.rotateLeft`).
7. `get_wide` — bounds guard (returns 0 past the frame instead of panicking).

## 8. Reproduction

```bash
# Real boot classpath (optional; defaults to .cache/boot/android-34):
tools/extract-bootclasspath.sh

# Build and run the real-library suite:
cargo build --release -p acos-cli
tools/diff-dexprogs.sh --corelib real
tools/diff-dexprogs.sh --corelib real --gc-stress

# One program with a JSON envelope:
./target/release/acos run-dex fixtures/build/dexprogs/01_hello/classes.dex \
    --main Main --corelib real --json

# Native usage trace:
./target/release/acos run-dex fixtures/build/dexprogs/01_hello/classes.dex \
    --main Main --corelib real --trace-natives-file /tmp/n.tsv
tools/oracle/native-diff.sh

# Startup table:
tools/oracle/startup-bench.sh 20
```
