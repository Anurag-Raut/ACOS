# W139-A — retained anonymous memory: census + lazy annotation parse

**Status:** implemented, measured, gates green. Clean worktree of HEAD
(`4d60477`), `CARGO_TARGET_DIR=.cache/target-w139a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `tools/guarded.sh`, release build, real corelib,
fixture `fixtures/build/apps/counter.apk`, warm on-disk boot cache.
`ACOS_PROFILE=1` for the steady anon figure, external `/proc/<pid>/status`
`VmHWM` polled for peak. No commit.

## 1. Result

| metric (warm, p50 of 5 release runs) | before (HEAD) | W139-A | Δ |
|---|---:|---:|---:|
| steady `RssAnon` after launch | **434.0 MiB** | **397.4 MiB** | **−36.6 MiB (−8.4 %)** |
| peak RSS (`VmHWM`) | 566.1 MiB | **510.3 MiB** | **−55.8 MiB (−9.9 %)** |
| `RssFile` APK mapping (control) | 115.6 MiB | 115.6 MiB | 0 |
| `vm_dex_bytes` (control) | 63.1 MiB | 63.1 MiB | 0 |
| `vm_code_bytes` (control) | 34.9 MiB | 34.9 MiB | 0 |
| reached stage | `on_resume` | `on_resume` | unchanged |

Per-run steady anon: before `434.0, 434.0, 432.8, 434.0, 432.8`; after
`397.4 ×5`. The after figure is flat across the five runs, so the delta is not
run-to-run noise. Peak is `VmHWM`; the before peak and before end-of-launch
`VmRSS` agree (566 MiB), so the 55.8 MiB peak delta is real but larger than the
steady delta because the peak also loses the annotation pages that the steady
snapshot had already dropped from some mappings.

## 2. The census (behind `ACOS_PROFILE=1`, safe code)

`acos-vm/src/profile.rs` now has `census_vm(vm)`: a hand-written deep-size walk
over the retained VM/DEX structures, emitted as a `census[]` array of
`{name, bytes, detail}` in the profile JSON (sorted by bytes) plus
`vm_rss_peak_bytes`. Every row is a structural estimate
(`len`/`capacity`/`String::capacity`), never a guess at allocator overhead.
`acos-dex` gained `DexFile::footprint()`, `DexFile::annotations_footprint()` and
`BootCache::footprint()` to make the parsed-DEX bytes countable.

Warm `counter.apk` launch after the change (`ACOS_PROFILE`, one run; identical
across runs):

| census row | MiB | detail |
|---|---:|---|
| `dex_raw` | 63.09 | 40 boot DEX files, `Arc<[u8]>`, process-shared |
| `vm_dex_handle_caches` | 29.04 | 40 × `string/field/method/class/virtual` caches |
| `dex_parsed_tables` | 22.88 | id tables + class defs (deduped by `Arc` pointer) |
| `vm_class_meta` | 15.63 | classes=2451, deep fields/maps/vtables |
| `vm_heap_slots` | 15.49 | len=148 114, cap=290 052 |
| `dex_decoded_strings` | 15.43 | materialised string tables |
| `vm_method_meta` | 2.15 | methods=44 740 |
| `vm_string_interner` | 1.73 | entries=18 617 |
| `vm_type_meta` | 0.25 | types=3 645 |
| `vm_native_registry` | 0.23 | entries=1 849 |
| `vm_class_by_desc` | 0.21 | descriptor → `ClassId` |
| `vm_dispatch_cache` | 0.16 | entries=5 035 |
| `vm_ir_quickening` | 0.12 | entries=5 854 |
| `dex_annotations` | 0.03 | parsed_dirs=2 |
| `vm_thread_stacks` / `throwable` / `monitors` / `converters` | <0.06 total | |
| **census total** | **166.50** | |
| steady `RssAnon` | **397.4** | |
| **residual not attributed by this walk** | **230.9** | `ServiceHost`/ATMS/Binder state, render surfaces, font/text caches, allocator class/slack, std runtime |

The global `BootCache` holds the same `Arc<DexFile>` values counted above, so it
is **not** added again to the census (that would double-count 100 MiB of shared
DEX); its identical totals are surfaced as flat `dex_bootcache_*` notes.

## 3. What was cut: lazy `annotations_directory_item` parsing

The pre-change jemalloc heap profile of a warm launch attributed ~32 MiB to
`acos_dex::annotation::parse_directory` / `read_set` / `decode_annotation`, all
retained for the process lifetime inside the global parsed-DEX cache. Nothing
in the VM class model uses them: only the reflection natives in
`acos-vm/src/natives/fwk_annotations.rs` do, and they touch a handful of
classes (`Typeface`'s `@UnsupportedAppUsage` sweep is the notable one).

Change (`acos-dex`):

* `DexFile::parse` no longer calls `parse_directory`; it records only the
  `class_def_item.annotations_off` (already a field). `ClassDef.annotations`
  stays in the `Borsh` form for wire compatibility but is always `None`.
* `DexFile::annotations(def)` now parses the directory on first access and
  caches it in `Arc<Mutex<HashMap<u32, Arc<AnnotationsDirectory>>>>`, keyed by
  the offset (unique within one DEX, shared by every VM that uses the parsed
  file). `from_tables` uses the same lazy path, so a restored process re-parses
  only the directories reflection asks for.
* `DexFile::footprint()` / `annotations_footprint()` expose the retained bytes.

After the change the census reports `parsed_dirs=2` / 0.03 MiB for annotations
(vs ~32 MiB eagerly), and `27_annotations_reflection` still passes, so
`getAnnotation`/`getAnnotations`/`getDeclaredAnnotations` behaviour is
unchanged.

## 4. Acceptance evidence (clean worktree `4d60477` + W139-A, release)

| gate | result |
|---|---|
| `cargo test -p acos-dex` | **14 / 14 pass** (incl. `annotations_directory_parses`) |
| `cargo test -p acos-runtime --test boot_image_cache` | **6 / 6 pass** |
| `cargo test -p acos-vm --test snapshot` | **16 / 16 pass** |
| `tools/diff-dexprogs.sh --corelib real` | **27 / 27 pass** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 / 27 pass** |
| `cargo clippy -p acos-dex -p acos-vm -p acos-runtime --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | pass |
| `#![forbid(unsafe_code)]` | preserved (no allocator, no unsafe added) |
| boot-cache format | unchanged; the W139-A binary read the cache written by HEAD (warm p50 above) |

## 5. Honest read / next levers

The census now names 166.5 MiB of the 397.4 MiB `RssAnon`; the remaining
**230.9 MiB** is host-side and outside this pass's walk (service-host/ATMS/Binder
state, acos-render surfaces/glyph caches, allocator size-class slack, std
runtime). The ranked next candidates from the census, in measured order:

1. **`vm_dex_handle_caches` 29.0 MiB** — per-VM `Vec` caches sized to the whole
   string/field/method/class table of each of 40 boot DEX files, plus one
   `virtual_cache` entry per call site. Sparse/lazily-sized caches (or a shared
   immutable prefix for boot DEX) would cut most of it without touching the
   hot dispatch path's fast case.
2. **`vm_heap_slots` 15.5 MiB at cap 290 052 for 148 114 live slots** — the
   slot `Vec` never shrinks after the boot peak; `shrink_to_fit` after a GC when
   `len < cap/2` (or a chunked slot table) would return ~7–8 MiB.
3. **`dex_parsed_tables` 22.9 MiB + `dex_decoded_strings` 15.4 MiB** — the
   `class_index` hash and the fully materialised string table are the largest
   lazily-built pieces. A per-string lazy decode (resolve via
   `string_data_offsets`, cache only touched strings) is the next safe slice.
4. **`vm_class_meta` 15.6 MiB** — `direct_map`/`vtable_index` duplicate
   `(String, String)` keys; interning the name/sig pair or keying by
   `(u32, u32)` interned ids would cut the string copies.

None of these were attempted here: the annotation cut was the single
highest-confidence item and is fully measured and gated. The 45-minute rule is
respected — one idea implemented, one measured result.

## 6. Files

* `crates/acos-dex/src/lib.rs` — `DexFile.annotations` cache, lazy
  `annotations()`, `Footprint`/`footprint()`, `annotations_footprint()`.
* `crates/acos-dex/src/cache.rs` — `BootCache::footprint()`.
* `crates/acos-dex/src/persist.rs` — `from_tables` initialises the lazy cache.
* `crates/acos-vm/src/profile.rs` — `census_vm`, `census[]` report field,
  `rss_peak_bytes`, `note_census`.
