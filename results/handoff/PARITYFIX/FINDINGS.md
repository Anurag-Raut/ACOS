# PARITYFIX — F1 and F2 resolved from the PARITY1 findings

Branch `parityfix` = `parity1` (`4a9cf841`) + `modules` (`11ea5019`) merge + this
work. Phone **not attached** (`adb devices` empty at start and end), so no live
replay was possible and no app was installed on or removed from the phone.

## F2 — `visible_bounds` clipped to the pre-rotation display: FIXED

**Cause (one place).** `LaunchMain.displayBounds(View)` probed
`v.getResources().getDisplayMetrics()` once, cached it in a static
(`sDisplayBoundsProbed`) and `visibleRectOnScreen` clipped every node's visible
rect to it. After a rotation the session display is 2400x1080
(`PANEL_W`/`PANEL_H`, swapped by `rotatedConfiguration()`), but the cached value
stayed 1080x2400 for the life of the process.

**Fix.** `displayBounds` reads the live session display (`PANEL_W` x `PANEL_H`)
on every call; `DisplayMetrics` is only the fallback for a panel that was never
sized. Nothing is cached.

**Tests (fail before, pass after).**
`tools/tests/test-tree-display-clip.py <rot1-rotate artifacts> --need-landscape`:
every tree's `visible_bounds` equals `bounds` clipped to that tree's own
application-window display, and the run must contain a landscape tree with a node
visible past x=1080 (a run that never rotated, or whose clip stops at the old
width, cannot pass).

| | trees consistent | landscape nodes past x=1080 | evidence |
|---|---|---|---|
| before (HEAD guest) | 3/7 (trees 5, 6, 8 fail: DecorView `[0,0,2400,1080]` visible `[0,0,1080,1080]`, 8 nodes each) | 0 | `f2-rotation/before-fix.txt` |
| after | 7/7 | yes | `f2-rotation/after-fix.txt`, `f2-rotation/rot1-verdict-after.json` |

The PARITY1 counter report (`acos_tree_inconsistent`, 8 nodes x 3 steps) is gone:
`counter/report.md` has 160 matched nodes, 180 differences, 180 allowed,
**0 unexpected**.

## F1 — "ACOS exposes an accessible name the phone does not": NOT an ACOS defect; the phone evidence was invalid

### What ACOS reports, and why it is the reference

ACOS's tree is built in exactly one place, `LaunchMain.appendViewJson`
(`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`). `text`, `hint`,
`content_desc` and `password` come from `View.createAccessibilityNodeInfo()`
(`AccessibilityNodeInfo.getText()/getHintText()/getContentDescription()/
isPassword()`) - the object `uiautomator dump` prints as `text=`/`content-desc=`.
It runs the real framework and the real React Native code (the RN `BaseViewManager`
sets the View's `contentDescription` from `accessibilityLabel`;
`ReactAccessibilityDelegate.onInitializeAccessibilityNodeInfo` only composes a
description when both text and description are empty;
`.cache/rn-0.81.4/.../uimanager/BaseViewManager.java:416-447`,
`ReactAccessibilityDelegate.java:242-251`). So for the WaniKani token field
(`accessibilityLabel="Personal access token"`, `placeholder="Paste your token"`,
`secureTextEntry`) the platform node is exactly what ACOS publishes:
`text="Paste your token"` (TextView `getTextForAccessibility()` hint fallback),
`hint="Paste your token"`, `content-desc="Personal access token"`,
`password=true`. Nothing in that code path differs between API 34 (ACOS) and the
phone for these fields, and `results/handoff/TREE1.md` §4 measured the same shape
live on the phone for Gaten's search field
(`text="猫, ねこ, neko..." content-desc="Search Japanese dictionary"`).

### Why PARITY1 saw a difference: the recorded phone capture cannot tell text from content-desc

`results/phone/gaten-f344272/*/phone/observations.json` were recorded
2026-11-14 11:42-12:06 with the PHONE2-era `replay_scenario.py`, which read the
agent-device snapshot and filed its computed `label` (the placeholder for an
`EditText`) under content-desc and dropped the node's own
`contentDescription`. TREE1 (18:15 the same day, `results/handoff/TREE1.md`
§1) found and fixed that mapping bug in the harness, but these captures were
never re-recorded. Measured on all 18 recorded Gaten captures:
**0 `text` entries in any step** (e.g. gaten-backstack 503 descs / 0 texts).
That is what every `text_vs_desc` / `accessibility_name_attribute` finding and
the "hard case" (`Personal access token` absent on the phone) actually measured.

### What changed

* `tools/phone/replay_scenario.py` records `a11y_source` (the tool that produced
  each step's tree; only `uiautomator-dump` is attribute-faithful).
* `tools/parity/diff_trees.py` gives a phone source an `attribute_fidelity`:
  `verified` only for `a11y_source: uiautomator-dump`. Against an
  `unverified` capture, name-placement differences (`accessibility_name_attribute`,
  a target's `text_vs_desc`/`text_changed`/`desc_changed`, and a content-desc that
  the capture does not carry) are reported as
  **`attribute_placement_unverified`**
  (`evidence-validity/attribute-placement-unverified`) - not as ACOS bugs, not as
  passes - and a failed `wait_for`/`assert_*` on such a capture is listed under
  "Phone observations that failed on a capture that cannot name attributes"
  instead of silently invalidating all later steps (a failed action still
  does). A `verified` capture keeps every finding and the conservative rule.
* ACOS side pinned against `AccessibilityNodeInfo` semantics by a fixture app
  with one view per React Native shape (`fixtures/apps/a11y-names-probe`:
  hint+label EditText with password input, hint+label EditText, text-only
  TextView, text+description TextView, labelled clickable container, and a
  description that exists only on the node info via an accessibility delegate):
  `scenarios/a11y-names.yaml` passes on ART (all selectors by the attribute the
  platform uses) and `tools/tests/test-a11y-names-tree.py` pins the exact
  `(text, hint, content_desc, password)` of the 7 nodes
  (`f1-a11y-names/test-a11y-names-tree.txt`, 7/7).
  `scenarios/negative/a11y-names-wrong.yaml` (placeholder selected as
  content-desc) fails with `ELEMENT_NOT_FOUND`
  (`f1-a11y-names/negative-verdict.json`), so ACOS does not invent a name under
  the wrong attribute.
* 11 new harness checks (`tools/tests/test-parity-harness.py`, 34 total): the
  attribute-fidelity rule, the unverified/verified split for each difference
  kind, and the failed-observation vs failed-action invalidation. Run against
  the PARITY1 `diff_trees.py` they error (`PhoneSide` has no
  `attribute_fidelity`).

### Result on the recorded phone captures

| report | phone source | attribute fidelity | unexpected (before -> after) | `compatibility-bug/accessibility-name*` before -> after | steps excluded as invalid |
|---|---|---|---|---|---|
| gaten-backstack | recorded-observations | unverified | 236 -> 331 (steps 13-24 now compared) | 4 findings -> **0** | 12 (13-24) -> **0** |
| gaten-lesson | recorded-observations | unverified | 169 -> 159 | 2 findings -> **0** | 21 -> 21 (7-27; the Anki action fails on the phone, legitimately) |
| gaten-home | recorded-observations | unverified | 271 -> 266 | 4 findings -> **0** | 0 |
| counter | recorded-snapshot | n/a | 24 -> **0** (the 8 `tree-consistency` findings) | - | 0 |
| form-validation | recorded-snapshot | n/a | 0 -> 0 | - | 0 |

The step-13/15 selector `content_desc "Personal access token"` no longer shows as
an ACOS-only name; the report lists the two failed phone observations as
unverified (`gaten-backstack/report.md`). The remaining Gaten findings are F3
(AnkiDroid on the phone), F4 (`settle: fast` onboarding timing) and F5 (composed
node bounds), unchanged.

### What is NOT proven, and the one action that proves it

`attribute_placement_unverified` is not "verified equal". The phone's own
`uiautomator dump` of the token screen is the only thing that settles it, and the
phone was detached. TREE1 §6 also recorded two live-phone placements that differ
for other node shapes (`gaten-search` step 13: `content_desc contains "Open 猫"`
is `text` on the device; `gaten-backstack` step 7: `content_desc exact
"Analytics"` is `text` on the device); those were measured with
`uiautomator dump` and are **not** explained by this change. They are listed below
with the scenarios that name the affected nodes and were not edited.

**Next action (phone attached):**
`tools/parity/run-parity.sh scenarios/gaten-backstack.yaml --phone-source live`
(then `gaten-lesson`, `gaten-home`) - the live run writes `a11y_source:
uiautomator-dump`, the report becomes `verified`, and any real placement
difference reappears as `compatibility-bug/accessibility-name*`.

### Scenarios whose `content_desc` selectors were reviewed

No ACOS name field changed (the tree builder is untouched for names), so no
scenario assertion changed meaning and none was weakened. Scenarios naming nodes
by `content_desc` (all unedited): `gaten-backstack`, `gaten-lesson`,
`gaten-home`, `gaten-tabs`, `gaten-support`, `gaten-search`, `gaten-settings`,
`gaten-kanji`, `gaten-recognition`, `gaten-bubble`, `gaten-bubble-overlay`,
`w19-probe-content-desc`, `w25-gaten-tap`, `w28-gaten-release-tap`,
`rel3s-gaten-scroll`, `a11y-names` (new). Two selectors are known (TREE1 §6,
live phone) to be placed differently on the device and are the open item above:
`gaten-search` step 13 (`Open 猫`) and `gaten-backstack` step 7 (`Analytics`).

The F2 fix changes one visible ACOS behaviour: after a rotation a node that lies
beyond the old width is now `visible: true` where it was `false`. Every scenario
with a `rotate` step was re-run on ART (list in the report below) with the same
verdict as before.

### F2 regression sweep (every scenario with a `rotate` step, on ART)

`rotate-regression/<scenario>.verdict.json`: `multi-activity`, `hello-views`,
`v1-recreate`, `todo-list`, `multi-activity-finish` pass; `counter` (parity run)
and `rot1-rotate` pass; the negatives `negative/rot1-rotate-wrong` and
`negative/counter-wrong-count` still fail with `ASSERTION_FAILED` at their own
step. (Gaten, counter, form-validation parity runs: 25/25, 29/29, 24/24, 28/28
steps pass on ART.)

## Tests run

| command | result |
|---|---|
| `python3 tools/tests/test-parity-harness.py` | 34 passed, 0 failed (`test-parity-harness.txt`) |
| `python3 tools/tests/test-a11y-names-tree.py <a11y-names artifacts>` | 7/7 |
| `python3 tools/tests/test-tree-display-clip.py <rot1-rotate artifacts> --need-landscape` | 7/7 trees (3/7 before the fix) |
| `cargo test -p acos-cli --bin acos` (under the ART lock) | 208 passed |
| `cargo test -p acos-art --test w31_tree_evidence` (under the ART lock) | 3 passed |
| `cargo test -p acos-protocol --test scenarios -p acos-verify --test scenarios` | 10 + 40 passed (the new scenarios parse) |
| `acos verify scenarios/a11y-names.yaml --engine art` | pass |
| `acos verify scenarios/negative/a11y-names-wrong.yaml --engine art` | fail (`ELEMENT_NOT_FOUND`), as required |
