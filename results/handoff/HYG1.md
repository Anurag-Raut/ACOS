# HYG1 handoff — test isolation in `acos-art`, and the `writeValue` wire layout in `parcelable.rs`

**Status: see the last section.** Worktree `/home/Qui/.paseo/worktrees/3oxa6xao/s3ab`, branch
`hyg1-tests-parcel` off `main` `863def1`. `compat-net1-connectivity` untouched.
Scratch: `.cache/hyg1/`, target dir `.cache/s3ab-target/`.

---

## 1. Task 1 — the S5 refusal test depended on process and host state

**Symptom (reproduced first, before any edit).** Alone, on the warm host, with the productised
data root:

```
$ tools/art-runtime/run-exclusive.sh --task HYG1 -- tools/guarded.sh --mem 8G --time 300 -- \
    env ACOS_ART_DATA=$PWD/.cache/art/data cargo test -p acos-art --test sandbox_s5 -- \
    refuses_to_run_when_no_cgroup_can_be_created --exact
test refuses_to_run_when_no_cgroup_can_be_created ... FAILED
panicked at crates/acos-art/tests/sandbox_s5.rs:96:
a host without a cgroup must refuse the run: RunCgroup { …, cg: "", … }
```

**Cause.** `RunCgroup::prepare` → `host_scope_capability()` (H15) answers from two caches before it
ever probes: a per-process `OnceLock` and a per-boot stamp `ACOS_ART_DATA/cgroup-capability.json`
(`.cache/art/data/…` held a valid stamp for the current boot id). The test faked a failing
`systemd-run` on `PATH`, so it only passed when it happened to be the first probe in a process *and*
the stamp was cold. Two other S5/S6 tests (`allocator_bomb…`, `fork_bomb…`) legitimately need the
real answer, so the dependence ran both ways.

**Fix (production behaviour unchanged).**
- `crates/acos-art/src/cgroup.rs`: the two `OnceLock`s are now one `Mutex<CapabilityCache>`
  (`root`, `answer`, `probe_dir`). `set_capability_root` keeps "first setter wins"; the answer keeps
  "one probe per process"; the per-boot stamp file is unchanged, same path, same format, same
  atomic write. Only new surface is `#[doc(hidden)] set_capability_test_seam(data_root, probe_dir)`.
  Production code never calls it; it forgets the in-process answer and names the data root and the
  directory holding `systemd-run`, so no test has to mutate the process `PATH` any more.
- `crates/acos-art/tests/sandbox_s5.rs`: the refusal test takes the guest mutex (the seam is
  process-global), points the seam at its own fresh scratch root and its own fake `systemd-run`,
  asserts the structured refusal, and restores the seam. No `PATH` mutation, no `ACOS_ART_DATA`
  dependence, no change to the assertion.
- New unit test `cgroup::tests::warm_stamp_answers_without_probing_and_a_cold_root_probes` is the
  teeth for both halves: a cold root with a failing probe refuses and writes **no** stamp; a stamp
  for this boot answers with **no usable systemd-run at all** (the H15 cache still working); another
  boot's stamp is stale and probes again.

**Verification (each run under one ART-lock acquisition, `.cache/hyg1/*.log`).**

| run | command | result |
|---|---|---|
| alone, warm host, twice in a row | `env ACOS_ART_DATA=.cache/art/data cargo test -p acos-art --test sandbox_s5 -- refuses_… --exact` | **1 passed** both times |
| `acos-art` lib unit tests | `cargo test -p acos-art --lib -- cgroup::tests` | **6 passed** |
| `acos-art`, default (parallel) mode | `cargo test -j 8 -p acos-art` | **rc=0, 151 passed, 0 failed** (`.cache/hyg1/art-default.log`) |
| `acos-art`, `--test-threads=1` | `cargo test -j 8 -p acos-art -- --test-threads=1` | **rc=0, 151 passed, 0 failed** (`.cache/hyg1/art-serial.log`) |

Before the fix the serial run fails on this test (the documented E13.8/G3cd note); after it, order
no longer matters. `--test-threads=1` remains the recipe for the S5/S6 corpus, but now because the
two bomb tests share a real pid/memory budget, not because of this test.

## 2. Task 2 — `writeValue`'s length word: **`VAL_MAP` was missing it, `VAL_PARCELABLE` had it right**

The task brief said `VAL_PARCELABLE` must not have a length word. That is **not** what API 34 does,
and the code is measured against the bytes, not the brief:

- `Parcel.writeValue` writes the tag, then — for `isLengthPrefixed(type)` only — a placeholder that
  is backpatched with `end - start` (`base/core/java/android/os/Parcel.java:2458-2481`).
- `isLengthPrefixed` lists `VAL_MAP`, `VAL_PARCELABLE`, `VAL_LIST`, `VAL_SPARSEARRAY`,
  `VAL_PARCELABLEARRAY`, `VAL_OBJECTARRAY`, `VAL_SERIALIZABLE` — nothing else (lines 4908-4926).
- `readValue` reads that word and compares it to the bytes consumed (lines 4529-4546).
- The shipped guest agrees: `.cache/boot/android-34/jars/framework.jar` (API 34) contains the
  `isLengthPrefixed` method in its dex.

So `VAL_PARCELABLE`'s length word (class name **and** payload, because the count starts after the
word) was already correct; **`VAL_MAP` was written and read without it**, which is what a real
framework client would misread: it would take `writeMap`'s size as the byte count and then `readInt`
the first key's type tag as the entry count. The `LIST_config` reply
(`provider.rs`, `Settings$NameValueCache` reads its `value` as a `HashMap`) is exactly this path,
and `acos-runtime/tests/ams_java.rs` only compared host bytes to peer bytes, so the wrong layout was
self-consistent and never failed a test.

**Fix.** `crates/acos-services/src/parcelable.rs`: `write_dynamic`/`read_dynamic` now share
`write_value_length_prefixed` / `read_value_length` / `check_value_length`, used by **both**
length-prefixed types it writes (`VAL_MAP`, `VAL_PARCELABLE`). The read side bounds the declared
length by the bytes actually left (before any allocation) and refuses a body that consumes a
different count than declared, where AOSP only logs `Slog.wtfStack`. A `VAL_PARCELABLE` with a null
class name now decodes as `D::Null` (AOSP: `readParcelableCreatorInternal` returns null for a null
name, `Parcel.java:5081-5085`), matching what the writer emits. Nothing else changed: the other
`VAL_*` types ACOS writes have no length word in AOSP and got none here.

**Golden tests.** `crates/acos-services/tests/parcel_dynamic_golden.rs` — 16 tests, one or more per
`VAL_*` type the module writes, asserting the exact little-endian word layout (tag, length word,
body) rather than a round trip, plus teeth: the old length-less map layout is refused, a body that
disagrees with its length word is refused, a length beyond the parcel is refused, a negative length
is refused. `crates/acos-services/tests/gate1_bounds.rs` (the W44 hostile-count gate) was updated to
the API-34 layout and gained a "length beyond the parcel" case; the hostile `i32::MAX` count is still
refused without allocating.

**Callers.** `grep -rn "val::MAP\|D::Map(" crates/` finds no other writer of the old layout:
`codec.rs` and `write_bundle` go through `write_dynamic` (fixed once), and `read_bundle`'s
`BaseBundle` length is a different, unchanged format. NET1's `write_parcelable_bundle` is untouched
and its tests pass.

**Verification.**

| run | command | result |
|---|---|---|
| golden + gate1 | `cargo test -p acos-services --test parcel_dynamic_golden --test gate1_bounds` | **4 passed + 16 passed** |
| `acos-services` | `cargo test -j 8 -p acos-services` | **rc=0, 41 binaries, 319 passed, 0 failed** (`.cache/hyg1/services2.log`) |

## 3. Gates and Gaten (the parcel change is on the binder path)

Run under one ART-lock acquisition each, with the release `acos`/`acos-art` from
`.cache/s3ab-target/release` (`.cache/hyg1/run_suite.sh`, JSON under
`.cache/hyg1/suite/`).

| gate | expected | measured |
|---|---|---|
| `v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`, `w15-dialog-menu`, `form-validation`, `suite/T03-render-good`, `v1-offscreen-pager` | pass | **8/8 pass** |
| `suite/T03-render-wrong` | `ASSERTION_FAILED` | **`ASSERTION_FAILED`** |
| `negative/offscreen-pager-visible` | `ELEMENT_OFFSCREEN` | **`ELEMENT_OFFSCREEN`** |
| `p11-progress-timeout` | `TIMEOUT` | **`TIMEOUT`** |

| Gaten flow | expected | measured |
|---|---|---|
| `scenarios/gaten-{home,tabs,settings,search,lesson}.yaml` | pass | **5/5 pass** |
| `scenarios/negative/gaten-*-wrong.yaml` | `ASSERTION_FAILED` | **5/5 `ASSERTION_FAILED`** |

`acos-verify` 66/66 and `acos-cli` 147/147 green (`rc=0`, `.cache/hyg1/verify.log`,
`.cache/hyg1/cli.log`).

## 4. State, caveats and the smallest next action

- Commits on `hyg1-tests-parcel`: see the log; nothing was pushed and no other branch or
  worktree was touched.
- The only new public surface is `#[doc(hidden)]
  acos_art::cgroup::set_capability_test_seam`. It exists because integration tests are built
  against the crate without `cfg(test)`; production never calls it, and the cache's production
  behaviour (one probe answer per process, one stamp per boot id, atomic write) is unchanged.
- The parcel fix is proven at the byte level for every type the module writes, and proven
  behaviour-neutral for the verified flows (all gates and Gaten flows identical). What is **not**
  yet proven end to end is a *non-empty* `HashMap` value crossing to a real framework reader:
  `LIST_config` answers an empty map on this image. Smallest next action: a scenario or fixture
  that makes the guest read a non-empty `VAL_MAP` (e.g. a `Settings$Config` provider call through
  `Settings$NameValueCache`) and assert the guest saw the entries.
- `cargo test -p acos-services` needs `-j 8` (not the default `-j 32`) inside the 12 GiB guard:
  at 32 jobs the linker fork fails `EAGAIN`/`lld` aborts (`error: could not exec the linker cc`
  in `.cache/hyg1/services.log`). No code change; record the flag if you re-run.
