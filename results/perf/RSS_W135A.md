# W135-A — launch anonymous-allocation cuts (transient boot-cache buffers)

**Status:** implemented, measured, honest on the peak-RSS result. Clean worktree
of `5a5dc2f` (HEAD), `CARGO_TARGET_DIR=.cache/target-w135a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `tools/guarded.sh`, release build, real corelib,
fixture `fixtures/build/apps/counter.apk`, warm on-disk boot cache. No commit.

## 1. What was cut

The warm launch decoded the on-disk boot-cache payload through a chain of
full-size heap copies. Each was eliminated without changing the payload format
(cache version stays 3) or any observable behaviour:

| # | site | before | after | buffer avoided |
|---|---|---|---|---|
| 1 | `boot_image_cache::decode` | `load()` read the ~25.5 MiB file into `buf`, then `decode` returned `payload.to_vec()` while `buf` was still live | `decode` consumes the read `Vec` and `drain`s the header in place | **~25.5 MiB / load** |
| 2 | `session_pool::decode_boot_payload` | copied the linked-image segment out of the payload with `.to_vec()` | returns the image as a **borrow** of the payload buffer (it only needs to live through the following `Vm::restore`) | **~21.2 MiB / load** |
| 3 | `acos-vm::profile` | no allocation accounting | `note_alloc(category, bytes)` call-site counters, surfaced as `allocations[]` in the `ACOS_PROFILE` report | diagnostic |

The `ACOS_PROFILE` report now carries a call-site allocation table. A
profile-enabled warm launch reports (two cache loads per launch):

```
allocations (ACOS_PROFILE=1):
  boot_cache_file_read     48.78 MiB  x2   (25.5 MiB read, twice)
  boot_cache_payload       48.78 MiB  x2   (retained payload, twice)
  boot_payload_image       42.32 MiB  x2   (logical image size; now BORROWED, not allocated)
  boot_image_serialized    42.32 MiB  x2   (borsh input to BootImage::from_bytes)
  boot_payload_snapshot     6.46 MiB  x2   (3.2 MiB owned snapshot, twice)
  vm_install_boot_image     3.37 MiB  x1   (per-restore clone estimate)
```

So per cache load the code now avoids **~46.7 MiB** of transient buffers (25.5
payload + 21.2 image); the launch performs two loads, for **~93 MiB** of
allocations that no longer happen at all.

## 2. Measurement

Host in the measurement window was loaded (load average 4.6–7.8), so wall time
is noisy; RSS is the primary metric. Each variant was measured against its own
warm cache with an identical `ACOS_*` environment (the snapshot `property_hash`
mixes every `ACOS_*` var, so cross-binary cache sharing is invalid — this was
also why an early interleaved A/B was discarded).

| metric (warm, p50) | before (5a5dc2f) | W135-A | Δ |
|---|---:|---:|---:|
| peak RSS (`VmHWM`) | 561.7 MiB | 563.9 MiB | +2.2 MiB (within the 560–620 MiB run spread) |
| steady RSS after launch (`ACOS_PROFILE` `rss_bytes`) | 621.4 MiB | 618.2 MiB | **−3.2 MiB** |
| steady `rss_anon_bytes` | 489.9 MiB | 486.8 MiB | **−3.1 MiB** |
| `rss_apk_bytes` (control) | 115.6 MiB | 115.6 MiB | 0 |
| `vm_code_bytes` (control) | 34.88 MiB | 34.88 MiB | 0 |
| `vm_dex_bytes` (control) | 63.09 MiB | 63.09 MiB | 0 |
| reached stage | `on_resume` | `on_resume` | unchanged |
| cache entry bytes | 25 576 627 | 25 576 627 | unchanged |

**Why peak RSS did not move.** `VmHWM` is reached *after* the restore, during
the app lifecycle (`bind_application`/`client_transaction`), not during the
boot-cache decode. The removed buffers are sequential, >128 KiB, `mmap`-backed
allocations that glibc already returns to the kernel on free, so freeing them
earlier does not lower the later high-water mark. The steady/end-of-launch anon
figure is the one that tracks them, and it drops by ~3 MiB.

**Allocator arenas are not the residual.** An external A/B (no code change) on
the baseline binary showed `MALLOC_ARENA_MAX=1` and `MALLOC_TRIM_THRESHOLD_=0`
leave the peak unchanged (806.3 MiB vs 806.3 MiB in a full-boot control,
805.2–806.4 MiB). The ~330–490 MiB anonymous residual is retained framework /
VM structures and lifecycle allocation, not unreturned arenas. This confirms
W111-A §5 guidance to avoid `malloc_trim` as a lever.

## 3. Where the retained anonymous memory actually is

A profile-enabled warm launch accounts for the resident `RssAnon` as:

| bucket | size |
|---|---:|
| raw boot DEX bytes (40 `Arc<[u8]>`, process-shared) | 63.1 MiB |
| decoded `MethodCode` (process-shared `Arc`) | 34.9 MiB |
| managed heap (live = peak) | 12.0 MiB |
| method + class metadata | 2.6 MiB |
| linked `BootImage` + per-restore clone | ~tens of MiB |

`vm_install_boot_image` (the per-`Vm::restore` clone of classes/methods/types)
is only **~3.4 MiB**, because every `MethodCode` body inside is an `Arc` shared
by pointer. That makes the "restore twice, clone twice" path a **latency**, not
RSS, lever: `RuntimeTemplate::from_cached` restores a throwaway `Vm` to recover
`activity_thread`, then `restore_runtime` restores again. The follow-on fix is
to persist `activity_thread` in the framed payload and drop the throwaway
restore (no payload-format version bump is needed only if the field is appended
behind a new `CACHE_FORMAT_VERSION`).

## 4. Acceptance evidence (clean worktree `5a5dc2f` + W135-A, release)

| gate | result |
|---|---|
| `cargo test -p acos-runtime --test boot_image_cache` | **6 / 6 pass** |
| `cargo test -p acos-runtime --lib payload_tests` | **3 / 3 pass** (crafted/truncated frames) |
| `cargo clippy -p acos-vm -p acos-runtime -p acos-dex --all-targets --no-deps -- -D warnings` | clean |
| `cargo check --workspace --all-targets` | pass |
| `tools/diff-dexprogs.sh --corelib real` | **27 / 27 pass** |
| `tools/diff-dexprogs.sh --corelib real --gc-stress` | **27 / 27 pass** |
| ladder / blocker | `on_resume`, same as baseline |
| `#![forbid(unsafe_code)]` | preserved (no global allocator installed) |

`git apply --check` of the patch (`crates` only) against HEAD is clean.

## 5. Files

* `crates/acos-runtime/src/boot_image_cache.rs` — `decode` consumes and drains
  the read buffer; no `payload.to_vec()`.
* `crates/acos-runtime/src/session_pool.rs` — `BootPayload<'a>` borrows the
  linked image; `from_cached` passes it without a copy; test adapted.
* `crates/acos-vm/src/profile.rs` — `note_alloc` / `alloc_notes_snapshot` and
  the `allocations[]` report field.
* `crates/acos-vm/src/snapshot.rs` — note the serialized image input size.
* `crates/acos-vm/src/vm.rs` — note the per-install clone estimate.

## 6. Honest recommendation

Keep the change: it is strictly fewer allocations with identical outputs and
green gates. It does **not** reach the primary BEYOND-BASELINE goal of a large
peak-RSS cut, because peak is lifecycle-bound, not decode-bound. The next
levers, in measured priority, are (1) lazy method-code decode / not persisting
bodies that are never invoked, (2) collapsing the double restore, and (3)
reducing the 63 MiB raw-DEX retention — none of which are transient-buffer
items.
