# GATENWORDc — Gaten's word sheet after the bubble tap

Branch `gatenwordc` (= `gatenwordb` + `modules`), 2026-11-18. Private runtime dirs per run
(`run-arm.sh`), ART runs through `run-exclusive.sh` + `guarded.sh`, binaries in
`.cache/gwc-target`. `~/Projects/gaten` untouched (APK from `.cache/gaten`, source read-only).

## Result

`scenarios/gaten-word-lookup.yaml`: PASS **5/5** (`rep-1…5`, 44 steps each); `gaten-bubble-overlay`
still PASS (`gate-bubble`, 28 steps). `sheet-frame.png` = the word sheet (猫 / ねこ・ネコ / Meanings).
The scenario: bubble tap → Gaten freezes its own dictionary screen (search "neko", known Japanese
text) → OCR finishes (status-rail node `Circle or tap Japanese text`; the "Screen scan ready" log is
`Log.d`, stripped from the release APK) → tap the first recognised word (猫, pixel position — the
overlay draws the screenshot as an image) → assert the sheet's nodes (`Close lookup`, `ねこ・ネコ`,
`Meanings`, `Select again`, the first meaning) + window stack → tap the sheet's `Close lookup` →
bubble windows back, sheet gone.

**Back is NOT covered — open gap (below).** The acceptance asked for "Back closes it".

## Two ACOS engine gaps found and fixed (each with a before/after proof)

1. **Compose windows had an empty UI tree** (`LaunchMain.java`, `enumerateProviderTree`). The tree
   dump only merged an `AccessibilityNodeProvider` through `findAccessibilityNodeInfosByText("")`,
   which Compose does not implement (it serves `createAccessibilityNodeInfo` + child ids only). The
   lookup overlay is Compose, so `tree` held the empty `AndroidComposeView` and every selector on it
   timed out (`ex2` before: `wait_for` 90 s timeout; after: the rail, the sheet and its buttons are
   nodes). The walk now follows the standard traversal (host node → child ids, depth first, bounded).
2. **Closing the lookup aborted the guest.** Layer handles were a fixed pool of 16 that is never
   returned (`SurfaceState::take_handle`); every window costs two (window layer + BLAST child), so
   the 9th window of the session got no layer, `createSurface` failed (`unknown binder handle`,
   `SurfaceComposerClient::createSurface error Invalid argument`) and `BLASTBufferQueue` asserted
   `mSurfaceControl must not be NULL` → SIGABRT. Evidence before: `w3` (`art-frames/native-crash.txt`,
   `logd-final.jsonl` seq 398-407). Pool is 128 now; test
   `a_session_can_create_many_more_layers_than_sixteen` fails with 16, passes with 128.
   (Smallest follow-up: recycle handles of destroyed layers with a tombstone epoch, or grow the pool
   from the host side between transactions — the pool is still a hard cap, now ~64 windows.)

## Open gap: Back on the word sheet

`key: back` on the open sheet is delivered to the focused overlay window and acked handled
(`acked 2 published 2`), but 3 s later the sheet is unchanged (`gaten-word-lookup-back-probe.yaml`,
frames in the earlier probe: sheet and window stack identical). The `Close lookup` tap works.
Gaten's path for Back: `OverlayRoot.dispatchKeyEvent` (BACK/UP) → `onBack` →
`host.onBackPressedDispatcher.onBackPressed()` → Compose `BackHandler { close() }` → `closing=true` →
`LaunchedEffect(closing)` waits for `overlayState`/`sheetState` to go idle, then `onDismiss()`.
The log also shows `WindowOnBackDispatcher: OnBackInvokedCallback is not enabled for the application`
for the overlay window. Not yet known which link stalls (key not reaching `OverlayRoot`, the
`BackHandler` callback not enabled, or the exit transition never going idle with `animations: off`).
Smallest next action: a guest probe that logs `OverlayRoot.dispatchKeyEvent` and the dispatcher's
`hasEnabledCallbacks()` (needs a debug build of Gaten or an ACOS-side view-hierarchy key trace).

## Other notes

* `cargo test -p acos-services`: 692 passed, 0 failed (83 result lines).
* The strict `capture` right after the sheet closes times out (the bubble re-appears animating): the
  scenario does not capture there.
* Files: `rep-*`/`gate-bubble`/`w5` verdicts + steps + logs (frames and trees pruned), `w3` (crash,
  pool 16), `w4` (Back + Close with the fix), `run-arm.sh`, `sheet-frame.png`.
