# MPROC1 status (branch `mproc1`, base `mproc0` @ 4d793e73)

Steps **1 and 2 are done, tested and committed**. Steps 3 (second process) and
4 (idle kill) are **not started**: they need the whole peer/routing seam and do
not fit in the time this run had left, and starting them without finishing would
leave a half-written seam. The orchestrator's step-3 design confirmation is
recorded below so the next run starts from it.

Commits:

| sha | what |
|---|---|
| `f11c281e` | step 1: `LaunchOpts::extra_apks`, `acos-art launch --extra-apk`, CLI/MCP/scenario plumbing, per-package PMS+ATMS install, per-package `getApplicationInfo` (the guest template that ignored the requested package is gone), fixture `multi-package-probe`, scenario `mproc1-multi-package` |
| `ba34685d` | step 2: intent capture target (`acos-services/src/intents.rs`, ATMS hook), session `ctl` op + `acos_service_ctl` / `acos session ctl`, scripted `ActivityResultItem` delivery in the guest, the `android.content.ClipData` wire codec, fixture `intent-probe`, scenario `mproc1-intents`, A/B driver |

Evidence: `results/handoff/MPROC1/evidence-step1.md`,
`results/handoff/MPROC1/evidence-step2.md`.

## Step 1 — extra APKs in a session (done)

- `LaunchOpts::extra_apks` + repeatable `acos-art launch --extra-apk`; each APK
  is staged into the run's private tree (H17), bound at its guest path, its
  native libs installed into a **per-package** tree
  (`/data/app-lib-extra/<pkg>`), the ABI gate applied, installed into the PMS at
  its own uid and seeded into ATMS with that uid. The launch reply carries
  `extra_packages`.
- Surfaces: `acos launch --extra-apk` (ART only), MCP
  `acos_launch {extra_apks:[...]}`, scenario `extra_apks:`. Launch-time only
  (a live session refuses a different set), and part of the zygote key.
- **Root cause fixed**: `IPackageManager.getApplicationInfo` (code 9) was
  answered from a guest-built template that ignored the requested package, so
  with a second package installed `getApplicationInfo(other)` and
  `createPackageContext(other)` returned the launched app. The template is gone;
  the host PMS answers the package that was asked for.
- Acceptance run: `acos verify scenarios/mproc1-multi-package.yaml --engine art`
  → pass, 6 steps; guest prints `ACOSPROBE ok pkg=dev.acos.fixtures.counter …`,
  `ACOSPROBE ai pkg=dev.acos.fixtures.counter …`, `ACOSPROBE ctx appInfo=
  dev.acos.fixtures.counter … app_name=Counter` (the **other** APK's resource).
  Without `--extra-apk` the same fixture prints `ACOSPROBE missing …` (honest).
- Also had to fix `acos-art` itself: `mproc0` did not compile (nine `state.app`
  field accesses after `PmsState` became `packages`); those are the accessor
  calls now.

## Step 2 — capture target + scripted result (done)

- Unresolved implicit `SEND`/`VIEW http(s)`/`PICK`/`GET_CONTENT` → recorded
  (`action`, `data`, `mime_type`, `categories`, `extras`, `flags`,
  `request_code`, `for_result`), component `com.acos.capture/.Capture`, no
  process, `START_SUCCESS`, `handled_by: "captured"`. Everything else stays
  `START_NOT_FOUND`.
- `ctl` op: session protocol + `acos_service_ctl` (MCP) + `acos session ctl`
  (CLI) with `op: last|list|clear|result`. `result` scripts
  `{result_code, data, mime_type, action, extras}` and the guest delivers it as
  the framework's own `ActivityResultItem` `ClientTransaction` (so
  `onActivityResult` runs for real), acking before the reply says
  `delivered:true`.
- **Root cause fixed (whole class)**: every `ACTION_SEND` intent carries a
  `ClipData` (`Instrumentation.execStartActivity` →
  `migrateExtraStreamToClipData`), and the Intent codec refused it — every share
  died with `IllegalArgumentException` before the host saw it. The body now
  decodes field by field, pinned by `dexdump` of the image's
  `ClipData`/`ClipDescription.writeToParcel` and by a guest's own
  `ClipData.writeToParcel` bytes replayed in a test.
- Tests: `cargo test -p acos-services --test mproc_intents` → 5 passed (capture,
  non-capture of an explicit typo, pick + scripted result round trip, the real
  120-byte ClipData consumed exactly, structured refusal for a stream item).
  `cargo test -p acos-services -p acos-protocol` all green.
  `acos verify scenarios/mproc1-intents.yaml --engine art` → pass, 6 steps.
- Evidence of the round trip: `tools/mproc1-intent-drive.py` (one live session:
  `ctl last`, `ctl result`, logs) shows `delivered: true` and the app's own
  `ACOSPROBE result request=42 result=-1 data=content://… type=image/png`.

## What remains (in order)

1. **Step 3 — the second process.** Orchestrator confirmed the seam (2026-11-16
   run): the binder peer listener accepts N tagged guest connections (one per
   app process) and `ServiceHost` binder records carry the owning process id; no
   new broker. Concretely:
   - `acos-binder-peer`: N connections, each prefixed with its process tag
     (`Peer::serve` currently accepts one); `tag -> BinderId` tables so
     deliveries and reverse transactions are addressed to the owning process;
     `ServiceHost` binder records gain `process_id`.
   - `launch_session`: keep the one-guest fast path; on the first
     intent/bind/provider call that resolves to an installed extra APK (or a
     declared `android:process`), fork a tagged child through the existing
     zygote contract (`zygote_prod` keeper `spawn <tag>`), with the second APK
     bound at `/data/app/<name>` (already staged by step 1).
   - ATMS: one `ProcessRecord` per tag (activities per process; foreground = the
     process of the resumed activity) so `IActivityManager` process state agrees.
   - Fixtures: A starts B's activity and gets a result; A binds a service in B
     (AIDL round trip).
   - Nothing in step 2 is thrown away: the capture target is the "no installed
     target" arm of the same resolution decision, and `extra_apks` is what makes
     a target installed.
2. **Step 4 — idle kill** (30 s default, `--idle-kill-seconds`):
   `onSaveInstanceState` → `kill <tag>` through the zygote FIFO → drop the tag's
   binder tables → `killed: {tag, reason:"idle", saved:true}` in the reply and
   `verdict.json`; re-spawn on the next call.
3. **A/B**: prepared, **not run** (`tools/mproc1-ab.sh`, arms `mproc0`/`mproc1`,
   phases `memory,cold,latency`, no extra APKs). Expectation: no regression
   beyond the bench's own noise rule. The bench owner runs it.

## Notes for the orchestrator

- `mproc0` did not compile (`acos-art` had nine stale `PmsState` field
  accesses). Fixed here in `f11c281e`; anyone branching from `mproc0` needs that
  commit.
- The ClipData codec covers the share/pick shapes ACOS produces. A stream share
  (`ACTION_SEND` + `EXTRA_STREAM`, i.e. an item carrying a Uri) is a structured
  `UNSUPPORTED_PARCELABLE` — that needs the ContentProvider module, not this
  track. Documented in `read_clip_data` and pinned by a test.
- The scripted result's `content://` URI is metadata: no provider serves its
  bytes yet (same ContentProvider module). The round trip that step 2 promises
  (capture record → scripted `resultCode` + data Intent → real
  `onActivityResult`) is complete.
- `acos_service_ctl` exists as the documented module-control tool for **one**
  module (`intents`); the v0.4.0 module framework will generalise it.
- Leftover risk for step 3: the peer's one-connection `serve` is also the
  zygote child's connection; step 3 must keep the "one launcher serves one guest"
  fast path free of cost when no second package is installed (the step-5 cost
  claim).
