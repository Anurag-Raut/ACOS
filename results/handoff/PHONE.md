# PHONE — handoff (PHONE-REF-1)

Branch `phone-ref-1` (base `3e11d9c`). Full report: `results/phone/PHONE-REF-1.md`.
Experiment entry: `EXPERIMENTS.md` E13.9.

## What this track delivered

1. **A reusable phone reference harness.**
   `tools/phone/replay_scenario.py` replays any scenario YAML on the phone via
   `agent-device` and emits the same per-step observation JSON as the ACOS side
   (built from `acos verify --artifacts`). `tools/phone/compare_observations.py`
   diffs the two into a markdown table. `--require-lock` holds
   `flock /home/Qui/Projects/acos/.cache/phone.lock` for the whole replay, so it
   is safe next to other agents.
2. **The measurement.** On the flows where both sides ran the *same* APK,
   ACOS and the phone agreed on every observable an agent acts on (texts, ids,
   enabled/checked, dialog contents, log lines and their order). Only pixel
   bounds differ (density 400 vs 420 dpi + API 35 insets).
   `form-validation` 28/28, `todo-list` 22/22 comparable, `hello-views` 16/16,
   `multi-activity` 14/14.
3. **No (a) ACOS bug found.** Three candidates were checked and rejected with
   evidence (dialog-in-tap-reply, inline-error sequence, dialog geometry).

## The three things that block a complete reference

1. **Gaten build mismatch.** The phone has `com.gaten` versionCode 12 installed
   2026-11-12 20:07 IST; the ACOS APK is built from gaten `f344272`
   (2026-11-12 21:12 IST). The phone's onboarding is the older coach-mark tour
   with **no `Skip`**, so all five `gaten-*` scenarios stop at step 2.
   *Smallest next action:* install an arm64 build of `f344272` on the phone,
   then run
   `python3 tools/phone/replay_scenario.py --source phone --scenario scenarios/gaten-home.yaml --out results/phone/evidence/gaten-home/phone --require-lock`
   and the same for `gaten-{tabs,settings,search,lesson}`.
2. **`input.rotate` unsupported on the ART backend** (`UNSUPPORTED_BACKEND`,
   "the ART session protocol has no rotate command yet"). Three fixture
   scenarios (`todo-list`, `hello-views`, `multi-activity`) have a `rotate` step
   and therefore end `unsupported` on ACOS while the phone completes them.
   *Owner/orchestrator decision:* is rotation in scope for ART?
3. **`verify` artifacts carry no tree/frame for `launch` and `assert_log`
   steps** (`evidence: ["step"]` only). The runtime reply does carry a tree, so
   this is an artifact gap, not a behaviour gap; it blocks artifact-level
   diffing of the first screen.

## Two phone-side facts worth remembering for later tracks

* **IME.** A real phone keeps the soft keyboard up across a rotation and it
  covers the list; the offscreen ACOS guest has no IME window. The phone harness
  dismisses it (`agent-device keyboard dismiss`) after a rotate. Any future
  reference replay of a text-entry + rotate flow needs the same.
* **Dialog window scope.** With an `AlertDialog` up, `agent-device`'s snapshot
  exposes only the dialog window; ACOS's tree keeps the app window plus the
  dialog. Both carry the dialog text, which is what an agent asserts on.

## Caveats

* ACOS numbers are from `3e11d9c`; `main` moved to `863def1` during the track
  and nothing was rebased.
* Gaten's x86_64 APK versionCode could not be read on this host (no
  `aapt`/`aapt2`; `acos inspect` does not report it). The build difference rests
  on the install timestamp vs the gaten commit timestamp plus the visible
  onboarding UI.
* `assert_region` pixel-hash steps are `not_compared` on the phone (hashes are
  calibrated to the ACOS frame geometry); re-deriving per-device hashes is a
  separate track.
* Phone evidence is ≤540 px wide; full captures stay in `.cache/phone-probe/`.

## Files

| path | role |
|---|---|
| `tools/phone/replay_scenario.py` | phone replay + ACOS artifact normalizer |
| `tools/phone/compare_observations.py` | per-step diff table |
| `results/phone/PHONE-REF-1.md` | the report |
| `results/phone/evidence/<flow>/{acos,phone}/observations.json` | per-step observations |
| `results/phone/evidence/<flow>/DIFF.md` | the per-flow table |
| `results/phone/evidence/<flow>/phone/screenshot-NN-*.png` | paired screenshots |
