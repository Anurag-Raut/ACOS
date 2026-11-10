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
