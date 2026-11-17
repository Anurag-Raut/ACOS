# RELGATE1 — v0.3.0 release gate, CORRECTNESS half

Worktree `/home/Qui/.t3/worktrees/acos/t3-9fde79f7-5b90a224f291fea1`, detached at
**f162b2d9** ("Merge branch 'testflake' into rel-030", version 0.3.0). Run
2026-11-16 09:07–10:0xZ. No code was changed; this directory is evidence only
(committed on branch `relgate1`).

Environment (per BRIEF_COMMON_0316):
`ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts`,
worktree `.cache/{art,gaten,boot/android-34}` symlinks, `fixtures/build` a
`cp -al` copy of the main checkout, private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`
under `$WT/.cache/rg1/rt`, `ACOS_BOOT_DIR`/`XDG_RUNTIME_DIR`/`ACOS_HOME` unset,
every ART run through `/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh`
(shared slots) wrapping `tools/guarded.sh`. Target dir `.cache/rg1-target`
(deleted at the end), 3 missing fixture APKs plus `scroll-list` built with
`tools/build-apps.sh` (`00-build-fixtures.log`).

## Summary table

| # | item | result | evidence |
|---|------|--------|----------|
| 1a | `cargo build --release --workspace` | **PASS** | `01-build-release.log` (`BUILD_EXIT=0`) |
| 1b | `cargo test --workspace --release --no-fail-fast` | **PASS with 2 known-env failures** (2283 passed / 2 failed / 27 ignored) | `02-test-*.log`, tally below |
| 2 | `#[ignore]`d ART batch (acos-cli, acos-art; acos-mcp has none) | **PASS after 2 environment fixes; 2 pre-existing gl1 failures** | `03-ignored-*.log` |
| 3 | every scenario in `scenarios/` (163 files) via `acos verify --engine art` | **146 as declared / 17 not**; 16 of the 17 fail identically on v0.2.2 (pre-existing), 1 explained (gaten-lesson, see finding F1) | `04-scenarios.log`, `04-scenarios-table.md` |
| 4 | 20 Gaten gate rows (10 positive + 10 negative) in fresh artifact dirs | **19 as declared / 1 not** (gaten-lesson, F1); fresh-dir integrity **20/20 clean** | `05-gaten.log`, `05-gaten-table.md`, `05-gaten-fresh-check.log` |
| 5 | Installed-prefix smoke (`tools/release/install.sh --prefix $WT/.cache/rg1-prefix`) | **PASS** | `06-install.log`, `06-version-doctor.log`, `06-prefix-verify-counter.json`, `06-mcp-roundtrip.jsonl` |

**No real code regression of v0.3.0 was found.** Every failure below is either a
documented environment gap or an identity-checked pre-existing failure.

## 1. Build and the workspace test suite

`cargo build --release --workspace --offline` → `BUILD_EXIT=0` (1m42s).

The suite was run per crate set so no ART-lock acquisition ran long (each ART-linked
crate under `run-exclusive.sh` + `guarded.sh`; pure-Rust crates guarded only):

| log | crates | passed | failed | ignored |
|-----|--------|--------|--------|---------|
| `02-test-groupA.log` | 17 non-ART crates | 678 | 0 | 1 |
| `02-test-groupB.log` | acos-vm, acos-runtime, acos-sandbox | 345 | 0 | 10 |
| `02-test-art.log` | acos-art | 220 | 0 | 1 |
| `02-test-acos-cli.log` | acos-cli | 294 | 0 | 15 |
| `02-test-acos-services.log` | acos-services | 365 | 0 | 0 |
| `02-test-acos-verify.log` | acos-verify | 151 | 0 | 0 |
| `02-test-acos-mcp-nortenv.log` | acos-mcp (final, clean env) | 160 | **1** | 0 |
| `02-test-acos-jni.log` | acos-jni | 70 | **1** | 0 |
| **total** | 23 crates | **2283** | **2** | **27** |

Every failure, re-run alone, classified:

| test | first failure | alone | classification |
|------|---------------|-------|----------------|
| `acos-mcp/tests/flutter_session.rs::flutter_launch_observe_tap_round_trip` | `02-test-acos-mcp.log` | still fails (`02-test-acos-mcp-shortrt.log`, `…-nortenv.log`) | **known env**: the shared fixture APK has `lib/x86_64/libapp.so` but no `libapp_probe.so`, so the Flutter semantics tree is empty (`unzip -l fixtures/build/apps/flutter-hello.apk`) — documented in `v0.3.0-NOTES.md` |
| `acos-jni/tests/flutter_hello.rs::flutter_hello_under_confinement` | `02-test-acos-jni.log` (`stage-app source=fixture`, empty tree) | same | **known env**, same fixture gap |
| acos-mcp `private_daemon::tests::private_dir_is_per_process_and_under_the_runtime_dir` (106-byte socket assertion), `private_daemon_is_reaped_when_the_server_exits`, `default_session_tools_use_the_private_art_daemon`, `acos_key_back_goes_to_the_art_daemon_and_reports_handled` | `02-test-acos-mcp.log` | all pass with a short runtime dir (`02-test-acos-mcp-nortenv.log`, `…-shortrt.log`); the message is literally `private session socket path …/.cache/rg1/rt/mcp-…/session.sock is too long for a unix socket` / `must be under the private runtime dir` | **known env**: my worktree's 71-character `ACOS_RUNTIME_DIR`; `acos-mcp` expects ≤100-byte `sun_path` and its `art_default` test also assumes `ACOS_RUNTIME_DIR` is unset. Not a code defect. |

No timing flake was observed: nothing failed in the suite that was not a fixture or
path-length gap.

## 2. The `#[ignore]`d ART batch

List (`03-ignored-list.log`, `cargo test --release -p <crate> -- --list --ignored`):
acos-cli **15** ignored tests, acos-mcp **0**, acos-art **1**
(`launch::shotz_png_tests::png_encoder_candidates_on_real_frames`).

| run | log | result |
|-----|-----|--------|
| `cargo test --release -p acos-cli -- --ignored` (the brief's exact command) | `03-ignored-batch.log` | `rel3c_capped_verify` 2 pass (12.08 s), then `gl1_kept_session` 2 fail → cargo stops at the first failing target, so the remaining binaries never ran |
| `… --no-fail-fast -- --ignored` | `03-ignored-cli-nofailfast.log` | `rel3c_capped_verify` 2 pass, `rel3e_handled` 1 pass, `rel3s_swipe` 1 pass, `w50_serve_art` 1 pass, `rel3s_scenario` **3 fail** (fixture + `ACOS_ART_BIN`), `gl1_kept_session` **2 fail** |
| same, `ACOS_ART_BIN` set + `scroll-list.apk` built | `03-ignored-envfixed.log` | `rel3s_scenario` **3 pass** (48.24 s), `touchdisp_dispatch` **3 pass** (13.36 s) |
| acos-art ignored, `SHOTZ_FRAMES=…/results/parity/w198a` | `03-ignored-art-shotz.log` | 1 pass |
| acos-art ignored, no `SHOTZ_FRAMES` | `03-ignored-batch.log` | 1 fail: `results/parity/w198a: No such file or directory` (worktree has no `results/parity/w198a`; the env var is documented in the test's own doc comment) — **known env** |

Failures, re-run alone and classified:

| test | first | alone | classification |
|------|-------|-------|----------------|
| `rel3s_scenario::scroll_until_stops_at_the_end_of_the_list`, `swipe_reveals_an_offscreen_row_and_scroll_until_taps_a_deep_row`, `scroll_until_whose_swipe_was_not_consumed_is_a_miss` | missing `scroll-list.apk`; `ACOS_ART_BIN=…/.cache/rel3s2-target/release/acos-art is not a file` | `03-ignored-envfixed.log`: all 3 pass | **known env**: the fixture was absent from the `cp -al` copy (built with `ACOS_APP_ONLY=scroll-list tools/build-apps.sh`), and `art_bin()` only looks in `.cache/{rel3h,rel3s,rel3s2}-target` unless `ACOS_ART_BIN` is exported — test brittleness w.r.t. `CARGO_TARGET_DIR`, not a product path |
| `gl1_kept_session::a_kept_session_survives_the_launching_client` | `03-ignored-batch.log` (`rel3s`…) → `03-failures-gl1-alone.log` | fails alone, deterministically, at line 168 | **pre-existing**: it writes `session observe --screenshot <test root>/kept.png`, and FB2c (e82bf0be, 2026-11-15) confined `screenshot` to the session's artifacts dir → `INVALID_ARGUMENT`. The test (df97fdfb, 2026-11-12) predates that commit and was never updated. Reproduced on the **v0.2.2 binary** (`03-failures-gl1-baseline022.log`) |
| `gl1_kept_session::a_sigkilled_daemon_takes_its_guests_with_it` | same | fails alone at line 392 (`guest … is still running after the daemon died`; the guests *are* gone in 0–3 ms, the extra assertion re-checks pids captured before the kill) | **pre-existing**: identical failure on the v0.2.2 binary (`03-failures-gl1-baseline022.log`, same 3 guests, `guests gone 0 ms`) |

## 3. Every scenario in `scenarios/` on ART

163 scenario documents (`scenarios/suite/INDEX.yaml` excluded: it is the suite
index, not a scenario). Command per file:

```
run-exclusive.sh --task RELGATE1 -- guarded.sh --mem 6G --time 600 -- \
  .cache/rg1-target/release/acos verify <file> --engine art \
    --artifacts results/release/v0.3.0-gate/correctness/04-scenarios/<name>/artifacts --json
```

**146 of 163 declared their expected outcome** (including every declared
`expect_error` code/reason, which the executor itself pins).
`04-scenarios-table.md` has one row per file (file, expected, got, error code,
steps, verdict path).

The 17 that did not, each re-run alone (`04-scenarios-rerun.log`) and then compared
against the **v0.2.2 release binary** with the same scenarios and APKs
(`04-scenarios-baseline022.log`, `04-scenarios-baseline022-table.md`):

| scenario | declared | RC f162b2d9 | v0.2.2 baseline | classification |
|---|---|---|---|---|
| `gaten-lesson` | pass | timeout at the final `wait_for` (43 s) | **pass** (14 s) | **F1** (only row that differs from the baseline) |
| `hello-views`, `multi-activity` | pass | timeout `no-draw-after-action` at `back` | same timeout | pre-existing |
| `negative/multi-wrong-after-back`, `negative/offscreen-pager-wait` | fail | timeout instead of the declared failure | same | pre-existing |
| `svg-viewbox` | pass | `ASSERTION_FAILED` (`canvasIsViewPx=false`) | timeout (`traversal`) | pre-existing (non-pass in both) |
| `suite/T07-form-good` | pass | `ELEMENT_OFFSCREEN` (`id=="error"` node not visible) | same | pre-existing |
| `s8-hostile-untrusted`, `w44-hostile-forest-untrusted`, `w44-hostile-probe-spam` | pass | `INSUFFICIENT_HOST_EVIDENCE` (untrusted run may not pass on a guest-asserted tree) | same | pre-existing (v0.2.2 trust rules) |
| `s8-hostile-region`, `w44-hostile-parcel` | pass | timeout `probe-floor` | same | pre-existing |
| `w15-broken-crash`, `w15-broken-missing-symbol`, `w25-gaten-launch`, `w26-gaten-debug-launch`, `w28-gaten-debug-launch` | pass | `GUEST_EXITED` | same | pre-existing; the w15-broken comment itself says "Expected to fail at launch, never pass", so `expectation: pass` is stale metadata |

## 4. The 20 Gaten gate rows

10 positive (`scenarios/gaten-{home,tabs,settings,search,lesson,kanji,recognition,support,backstack,content-welcome}.yaml`)
and their 10 `scenarios/negative/gaten-*-wrong.yaml` twins, each into its own fresh
artifact directory (`05-gaten.log`, `05-gaten-table.md`).

19 of 20 as declared. The negative twins all fail exactly as declared
(`ASSERTION_FAILED`, `failed_step` naming the wrong text). Gaten positives:
`gaten-home` 14/14 steps, `gaten-tabs`, `gaten-settings`, `gaten-search`,
`gaten-kanji`, `gaten-recognition`, `gaten-support`, `gaten-backstack`,
`gaten-content-welcome` all pass on `art-reference`.

`gaten-lesson` is the one gate row that does not pass — see F1.

Fresh-artifact check (`05-gaten-fresh-check.log`, `fresh-check.py`): for all 20 rows
one `run.json`/`verdict.json`/`repro.json`, one `request_id` and one `session_id`
across every document, every file mtime inside `[started_at-120 s, finished_at+120 s]`,
every evidence path present, and no document mentioning another run's `request_id`.
**20/20 clean, 0 findings.** `acos` itself refuses a reused artifacts dir with
`INVALID_ARGUMENT … already holds run req_…` (observed while preparing F1's control run,
`05-gaten-lesson-ime/two-back.err`).

## 5. Installed-prefix smoke

`tools/release/install.sh --prefix $WT/.cache/rg1-prefix --from-target .cache/rg1-target`
(never `~/.local`; `env -u ACOS_HOME`) → `INSTALL_EXIT=0` (`06-install.log`).

| check | result |
|---|---|
| `…/rg1-prefix/bin/acos --version` | `acos 0.3.0` |
| `…/bin/acos doctor --json` | `"ok": true`, 28/28 checks `ok` (install_*, android_sdk, build_tools, aapt2, d8, zipalign, apksigner, android_jar, java, javac, keytool, ndk_clang, …) |
| `…/bin/acos verify scenarios/counter.yaml --engine art` | `status pass`, `ok true`, `fidelity art-reference` |
| MCP `initialize` → `notifications/initialized` → `tools/list` → `acos_help{workflow}` from the prefix | serverInfo `{name: acos-mcp, version: 0.3.0}`, protocolVersion `2025-06-18`, 19 tools, `acos_help` 26 189 bytes, `SMOKE ok` (`06-mcp-roundtrip.jsonl`) |

Install note (environment, not product): the first attempt failed **closed** —
`install.sh` refuses a copied tree with a symlink that escapes the install root, and
the *owner's* `/home/Qui/Projects/acos/.cache/boot/android-34` currently contains a
self-referencing `android-34 -> …/boot/android-34` (created 2026-11-16 11:35, also
`boot -> …/boot`) that is copied into the prefix. I pointed the worktree's boot
symlink at a hard-linked, cleaned copy (`.cache/rg1/boot-clean`, stray link removed)
and installed again. The owner's cache was not modified. Worth fixing before the
real release install from any worktree.

## Findings for the orchestrator

**F1 — `scenarios/gaten-lesson.yaml` no longer passes on v0.3.0 (the only Gaten
gate row, and the only scenario that differs from the v0.2.2 baseline).**

Evidence: `05-gaten/gaten-lesson/artifacts/verdict.json` — steps 0–24 pass, step 25
(`wait_for text*="Recently looked up"`) times out after 30 085 ms; the tree at frame
24 (the `back` step) and at frame 54 (deadline) are **the same WaniKani form**, with
`input.handled: true`, `handled_evidence {source: guest, published 2, acked 2}` and
`pixels_changed: true` for the `back`.

Cause: that `back` follows `type:` into the password field, so it lands on the **real
bound IME** introduced in v0.3.0 (REL3I/REL3K: "Back hides the soft input
(`FINISHED(handled=true)`) and the app sees the hide through the framework's own
insets path"). The first Back hides the keyboard — exactly what AOSP does — and the
scenario, written for v0.2.2's absent IME, expects Home after one Back.

Proof, not inference: a control copy of the scenario with a second `back:` before the
same `wait_for` (`05-gaten-lesson-ime/two-back-2.json`, `…/two-back-2/`) **passes**
(`status pass`, 29 steps; step 24 back 147 ms = IME hide, step 25 back 1354 ms = pop
to Home, step 26 `wait_for` 85 ms). `scenarios/back-layers.yaml` and
`scenarios/back-layers-ime.yaml` (the REL3K2 Back-ordering fixtures) pass on this
build in the same sweep, i.e. the new semantics are the intended ones and are tested.

So this is **not a code regression**; it is one stale release-gate scenario. I did
not edit it (no code changes in this pass). Smallest next action: add one `back: {}`
to `scenarios/gaten-lesson.yaml` (or assert the IME-hide with `back-layers`-style
expectations) and re-run the row; alternatively decide that a gate row must not
depend on IME state and type into the token field last.

**F2 — two ignored ART tests (`gl1_kept_session`) fail on the RC and on v0.2.2.**
`a_kept_session_survives_the_launching_client` is a stale test versus FB2c's
screenshot confinement; `a_sigkilled_daemon_takes_its_guests_with_it` re-checks pids
captured before the SIGKILL and sees a process that `guest_processes` no longer
matches (the guests are gone within 0–3 ms — the guarantee itself holds). Both were
already failing before rel-030.

**F3 — test environment sensitivities worth knowing before the timing half**
(not release blockers): `acos-mcp`'s `art_default`/`private_daemon` tests need an
`ACOS_RUNTIME_DIR` short enough for `sun_path` (≤100 bytes) and *unset* when the test
supplies its own; `rel3s_scenario`/`touchdisp_dispatch` need `ACOS_ART_BIN` when
`CARGO_TARGET_DIR` is not one of their hardcoded `.cache/{rel3h,rel3s,rel3s2,touchdisp}-target`
paths; `acos-art`'s `png_encoder_candidates…` needs `SHOTZ_FRAMES`.

## Housekeeping

* Full per-run artifacts (frames, art-frames, tree, logs, steps) were written under
  this directory during the runs and **pruned to the JSON evidence** for the commit
  (they were ~800 MB, mostly `tree/` at 488 MB). Every table's `verdict path` column
  still resolves. Regenerate a run with the exact command recorded in the log files.
* Logs: `00-…` fixtures, `01-…` build, `02-…` tests, `03-…` ignored batch, `04-…`
  scenarios + baseline, `05-…` Gaten + F1 controls, `06-…` install smoke,
  `07-baseline-build.log` (v0.2.2 binary used only for the pre-existing/regression
  comparison).
* Deleted at the end: `.cache/rg1-target`, `.cache/rg1-base-target`,
  `.cache/rg1-base-src`, `.cache/rg1/boot-clean`, `.cache/rg1-prefix`,
  `.cache/rg1/smoke`, `.cache/rg1/rt*`. Nothing outside this worktree was modified;
  the owner's `.cache` and `~/.local` were read only.
* The 60-minute rule: the gate scope (build + 23-crate suite + ignored batch + 163
  scenarios + 20 gate rows + install smoke + a v0.2.2 control build) is longer than
  one hour; I extended once after recording the reason (only the control runs
  remained) and did not start any new investigation.
