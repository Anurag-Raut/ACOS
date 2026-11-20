# PARITY1 — findings (classified)

Every unexpected difference the harness reported, grouped by cause. Evidence
paths are relative to this directory; the raw ACOS artifacts stay in
`.cache/parity/runs/<scenario>/` (trees, frames, step records) and are named
where they matter.

Headline: **one ACOS compatibility bug, one ACOS evidence bug, and one ACOS
false pass** on the Gaten flows; the fixture apps' own trees match. The rest of
the Gaten differences are device state and evidence-validity classes.

---

## F1 — `compatibility-bug/accessibility-name`: ACOS exposes an accessible name
the phone does not

**What the harness measured.** On Gaten (React Native), the same string is a
node's **`text`** on ACOS and its **`content-desc`** on the phone — and on some
fields ACOS exposes a name that **does not exist in the phone's tree at all**.

| scenario | steps | finding |
|---|---|---|
| gaten-home | 5, 8, 9 | `text_vs_desc`: "Recently looked up", "Anki", "Daily streak" |
| gaten-backstack | 5, 10, 12, 17 | `text_vs_desc`: "Recently looked up", "Not connected", "Connect" |
| gaten-lesson | 5 | `text_vs_desc`: "Recently looked up" |
| all three | every step | `accessibility_name_attribute`: 8–12 strings per step present on both sides but under different attributes |

**The hard case (an ACOS false pass).** `scenarios/gaten-backstack.yaml` steps 13
and 15 select the token field by `content_desc: {exact: "Personal access token"}`
and pass on ACOS. On the phone:

```
ACOS  tree/14.json   text="Paste your token"   content_desc="Personal access token"
phone observations   text=""                   content_desc="Paste your token"   (no "Personal access token" anywhere)
```

(`gaten-backstack/evidence/13-wait_for.json` is the ACOS step record;
`gaten-backstack/phone-source.json` names the phone capture.) The phone replay
therefore fails at step 13 (`ELEMENT_NOT_FOUND`) and every comparison from step
13 on is excluded as invalid evidence (`invalid_steps` in `report.json`).
`gaten-lesson` steps 19/20/23 select `"WaniKani personal access token"` the same
way and fail the same way.

**Classification:** ACOS defect — the tree an agent reads is not the tree the
phone exposes. A scenario that only passes on ACOS is a false pass, exactly the
risk `docs/OPERATING_DIRECTIVES.md` §1 ("never fake") is about. PHONE2 §5.1
already called this a candidate; PARITY1 now has per-step evidence for it.

**Smallest next action:** decide the model for RN accessibility names: either
ACOS's tree must expose the same attribute the platform does (prefer the
platform's `content-desc` for a `View` whose label is a description), or the
scenario must not treat ACOS's placement as evidence. Then re-run
`tools/parity/run-parity.sh scenarios/gaten-backstack.yaml` with the phone
attached.

---

## F2 — `acos-evidence/tree-consistency`: `visible_bounds` is clipped to the
pre-rotation display after `rotate`

**What the harness measured.** After the counter scenario rotates to landscape,
ACOS's tree reports the session display as `2400x1080` and each node's `bounds`
correctly, but `visible_bounds` is clipped to the **portrait width 1080**:

```
tree/8.json (landscape, display [0,0,2400,1080]):
  DecorView              bounds [0,0,2400,1080]   visible_bounds [0,0,1080,1080]
  android:id/content     bounds [128,189,2400,1017] visible_bounds [128,189,1080,1017]
  android:id/statusBarBackground bounds [0,0,2400,63] visible_bounds [0,0,1080,63]
  ... 8 nodes, steps 13–15
```

Evidence: `counter/evidence/tree-8-landscape.json` (the whole tree),
`counter/report.json` (`acos_tree_inconsistent`, 8 findings × 3 steps).

**Classification:** ACOS defect in the tree an agent reads (rotation), not a
parity difference — the phone side is irrelevant here; the harness checks the
tree against itself. An agent that crops a screenshot or computes a region from
`visible_bounds` after a rotation sees a window that is 1080 px wide instead of
2400.

**Smallest next action:** fix the display rect used for visible clipping in the
view-tree walk (it must come from the session display at capture time, not a
cached configuration); the harness reports it automatically once fixed.

---

## F3 — `labels/only-in-acos` + `only-on-phone`, `content-desc changed`:
the phone has AnkiDroid installed, ACOS does not

The phone's device state has **AnkiDroid installed without collection
permission**; ACOS's image does not ship it. Gaten's Anki card therefore renders
the *permission* state on the phone and the *not installed* state on ACOS:

| ACOS (gaten-home steps 10–13) | phone (same steps) |
|---|---|
| `Anki is not installed.` | `Anki access needed.` |
| `Open the Anki installation flow.` | `Allow Gaten to read collection statistics.` |
| `Install` | `Grant access` / `Permission needed` / `Waiting for permission.` |
| `Anki Fri: upcoming` … | `Anki Fri: study activity unknown` … |

The Analytics container's composed content-desc differs for the same reason
(`desc_changed`, `content-desc` classification), and the composed label sets
differ one-for-one.

**Classification:** environment difference (device state), not an ACOS defect
and not a platform difference. It is *reported*, not allowlisted, because the
harness must not hide a difference caused by the device it runs on.

**Smallest next action:** pick the reference state — either a phone without
AnkiDroid, or an ACOS image with AnkiDroid installed and no permission, so the
two sides run the same app state. `F3` disappears from the report when they
match.

---

## F4 — `labels/only-in-acos|only-on-phone` on `settle: fast` steps: onboarding
timing, not evidence

Steps 1–4 of every Gaten scenario tap through the onboarding `Skip` pages with
`settle: fast`. The scenario itself says those frames are navigation, not
evidence (`scenarios/gaten-home.yaml`), because the pages run a looping
Reanimated timeline and a 200 ms auto-advance. ACOS's bound frame is captured
mid-transition while the phone's `uiautomator dump` runs after agent-device's
settle, so the two show neighbouring onboarding pages.

**Classification:** evidence validity (`timing/unslept-step`), 68–80 findings
per scenario. **Smallest next action:** none for the harness; it already labels
them. A scenario that wants onboarding parity would need a settled capture on
both sides.

---

## F5 — `layout/bounds` on RN composed nodes

Six findings in `gaten-home`, six in `gaten-backstack`, two in `gaten-lesson`.
The phone's `uiautomator` node is the *composed* accessibility node
(`55,2130–1025,2260` for `Set up Gaten`), while ACOS's node with the same name is
the framework view that carries it (`58,2117–1022,2253`) — up to ~13 px / 3 dp
apart. This is the same accessibility-tree modelling difference as F1, measured
on the rect instead of the name.

**Classification:** tool/model difference in the a11y tree (visible in the
report, not allowlisted). **Smallest next action:** falls out of F1's decision.

---

## What matched

* **counter**: every app node on every compared step has the same text, class
  role, enabled/checked/focused state and dp-relative layout; all differences
  are the documented density/edge-to-edge/frame-shift ones, except F2.
* **form-validation**: 25/28 steps, 225 matched nodes, **250/250 differences
  allowlisted, 0 findings**. Text (including the typed values and the
  `Name is required` / `Email is required` / `Invalid email` errors), the
  `Submitted: Ada` dialog, the button states and the dp layout all agree.
* **Gaten**: the asserted strings are present on both sides (the label-set
  difference is F1/F3, not missing UI); the onboarding, Home, Analytics and
  WaniKani pages are the same screens on both sides when the phone reaches them.

## Files

| file | what |
|---|---|
| `counter/report.md` | per-step table + F2 |
| `form-validation/report.md` | per-step table, zero findings |
| `gaten-backstack/report.md` | 108 findings, 100 invalid differences (steps 13–24, F1) |
| `gaten-lesson/report.md` | 101 findings, 494 invalid differences (steps 7–27, F1 + F3) |
| `gaten-home/report.md` | 106 findings (F1, F3, F4, F5) |
| `*/report.json` | every difference record, with dp deltas and explanations |
| `*/acos-summary.json` | the ACOS run's verdict and per-step status |
| `*/phone-source.json` | the exact phone capture used, with hashes |
| `runs.json` | the manifest: HEAD, per-scenario summaries and findings |
