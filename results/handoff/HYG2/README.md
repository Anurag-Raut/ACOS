# HYG2 — a skipped test is never a pass; a PendingIntent answers its creator

Branch `hyg2` from `modules` (`aaf518bf8`). Private `ACOS_RUNTIME_DIR`/
`ACOS_SESSION_SOCKET` (`.cache/rt-hyg2`), `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`,
target `.cache/hyg2-target`; every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`.

**Status: DONE except one host-tool gate (§4.3).** 946 crate tests pass, 2 fail
and they are the two `spike_net_boundary` tests whose prerequisite (the pinned
`pasta` binary) does not exist on this host; **skip count 0** (§3.1).

## 1. What was wrong

### 1.1 A skip looked like a pass

`cargo test` printed `test result: ok` while a test asserted nothing.
BACKSETTLE's `results/handoff/BACKSETTLE/suite3.log` is the documented instance:
`run-exclusive.sh` exports a **fresh, empty** `ACOS_ART_DATA` (correct — it is
the per-invocation scratch dir), and the ART-booting tests read that same
variable as *their warm cache*:

```
SKIP cleanup test: no cached guest dex under .cache/art-locks/data-BACKSETTLE-… —
     run `acos-art render-app --apk fixtures/build/apps/counter.apk` once
```

Six `tests/cleanup.rs` cases printed that and the suite was green. Three test
files (`cleanup.rs`, `launch_hang.rs`, `r3_crash_evidence.rs`) each carried their
own copy of the same rule and each resolved the warm data differently
(`ACOS_ART_TEST_DATA`, a "warm" `ACOS_ART_DATA`, the productised cache), and
~150 further `eprintln!("SKIP …"); return;` sites existed across the workspace.

### 1.2 The creator uid was the fixture uid

`PendingIntent.getCreatorUid()`/`getCreatorPackage()` read
`IActivityManager.getInfoForIntentSender` (`PendingIntent.java:1600`,
`getCachedInfo`). AOSP answers `PendingIntentRecord.key.packageName` + the
record's `uid` (the `Binder.getCallingUid()` of `getIntentSender`). ACOS
answered `pms::FIXTURE_UID` for every record, so in a two-app session the second
app's pending intent reported the primary package's uid (PROCUID README §7
item 2).

**A second bug fell out of the ART probe** (§2.2): `getIntentSenderWithFeature`
(code 63 — the form `PendingIntent.getBroadcast`/`getActivity` actually call)
inserts the `featureId` *after* the package name, and the host read argument
`1 + base` as the package. It therefore stored the feature id — `null` for every
`PendingIntent` — so `mCreatorPackage` was `""` and two apps' senders could
collide on the same key (the key contains the package).

## 2. The fix

### 2.1 One rule for a missing test prerequisite

`crates/acos-test-support/src/prereq.rs`:

* `require_prereq(present, test, reason)` — a missing prerequisite **fails** the
  test, naming the test, the reason and the command that provides it. A counted
  skip exists only behind `ACOS_ALLOW_ART_SKIP=1` (or
  `ACOS_ALLOW_TEST_SKIP=1`); it prints the reason with a running count and a
  per-process `SKIP SUMMARY: N` line on exit, so a log with skips cannot be
  mistaken for a log without them.
* `art_inputs`/`require_art` — one resolution rule for the ART prerequisites
  (`$ACOS_ART_SYSROOT`, and the warm `/data` from `$ACOS_ART_TEST_DATA` →
  `$ACOS_ART_WARM_DATA` → a warm `$ACOS_ART_DATA` → `<checkout>/.cache/art/data`,
  each only when it holds `app/guest/classes.dex`), replacing the three copies.
  **`ACOS_ART_WARM_DATA` is the warm cache; `ACOS_ART_DATA` is scratch** — that
  conflation was the root cause.
* `tools/art-runtime/run-exclusive.sh` exports `ACOS_ART_WARM_DATA` (the caller's
  checkout cache) inside the lock, so the default path runs the ART cases.
* `tools/art-runtime/test-crates.sh` is the documented command: warm data inside
  the lock, `cargo test`, then a `SKIP`-line count that **fails** the run when it
  is not zero and prints `SKIP COUNT: 0` when it is.

Converted sites: `acos-art` (`tests/cleanup.rs` 7, `tests/launch_hang.rs`,
`tests/r3_crash_evidence.rs`, `tests/sandbox_s5.rs` 2, `tests/sandbox_s6.rs` 2,
`tests/spike_net_boundary.rs` 5, `src/landlock.rs`) and `acos-services`
(`tests/host.rs`, `tests/manifest_launch_timing.rs`, `src/pms.rs`,
`src/manifest_resolve.rs`). Remaining sites in other crates are listed in §5.

### 2.2 The creator identity

* `PendingIntentRecord.uid`, set from `acos_parcel::calling_uid()` in
  `SenderTable::get_intent_sender`, reported in `to_json`.
* `ActivityManagerService.getInfoForIntentSender` answers `r.uid` (not
  `FIXTURE_UID`).
* `getIntentSender`/`getIntentSenderWithFeature` read the package at argument
  **1** in both forms (`base` still shifts `resultWho` and everything after).
* The ART probe is a new step in `scenarios/mproc3-two-process.yaml`: `proc-b`
  (the session's *second* app) creates a real `PendingIntent.getBroadcast` and
  renders `getCreatorUid()`/`getCreatorPackage()` next to `Process.myUid()` /
  `getPackageName()` with a verdict, so the assertion cannot pass on a constant.

## 3. Results

### 3.1 Crate tests — `suite.log` / `suite.stdout`

```
tools/art-runtime/test-crates.sh HYG2 -p acos-art -p acos-services --no-fail-fast
```

```
passed 946  failed 2  ignored 3
error: 1 target failed:
    `-p acos-art --test spike_net_boundary`      (both tests: no pasta binary)
SKIP COUNT: 0 (SKIP SUMMARY lines: 0; log: …/HYG2/suite.log)
```

The 3 ignored are pre-existing: two ```ignore doc-tests in `acos-services`
(`src/module.rs`, `src/refs.rs`) and the `shotz_png_tests` measurement harness
(`#[ignore = "…needs $SHOTZ_FRAMES"]`).

`tests/cleanup.rs` ran **9/9 in 20.1 s** — the six cases that used to skip.

### 3.2 A missing prerequisite fails loudly — `prereq-fails-loudly.log`

```
## 1. default path, no ART sysroot: the test FAILS and names the artefact
test guest_cannot_overwrite_boot_art_or_the_classpath ... FAILED
FAIL cleanup test: missing prerequisite: no ART sysroot at /nonexistent/sysroot — run tools/art-sysroot.sh
test result: FAILED. 0 passed; 1 failed; …

## 2. explicit opt-out: a counted skip, with the summary count
test result: ok. 9 passed; … (7 returned early)
SKIP SUMMARY: 7 test(s) skipped in this process because ACOS_ALLOW_ART_SKIP is set; the default path must show 0.
```

The unit test `prereq::tests::a_missing_prerequisite_fails_loudly_unless_the_opt_out_is_set`
pins the same behaviour inside the crate (it fails on the old helper).

### 3.3 PendingIntent creator — `procuid_pending_intent` (services) + ART probe

`crates/acos-services/tests/procuid_pending_intent.rs` (3 tests, wire form 62/65
under two calling identities) — proc-a creates, proc-b reads:

```
test a_pending_intent_answers_the_uid_of_the_app_that_created_it ... ok
test the_with_feature_form_keeps_the_package_name_out_of_the_feature_id_slot ... ok
test a_binder_that_is_not_a_sender_has_no_creator ... ok
```

Both bug tests were shown red first: with `getInfoForIntentSender` answering
`FIXTURE_UID` the first failed with `left: ("dev.acos.fixtures.procb", 10192)`,
`right: ("…procb", 10193)`; with the old `1 + base` package read the second
failed with `left: ("", 10193)`.

ART probe (the new step in `mproc3-two-process`, `runs/mproc3-two-process`):

```
ACOSPROBE b pi creator=10193/dev.acos.fixtures.procb mine=10193/dev.acos.fixtures.procb verdict=ok
```

Before the fix the same step printed `creator=10193/ … verdict=BAD` (uid was
already right, the package was empty) — that run is quoted in §4.3's note.

### 3.4 Scenarios — `runs/runs.tsv`

| scenario | verdict |
|---|---|
| notif-perm-granted | pass |
| notif-perm-denied | pass |
| location-pending-intent | pass (18 steps) |
| mproc3-two-process | pass (17 steps, incl. the creator probe) |
| binderpid | pass |

### 3.5 Fixtures

`fixtures/build` was rebuilt in this worktree (`build-apps.log` 37 APKs,
`build-dogfood.log` 12, `build-fixtures.log` 28 dex programs) — the hardlink copy
from the main checkout lacked `proc-a.apk`, `service-provider.apk` and the dex
corpus, which the new loud failures correctly refused to skip over.

## 4. What remains

### 4.1 The two `spike_net_boundary` tests (host tool, not ART)

`guest_cannot_change_the_lan_drop_rules` and
`boundary_failure_never_starts_the_guest` need the pinned `pasta` binary
(`ACOS_SPIKE_NET_PASTA`, NET6 §6). It does not exist on this host — `find / -xdev
-name pasta -type f` returns nothing, matching MERGEMPROC §6 ("not present on
this host"). They now **fail** instead of skipping, which is the point of HYG2;
the acceptance run above is therefore 946/948. Smallest next action: install the
pinned `pasta` (1.5 MB, `research/spike-net/artifacts/passt-build.txt` records the
commit and sha256), or convert those two tests into an assertion of the
documented fail-closed behaviour when no pasta is present. **Orchestrator
decision.**

### 4.2 Remaining skip sites (not converted, same class)

`grep -rn '"SKIP' crates/*/tests crates/*/src | grep -v acos-test-support`
still lists sites in `acos-vm` (dexprogs/corpus), `acos-runtime`, `acos-render`,
`acos-framework-natives`, `acos-cli` (ART-booting: `session.rs`,
`gl1_kept_session.rs`, `native_launch.rs`, `serve_daemon.rs`, `w50_serve_art.rs`,
`rel3*`), `acos-dex`, `acos-apk`, `acos-arm64`, `acos-jni`, `acos-webview`,
`acos-mcp`. Each is the same mechanical change
(`require_prereq(cond, "<test>", "<reason>")`) but each needs its prerequisite
present in this environment to be verified, and converting an unverified one
would ship a suite that fails for a reason nobody checked. They are one
`sed`-shaped pass per crate.

### 4.3 Cross-process handoff of a `PendingIntent` (why the probe is in proc-b)

`proc-a` cannot hand its `PendingIntent` to `proc-b` through an `Intent` extra:
`acos-runtime/src/ams_java.rs` refuses to re-materialise a `Parcelable` extra
into the guest ("no class-loader construction for it"), the same gap
`scenarios/launch-extras-parcelable.yaml` documents. So the ART probe reads the
creator identity of a pending intent **created in proc-b** (the non-primary app,
which is exactly the case the bug broke), while the services test proves the
"a creates, b reads" direction over the wire. Materialising parcelable extras in
the VM would let the ART probe do both — a separate item.

## 5. Files

* `crates/acos-test-support/src/prereq.rs` (+ `lib.rs`, `Cargo.toml`: `libc` for
  the `atexit` summary), `crates/acos-art/tests/*`, `crates/acos-art/src/landlock.rs`,
  `crates/acos-services/{src,tests}/*`, `crates/acos-services/Cargo.toml`
  (dev-dependency).
* `crates/acos-services/tests/procuid_pending_intent.rs` (new).
* `tools/art-runtime/run-exclusive.sh` (`ACOS_ART_WARM_DATA`),
  `tools/art-runtime/test-crates.sh` (new).
* `scenarios/mproc3-two-process.yaml` (+ the `creator` step),
  `fixtures/apps/proc-b` (the probe view and `showPendingIntentCreator`).
* Evidence: `suite.log`, `suite.stdout`, `prereq-fails-loudly.log`,
  `build-*.log`, `runs/` (verdicts, logs, `runs.tsv`), `run-scenarios.sh`.
