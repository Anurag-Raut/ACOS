# LAYER1 — finish the window layer stack

Branch `layer1` (from `layer0` = `6b00f943`). Owner decisions applied: the system
band order is **transcribed from AOSP source** (O1/O2), multi-window pixels stay
C1-only (D3), and the ROT9 handle pool is out of scope.

## What changed

1. **A layer table from source, not prose.**
   `tools/fetch-aosp-reference.sh` now sparse-checks out
   `frameworks/base/services/core/java/com/android/server/policy` (and `/wm`, for
   `canAddInternalSystemWindow`) at `android-14.0.0_r75`.
   `crates/acos-wm/src/layers.rs` replaces the derived `LayerBand` enum with
   `policy_layer(type, can_add_internal_system_window)`, a line-for-line
   transcription of `WindowManagerPolicy.getWindowLayerFromTypeLw()`
   (`WindowManagerPolicy.java:525-630`), plus `sub_window_layer()` (`:656-674`)
   and the `WindowManagerPolicyConstants` sub-layer constants.
   `tools/gen-window-layer-table.py` regenerates `docs/WINDOW_LAYERS.md` from the
   cache, one row per `case` arm with `file:line`.
   `policy_layers_match_the_aosp_table` pins every row (including the three
   permission-dependent arms) and the sub-window offsets.

   The source table answers O1/O2 and **corrects** LAYER0's derived order:
   `TYPE_APPLICATION_OVERLAY` = 11, IME = 13, status bar = 15, shade = 17,
   navigation bar = 24, **accessibility overlay = 31**; a normal app's
   `TYPE_SYSTEM_ALERT` = 9 is *below* the application overlay (12 only for an
   internal system window); the status bar is *below* the notification shade.

2. **Same-layer order is creation order, not binder id.**
   `WindowRecord.seq` (a monotonic per-display allocator) is the tie-breaker in
   `WindowLayerStack::ordered()`. Before this, two `TYPE_APPLICATION` windows
   (activity + dialog) were ordered by the numeric `IWindow` binder id.

3. **Focus follows the stack, on add / remove / relayout.**
   `WmsState::publish_focus_transition()` queues
   `WindowFocusChanged(false)` for the old target and `(true)` for the new
   `focus_target()`; the record's `focusable` now honours the flag rather than
   `!is_system_window`. `InputDispatch::set_focus()` publishes the real
   `InputMessage::FOCUS` on the same policy target (registration no longer
   blindly focuses the newest window). `WindowSessionService` calls it after
   add, remove and relayout.

4. **The ART/`verify` path routes taps by the stack.**
   The CLI sends **display** coordinates; `launch.rs` hit-tests the WMS layer
   stack, picks the topmost touchable window, subtracts that record's frame
   origin and publishes the gesture on *that* window's `InputChannel`
   (`InputHost::tap_window`). The old path sent every tap to the newest
   registered channel.

5. **The tree and screenshot carry the window.**
   The guest probe's `windows[]` now has `id`, raw `type`, `focus`, `touch`,
   root `bounds` and `parent` (sub-windows). The host applies the one policy
   table, orders `windows[]` bottom-first and tags every node with
   `window_id` / `window_type` / `window_kind` / `window_z`
   (`acos-protocol::UiNode`), orders the roots by window z, and serialises the
   `windows[]` block (`acos-protocol::UiTree`). `tree_json` (observe/launch/
   MCP replies) carries the block; the screenshot names the window count.
   Tags are short (id is 7 hex chars) and `window_z` is omitted only on a
   pre-LAYER1 probe (REL3C cap). Content settle hashes kind/z, so a stack
   change restarts the content window.

6. **Scenario assertion.** `assert_windows` (`order`, `types`, `focus`,
   `focus_type`) reads the tree's `windows[]`; it is exact (count + order) and
   fails closed on a tree without a window block.

## Evidence (all on `--engine art`, real framework, shared lock)

| run | result | file |
|---|---|---|
| `layer0-overlay-stack` (18 steps) | **pass** | `results/handoff/LAYER1/artifacts/overlay-run4/verdict.json`, `overlay-run4.json` |
| `w27-dialog-window` | **pass** | `artifacts/w27/verdict.json` |
| `w54-dialog-pixels` | **pass** | `artifacts/w54/verdict.json` |
| Gaten lookup-mode disclosure dialog | **not available** in this worktree | no scenario named it (grep of `scenarios/`, `results/`, `/home/Qui/Projects/acos/.cache/gaten`) |

The overlay gate asserts, in one run: `windows[]` order
`TYPE_BASE_APPLICATION < TYPE_APPLICATION (dialog) < TYPE_APPLICATION_OVERLAY
×2` with the activity focused before the dialog and the dialog focused after;
the overlay's flat red above the activity's flat blue in the PNG; a tap on
overlay A (`overlay_tapped`), a tap beside it (`activity_tapped`), and a tap on
the `FLAG_NOT_TOUCHABLE` overlay B passing through (`activity_tapped`).

The C1 one-layer fast path is unchanged: `w35_composite`
(`a_single_full_window_layer_passes_through_with_its_own_stride`) is green and
is the micro-check in the prepared A/B driver.

## Not run here

`tools/bench/layer0-single-window-ab.sh` is prepared for the bench owner; see
`RUNBOOK.md`. No exclusive timing run was made (brief: benchmarks are batched).
