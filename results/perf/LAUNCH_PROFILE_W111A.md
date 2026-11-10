# W111-A — launch clinit lifting + RSS accounting

**Status:** measured. Release build in a clean worktree of `2c9fdb8`
(`.cache/wt-w111a`, `CARGO_TARGET_DIR=.cache/target-w111a`,
`CARGO_PROFILE_DEV_DEBUG=0`), fixture `fixtures/build/apps/counter.apk`, real
corelib, warm on-disk boot cache. Raw artifacts:

| artifact | contents |
|---|---|
| `results/bench/w111a-profile-warm.json` | `ACOS_PROFILE` of a genuine warm-cache hit (same binary, `ACOS_*` env pinned so the property hash matches) |
| `results/bench/w111a-clinit-prime.json` | 5 interleaved fresh-process A/B rounds, same binary, `ACOS_CLINIT_PRIME` on/off |
| `results/bench/w111a-after-unprofiled.json` | 5 fresh-process warm-cache launches, priming on |

## 1. Where the clinit time actually goes

Baseline warm-launch profile (`ACOS_PROFILE`, cache hit) and the same run after
priming the deterministic classes into the template:

| counter | baseline | W111-A | Δ |
|---|---:|---:|---:|
| `<clinit>` count | 445 | **284** | **−161** |
| `<clinit>` total | 535.6 ms | **292.6 ms** | **−243 ms** |
| executed bytecode | 26.64 M | **23.54 M** | **−3.10 M** |
| `Font$Builder.nBuild` (384) | 73.7 ms | **59.9 ms** | −13.8 ms |
| `String.charAt` | 726 k | **576 k** | −150 k |

Class-by-class (baseline): `Typeface` 248 ms, `PackagePartitions` 93 ms,
`Providers` 66 ms, `SystemServiceRegistry` 28 ms, `Settings$Global` 22 ms,
`Configuration` 8, `LocaleList` 8, `Locale` 7. `Typeface` is pure framework
Java: it parses `fonts.xml` (726 k `String.charAt`) and builds 384 `Font` +
32 `FontFamily`/`Typeface` objects through the font natives (`nBuild` 384×,
`Class.newInstance` 32×, `Field.getAnnotationNative` 559×, `FileChannel.map0`
203×). It is **not** one hot native and not a font-buffer copy (that copy was
removed in W109-A); it is interpreted framework bytecode plus many small
natives.

## 2. What landed: deterministic `<clinit>` lifted into the template

`RuntimeTemplate::boot_with_options` now calls
`prime_deterministic_classes` after `attach` and before `snapshot()`. Each
class is loaded, initialised, and — only if it ends `Init` — kept in the
snapshot. `ACOSSNAP` already serialises per-class `InitState` and statics
(`snapshot.rs` writes `init_state_tag(c.init)` and the static slots), so a
restored template finds the class `Init` and `initialize_class` returns
immediately. A class that fails is reverted to `NotInit`, so priming can never
turn a would-be-lazy failure into a permanent `Failed`.

Primed set (`DETERMINISTIC_CLINIT_CLASSES`): `PackagePartitions`, `Providers`,
`SystemServiceRegistry`, `Configuration`, `LocaleList`, `Locale`. These read
only the boot image / system properties / `/system` files.

**Guard rails**

* `android.provider.Settings$Global` was tried and **rejected**: its `<clinit>`
  installs an `android.provider.$Proxy0` dynamic proxy class. The snapshot can
  carry the descriptor but `Vm::restore` cannot relink a class with no boot-DEX
  body, so priming it made every warm restore fail with
  `Landroid/provider/$Proxy0; is not implemented`. It is deliberately absent
  (documented in the source).
* `Typeface` is deliberately absent: its `<clinit>` fills the process-global
  font handle tables (`natives::fwk_fonts` `FONTS`, `gfx_text` `typefaces`),
  which `ACOSSNAP` does not carry. A cross-process restore would leave dangling
  native handles. Moving it into the template therefore needs a font-table
  reseed/import first (the same mechanism `reseed_after_restore` already
  provides for loopers/parcels/binders).
* `boot_image_cache::CACHE_FORMAT_VERSION` bumped 1 → 2 so pre-W111-A entries
  (which lack the primed state) are not reused.

**Correctness test.** `restored_template_carries_deterministic_clinit`
(`crates/acos-runtime/tests/boot_image_cache.rs`) asserts `PackagePartitions`
is primed and that every primed class is `Init` in a fresh restore. The
pre-existing `cold_restore_matches_uncached_ladder` (primed cache-restored child
vs unprimed uncached launch) still passes, so the lifecycle ladder is
unchanged.

## 3. Honest A/B (5 interleaved fresh processes, same binary, release)

`ACOS_CLINIT_PRIME=1` vs `0`, separate warm caches, alternating start order:

| metric | before (no prime) | after (prime) | Δ |
|---|---:|---:|---:|
| launch wall, p50 | 797 ms | **765 ms** | **−32 ms (−4.0 %)** |
| launch wall, min | 775 ms | 754 ms | −21 ms |
| peak RSS, p50 | 513.8 MiB | 523.4 MiB | **+9.6 MiB (+1.9 %)** |
| boot-image snapshot | 224 KB | 1.66 MB | +1.4 MB |
| reached / blocker | `client_transaction` / `Resources$NotFoundException` | same | unchanged |

The profiled `−243 ms` is much larger than the unprofiled `−32 ms`: the
profiler's per-native `Instant::now()` + mutex (≈875 k calls) is charged to
`clinit_ns` and `natives[].ns`, so the profiled clinit total overstates the
real saving. The honest unprofiled number is the 32 ms row. The RSS cost is the
primed objects retained in the snapshot/restored heap (mostly
`PackagePartitions`/`Providers` caches); it is a real trade, not noise.

**Recommendation:** keep priming for wall-time-sensitive launches; provide the
`ACOS_CLINIT_PRIME=0` escape (already wired) where the ~10 MiB matters more.

## 4. RSS accounting (warm launch, `VmHWM` 523.5 MiB)

`ACOS_PROFILE` now rolls `/proc/self/smaps` `Rss:` up by mapping category
(`note_rss_by_mapping`) in addition to the VM counters:

| category | RSS |
|---|---:|
| anonymous (`[heap]`, `[anon:*]`, `[stack]`) — allocator arenas + transient parse buffers | **443.1 MiB** |
| APK/file-backed (`framework-res.apk`, fixture APK) | **65.8 MiB** |
| executable code (`.so`/binary) | 10.9 MiB |
| other file-backed | 3.7 MiB |
| font files `.ttf/.otf/.ttc` | **0 MiB resident** (mmap'd, pages not faulted) |
| dex/jar/img | 0 MiB |
| **mapped total** | **~523.5 MiB** |
| &nbsp;&nbsp;↳ VM raw DEX (`vm_dex_bytes`) | 63.1 MiB |
| &nbsp;&nbsp;↳ decoded code (`vm_code_bytes`) | 29.9 MiB |
| &nbsp;&nbsp;↳ managed heap (live = peak) | 11.4 MiB |
| &nbsp;&nbsp;↳ method + class metadata | 2.3 MiB |

This resolves the W109-A "~408 MiB unaccounted": it is **anonymous allocator
memory (±443 MiB) plus a 65.8 MiB APK mapping**, not font RSS. The 75 MiB font
figure from W109-A is the on-disk extracted collection; the mmaps are not
resident during `Typeface.<clinit>` (only `FileChannel.map0` + touch-one-byte,
and pages are faulted lazily on first shaping).

## 5. Ranked next steps

1. **Reseed font native tables so `Typeface` can be primed.** Export/import
   `FontShared`/`TextShared` (or rebuild them from the restored Java
   `Font`/`FontFamily`/`Typeface` objects and rewrite their handle fields) and
   add it to `reseed_after_restore`. That removes the remaining ~245 ms clinit
   and is the single largest launch item left.
2. **Anonymous 443 MiB.** It is the allocator, not named VM state. Candidate
   levers: `malloc_trim(0)` after boot-image restore, `MALLOC_ARENA_MAX=1` for
   the single-threaded launch (the process runs `vm_threads=1`), and removing
   transient parse buffers. Not attempted here because they are process-wide
   and timing-sensitive.
3. `Settings$Global` cannot be primed until dynamic-proxy classes survive
   restore; that needs the same snapshot-relink work.
4. Font basename → collection entry is now cached in `fwk_fonts`
   (`resolve_font_file`), so the 384 `nBuild` calls resolve once per file
   instead of scanning every family; `nBuild` dropped 73.7 → 59.9 ms profiled.

## 6. Acceptance evidence (clean worktree, W111-A)

| gate | result |
|---|---|
| `cargo test -p acos-runtime --test boot_image_cache` | **6/6 pass** (incl. new priming guard + cold-restore ladder) |
| `cargo check --workspace --all-targets` | pass |
| `cargo clippy -p acos-vm -p acos-runtime --all-targets --no-deps` | **0 warnings** |
| `tools/diff-dexprogs.sh --corelib real` | **27/27 pass** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27/27 pass** |
| behaviour | `bind`/`app`/`client_transaction` ladder unchanged; same blocker |
