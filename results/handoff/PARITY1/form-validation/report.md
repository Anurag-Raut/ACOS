# Parity: form-validation

* generated: 2026-11-18T08:37:41+00:00
* scenario: `scenarios/form-validation.yaml` (sha256 `a74b24cfd898`)
* ACOS: `.cache/parity1/runs/form-validation` (art / pass, 1080x2400 @420 dpi)
* phone: **recorded-snapshot** (physical-device (recorded capture)), coverage **full-tree**, 1080x2400 @400 dpi, `fixtures/reference-device/form-validation`
* bounds tolerance: 1.5 dp (bounds converted with each side's own density)

## Summary

* steps compared: **25/28** (3 without an ACOS tree)
* matched nodes: **225**, exact matches: **0**
* differences: **250** (250 documented platform differences, **0 unexpected**) in **0 findings**

> The phone was not attached for this run: the comparison uses the checked-in physical-device capture below. It is real moto g35 data, but it is not a fresh replay; re-run with the phone attached to refresh it.
> Recorded agent-device snapshots do not expose the framework decor chrome (action bar container/toolbar, status/navigation bar backgrounds) that a live `uiautomator dump` does; those ids are allowlisted as a capture-tool scope difference (fixtures/reference-device/COMPARISON.md).

## Per step

| # | step | ACOS frame | nodes A/P | matched | exact | allowed | unexpected |
|---|---|---|---|---|---|---|---|
| 0 | `launch dev.acos.fixtures.formvalidation/.MainActivity` | 2 (next_step) | 15/8 | 8 | 0 | 9 | 0 |
| 1 | `wait_for {"id":"submit"}` | 2 (own) | 15/8 | 8 | 0 | 9 | 0 |
| 2 | `assert_text {"id":"name"}` | 2 (own) | 15/8 | 8 | 0 | 9 | 0 |
| 3 | `assert_text {"id":"email"}` | 2 (own) | 15/8 | 8 | 0 | 9 | 0 |
| 4 | `assert_text {"id":"submit"}` | 2 (own) | 15/8 | 8 | 0 | 9 | 0 |
| 5 | `assert_log` | - | - | - | - | - | no_acos_tree |
| 6 | `tap {"id":"submit"}` | 4 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 7 | `assert_log` | 4 (next_step) | 15/9 | 9 | 0 | 10 | 0 |
| 8 | `assert_text {"id":"error"}` | 4 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 9 | `tap {"id":"name"}` | 6 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 10 | `type {"id":"name"}` | 7 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 11 | `tap {"id":"submit"}` | 9 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 12 | `assert_log` | 9 (next_step) | 15/9 | 9 | 0 | 10 | 0 |
| 13 | `assert_text {"id":"error"}` | 9 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 14 | `tap {"id":"email"}` | 11 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 15 | `type {"id":"email"}` | 12 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 16 | `tap {"id":"submit"}` | 14 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 17 | `assert_log` | 14 (next_step) | 15/9 | 9 | 0 | 10 | 0 |
| 18 | `assert_text {"id":"error"}` | 14 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 19 | `capture after-invalid-email` | 15 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 20 | `type {"id":"email"}` | 29 (own) | 15/9 | 9 | 0 | 10 | 0 |
| 21 | `tap {"id":"submit"}` | 31 (own) | 24/10 | 10 | 0 | 11 | 0 |
| 22 | `assert_log` | 31 (next_step) | 24/10 | 10 | 0 | 11 | 0 |
| 23 | `assert_text {"id":"message"}` | 31 (own) | 24/10 | 10 | 0 | 11 | 0 |
| 24 | `capture success-dialog` | 32 (own) | 24/10 | 10 | 0 | 11 | 0 |
| 25 | `assert_region` | 33 (own) | 24/10 | 10 | 0 | 11 | 0 |
| 26 | `assert_region` | - | - | - | - | - | no_phone_capture |
| 27 | `assert_no_crash` | - | - | - | - | - | no_acos_tree |

## Findings (unexpected differences)

None: every difference is on the expected-difference allowlist.

## Documented platform differences (allowlisted)

| allowlist id | kinds | # | reason |
|---|---|---|---|
| `acos-invisible-node` | extra_in_acos×20 | 20 | ACOS's tree exposes framework nodes it marks `visible: false` (a zero-width ActionMenuView overflow menu, a collapsed ActionBarContextView). Neither `uiautomator dump` nor agent-device reports a node with no visible area, so the phone has no counterpart. An agent cannot see or act on these; the entry is limited to nodes ACOS itself marks invisible. |
| `api35-edge-to-edge` | bounds_changed×60 | 60 | API 35 draws the app window edge-to-edge (bottom == display bottom); the API-34 image ends the app window above the navigation bar. The comparator marks a full-width node that reaches the phone's display bottom but not ACOS's as `edge_to_edge`; the app's own dp layout inside it still has to match. Evidence: fixtures/reference-device/COMPARISON.md ("API 35 vs API 34 ... shift the content origin"). |
| `display-dp-size` | bounds_changed×25 | 25 | The phone is 1080x2400 @400dpi = 432x960dp; the ACOS reference session is 1080x2400 @420dpi = 411x914dp. A node that spans the display (the window root) necessarily has a different dp extent. Bounds are compared in dp so that dp-sized children match exactly; this single entry covers the one node whose size is the display itself. |
| `display-width-follows-screen` | bounds_changed×85 | 85 | A match_parent or right-aligned node follows the display's dp width: the phone's display is 432 dp wide and the ACOS reference session's is 411 dp (both 1080 px, 400 vs 420 dpi), so such a node is up to 20.6 dp wider or sits up to 20.6 dp further right. The comparator only marks a node with `display_width` when its anchored dp margin(s) to the display edge match and no other edge moved; a genuine size change is not covered. |
| `recorded-capture-tool-scope` | extra_in_acos×5 | 5 | The recorded agent-device accessibility snapshot does not expose the framework's DecorView chrome (action bar container/toolbar, the status/navigation bar backgrounds). A live `uiautomator dump` does expose them, so this is a capture-tool scope difference in the recorded tier, not a missing UI element. Evidence: fixtures/reference-device/COMPARISON.md, and the live counter dump in results/handoff/PARITY1/ (uiautomator lists statusBarBackground). |
| `window-frame-origin-shift` | bounds_changed×55 | 55 | The status bar band is 128 px (48.8 dp) on the API-34 image and 108 px (43.2 dp) on the Android 15 phone, so the whole app frame sits ~5.6 dp higher on the phone and every node in it is translated by that amount. The comparator measures the frame shift from `android:id/content`'s top-left edge and marks a size-preserving translation with it; a node that moved by anything else is not covered by this entry. |

## Settings

```json
{
  "allowlist": "tools/parity/allowlist.yaml",
  "bounds_normalisation": "px * 160 / dpi, per side",
  "bounds_tolerance_dp": 1.5,
  "strict_class": false
}
```
