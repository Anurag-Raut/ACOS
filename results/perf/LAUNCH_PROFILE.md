# W109-A — `acos launch counter.apk` launch profile

**Status:** measured. Release build in a clean worktree of `791f14d`
(`.cache/wt-w109a`, `CARGO_TARGET_DIR=.cache/target-w109a`,
`CARGO_PROFILE_DEV_DEBUG=0`), fixture
`fixtures/build/apps/counter.apk`, real corelib, warm boot cache (the default
`ACOS_CACHE_DIR` path). Raw numbers: `/tmp` run captured into
`results/bench/w109a-launch.json` (before/after) and the profiler JSON quoted
below.

> **Metric-type honesty.** The `w102a-launch.json` baseline (5.5 s / ~540 MB) was
> a **dev-profile** launch. This wave profiles and times the **release** build
> (the task requires `--release` for timing). The release launch is ~0.8 s wall
> / ~0.54 GB peak RSS; the profiling and optimisation conclusions hold for the
> release path.

## 1. The profiling facility

`ACOS_PROFILE=1` turns on a counter/timer set inside `acos-vm` and `acos-runtime`.
When disabled the whole facility is one relaxed atomic load (`profile::enabled`)
per instrumented boundary and nothing in the interpreter's hot loop. When
enabled it emits one JSON object to `$ACOS_PROFILE_OUT` (a path) or stderr at
the end of a launch:

| Category | JSON field | Source |
|---|---|---|
| per-stage wall | `stages[]` (`name`, `count`, `ns`) | `acos-runtime` `do_launch` + `lifecycle.rs` hooks |
| executed bytecode | `counters.instructions` | `interp::run` / `run_current_until_body` |
| class link count/time | `counters.class_links`, `class_link_ns`, `class_links_by_class[]` | `Vm::link_class` |
| class parse count/time | `counters.class_parses`, `class_parse_ns`, `class_parses_by_class[]` | `Vm::load_dex_class_body` |
| native call count/time by `(class,name)` | `natives[]` | `interp::exec_one` native path |
| GC count/time | `counters.gc_count`, `gc_ns` | `Vm::gc` |
| `<clinit>` count/time by class | `counters.clinit_count`, `clinit_ns`, `clinit_by_class[]` (sorted) | `interp::initialize_class` |
| thrown exceptions | `counters.exceptions` | `Vm::throw_new` |
| memory by category | `notes{}` + `rss_bytes` | `note_vm_footprint` + `/proc/self/statm` |

**Caveat on `natives[].ns`.** The per-native time is inflated by the profiler's
own per-call `Instant::now()` + mutex update (≈120 ns/call at 875 k calls ≈
100 ms). The call **counts** and the ranking of low-frequency expensive natives
(`nBuild`, `Class.newInstance`, `nativeGetStringBlock`, `nativeOpenXmlAsset`,
`FileChannelImpl.map0`) are trustworthy; high-frequency natives
(`String.charAt`) should be read for count only. Stage, clinit, class-parse and
class-link timings are unaffected by this (they are not per-instruction).

Reproduce:

```sh
CARGO_TARGET_DIR=.cache/target-w109a CARGO_PROFILE_DEV_DEBUG=0 \
  cargo build --release -p acos-cli
ACOS_PROFILE=1 ACOS_PROFILE_OUT=/tmp/prof.json \
  .cache/target-w109a/release/acos launch --json fixtures/build/apps/counter.apk
```

## 2. Where the launch goes (release, warm boot cache, profiler on)

| Stage | wall | note |
|---|---:|---|
| `apk_parse` | 0.11 ms | `Apk::open` + manifest |
| `boot_runtime` | 128.8 ms | boot-image cache miss/load + `Vm::new` + boot / restore |
| `register_natives` | 0.08 ms | APK JNI routing |
| `install_package` | 0.10 ms | PMS seed |
| `load_app_dex` | 0.03 ms | app `classes.dex` parse |
| `lifecycle` | **743.4 ms** | `boot_and_run_lifecycle_from_boot` |
| &nbsp;&nbsp;↳ `bind_application` | **675.3 ms** | bind + `run_phase` + app onCreate + `installContentProviders` |
| total process wall | ~0.91 s (profiled), ~0.81 s (unprofiled) | |

`client_transaction`/`activity_on_create` never complete: the counter fixture
throws `Resources$NotFoundException: android:drawable/ic_ab_back_material`
inside `Toolbar` construction, so `deliver_launch_transaction` returns an error
before that stage boundary. This is the same blocker recorded in §9.4 and is
unchanged by this wave.

**Top cost: class initialisation inside `bind_application`, not class parsing.**

| Item | total |
|---|---:|
| `<clinit>` time (449 classes) | 497.8 ms |
| &nbsp;&nbsp;`android.graphics.Typeface` | **257.3 ms** |
| &nbsp;&nbsp;`android.content.pm.PackagePartitions` | 72.8 ms |
| &nbsp;&nbsp;`sun.security.jca.Providers` | 45.1 ms |
| &nbsp;&nbsp;`android.app.SystemServiceRegistry` | 25.5 ms |
| &nbsp;&nbsp;`android.provider.Settings$Global` | 22.9 ms |
| class parse (1917 classes) | 113.2 ms |
| class link (2029 classes) | 15.4 ms |
| executed bytecode | 26.6 M instructions |
| GC | 0 (launch never crosses the GC threshold) |
| exceptions | 11 |
| native calls (count) | 875 068 |

`Typeface.<clinit>` alone is ~32 % of `bind_application`: it enumerates every
system font family and builds 384 `Font` objects (the framework's real
behaviour). Top natives inside it: `Font$Builder.nBuild` 76.6 ms ×384,
`Class.newInstance` 43.8 ms ×32, `String.charAt` 726 526 calls.

## 3. What holds the ~515 MB RSS

Guest objects are tiny; the RSS is almost entirely host-side parse state and
native memory of the boot image.

| Category | Size | Notes |
|---|---:|---|
| raw DEX bytes held (`vm_dex_bytes`) | 63.1 MiB | 40 loaded DEX files (`Arc<[u8]>` per `DexFile`) |
| decoded `MethodCode` (`vm_code_bytes`) | 30.0 MiB | `insns` + `raw` + `pc_to_idx` + tries + line table, deduped by `Arc` |
| managed heap (`vm_heap_peak_bytes`) | 11.4 MiB | live guest objects |
| method metadata (`vm_method_meta_bytes`) | 1.9 MiB | 38 722 methods' names/sigs |
| class metadata (`vm_class_meta_bytes`) | 0.4 MiB | 2 029 class records |
| **accounted** | **~106.8 MiB** | |
| **unaccounted residual** | **~408 MiB** | resource table (`framework-res.apk` 33 MiB), font collection (`.cache/boot/android-34/fonts` 75 MiB), boot-image snapshot buffers, lazy `DexFile` string tables / class indices, native memory and allocator retention |

The residual is the honest open item: besides the 75 MiB font collection and
33 MiB `framework-res.apk`, the host allocator retains transient parse buffers,
so peak RSS is much larger than retained guest state. Three of the four
instrumented classes of parse state are already shared process-wide (DEX parse
`Arc`, `MethodCode` code cache); the raw DEX bytes are the largest *named*
category.

## 4. Optimisation landed

**Remove the per-font copy in `Font$Builder.nBuild`** (`acos-vm/natives/fwk_fonts.rs`).

`fb_build` called `read_font_buffer`, which copies the entire `ByteBuffer`
region (the whole TTF/OTF, up to hundreds of KB) out of native memory into a
temporary `Vec` — and then discarded the bytes, keeping only the direct
address. The framework's `nBuild` stores the address for later rendering and
never reads the buffer through this call. Replacing it with `font_buffer_addr`
(validate the buffer shape, touch the first byte to keep the mapped-region
check, and return the address) removes a transient copy of up to ~75 MiB of
font data per boot. Observable behaviour is unchanged.

A second, mechanical fix: the `boot_runtime`/`register_natives` stage timer was
not reset between the two, so `register_natives` originally reported the
cumulative boot time. It is now 0.08 ms, as expected.

## 5. Before / after (5 interleaved fresh-process rounds, release)

A/B alternating order per round, unprofiled. Raw per-round samples in
`results/bench/w109a-launch.json`.

| Metric | before (pristine `791f14d`) | after (W109-A) | Δ |
|---|---:|---:|---:|
| launch wall, p50 | 821 ms | **809 ms** | −1.5 % |
| launch wall, min | 799 ms | **797 ms** | — |
| peak RSS, p50 | 557.3 MiB | **538.6 MiB** | **−18.6 MiB (−3.3 %)** |
| reached / blocker | `client_transaction` / `Resources$NotFoundException` | same | unchanged |

The wall change is inside host noise; the RSS change is the real win and is
directly attributable to the font-copy removal (transient copy previously
counted in `VmHWM`).

## 6. Ranked next opportunities (not landed this wave)

1. **`Typeface.<clinit>` 257 ms.** The framework deliberately builds the whole
   system font list at init. A boot-image-resident font table (or deferring
   `Typeface` until a `Typeface` is actually requested) would remove most of it,
   but both change class-init ordering/observability, so they need a dedicated
   wave with a fidelity check.
2. **`Class.newInstance` 43.8 ms ×32** and `Field.getAnnotationNative` 9.5 ms
   ×559: memoise the reflective constructor/annotation lookups (pure function of
   `MethodId`), safe and local to the reflection natives.
3. **Unaccounted ~408 MiB RSS**: add categories for the resource table and font
   collection, then make `framework-res.apk`/font loading share one process-wide
   parsed table (both are already loaded once, so this is measurement +
   avoiding duplicate host buffers).
4. **Class parse 113 ms**: the raw DEX parse is already shared; the remaining
   cost is the lazy `class_index` build and eager string-constant interning in
   `add_dex_method_from`. The latter is a deliberate post-OOM safety property —
   only remove it behind evidence that allocation-after-OOM still behaves.

## 7. Acceptance evidence

| Gate | Result |
|---|---|
| `cargo check --workspace --all-targets` | pass |
| `cargo clippy --workspace --all-targets --no-deps` | 6 warnings, **all pre-existing in `acos-vm/src/natives/gfx_vector.rs`** (render, W104-A); zero warnings in W109-A files |
| `diff-dexprogs.sh --corelib real` | 27 / 27 pass |
| `diff-dexprogs.sh --corelib real --gc-stress` | 27 / 27 pass |
| `cargo test -p acos-vm -p acos-runtime` | 49 suites pass; `render_views::pipeline_generated_layouts_match_art_reference` fails because the boot dir lacks `android.test.mock/classes.dex` — a pre-existing fixture gap unrelated to this wave |
