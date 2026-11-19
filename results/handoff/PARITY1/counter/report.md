# Parity: counter

* generated: 2026-11-18T08:37:41+00:00
* scenario: `scenarios/counter.yaml` (sha256 `45506d6ab497`)
* ACOS: `.cache/parity1/runs/counter` (art / pass, 1080x2400 @420 dpi)
* phone: **recorded-snapshot** (physical-device (recorded capture)), coverage **full-tree**, 1080x2400 @400 dpi, `fixtures/reference-device/counter`
* bounds tolerance: 1.5 dp (bounds converted with each side's own density)

## Summary

* steps compared: **20/24** (4 without an ACOS tree)
* matched nodes: **160**, exact matches: **0**
* differences: **204** (180 documented platform differences, **24 unexpected**) in **8 findings**

> The phone was not attached for this run: the comparison uses the checked-in physical-device capture below. It is real moto g35 data, but it is not a fresh replay; re-run with the phone attached to refresh it.
> Recorded agent-device snapshots do not expose the framework decor chrome (action bar container/toolbar, status/navigation bar backgrounds) that a live `uiautomator dump` does; those ids are allowlisted as a capture-tool scope difference (fixtures/reference-device/COMPARISON.md).

## Per step

| # | step | ACOS frame | nodes A/P | matched | exact | allowed | unexpected |
|---|---|---|---|---|---|---|---|
| 0 | `launch dev.acos.fixtures.counter/.MainActivity` | 2 (next_step) | 14/8 | 8 | 0 | 9 | 0 |
| 1 | `wait_for {"id":"count"}` | 2 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 2 | `assert_text {"id":"count"}` | 2 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 3 | `assert_text {"id":"inc"}` | 2 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 4 | `assert_text {"id":"reset"}` | 2 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 5 | `assert_log` | - | - | - | - | - | no_acos_tree |
| 6 | `tap {"id":"inc"}` | 4 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 7 | `assert_log` | 4 (next_step) | 14/8 | 8 | 0 | 9 | 0 |
| 8 | `assert_text {"id":"count"}` | 4 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 9 | `capture after-inc1` | 5 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 10 | `tap {"id":"inc"}` | 7 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 11 | `assert_log` | 7 (next_step) | 14/8 | 8 | 0 | 9 | 0 |
| 12 | `assert_text {"id":"count"}` | 7 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 13 | `rotate` | 8 (own) | 14/8 | 8 | 0 | 9 | 8 |
| 14 | `assert_log` | 8 (next_step) | 14/8 | 8 | 0 | 9 | 8 |
| 15 | `assert_text {"id":"count"}` | 8 (own) | 14/8 | 8 | 0 | 9 | 8 |
| 16 | `rotate` | 9 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 17 | `assert_text {"id":"count"}` | 9 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 18 | `tap {"id":"reset"}` | 11 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 19 | `assert_log` | 11 (next_step) | 14/8 | 8 | 0 | 9 | 0 |
| 20 | `assert_text {"id":"count"}` | 11 (own) | 14/8 | 8 | 0 | 9 | 0 |
| 21 | `back` | - | - | - | - | - | no_phone_capture |
| 22 | `assert_no_crash` | - | - | - | - | - | no_phone_capture |
| 23 | `assert_not_visible {"id":"count"}` | - | - | - | - | - | no_phone_capture |

## Findings (unexpected differences)

| classification | kind | node | steps | # |
|---|---|---|---|---|
| acos-evidence/tree-consistency | acos_tree_inconsistent | `class:container@id:android:id/content#0` | 13, 14, 15 | 3 |
| acos-evidence/tree-consistency | acos_tree_inconsistent | `class:container@root#0` | 13, 14, 15 | 3 |
| acos-evidence/tree-consistency | acos_tree_inconsistent | `id:android:id/action_bar` | 13, 14, 15 | 3 |
| acos-evidence/tree-consistency | acos_tree_inconsistent | `id:android:id/action_bar_container` | 13, 14, 15 | 3 |
| acos-evidence/tree-consistency | acos_tree_inconsistent | `id:android:id/content` | 13, 14, 15 | 3 |
| acos-evidence/tree-consistency | acos_tree_inconsistent | `id:android:id/decor_content_parent` | 13, 14, 15 | 3 |
| acos-evidence/tree-consistency | acos_tree_inconsistent | `id:android:id/navigationBarBackground` | 13, 14, 15 | 3 |
| acos-evidence/tree-consistency | acos_tree_inconsistent | `id:android:id/statusBarBackground` | 13, 14, 15 | 3 |

* **acos_tree_inconsistent** `class:container@id:android:id/content#0` (acos-evidence/tree-consistency): `{"field": "visible_bounds", "acos": {"bounds_px": [128, 189, 2400, 1017], "visible_bounds_px": [128, 189, 1080, 1017], "display_px": [0, 0, 2400, 1080], "expected_visible_bounds_px": [128, 189, 2400, 1017]}, "reason": "ACOS's visible_bounds is not the node's bounds clipped to the session display"}`
* **acos_tree_inconsistent** `class:container@root#0` (acos-evidence/tree-consistency): `{"field": "visible_bounds", "acos": {"bounds_px": [0, 0, 2400, 1080], "visible_bounds_px": [0, 0, 1080, 1080], "display_px": [0, 0, 2400, 1080], "expected_visible_bounds_px": [0, 0, 2400, 1080]}, "reason": "ACOS's visible_bounds is not the node's bounds clipped to the session display"}`
* **acos_tree_inconsistent** `id:android:id/action_bar` (acos-evidence/tree-consistency): `{"field": "visible_bounds", "acos": {"bounds_px": [128, 63, 2400, 189], "visible_bounds_px": [128, 63, 1080, 189], "display_px": [0, 0, 2400, 1080], "expected_visible_bounds_px": [128, 63, 2400, 189]}, "reason": "ACOS's visible_bounds is not the node's bounds clipped to the session display"}`
* **acos_tree_inconsistent** `id:android:id/action_bar_container` (acos-evidence/tree-consistency): `{"field": "visible_bounds", "acos": {"bounds_px": [128, 63, 2400, 189], "visible_bounds_px": [128, 63, 1080, 189], "display_px": [0, 0, 2400, 1080], "expected_visible_bounds_px": [128, 63, 2400, 189]}, "reason": "ACOS's visible_bounds is not the node's bounds clipped to the session display"}`
* **acos_tree_inconsistent** `id:android:id/content` (acos-evidence/tree-consistency): `{"field": "visible_bounds", "acos": {"bounds_px": [128, 189, 2400, 1017], "visible_bounds_px": [128, 189, 1080, 1017], "display_px": [0, 0, 2400, 1080], "expected_visible_bounds_px": [128, 189, 2400, 1017]}, "reason": "ACOS's visible_bounds is not the node's bounds clipped to the session display"}`
* **acos_tree_inconsistent** `id:android:id/decor_content_parent` (acos-evidence/tree-consistency): `{"field": "visible_bounds", "acos": {"bounds_px": [128, 0, 2400, 1017], "visible_bounds_px": [128, 0, 1080, 1017], "display_px": [0, 0, 2400, 1080], "expected_visible_bounds_px": [128, 0, 2400, 1017]}, "reason": "ACOS's visible_bounds is not the node's bounds clipped to the session display"}`
* **acos_tree_inconsistent** `id:android:id/navigationBarBackground` (acos-evidence/tree-consistency): `{"field": "visible_bounds", "acos": {"bounds_px": [0, 1017, 2400, 1080], "visible_bounds_px": [0, 1017, 1080, 1080], "display_px": [0, 0, 2400, 1080], "expected_visible_bounds_px": [0, 1017, 2400, 1080]}, "reason": "ACOS's visible_bounds is not the node's bounds clipped to the session display"}`
* **acos_tree_inconsistent** `id:android:id/statusBarBackground` (acos-evidence/tree-consistency): `{"field": "visible_bounds", "acos": {"bounds_px": [0, 0, 2400, 63], "visible_bounds_px": [0, 0, 1080, 63], "display_px": [0, 0, 2400, 1080], "expected_visible_bounds_px": [0, 0, 2400, 63]}, "reason": "ACOS's visible_bounds is not the node's bounds clipped to the session display"}`

## Documented platform differences (allowlisted)

| allowlist id | kinds | # | reason |
|---|---|---|---|
| `acos-invisible-node` | extra_in_acos×20 | 20 | ACOS's tree exposes framework nodes it marks `visible: false` (a zero-width ActionMenuView overflow menu, a collapsed ActionBarContextView). Neither `uiautomator dump` nor agent-device reports a node with no visible area, so the phone has no counterpart. An agent cannot see or act on these; the entry is limited to nodes ACOS itself marks invisible. |
| `api35-edge-to-edge` | bounds_changed×60 | 60 | API 35 draws the app window edge-to-edge (bottom == display bottom); the API-34 image ends the app window above the navigation bar. The comparator marks a full-width node that reaches the phone's display bottom but not ACOS's as `edge_to_edge`; the app's own dp layout inside it still has to match. Evidence: fixtures/reference-device/COMPARISON.md ("API 35 vs API 34 ... shift the content origin"). |
| `display-dp-size` | bounds_changed×20 | 20 | The phone is 1080x2400 @400dpi = 432x960dp; the ACOS reference session is 1080x2400 @420dpi = 411x914dp. A node that spans the display (the window root) necessarily has a different dp extent. Bounds are compared in dp so that dp-sized children match exactly; this single entry covers the one node whose size is the display itself. |
| `window-frame-origin-shift` | bounds_changed×80 | 80 | The status bar band is 128 px (48.8 dp) on the API-34 image and 108 px (43.2 dp) on the Android 15 phone, so the whole app frame sits ~5.6 dp higher on the phone and every node in it is translated by that amount. The comparator measures the frame shift from `android:id/content`'s top-left edge and marks a size-preserving translation with it; a node that moved by anything else is not covered by this entry. |

## Settings

```json
{
  "allowlist": "tools/parity/allowlist.yaml",
  "bounds_normalisation": "px * 160 / dpi, per side",
  "bounds_tolerance_dp": 1.5,
  "strict_class": false
}
```
