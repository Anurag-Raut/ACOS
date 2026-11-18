# MERGE040D — glship + gpuspike + wvkill + emubench into `modules`, then three follow-ups

Branch `merge040d` from `modules` @ `aaf518bf8` (MERGE040C). One commit per track,
then one commit per follow-up. Conflict table + intent first, then the three
follow-ups, then the verification.

```
c15f38b3d MERGE040D(1/4) glship   f0e15daa   (merge, 1 resolved conflict)
4455ad6d5 MERGE040D(2/4) gpuspike 31dfa9f8   (merge, 3 resolved conflicts)
993086d59 MERGE040D(3/4) wvkill   d90b47e61  (merge, auto; = wvscope 81693fbf + WVKILL)
7c7586760 MERGE040D(4/4) emubench 7eca4825   (merge, auto)
7c5022e41 MERGE040D(a)  the GL provider contract is its own crate
da34e235e MERGE040D(b)  warm launch measured at one observation boundary
45e88c92e MERGE040D(c)  the install row compares (ACOS "APK ready")   <- HEAD
```

`cargo check --workspace --all-targets`: green after every step
(`00-cargo-check-final.txt`, and each merge commit's message).

## 1. Nothing was squashed: no branch carries > 5 MB

Checked every commit's tree in `modules..<branch>` (not just the tip), largest
*added* blob per branch:

| branch | commits | largest added blob |
|---|---:|---|
| glship `f0e15daa` | 8 | 0.94 MB `results/handoff/GLSPIKE2/…/launcher.stderr.log` |
| gpuspike `31dfa9f8` | 11 | 0.94 MB (same file, shared history) |
| wvkill `d90b47e61` | 2 | 0.11 MB `results/handoff/WVSCOPE/evidence/cargo-test-3crates.log` |
| emubench `7eca4825` | 5 | 0.46 MB `EXPERIMENTS.md` |

The 10.41 MB `results/native-loader/inventory/gaten-after.json` that appears in
every tree is **already in `modules`** and unchanged by these branches, so
merging does not re-add it. **Nothing was dropped, no `--squash` was needed**
(contrast MERGE040C, whose backsettle history carried ~157 MB of matrix frames).

## 2. Conflicts and resolutions (one implementation per seam)

### 2.1 glship — `tools/build-apps.sh` (1 conflict)

Theirs re-merged the two `for app in` headers into one short list; ours
(MERGE040A/B) is the **union** list, which is the fix for the shadowed loop:
taking theirs would silently drop `permissioncontroller`, `perm-probe`,
`systemui`, `launch-extras-probe`. Resolution: **ours**, plus glship's addition
on top (`build_apk egl-probe`). Everything else auto-merged (gles.rs, the
`Sandbox::command` bind, props.rs, doctor's `gles` check, acos-mcp capabilities,
`art-gles.sh`/`art-sysroot.sh`, `install.sh`, `LaunchMain`, `egl-probe` and
`gles-software` scenarios). `a11y-probe`'s fixture and
`scenarios/a11y-screenshot-wrap.yaml`: only glship touched them since the merge
base (`git diff 738796898..modules` is empty for both), so taking theirs is not a
regression.

### 2.2 gpuspike — `gles.rs`, `lib.rs`, `art-gles.sh` (3 conflicts)

**One selection rule, not two.** glship's `gles.rs` is the base
(`status`/`selected`, default ON when installed, `<provider>/.verified` cache).
GPUSPIKE's additions are grafted onto it: manifest `props` + `render_node`,
`valid_prop_key`/`valid_render_node`, `Provider::render_node`, and
`selected_props()` **built from `status()`** (so `props.rs`'s property table and
`Sandbox::command` share one rule). Dropped with the merge, because glship's
version supersedes them:

- gpuspike's `selected()` (flag-only, default off) — glship's `selected()` is
  `status().provider`, so `mesa-host` stays **opt-in**: nothing selects it
  implicitly and an uninstalled explicit name is a structured error;
- gpuspike's always-hash `verify()` — **mesa-host now verifies through glship's
  `.verified` cache** (the brief's requirement), so a host-GPU run pays the same
  one-time digest and 8 `stat`s per launch after;
- its duplicate `ENV_LOCK` test guard and its `selected_tags_for` test helper
  (both existed in glship's side under one name).

`lib.rs`: glship's `gles::status(r)` block kept, gpuspike's
`gpu_landlock_plan` + `--dev-bind` render node added **inside it** — one
`Some(provider)`, one `verify`, one `lib_dir_ready`.
`art-gles.sh`: union — glship's provenance block and cache note with
gpuspike's `--extra-lib/--extra-hw/--prop/--render-node` docs, argv and manifest
keys (and the stale "caches the digest under `<sysroot>/../gles-verify/`" comment
corrected to `<provider>/.verified`, which is what the code does).

Seam grep after the merge (`crates/acos-art/src/gles.rs`): one each of
`status`, `selected`, `selected_tags`, `selected_props`, `load`, `verify`,
`verify_full`, `signature`, `verify_cache_path`, `render_node`,
`provider_dir_override`, `capability_for`, `valid_render_node`,
`valid_prop_key`; `gles::` tests 23/23 pass.

### 2.3 wvkill — no conflict (auto-merge)

d90b47e61 = wvscope `81693fbf` + the engine-SIGKILL fix, on top of wvtail which
`modules` already carries (MERGE040C), so the chain applies linearly. Merged
whole (wvscope included), per the orchestrator's instruction.

Seam grep: one `View::render_input`, one `wait_input_ack`, one `InputAcks`, one
`Attempt`, one `FairGate::pull`, one `build_id()` (`acos-webview/src/lib.rs`)
backed by the single `build-support/fingerprint.rs` that `acos-mcp/build.rs` now
also uses (140 lines removed there), one `Gate::endpoint`. The input-priority tap
path MERGE040B cut to one implementation is unchanged context in this diff.

### 2.4 emubench — no conflict (auto-merge)

Adds only: BENCHMARKS §13, EXPERIMENTS entries,
`tools/art-runtime/emubench/{emu_bench,emu_gate}.py` + `run-emubench.sh`, the
evidence bundle and 7 lines in `tools/run-matrix.sh`.

## 3. Follow-up (a): the provider contract is a crate — `acos-gles-provider`

`crates/acos-gles-provider` (new; deps `serde` + `serde_json` only) owns the
selection rule, the manifest (`tag`, `vulkan_tag`, `props`, `render_node`, sha256
per file), the verify cache, `valid_render_node`/`valid_prop_key`, the render
node, `describe`/`staged_bytes` and the 23 tests. Every function takes the guest
sysroot as `&Path`; the crate reads only the `ACOS_GUEST_GLES*` variables, so it
is a pure function of a path plus the flag (no dependency on the ART launcher).

**The name, and why it is not literally `acos-gles`:** that name is already the
guest-side GLES2/EGL **substrate** (ADR-0025 — `glsl.rs`, `raster.rs`, the JNI
entry points `acos-jni` installs). Renaming a shipped, guest-ABI-facing crate
mid-cycle would touch `acos-jni`, `acos-runtime`, their benches and every branch
in flight, so the provider crate takes the qualified name; the crate docs record
the one-commit rename if the orchestrator prefers it.

Callers:

| crate | change |
|---|---|
| `acos-art` | `src/gles.rs` is now a ~200-line facade (`ArtRoot` → `&root.sysroot`) + `From<acos_gles_provider::Error> for Error`; the launcher, property table and landlock plan are unchanged; `build_cache::sha256_hex` delegates (one digest implementation for art + provider) |
| `acos-cli` | `root::sysroot()` (testable rule) and `doctor`'s `check_gles()` asks the crate |
| `acos-mcp` | **`acos-art` removed from its dependencies**; `paths::sysroot()` + `gles_json()` ask the crate |

`cargo tree -p acos-mcp | grep -c acos-art` → **0** (`10-cargo-tree-mcp.txt`).
The `acos-gles` line that remains under `acos-mcp` is the substrate via
`acos-jni`.

Tests: provider 23, `acos-art` 200 (incl. a new facade test that pins
`<ArtRoot>/sysroot/gles` — the seam that would silently report "not installed" if
the translation were wrong), `acos-mcp` 95, `acos-cli` 201 (`--bins`).

## 4. Follow-up (b): warm launch at one observation boundary

Definition (BENCHMARKS §13.3): a launch is finished when the UI **tree** can be
read, reports the expected content, and is identical to the previous read. That
is ACOS's strict settle; the emulator is now measured the same way
(`settled_tree()`: two consecutive agreeing `uiautomator` dumps with `count 0`,
timer started before `am start`).

- `emu_bench.py`: `launch_to_tree()`, `settled_tree()`, the `n1-warm-tree` phase
  (one instance, ≤2048 MiB guest, RAM-guarded, `stop_all()` in `finally`, 600 s);
  the old frame-change row is still recorded from the same launch.
- `emu_gate.py`: `warm_launch` pairs `m4_launch_strict_ms` ↔ `warm_launch_tree_ms`;
  the frame-only comparison is a `REFERENCE` row (printed, never scored).
- Measured (exclusive, guarded `--mem 24G`, host peak **35.6 %**): emulator
  **5004.9 ms** p50 (n=5, p90 5788, spread 19.5 %) → ACOS 288.4 ms,
  **−94.2 %, better**. Self-test 14 checks (4 new), 0 failed.

## 5. Follow-up (c): the install row compares (ACOS "APK ready")

`acos_bench.py --phase install`: per rep, a fresh session, and the launcher's own
stage ladder (`ACOS_ART_STAGE_TIMING=1`, forwarded by the engine) gives
`launch-entry` → `guest-exec` — "APK path → launchable": ingest (package facts,
manifest, dex list), staging into `<data>/app`, native-lib extraction,
DEX/classpath prep; every rung inside the window is a substage delta.

- **`m9_apk_ready_ms` = 10.0 ms** p50 (n=5: 50/10/10/10/0; 10 ms = one jiffy of
  the instrumentation) vs the emulator's first install **1358.8 ms** → **−99.3 %**.
- **`m9_apk_ready_cold_ms` = 20730 ms** (first launch in an empty scratch: it
  also builds the launcher's shared caches — derive_classpath, linkerconfig,
  guest DEX, shims). Recorded, **never** mixed into the per-APK number; the
  emulator pays its equivalent inside its 13.6 s cold boot.
- No ladder → no number (loud error), never a fabricated one.

## 6. Verification

| # | Check | Result | Evidence |
|---|---|---|---|
| 1 | `cargo check --workspace --all-targets` | green (warnings only) | `00-cargo-check-final.txt` |
| 2 | `cargo test -p acos-art -p acos-gles-provider -p acos-verify -p acos-protocol` | **570 passed**, 0 failed (29 targets) | `01-…log` |
| 3 | `cargo test -p acos-services -p acos-webview` | **739 passed**, 0 failed (97 targets) | `02-…log` |
| 4 | `cargo test -p acos-cli -p acos-mcp` | **480 passed**, 0 failed (46 targets) | `03-…log` |
| 5 | `ACOS_WEBVIEW_TEST=1` `engine_session_end` + `broker_scope` + `broker_build` | 4/4 passed (the WVKILL fix and the scope/build guards) | `04-…log` |
| 6 | `tools/check-tree-clean.sh` | clean (0 tracked changes) | run at `45e88c92e` |
| 7 | gles-software ×3 (default provider) | 3/3 pass, false_pass 0 | `05-matrix-gles-webview.json` |
| 8 | webview-probe ×3, webview-fair ×3 | 6/6 pass, false_pass 0, **0 engine restarts** in `broker.log` | `06-matrix-webview.json` |
| 9 | counter, form-validation, a11y-probe, a11y-screenshot-wrap, back-layers, gaten-backstack, mproc3-two-process, sysui1-shade | 8/8 pass | `07-matrix-named.json` |
| 10 | `ACOS_GUEST_GLES=off` counter + a11y-screenshot-wrap | 2/2 pass (escape hatch works, nothing regresses) | `08-matrix-gles-off.json` |
| 11 | `run-matrix.sh --no-build` (full: 32 negatives + counter) | **false_pass 0, infra_timeout 0, unclassified 0, negative_correct 32/32**, gates all true | `09-matrix-full.json` |
| 12 | `emu_gate.py` on the merged tree's numbers | **12 metrics compared, 0 worse** (warm launch −94.2 %, install −99.3 %) | `gate/gate.md`, `gate/gate.json`, `gate/gate.txt` |
| 13 | `cargo tree -p acos-mcp` | 0 `acos-art` | `10-cargo-tree-mcp.txt` |

ART environment: private `ACOS_RUNTIME_DIR=$WT/.cache/rt`,
`ACOS_SESSION_SOCKET=$WT/.cache/rt/session.sock`,
`ACOS_WEBVIEW_SOCKET_DIR=$WT/.cache/wvrun`, `ACOS_FONT_DIR` exported,
`ACOS_ART_BIN`/`ACOS_WEBVIEW_BIN` = this worktree's release binaries, every run
through `run-exclusive.sh` + `guarded.sh`. The matrix rows are
`verdict_eligible=false` where the host gate refused on loadavg (other agents'
builds); they are correctness rows and no timing is claimed from them.

Two real defects the runs surfaced (both were *correct* refusals, not silent
passes):

1. webview-probe/fair failed `GUEST_EXITED` until `acos-webview` was built in the
   same tree — the stale-binary arm was actually the WVSCOPE build-id guard
   saying `BUILD_MISMATCH: client build …7c5022e… != broker build …45e88c9…`.
   Rebuilt all four binaries at HEAD; 6/6 pass afterwards. (Good sign for
   WVSCOPE, and no engine restart in `broker.log`.)
2. `.cache/webview` (the pinned `chrome-headless-shell`) was missing from this
   worktree; symlinked to the shared cache like `.cache/art`. No download.

## 7. Cost

- Peak host RAM observed: **35.6 %** (emulator N=1 run, `host_log`);
  cargo runs stayed within `guarded.sh --mem 12–14 G`.
- Disk: 76 % → peak **85 %** during the release builds; both target dirs and the
  `fixtures/build` copy are deleted at the end (see the final report).
- No emulator left running (`stop_all()` in `finally`; the phase log ends with
  `stop emulator-5554`).

## 8. What remains / decisions for the orchestrator

1. The crate name: `acos-gles-provider` vs the brief's `acos-gles` (§3). One
   commit either way if you want the short name (rename the substrate instead).
2. The v0.3.0 release-gate JSON predates `m9_apk_ready_ms`; the gate run used
   `results/bench/merge040d/acos-gate-doc.json` (gate metrics copied verbatim +
   the one new row, provenance recorded). Future gates should emit the install
   phase in their own run.
3. `n1-warm-tree` should be part of the standard baseline (`run-emubench.sh`
   now accepts it; the committed baseline carries the phase) — otherwise the
   next re-measure would silently fall back to the frame-only row.
