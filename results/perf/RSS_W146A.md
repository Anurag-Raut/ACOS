# W146-A — sparse DEX handle caches (-13.5 MiB steady anon, -13.5 MiB peak) + residual smaps census

**Status:** one cut implemented, measured and gated; items 2–4 analysed but not
landed (explained in §4/§5); the residual is now attributable by mapping. Clean
worktree of committed HEAD (`992f3a9`), `CARGO_TARGET_DIR=.cache/target-w146a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `tools/guarded.sh`, release build, real corelib,
fixture `fixtures/build/apps/counter.apk`, warm on-disk boot cache. No commit.
Patch: `results/perf/RSS_W146A.patch` (`git apply --check` against HEAD is
clean).

## 1. Result — sparse `virtual_cache`

The W139-A census named `vm_dex_handle_caches` at 29.04 MiB. Extending the
census with a per-cache breakdown showed the cost is almost entirely the
**polymorphic inline cache**:

| per-VM DEX cache | before | after |
|---|---:|---:|
| `string_cache` (`Vec<Option<Handle>>`) | 4.45 MiB | 4.45 MiB |
| `field_cache` (`Vec<Option<ResolvedField>>`) | 3.66 MiB | 3.66 MiB |
| `method_cache` (`Vec<Option<MethodId>>`) | 3.40 MiB | 3.40 MiB |
| `class_cache` (`Vec<Option<ClassId>>`) | 0.50 MiB | 0.50 MiB |
| `virtual_cache` (`Vec<Option<VirtualCache>>`) | **17.02 MiB** | **3.49 MiB** |
| **total** | **29.04 MiB** | **15.51 MiB** |

`virtual_cache` is sized to the DEX `method_ids` table (446 k slots across the
40 boot DEX) at 40 B/slot (`VirtualCache` is `[(u32,u32); 4] + 2`), but only the
~3 000 executed virtual/interface call sites ever populate an entry. The slot is
now `Option<Box<VirtualCache>>` — **8 B of dense pointer per slot, heap value
only when populated** — which keeps the interpreter's O(1) two-level array probe
(`get`/`get_mut` + `as_ref`) exactly as before. No `HashMap`, no hashing on the
hot path, no behaviour change. `add_shared_dex` (`vec![None; methods]`) and the
restore-time clear (`*slot = None`) are unchanged; the only code delta in the
interpreter is the insertion arm (`Some(Box::new(vc))`).

### Measured (warm, p50 of 5 release runs)

| metric | before (HEAD `992f3a9`) | W146-A | Δ |
|---|---:|---:|---:|
| steady `RssAnon` (smaps rollup in `ACOS_PROFILE`) | **384.6 MiB** | **371.1 MiB** | **−13.5 MiB (−3.5 %)** |
| peak `VmHWM` (`vm_rss_peak_bytes`) | **479.2 MiB** | **465.7 MiB** | **−13.5 MiB (−2.8 %)** |
| `vm_dex_handle_caches` (census) | 29.04 MiB | 15.51 MiB | −13.53 MiB |
| `rss_apk_bytes` (control) | 79.8 MiB | 79.8 MiB | 0 |
| `vm_dex_bytes` (control) | 63.09 MiB | 63.09 MiB | 0 |
| `vm_code_bytes` (control) | 32.5 MiB | 32.5 MiB | 0 |
| reached stage | `first_frame` | `first_frame` | unchanged |

Per-run anon — before `384.6 ×5`; after `371.1, 371.1, 371.2, 371.1, 545.8`.
The 5th after-run was a boot-cache miss (it re-booted, `boot_runtime` 0.64 s
instead of 0.18 s); the four warm runs are flat at 371.1 MiB and the p50 is
371.1. Peak likewise `465.6, 465.6, 465.7, 465.8, 659.4` (p50 465.7). Both the
steady and peak deltas are exactly the removed slot bytes, so the boxed slots
are resident in the same place they were before. Raw profiles:
`results/bench/w146a-base-profile-warm.json`,
`results/bench/w146a-boxed-profile-warm.json`.

The win is **13.5 MiB > 5 MiB**, so it is kept.

## 2. Acceptance evidence (clean worktree `992f3a9` + W146-A, release)

| gate | result |
|---|---|
| `tools/diff-dexprogs.sh --corelib real` | **27 / 27 pass** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 / 27 pass** |
| `cargo test -p acos-dex -p acos-vm -p acos-runtime` | green **except** `acos-runtime --test pms`, which fails identically on unmodified HEAD (pre-existing golden mismatch, see below) |
| `cargo clippy -p acos-dex -p acos-vm -p acos-runtime --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | pass |
| `#![forbid(unsafe_code)]` | preserved (only `Box`, no unsafe) |
| boot-cache format | **unchanged** — the per-VM caches are not serialized (restore clears and rebuilds them), so `SNAPSHOT_FORMAT_VERSION`/`CACHE_FORMAT_VERSION` need no bump and old entries still load |

Pre-existing failure (verified by `git stash` + rerun on unmodified HEAD):
`pms_java_app_info_matches_the_reference_capture` panics `PMS golden mismatch`
in this worktree both with and without the W146-A diff, so it is environmental
(worktree lacks committed fixtures/reference state), not a regression.

## 3. Where the residual lives (item 5)

### 3.1 The census was extended

`acos-vm/src/profile.rs` now:
* emits `vm_dex_handle_caches_breakdown` (per-cache string/field/method/class/
  virtual bytes + the `VirtualCache` value/slot size);
* records the **20 largest anonymous `/proc/self/smaps` mappings** as
  `smaps_anon_00 … smaps_anon_19` census rows, each with resident bytes and the
  mapping header (`start-end perms`), so the residual is attributable by
  address range and size rather than a single "unaccounted" number. This is
  read-only diagnostics (no behaviour change, no allocation on the measured
  structures).

### 3.2 Warm-launch account (`ACOS_PROFILE`, W146-A)

| bucket | MiB | source |
|---|---:|---|
| `dex_raw` (40 `Arc<[u8]>`, process-shared) | 63.09 | census |
| `dex_parsed_tables` (id tables + class defs) | 22.88 | census |
| decoded `MethodCode` (`vm_code_bytes`, not in census) | 32.52 | note |
| `vm_dex_handle_caches` (after this change) | 15.51 | census |
| `dex_decoded_strings` (materialised string tables) | 15.43 | census |
| `vm_class_meta` (classes=2164) | 14.53 | census |
| `vm_heap_slots` (len 142 107 / cap 145 030) | 7.75 | census |
| `vm_method_meta`/`type_meta`/`interner`/registry/… | ~4.6 | census |
| retained boot-payload snapshot (`Arc<[u8]>`) | 3.39 | `allocations[]` |
| **named total** | **~180** | |
| steady `RssAnon` | **371.1** | note |
| **residual not named by the walk** | **~191** | |

### 3.3 The residual is five large glibc arena mappings, but the bytes are live

Largest anonymous mappings at end of launch (MiB, `start-end perms`):

```
120.43  7f9114000000-7f911b8c0000 rw-p
 67.39  7f9123aad000-7f9127e63000 rw-p
 63.80  7f8e3c000000-7f8e3ffff000 rw-p
 63.58  7f911c000000-7f911ff94000 rw-p
 32.80  7f8e30000000-7f8e320ef000 rw-p
  9.80  …   9.09  …   2.74  …   (frame/buffer-sized tails)
```

External A/B with `MALLOC_ARENA_MAX=1 MALLOC_TRIM_THRESHOLD_=0` (env is not
`ACOS_*`, so the warm snapshot still hits):

| | anon | peak | top mapping |
|---|---:|---:|---:|
| default | 371.1 MiB | 466.0 MiB | 120.4 MiB |
| `MALLOC_ARENA_MAX=1` | 364.6 MiB | 460.0 MiB | 186.3 MiB |

The arenas consolidate (120.4+63.8+63.6 → 186.3) but the total only drops
**6.5 MiB**: the 64 MiB-shaped mappings are glibc **arena heaps holding live
allocations**, not per-arena overhead. So arenas are not a lever (consistent
with W135-A §2); the residual is real retained bytes that the VM walk does not
reach.

Not yet named, ranked by likely size (this pass measured the mapping shape but
did not finish per-site tagging inside the W144-A-owned host/render crates, so
these are the remaining candidates rather than measured figures):
1. **Host framework state** — `ServiceHost`/ATMS/Binder/parcel registries and
   the preserved `ActivityThread` object graph. Small in guest-heap bytes
   (`vm_heap_live_bytes` 11.45 MiB) but each Java object carries a host `Vec`
   allocation, so the host side is a multiple of that.
2. **Render surfaces / `FrameStore`** — `1080×2400×4 ≈ 9.9 MiB` per live frame
   buffer; the 9.80 and 9.09 MiB mappings are the right shape for one or two of
   these (W133-A's `first_frame` path).
3. **Resources** — parsed `ResTable` copies for `framework-res.apk` + the app
   APK. `rss_apk_bytes` is 79.8 MiB of read-only file mapping (not anon); the
   *parsed* copy is anon and is not counted.
4. **Font/FreeType** — the process-global font tables (`fwk_fonts`,
   `gfx_text`) hold font bytes in anon memory; `rss_fonts_bytes` is 0 because
   the `.ttf`s are read, not `mmap`ed.
5. **Logs / green-thread stacks / JNI** — `counter.apk` loads no native code
   (`rss_dex_jar_bytes=0`, native-loader inventory empty), so JNI/loader
   mappings are ~0; log buffers are small.

The missing instrument to close this is a `profile::note_alloc` tag at the
`FrameStore` buffer construction, the `ResTable` parse, and the font-table
load; those sites live in the W144-A (render/wm/gfx) and W138-A (res natives)
ownership lanes, so they were deliberately left untouched here.

## 4. Item 2 (heap-slot shrink) — no warm win, not landed

`vm_heap_slots` shows `len=142 107 cap=145 030` on the **warm** launch
(7.75 MiB). The `cap=290 060 / len≈150 000` from the W139-A census is a
**cold-boot** state: the on-disk snapshot serialises `heap.objects.len()`
(not capacity) and `Vm::restore` rebuilds `Vec::with_capacity(object_slots)`, so
every restored (warm) VM already starts compact. A post-boot
`shrink_to_fit` would therefore only trim the one cold-boot process (which is a
different measurement than the required warm p50) and could only be validated
against the cold path. Per the `>=5 MiB` rule it is not a warm win, so it was
not landed; W145-A's chunked slot table is the right owner for any cold-boot /
peak-arena work (coordinated: no layout changes here).

## 5. Items 3–4 — not landed (45-minute rule)

* **Lazy per-string decode** (`dex_decoded_strings` 15.4 MiB): class loading
  touches a large fraction of the boot string table, and `DexFile::strings()` is
  already lazily materialised on first access (W139-A). A per-string cache
  (`OnceCell` per index / LRU-less decoded side table) is behaviour-preserving
  but needs a `string()` accessor change used across the VM; not attempted in
  the remaining window.
* **Interned class-map keys** (`vm_class_meta` 14.5 MiB): `direct_map` /
  `vtable_index` key on owned `(String, String)` pairs. Keying on interned
  `(u32, u32)` ids would cut several MiB but touches `class.rs` link/resolve,
  which is on the W145-A-adjacent hot path; left for a dedicated pass.

## 6. Files

* `crates/acos-vm/src/vm.rs` — `DexCtx::virtual_cache` is now
  `Vec<Option<Box<VirtualCache>>>` (dense 8 B slot, lazily boxed value).
* `crates/acos-vm/src/interp.rs` — the inline-cache fill arm boxes the new
  `VirtualCache`; probe path unchanged.
* `crates/acos-vm/src/profile.rs` — `vm_dex_handle_caches_breakdown` census row;
  `smaps_anon_NN` largest-anonymous-mapping rows.
* Raw profiles: `results/bench/w146a-base-profile-warm.json`,
  `results/bench/w146a-boxed-profile-warm.json`; patch:
  `results/perf/RSS_W146A.patch`.

## 7. Honest read / next levers

The 13.5 MiB cut is real, flat across runs, and moves both the steady-anon and
peak-VmHWM metrics by the exact removed slot bytes with all outputs identical.
The residual is now mapped to five glibc arenas holding ~364 MiB of **live**
bytes; only ~6.5 MiB of that is arena overhead, so the next work is per-site
tagging in the host/render/font lanes, not allocator tuning. Ranked next:
1. `profile::note_alloc` at `FrameStore` buffers + `ResTable` parse + font-table
   load (needs W144-A/W138-A coordination) to close the ~191 MiB gap.
2. `vm_class_meta` key interning (item 4).
3. Per-string lazy decode (item 3).
