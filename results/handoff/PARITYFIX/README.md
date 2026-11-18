# PARITYFIX — F1 / F2 from the PARITY1 findings

Branch `parityfix` (`parity1` + `modules`). Read `FINDINGS.md` first.

* **F2 fixed** (guest `LaunchMain.displayBounds` no longer caches the pre-rotation
  display); test fails before, passes after (`f2-rotation/`).
* **F1: not an ACOS defect.** The only phone evidence for it (the recorded Gaten
  captures) was made with the pre-TREE1 agent-device mapping and cannot tell text
  from content-desc (0 `text` nodes in 18 captures). The harness now says so
  (`attribute_placement_unverified`) instead of filing it as an ACOS bug; ACOS's
  own tree is pinned to `AccessibilityNodeInfo` by a new fixture + scenario +
  negative twin (`f1-a11y-names/`). **The phone was detached, so the live
  confirmation is still the one open action** (see FINDINGS.md "What is NOT
  proven").
* Updated parity reports for the five scenarios: `<scenario>/report.{md,json}`,
  `runs.json`.
* `rotate-regression/`: ART verdicts of every scenario with a `rotate` step.
