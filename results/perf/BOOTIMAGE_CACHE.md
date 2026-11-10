# W114-A — on-disk serialization of the fully linked `BootImage`

**Status:** implemented, measured. Clean worktree of `33e0c77` (= current `main`,
after W111-A), `CARGO_TARGET_DIR=.cache/target-w114a`,
`CARGO_PROFILE_DEV_DEBUG=0`, release build, real corelib, fixture
`fixtures/build/apps/counter.apk`.

## 1. What was cached before

`crates/acos-runtime/src/boot_image_cache.rs` (T-43b / W96-A) persisted the
framework `ACOSSNAP` heap+class-descriptor snapshot. On a cold process,
`Vm::restore` re-linked the boot classpath from DEX (`Vm::new` + `load_class`
for every class descriptor) and then re-applied the snapshot. W96-A measured
~65 ms for that restore; the "next lever" it named was to persist the
**linked** class metadata. That is what this wave adds.

The in-process `BootImage` (`crates/acos-vm/src/snapshot.rs`) already existed:
one clone-once image of `classes`/`methods`/`types`/maps/decoded `MethodCode`
bodies, shared by every restore in one process. It could not cross a process
boundary.

## 2. What this wave serializes

`BootImage` is written to a compact borsh stream and stored inside the existing
framed on-disk payload:

* `classes` (descriptor, superclass, interfaces, field layouts, vtables,
  direct/virtual maps, `InitState`, `abstract_methods`),
* `methods` (identity, access, `insn` sizes, and `MethodImpl` — `Dex` with its
  full pre-decoded `MethodCode`: `insns`, `pc_to_idx`, `raw`, tries, line
  table; `Abstract`; `Native` as a tag),
* `types`, `type_by_desc`, `class_by_desc`, `loaded_array_elems`,
  `builtin_alloc`, the special class ids, `boot_dexs`, `boot_lazy_jars`,
  `boot_dir`,
* the **provider path list** (`DexCtx::source`, added this wave) so a fresh
  process re-opens the identical `vm.dexs` order through the process-global
  `acos_dex::shared_dex` cache.

A raw `NativeFn` cannot cross a process: `MethodImpl::Native` decodes to a
placeholder and `Vm::install_boot_image` re-binds every native from the
freshly installed registry by `(class descriptor, name, signature)`
(`Vm::rebind_serialized_natives`). A miss degrades to `Abstract` (a structural
`UNSUPPORTED_JAVA_API`), never the wrong native. This also re-applies the
VM-internal bytecode overrides that `bind_dex_native` installs at boot.

Cache-format details:

* `CACHE_FORMAT_VERSION` → **2**; the version is mixed into `image_key`, so old
  entries do not collide.
* `ACOS_NO_BOOT_CACHE=1` / `ACOS_BOOT_CACHE=0` still disable the cache.
* `ACOS_BOOT_CACHE_NO_IMAGE=1` stores/reads a v2 entry **without** the linked
  image — the same-binary A/B escape used for the table below.
* Every write stays `tmp + fsync + rename` under a private `0700` dir, guarded
  by the `O_EXCL` lock and the bounded LRU. The file is read with a plain
  `read` into a `Vec` under the 64 MiB cap; the crates remain
  `#![forbid(unsafe_code)]` (borsh is safe Rust).
* Any header/version/key/checksum/length failure or a borsh decode failure is a
  **miss**; `RuntimeTemplate::cached_hit` drops the entry and falls back to a
  real boot. A mismatched image simply fails the class-descriptor key and the
  restore re-links.

## 3. Measurement

The snapshot's `property_hash` mixes **every `ACOS_*` environment variable**, so
the populate and warm runs of one pair were run with an identical environment
(fixed `ACOS_PROFILE_OUT`). 3 warm fresh-process rounds per pair, interleaved
order. Raw: `results/bench/w114a-cache.json`.

| metric (warm fresh process) | before (no linked image) | after (linked image) | Δ |
|---|---:|---:|---:|
| `boot_runtime`, p50 | 192.7 ms | **175.3 ms** | **−17.4 ms (−9.0 %)** |
| `boot_runtime`, min | 191.1 ms | 172.6 ms | −18.5 ms |
| launch wall, p50 | 727 ms | 721 ms | −6 ms |
| peak RSS, p50 | 577 MiB | 571 MiB | −6 MiB |
| boot-class `class_parses` | 1931 | **962** | −969 |
| cache entry bytes | 1 657 776 | **21 068 681** | +19.4 MiB |
| blocker | `ImageDecoder.nCreate` | `ImageDecoder.nCreate` | same |

The 962 remaining parses after the image are the **application** DEX classes
loaded per launch; the 969 removed are the boot-classpath re-link. `<clinit>`
count is unchanged (287) because the pre-attach snapshot state is the same.

**The ~13 ms target was not reached.** Honest breakdown of why:

1. The restore is now dominated by `BootImage::from_bytes`, which (a) re-opens
   and parses the ~63 MiB boot DEX provider set through `acos_dex::shared_dex`
   and (b) borsh-decodes ~19 MiB / 1027 classes / 38 k methods / decoded
   `MethodCode`. That costs roughly as much as the class re-link it replaces.
2. The heap/snapshot decode, `reseed_after_restore`, and the per-launch app DEX
   load are untouched and together dominate `boot_runtime`.
3. To reach 13 ms the *parsed* DEX provider tables (string/type/proto/field/
   method id pools) would also have to be serialized and zero-copy decoded, not
   just the link output. That is a follow-on wave.

The linked image is therefore a **real but modest** win (−17 ms, −9 %) that
costs 12× cache size. It is correct and gated; whether to merge it should be
weighed against the size/cost. The honest recommendation is to keep it behind
the existing cache and revisit the DEX-table serialization before declaring
the lever closed.

Full-boot control (`ACOS_NO_BOOT_CACHE=1`): `Vm::new` alone ~17 ms, launch wall
~765 ms, 448 `<clinit>` — unchanged.

## 4. Acceptance evidence

| Gate | Result |
|---|---|
| `cargo test -p acos-vm` | all suites pass |
| `cargo test -p acos-runtime` | all suites pass (1 pre-existing ignored) |
| `cargo check --workspace --all-targets` | pass |
| `cargo clippy -p acos-vm -p acos-runtime --all-targets --no-deps -- -D warnings` | clean |
| `tools/diff-dexprogs.sh --corelib real` | **27 / 27 pass** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 / 27 pass** |
| `acos launch counter.apk` blocker | `UNSUPPORTED_JAVA_API ImageDecoder.nCreate` (same as before) |
| corrupt/truncated/oversized/version/key fallback | cache unit tests pass |

## 5. Files

* `crates/acos-vm/src/snapshot.rs` — `BootImage::{to_bytes,from_bytes}`,
  `Vm::boot_image_bytes`, `Vm::restore_with_boot_image`.
* `crates/acos-vm/src/class.rs` — borsh derives + manual `MethodImpl` codec.
* `crates/acos-vm/src/vm.rs` — `DexCtx::source`, `rebind_serialized_natives`,
  rebind on install.
* `crates/acos-vm/src/corelib/mod.rs` — `BuiltinAlloc` derive.
* `crates/acos-runtime/src/session_pool.rs` — framed payload carries the image;
  capture on store, `restore_with_boot_image` on hit; `ACOS_BOOT_CACHE_NO_IMAGE`
  A/B escape.
* `crates/acos-runtime/src/boot_image_cache.rs` — `CACHE_FORMAT_VERSION = 2`.
* `tools/diff-dexprogs.sh` — honours `CARGO_TARGET_DIR` and `ACOS_BIN`.

---

# W131-A — parsed DEX pool serialization + boot-cache hardening

**Status:** implemented, measured, honest negative on the latency target. Clean
worktree of `47e1c38` (HEAD), `CARGO_TARGET_DIR=.cache/target-w131a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `tools/guarded.sh`, release build, real corelib,
fixture `fixtures/build/apps/counter.apk`.

## 1. What this wave adds

The W114-A linked `BootImage` still re-parsed the three eager boot DEX files on
every cold process (`BootImage::from_bytes` → `acos_dex::shared_dex`). This wave
persists the **parsed id/string/type/method pools** of each provider inside the
same framed cache payload, so a restore can rebuild each `acos_dex::DexFile`
from the tables plus a plain re-read of the raw bytes instead of re-running
`DexFile::parse`.

* `crates/acos-dex/src/persist.rs` (new): `DexTables` (header, `string_data_offs`,
  `type_ids`, `proto_ids`, `field_ids`, `method_ids`, `class_defs`,
  `call_site_ids`, `method_handles`, `map`, optional `hiddenapi`) with borsh
  derives; `DexFile::{to_tables, from_tables}`. `from_tables` validates the raw
  magic/size and header/pool length agreement, and returns a structured
  `DexError` on any mismatch.
* `crates/acos-dex/src/cache.rs`: `BootCache::get_or_load_with_tables(path,
  Option<DexTables>)`. A table failure falls back transparently to
  `DexFile::parse`; the cache never installs a half-validated file.
* `crates/acos-vm/src/snapshot.rs`: `SerializedBootImage` gains
  `dex_tables: Vec<Option<DexTables>>`; `to_bytes` snapshots the pools and
  `from_bytes` consumes them. `ACOS_BOOT_CACHE_NO_DEX_TABLES=1` is the
  same-binary A/B escape.
* `CACHE_FORMAT_VERSION` 2 → 3 (old entries miss).

## 2. Hardening (W121-A #7, #8, #9)

* **#7 checked frame arithmetic.** `session_pool::decode_boot_payload` now does
  every `pos + n` through `checked_add` filtered by `end <= bytes.len()`; a
  crafted `snap_len`/`img_len` (including `u64::MAX`) is a clean `Err`. Covered
  by `session_pool::payload_tests`.
* **#8 boot-dir containment.** `BootImage::from_bytes` canonicalises
  `boot_dir` and every `dex_paths` entry and rejects a provider outside the
  boot tree (or one that does not resolve). A poisoned cache entry is a
  structured miss, not an arbitrary host file read.
* **#9 cache dir identity.** `ensure_private_dir` rejects a symlinked root
  before `create_dir_all`, re-checks after creation, requires
  `st_uid == euid` (read from `/proc/self/status`, no `unsafe`), and verifies
  mode `0700` after tightening. Tested by `symlinked_cache_dir_is_rejected` /
  `non_directory_cache_root_is_rejected`.

## 3. Measurement (5 interleaved fresh-process release runs)

Separate warm cache dirs per variant, fixed `ACOS_CACHE_DIR`/`ACOS_PROFILE_OUT`
per variant so the snapshot `property_hash` matches; alternating order.

| metric (warm, p50 of 5) | before (no DEX tables) | after (DEX tables) | Δ |
|---|---:|---:|---:|
| `boot_runtime` | **188.9 ms** | **193.2 ms** | **+4.3 ms (+2.3 %)** |
| launch wall | **938 ms** | **949 ms** | **+11 ms (+1.2 %)** |
| peak RSS | 537 MiB | 536 MiB | −1 MiB |
| boot-class `class_parses` | 1 096 | 1 096 | 0 |
| cache entry bytes | 25 576 627 | **46 906 082** | **+21.3 MB (+83 %)** |
| blocker | `PropertyValuesHolder.nGetFloatMethod` | same | unchanged |

The raw W114-A baseline (`boot_runtime` p50 175.3 ms) is not directly
comparable: this host/HEAD measures the no-tables control at 188.9 ms. Within
one binary, serializing the parsed pools is a **net loss** of ~4 ms and 21 MB.

**Why it loses.** The three eager providers total ~9.1 MiB; `DexFile::parse`
for them is only a few milliseconds, while the borsh decode of the 21.3 MB
`DexTables` blob costs more than the parse it replaces. The raw read (~9 MiB)
is paid in both variants. `from_tables` does rebuild a full `DexFile`, so
correctness is unaffected, but the ~13–30 ms target is not reachable this way
with the current eager providers.

**Honest recommendation.** Keep the feature behind the cache and behind
`ACOS_BOOT_CACHE_NO_DEX_TABLES=1`, but do not enable it by default: it is
strictly worse on time and size. The real levers remain the anonymous ~443 MiB
RSS (W111-A §4) and the framework `<clinit>` time.

## 4. Acceptance evidence (clean worktree, W131-A)

| gate | result |
|---|---|
| `cargo test -p acos-runtime boot_image_cache` | **12/12 pass** (incl. 2 new hardening tests) |
| `cargo test -p acos-runtime payload_tests` | **3/3 pass** (crafted/truncated frames) |
| `cargo test -p acos-dex -p acos-vm -p acos-runtime` | green (one pre-existing PMS golden failure, `pms_java_app_info_matches_the_reference_capture`, independent of this diff) |
| `cargo clippy -p acos-dex -p acos-vm -p acos-runtime --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | pass |
| `tools/diff-dexprogs.sh --corelib real` | **27 / 27 pass** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 / 27 pass** |
| `acos launch counter.apk` ladder | same stages, same blocker as the no-tables control |

## 5. Files

* `crates/acos-dex/src/persist.rs` (new) — `DexTables`, `to_tables`/`from_tables`.
* `crates/acos-dex/src/{lib,annotation,encoded,hiddenapi,cache}.rs` — borsh
  derives, `get_or_load_with_tables`.
* `crates/acos-vm/src/snapshot.rs` — `SerializedBootImage::dex_tables`,
  `to_bytes`/`from_bytes`, boot-dir containment.
* `crates/acos-runtime/src/boot_image_cache.rs` — `CACHE_FORMAT_VERSION = 3`,
  owner/symlink/mode checks + tests.
* `crates/acos-runtime/src/session_pool.rs` — checked `decode_boot_payload`,
  frame tests.
