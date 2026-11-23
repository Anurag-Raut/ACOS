# UI-tree parity: ACOS vs the phone, per scenario step

Status: **implemented** (PARITY1, v0.5.0 parity harness). Owner direction:
`docs/OWNER_DIRECTION_2026-11-18.md` (~100 % compatibility) and
`docs/OPERATING_DIRECTIVES.md` §1 ("a parity check for every app (ACOS tree and
frame vs phone: matches / list of differences)").

```bash
tools/parity/run-parity.sh scenarios/counter.yaml
tools/parity/run-parity.sh scenarios/gaten-backstack.yaml --out results/parity/gaten-backstack
```

One command runs **the same scenario** twice and diffs the UI tree after every
step:

1. **ACOS ART** — `acos verify <scenario> --engine art` under the shared ART
   lock (`tools/art-runtime/run-exclusive.sh` + `tools/guard/guarded.sh`). The run's
   own artifacts are the ACOS side: `verdict.json` lists each step and the frame
   it bound, `tree/<frame_seq>.json` is the tree at that frame.
2. **phone** — `tools/phone/replay_scenario.py` replays the same steps on the
   device through agent-device + `uiautomator dump`, writing
   `tree-<NN>-<step>.json` per step. If the phone is not attached, the best
   recorded physical-device capture for that scenario is used instead and the
   report says so (a missing phone is never a pass).
3. **diff** — `tools/parity/diff_trees.py` compares the two trees per step and
   writes `report.json` (machine-readable, every difference) and `report.md`
   (the human table).

Exit codes: `0` compared with only documented platform differences, `1`
compared with unexpected differences (findings), `2` usage/environment, `3`
nothing to compare on one side (skipped — never a pass).

## What is compared, per step

Nodes are paired **within the window an agent is looking at** (the topmost
window: a dialog, not the activity behind it):

| key | when |
|---|---|
| `id:<resource-id>` | the node has a resource-id |
| `text:<text>` | no id, has text |
| `desc:<content-desc>` | no id/text, has a content description |
| `class:<role>@<ancestor id>#<n>` | a container with none of the above: same class role under the same identified ancestor, same ordinal |

Containers are paired by tree position, not by proximity, so a dialog whose
frame is centred 32 dp lower still pairs its children. A rescue pass pairs a
node that one platform does not expose by id when the text or content-desc is
identical (that reports **one** field difference instead of a missing + an
extra node).

For every pair:

* **text** and **content-desc** (whitespace-collapsed; NBSP/narrow-NBSP
  normalised). A name that moved between the two attributes is its own kind,
  `text_vs_desc` — that is a real selector-affecting difference, not a rename.
* **enabled / checked / focused / selected** — only where both sides report a
  value.
* **visibility**.
* **class role** (`button`, `edit`, `text`, `image`, `list`, `check`, `seek`,
  `container`, ...). A different concrete class with the same role is
  `class_name_differs` (informational, shown only with `--strict-class`),
  because `DialogTitle` vs `TextView` is a framework subclass, not a different
  widget.
* **bounds in dp** — each side's pixels are converted with **its own** density
  (`px * 160 / dpi`), so the same dp layout compares equal at 400 and 420 dpi.
  Default tolerance `--tolerance-dp 1.5`. A difference is reported with its dp
  delta per edge.

ACOS's tree is also checked against itself: `visible_bounds` must be the node's
`bounds` clipped to the session display **of that tree** (the application
window's bounds in the same capture). That is how PARITY1 found the rotation
bug (`acos_tree_inconsistent`); PARITYFIX fixed it in the guest
(`LaunchMain.displayBounds` reads the live session display, no longer a value
cached at the first capture) and pinned it with
`tools/tests/test-tree-display-clip.py <rot1-rotate artifacts> --need-landscape`.

## Expected differences vs findings

`tools/parity/allowlist.yaml` lists every difference the two platforms are
expected to produce, each with its cause and evidence:

| id | what it covers |
|---|---|
| `display-dp-size` | the phone is 1080x2400 @400 dpi (432x960 dp), the ACOS reference session 1080x2400 @420 dpi (411x914 dp); a full-display node cannot have the same dp extent |
| `api35-edge-to-edge` | Android 15 draws the app window under the navigation bar; the API-34 image ends it above it |
| `window-frame-origin-shift` | the status-bar band is 128 px (48.8 dp) on the API-34 image and 108 px (43.2 dp) on the phone, so the whole frame is translated ~5.6 dp (measured per step from `android:id/content`'s top-left edge) |
| `display-width-follows-screen` | match_parent / right-aligned nodes follow the display's dp width (up to 20.6 dp) while keeping their dp margin to the anchored edge |
| `display-height-follows-screen` | the same on the vertical axis |
| `recorded-capture-tool-scope` | **recorded sources only**: the agent-device snapshot does not expose framework decor chrome (action bar, status/navigation bar backgrounds, in-dialog panels) that a live `uiautomator dump` and ACOS both expose |
| `acos-invisible-node` | nodes ACOS itself marks `visible: false` (zero-area overflow menu, collapsed context bar); neither phone tool reports them |

Anything not matched by an entry is a **finding**, and findings are classified:
`compatibility-bug/text|content-desc|accessibility-name|accessibility-name-attribute|state|visibility|widget`,
`layout/bounds`, `structure/missing-in-acos|extra-in-acos|target-missing-in-acos`,
`labels/only-in-acos|only-on-phone`, `acos-evidence/tree-consistency`.

Two classes are about **evidence validity**, not compatibility:

* `timing/unslept-step` — every step in the finding is a `settle: fast`
  navigation step. The scenario itself says its frame is not evidence, and the
  two captures are taken at different points of the transition.
* `phone-scenario-failed` — the phone replay already failed at an earlier step
  (e.g. the device state makes a screen unreachable), so the two sides are
  showing different screens. The report names the first failing step.

## Phone sources

| source | when | coverage |
|---|---|---|
| `live` | a device answers `adb -s <serial> get-state` | full tree per step |
| `recorded-snapshot` | `fixtures/reference-device/<scenario>/snapshot-*.json` | full tree per step (recorded moto g35 capture) |
| `recorded-observations` | `results/phone/gaten-f344272/<scenario>/phone/observations.json` | label set (text ∪ content-desc) + the resolved target |

`auto` (the default) picks live, then recorded-snapshot, then
recorded-observations, and exits 3 if none exists. The report always states the
tier and coverage, and a recorded run carries a caveat that it is not fresh.

In `text-set+target` coverage the label sets are compared as **text ∪
content-desc**, and a string present on both sides under a different attribute
is reported once as `accessibility_name_attribute` rather than twice as a set
difference.

### Attribute fidelity (PARITYFIX F1)

Whether a name is a node's **text** or its **content-desc** is only comparable
against a tree read from `uiautomator dump` (the platform's
`AccessibilityNodeInfo`). `replay_scenario.py` now records `a11y_source` in
`observations.json` and every `tree-NN-*.json`; the report states the phone's
`attribute_fidelity` (`verified` | `unverified` | `n/a`).

A capture **without** `a11y_source: uiautomator-dump` is `unverified`. That
covers every 2026-11-14 Gaten capture under `results/phone/gaten-f344272/`: they
were taken with the agent-device snapshot whose computed `label` replaced the
node fields (`results/handoff/TREE1.md` §1) - none of their steps has a single
`text` node, every name sits under content-desc. Against such a capture:

* `accessibility_name_attribute` and a target's `text_vs_desc` / `text_changed` /
  `desc_changed` are reported as **`attribute_placement_unverified`**
  (`evidence-validity/attribute-placement-unverified`), never as an ACOS
  compatibility bug and never as a pass; the only thing that settles them is a
  live replay;
* a failed `wait_for` / `assert_*` / `capture` observation does not invalidate
  the steps after it (it leaves the phone on its screen and is exactly the
  mapping artefact), and is listed under "Phone observations that failed on a
  capture that cannot name attributes". A failed action (`tap`, `type`, `back`,
  ...) still invalidates every later step.

A `verified` capture keeps every attribute-level finding and the conservative
invalidation rule.

ACOS's own side of the question is pinned by `scenarios/a11y-names.yaml` +
`tools/tests/test-a11y-names-tree.py` (fixture `a11y-names-probe`, one view per
React Native accessibility shape): the tree publishes
`AccessibilityNodeInfo.getText()/getHintText()/getContentDescription()` of the
node, a delegate-composed description included, and never a name under an
attribute the platform does not use; `scenarios/negative/a11y-names-wrong.yaml`
selects a placeholder as content-desc and must fail.

## Phone safety

* Only the scenario's own package is installed. When the scenario names an
  x86_64 APK (Gaten), the arm64 build of the same app from the same directory is
  installed instead; the harness refuses a live run when no arm64 sibling
  exists, because testing a different build is not parity.
* If the package was already installed, its APK is pulled to
  `.cache/parity/backup-<pkg>-<versionCode>.apk` before the run and reinstalled
  afterwards ("leave the device as found"). If it was not installed, it is
  uninstalled. `--keep-apps` skips the restore.
* The replay runs with `--no-device-settings`: it never writes
  `stay_on_while_plugged_in` (or any other setting). It keeps the screen awake
  with `KEYCODE_WAKEUP`, which is a key event, and dismisses a **non-secure**
  keyguard before a dump. (A dozing phone dumps SystemUI windows and nothing of
  the app — PARITY1 observed exactly that.)
* All device work is serialised on `.cache/phone.lock`
  (`flock`, 600 s timeout → exit 3).

## Options

| flag | meaning |
|---|---|
| `--serial` | device serial (default `$ACOS_DEVICE` or `ZA222WGJPP`) |
| `--out DIR` | report directory (default `results/parity/<scenario>`) |
| `--acos-artifacts DIR` | where the ACOS run writes / is read from |
| `--reuse-acos` | do not run ACOS; diff the artifacts already there |
| `--phone-source` | `auto`, `live`, `recorded-snapshot`, `recorded-observations` |
| `--phone-dir DIR` | recorded capture directory (implied by the source) |
| `--phone-apk PATH` | APK to install instead of the scenario's |
| `--allowlist FILE` | expected-difference file (default `tools/parity/allowlist.yaml`) |
| `--tolerance-dp F` | bounds tolerance in dp (default 1.5) |
| `--no-install` | never install/uninstall; require the app to be present |
| `--keep-apps` | leave the installed app in place |
| `--exclusive` | take every ART slot (only for timing runs; parity does not need it) |

## Environment

The ACOS phase needs the worktree test environment from
`.cache/standby/BRIEF_COMMON_0316.md` (`.cache/boot/android-34`, `.cache/art`,
`fixtures/build`, `ACOS_FONT_DIR`) and a release `acos` + `acos-art`. The script
defaults to `.cache/parity1-target/release/{acos,acos-art}` and honours
`ACOS_BIN` / `ACOS_ART_BIN`. It sets a **private** `ACOS_RUNTIME_DIR`,
`ACOS_SESSION_SOCKET` and `ACOS_VIEW_TEST_DIR` under `.cache/parity/` — never the
owner's.

## Reading a report

`report.md` starts with the sources and coverage, then a per-step table
(`nodes A/P`, `matched`, `exact`, `allowed`, `unexpected`), then the findings
table (classified) and finally the documented platform differences with their
reasons and counts. `report.json` carries every difference record: kind, node
key, both node snapshots, dp deltas, the explanation that was attached
(`explain: decor_shift|edge_to_edge|display_width|display_height`),
`allowed_by` and the allowlist reason.

## Tests

```bash
python3 tools/tests/test-parity-harness.py     # 32 checks, no device needed
```

Covers dp normalisation, container pairing, `text_vs_desc`, state/visibility,
allowlist scoping (including "a recorded-only entry must not leak into a live
run"), the frame-shift and edge-to-edge explanations, the ACOS
`visible_bounds` consistency check, uiautomator XML parsing, package filtering,
the real recorded capture, and the "step without a bound frame borrows the next
tree of its epoch" rule.

## Known limits

* The harness compares the **trees**, not the pixels. `acos parity --device`
  (frame parity, `tools/parity/live.sh`) still covers the screenshot side.
* `recorded-observations` only has the label set + resolved target, so it
  cannot report node structure or state for Gaten scenarios.
* A recorded capture is a point in time: it says nothing about a *fresh* build.
* The `settle: fast` steps of the Gaten onboarding are inherently unstable;
  they are classified as timing, not as compatibility.
* Scenario steps ACOS records without a frame (`launch`, `assert_log`,
  `assert_no_crash`) borrow the next tree of the same epoch (no action in
  between); the report marks them `tree_source: next_step`.
* Log assertions are not part of the tree diff (the phone's logcat is global).
