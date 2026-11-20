# PARITY1 — per-step UI-tree parity harness (v0.5.0, first step)

Branch `parity1` (base `modules` = `aaf518bf8`). Harness: `tools/parity/run-parity.sh`,
comparator `tools/parity/diff_trees.py`, node model `tools/parity/parity_nodes.py`,
expected-difference list `tools/parity/allowlist.yaml`, docs `docs/PARITY.md`.

**The phone was not attached when the harness ran.** `adb devices` was empty (no
USB device) at the start of the evidence runs; the phone had answered earlier in
the same session, long enough to confirm the live `uiautomator` path and the
display/awake handling. Every report below therefore uses the checked-in
physical-device capture for that scenario and says so in its header. That is the
recorded-fixture path the brief allows; it is real moto g35 data, not a fresh
replay. `runs.json` records this.

## What was run

| scenario | ACOS ART | phone source | coverage | steps compared | matched nodes | diffs (allowed / unexpected) | findings | excluded (invalid) |
|---|---|---|---|---|---|---|---|---|
| counter | pass | `fixtures/reference-device/counter` (recorded 2026-11-10) | full tree | 20/24 | 160 | 204 (180 / **24**) | **8** | 0 |
| form-validation | pass | `fixtures/reference-device/form-validation` (recorded) | full tree | 25/28 | 225 | 250 (250 / 0) | 0 | 0 |
| gaten-backstack | pass | `results/phone/gaten-f344272/gaten-backstack/phone` | labels+target | 24/25 | 326 | 236 (0 / 236) | 108 | 100 diffs, steps 13–24 |
| gaten-lesson | pass | `results/phone/gaten-f344272/gaten-lesson/phone` | labels+target | 27/29 | 71 | 169 (0 / 169) | 101 | 494 diffs, steps 7–27 |
| gaten-home | pass | `results/phone/gaten-f344272/gaten-home/phone` | labels+target | 13/14 | 208 | 271 (0 / 271) | 106 | 0 |

ACOS side: `acos verify <scenario> --engine art` on this branch
(ACOS 0.3.0, ART engine, process isolation, API-34 image). The fixture scenarios
use `display.density_dpi: 420`; the three Gaten scenarios use `400` (the phone's
density) — that is why the same-density Gaten reports have no
`display-dp-size`/`display-width` differences while counter/form-validation do.

"Excluded (invalid)" is a step at or after the phone replay's first failure: the
phone is not on the screen the scenario asked for, so those differences are kept
in `report.json` (marked `invalid`) but not counted as findings.

Each scenario directory holds:

```
report.md            the human per-step table + findings + documented differences
report.json          every difference record (nodes, dp deltas, explanations, allowlist)
acos-summary.json    the ACOS run's verdict and per-step statuses
phone-source.json    the phone capture used, its device facts and file hashes
evidence/            only where a finding needs a raw artifact in-repo
```

The raw ACOS artifacts (trees, frames, step records) are large and stay in
`.cache/parity1/runs/<scenario>/` (a fresh run defaults to
`.cache/parity/runs/<scenario>/`); each report's `acos.artifacts` names the exact
path it read.

## How to read the numbers

`differences` counts every field difference per node per step; `findings` counts
distinct (kind, node) findings across the scenario. A node that moved with the
window frame produces one `bounds_changed` record that is **allowed** by
`window-frame-origin-shift` (the status-bar band is 128 px on the API-34 image
and 108 px on the phone), so counter/form-validation are dominated by allowed
records and the findings are the interesting part.

`gaten-lesson` and `gaten-backstack` have phone-side replay failures
(`failed_steps` in `phone-source.json` and `report.md`). In both cases the cause is
finding **F1**: the scenario selects a field by `content_desc "Personal access
token"`, which ACOS exposes and the real phone does not — the phone replay then
cannot leave that screen, so every step from there is excluded as invalid
evidence. The Gaten observations are label-set coverage, which is why their
findings are label/name/bounds classes rather than node structure.

## Findings

See `FINDINGS.md`: every unexpected finding, its class, its evidence path, and
whether it is an ACOS defect, a documented platform difference, an environment
difference, or an invalid comparison (timing / phone-side failure).

## Re-run

```bash
# full (needs the phone attached for a live replay):
tools/parity/run-parity.sh scenarios/counter.yaml

# offline, against the recorded capture (what produced these reports):
tools/parity/run-parity.sh scenarios/counter.yaml \
    --reuse-acos --acos-artifacts .cache/parity/runs/counter \
    --out results/handoff/PARITY1/counter
```

`tools/parity/run-parity.sh --help` lists the options; `docs/PARITY.md` explains
the matching, the dp normalisation, the allowlist and the phone safety rules.
