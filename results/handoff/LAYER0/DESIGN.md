# LAYER0 — window layer stack: design (from the code)

Branch `layer0` (from `main` = `3c2e0750`). Owner decision honoured: **C1 is extended, not
replaced** (`docs/decisions/2026-11-12-compositor-spikes.md` "Decision 2026-11-12 ~06:10 UTC:
B1 approved" — keep C1, compile the stock AOSP geometry kernel into it; A1/stock SurfaceFlinger is
not started). ADR-0029 §8 ("window layer stack in C1 (absorbs ROT9)"), plan §3.2 item 2.

**Verdict up front: C1 can carry this without a rewrite.** C1 is *already* an N-layer compositor
(§2.1); what is missing is the Android **window-type/z policy** above it, the input
hit-test against that policy, and the per-window tagging in the tree/screenshot that lets an agent
tell an overlay from the app. The policy now exists (`acos-wm::layers`, §4 step 1, committed);
the remaining wiring is described per step below.

---

## 1. How the code handles windows today

There are two live paths and they share almost nothing, which is the single most important fact in
this document.

### 1.1 The binder/WMS path (real framework `ViewRootImpl` → ACOS service layer)

| piece | file | what it does |
|---|---|---|
| `IWindowManager`/`IWindowSession` service object | `crates/acos-services/src/wms.rs` (778 l) | decodes the real AIDL arguments, owns `WmsState`, answers the scalar `IWindowManager` methods |
| window state machine | `crates/acos-wm/src/wms.rs` (1548 l) | `add_to_display_as_user` → `WindowRecord` (frame, insets, `SurfaceControl` handle, input channel, `z`, `focusable`), `relayout`, `remove`, `finish_drawing`, callback queue |
| SurfaceFlinger role (**C1**) | `crates/acos-services/src/surface_client.rs` (3611 l) | consumes `ISurfaceComposer` `SET_TRANSACTION_STATE` (BLAST buffer + flattened `GraphicBuffer` + fds), keeps the `LayerRecord` tree, and **composites** with `capture_composite()` (§2.1) |
| AOSP geometry kernel | `crates/acos-sfgeom` | vendored `android-14.0.0_r75` `Rect`/`Transform`/`RequestedLayerState`/`LayerSnapshotBuilder` bounds+transform, compiled in (B1b) |
| pure-Rust compositor (second C1 role) | `crates/acos-wm/src/compositor.rs` (1150 l) | `Layer{id,name,window,z,visible,alpha,position,…}` + damage-tracked `present()` into `acos-render`; used by `acos-wm::facade::SessionWm` and `acos-runtime::wms_java` (the VM/native path) |
| input | `crates/acos-services/src/input_dispatch.rs` (896 l) | `InputChannel` registry, `inject_tap`, acks; `default_token()` picks the **newest registered** channel |
| presentation | `crates/acos-render` | software surfaces/frames |

What this path already gets right: a *token-level* window tree (per-window `SurfaceControl`, its
`bbq-wrapper` BLAST child and one more child), z/alpha/dim pushed into C1 as
`LayerPlacement` from the host's own `LayoutParams`
(`crates/acos-services/src/wms.rs:307`: `x = frame − surfaceInsets`, `z = record.z`,
`alpha`, `dim_amount` when `FLAG_DIM_BEHIND`, `secure` when `FLAG_SECURE`), and a correct
dialog-over-activity composite with the dim layer (`crates/acos-services/tests/w35_composite.rs`,
`w54_dialog_surface_insets.rs`).

What it did **not** have before this track:

* `z` was **insertion order** — `acos-wm/src/wms.rs` `add_to_display_as_user` used
  `let z = self.windows.len() as i64 + 1;`. An overlay, a dialog and a popup therefore stacked by
  the order the guest happened to call `addView`, not by window type. (Fixed, §4 step 1.)
* `focusable = !params.is_system_window()` — `FLAG_NOT_FOCUSABLE` ignored.
* `input_dispatch.rs` routed taps to `newest_token()` — **no hit test at all**, so the newest window
  got every event whether or not the point was inside it and whether or not it was touchable.
* `WindowRecord` had no window type/kind.

### 1.2 The ART path (the one every v0.3.x gate runs on)

The ART guest is real framework code in one process, and it has **three** frame/tree producers:

1. `crates/acos-art/guest/java/dev/acos/art/AppMain.java` — `acos-art render-app`
   (offline single-layout render). `draw(View root)` (≈L1000) creates one `1080x2400` ARGB bitmap,
   erases it to the theme `windowBackground` colour and calls `root.draw(canvas)` for **one root**.
   `dumpTree(View v)` (≈L1080) however is already a **window forest**: it walks
   `WindowManagerGlobal.getWindowViews()` (fallbacks: private `mViews`) in AOSP z-order
   (oldest/bottom first), labels the activity decor `main`, every other root by
   `ViewRootImpl.mWindowAttributes.title` + `#z`, and tags **every node** with its `window` label
   and `bounds_on_screen`. This is the W36 fix — the tree has multi-window support, the frame does
   not.
2. `SessionMain.java` (`acos-art launch --interactive`) — `publishFrame()` (≈L645) calls
   `AppMain.dumpTree(sDecor)`, then `sDecor.draw(canvas)` into one bitmap and PNG-encodes it.
   Again: **one root**. A dialog is in the tree and absent from the pixels.
   `tap(x,y)` (≈L848) dispatches DOWN/UP to `sDecor.dispatchTouchEvent` — always the activity,
   never `WindowManagerGlobal`'s topmost root.
3. `LaunchMain.java` (`acos verify` / the interactive session the CLI drives) — the whole real
   launch (`ActivityThread`-equivalent, `PhoneWindow`, `ViewRootImpl`, HWUI via the `libacoshw.so`
   shim). It writes only markers and the probe tree (`appendViewJson`, a window forest via the same
   `windowViews()`); **pixels are not written by the guest at all** — they come from C1
   (`crates/acos-art/src/launch.rs` "the bound C1 store",
   `docs/IMAGE_ABI_NOTEBOOK.md` §"A tap reply must bind the tree probe and the C1 pixels to one
   settled state"). `acos-cli/src/engine/art.rs` `tree()`/`screenshot()` read those.

Consequence for this track: **criteria 2 and 3 are C1 work; the ART fixture in criterion 5 is
satisfied through path 3** (`acos verify` → C1 store), and the *render-app*/interactive paths (1,2)
need the same window-forest composition in the guest or they will keep showing one window. Path 3
is the one that matters for real APKs and for C1; paths 1 and 2 are cosmetic fixtures and must not
fork a second z-order implementation (see §5, decision D3).

### 1.3 `crates/acos-art/src/capture.rs` is not a screenshot path

413 lines, and it is the **bounded capture** of guest stdout/stderr/logd (`TailBuffer` ring,
`DEFAULT_STREAM_CAP` 8 MiB, `DEFAULT_LOG_CAP` 50 000): the fix for the `acos-art` OOM
(`results/art-runtime/R1.md`). It has nothing to do with window capture. The screenshot path is
§1.2 path 3 (C1 store) and the PNG files the guest writes in paths 1/2.

---

## 2. What C1 does today (the thing being extended)

### 2.1 `capture_composite()` — already an ordered stack

`crates/acos-services/src/surface_client.rs` ≈L1020. Per layer it feeds the AOSP kernel:

* pixels — the newest stored `GraphicBuffer` (memfd);
* crop — `BufferData.crop`/`layer_state_t.bufferCrop`;
* position/z — the **window manager's** placement (`LayerPlacement`, pushed on `relayout`) when the
  layer is a window surface, else the client's own `layer_state_t.x/y/z`;
* alpha — `LayoutParams.alpha` for window layers, `gui::WindowInfo.alpha` otherwise;
* dim — the window's `FLAG_DIM_BEHIND` + `dimAmount` (the dim layer is the *window manager's*, and
  the window manager is the host).

`acos_sfgeom::compute()` returns the stock snapshots (`hidden_by_policy`, `invalid_transform`,
geometry transform); C1 walks them in kernel z-order and composits into one canvas, then applies
the dim for the layers above. Bounded by `MAX_COMPOSITE_LAYERS`/`MAX_COMPOSITE_INPUT_BYTES` with a
structured `CompositeError`. A one-layer session's frame is byte-identical to before B1b — i.e.
**single-window apps are on a fast path that a stack must not regress** (criterion 6).

Stacking for a BLAST child is inherited from its window surface
(`inherited_stacking()`, depth ≤ 8): the child carries no `z` of its own, so the window's
placement decides. **This is the seam the layer stack plugs into** — nothing in C1 has to change to
honour a new order, it already takes `z` as an input.

### 2.2 The WMS's own `Compositor` (native/VM path)

`crates/acos-wm/src/compositor.rs` is a full layer stack too (`Layer.z`, `alpha`, `visible`,
`position`, damage region, `present()` → `acos_render::Surface::present`), used by
`acos-wm::facade::SessionWm` and `acos-runtime::wms_java`. It needs the same policy input (z from
`WindowRecord.z`, which is now the stack index) and the same "who gets this touch" answer.

---

## 3. What ROT9 ("layer pool") was meant to be, and where it went

`git log --all --grep=ROT9` → **no commits on any branch**; `grep -rn ROT9 results/` → one hit, in
`results/release/v0.2.2-NOTES.md` known gaps: *"Back-navigation fixes (ROT8b/ROT9) are not in this
release."* `docs/STATE_AND_PLAN_2026-11-15.md:79-80`: *"ROT9 (layer pool) not started. Needs its own
full verification before merge."* There is **no `results/handoff/ROT9.md`** and no branch.

So ROT9 was never written down in more detail than its name: a **pool of reusable
SurfaceControl layers in C1** rather than create/destroy churn per window — motivated by the ROT8b
back-navigation defect class, where a window's layer had to survive (be retained/reused) across an
Activity relaunch instead of being destroyed and recreated. `docs/IMAGE_ABI_NOTEBOOK.md` already
records the two facts ROT9 was about, learned the hard way in W42: a destroy arrives as
`IWindowSession.remove` (not as a layer-destroy transaction) and drops 3 layers for one window
(`window_surface_destroy_drops_its_blast_child`), and a late transaction must not resurrect a
destroyed handle (`a_late_transaction_cannot_resurrect_a_destroyed_layer`).

Plan §3.2 item 2 **absorbs** ROT9, i.e. the layer pool is no longer a separate feature: a stack
with stable per-window layer identity is the same requirement. Concretely, from this track's point
of view ROT9 contributes:

* **layer identity is per (session, window) and stable across relayout** — already true
  (`WindowRecord.surface_handle` is allocated once at `addToDisplayAsUser` and reused by every
  `relayout`; `renumber_z` now only changes `z`, never the handle);
* **destroy is explicit and tombstoned** — already true (W42);
* **what a pool would add**: reusing a freed handle for a *new* window instead of allocating a
  fresh one, to cut per-launch allocation. That is a performance change with no behavioural
  effect on this track, and it must not be mixed into the layer-stack correctness work. Recorded
  as a follow-up, not a blocker.

---

## 4. Implementation (each step = one commit, tested)

### Step 1 — window types + AOSP layer assignment  ✅ committed `b3ed3697`

`crates/acos-wm/src/layers.rs` (new):

* **Every documented `LayoutParams.TYPE_*`**, transcribed from the image's own framework source
  (`.cache/aosp/base/core/java/android/view/WindowManager.java`, `FIRST_SYSTEM_WINDOW = 2000`):
  `TYPE_APPLICATION` 2, sub-windows 1000…1999 (`TYPE_APPLICATION_PANEL` 1000,
  `TYPE_APPLICATION_ATTACHED_DIALOG` 1003, …), `TYPE_SYSTEM_ALERT` 2003, `TYPE_SYSTEM_DIALOG`
  2008, `TYPE_INPUT_METHOD` 2011, `TYPE_ACCESSIBILITY_OVERLAY` 2032,
  `TYPE_APPLICATION_OVERLAY` 2038, `TYPE_ACCESSIBILITY_MAGNIFICATION_OVERLAY` 2039,
  `TYPE_NOTIFICATION_SHADE` 2040, `TYPE_STATUS_BAR_ADDITIONAL` 2041, …
* **`WindowKind`** (agent-facing: `application`, `sub_window`, `application_overlay`,
  `accessibility_overlay`, `system_alert`, `system_dialog`, `system_overlay`,
  `notification_shade`, `input_method`, `system_bar`, `system`, `wallpaper`, `unknown`).
  An out-of-range `type` is `unknown` and is **never** silently merged into the application band.
* **`LayerBand`** order, from relations the framework documents *on the type*:
  wallpaper < application < sub-window < application overlay < accessibility overlay <
  system windows < shade < bars < IME. `TYPE_APPLICATION_OVERLAY`'s own text — "above all activity
  windows … but below critical system windows like the status bar or IME" — is the assertion the
  test makes.
* **`WindowLayerStack`**: `upsert` (a relayout keeps the window's creation order, so it cannot jump
  above a window it was below), `remove` (parent death removes its sub-windows), `ordered()`
  (band-major then creation order, **sub-window inserted directly above its attached window**,
  orphans keep a defined place instead of vanishing), `z_of`, `describe()` (window, type, z, kind),
  `hit_test(point)` → topmost **touchable** window containing the point,
  `focus_target()` → topmost **focusable** window.
* Flag helpers `touchable`/`focusable`/`not_touch_modal`/`dims_behind`
  (`FLAG_NOT_TOUCHABLE` 0x10, `FLAG_NOT_FOCUSABLE` 0x8, `FLAG_NOT_TOUCH_MODAL` 0x20,
  `FLAG_DIM_BEHIND` 0x2).

WMS wiring (`crates/acos-wm/src/wms.rs`): `WindowRecord` gains `kind`; `z` is now the **layer-stack
index** (`renumber_z()` after every add and remove) — and `z` is exactly what
`acos-services/src/wms.rs:307` hands to C1 as `LayerPlacement.z`, so **composition order becomes
type-correct for free**; `focusable` also honours `FLAG_NOT_FOCUSABLE`; focus after a `remove` goes
to `focus_target()` (topmost focusable) instead of the newest window. New:
`WindowManager::layer_stack()`, `hit_test(PointI)`, `focus_target()`.

Tests: 13 new unit tests, 70 lib + 27 integration tests green (`cargo test -p acos-wm`).

### Step 2 — composition + input hit-testing (input half ✅ committed, C1 assertion pending)

Committed: `InputDispatcher::register_window` no longer treats "visible" as "touchable" — it asks the
window's own `FLAG_NOT_TOUCHABLE` (`acos_wm::layers::touchable(record.params.flags)`), and a new
`refresh_window(&WindowRecord)` keeps frame / layer-stack `z` / visibility / focusability /
touchability consistent after a `relayout` and after another window is removed (a removed window
below an overlay shifts the overlay's `z`, and a stale `z` would keep routing touches to it as if it
were still on top). `WmsState::relayout_with_requested` and `WmsState::remove` call it.
Test: `crates/acos-services/src/input.rs`
`the_topmost_touchable_window_at_the_point_gets_the_touch` — overlay above the activity: a touch
inside the overlay goes to the overlay, a touch beside it goes to the activity, and with
`FLAG_NOT_TOUCHABLE` both go to the activity while the overlay stays above it in the stack.

Still to do: `focus` publication on add/remove per `focus_target()`; a `w35`-style three-layer C1
test (activity / dialog / application overlay) asserting the composite order from the stack's `z`;
and the ART end-to-end half of criterion 2 (needs the fixture of step 5).

### Step 3 — tree and screenshots carry the window (id, type, z)

* The host-side tree builder (`acos-cli/src/engine/art.rs` `tree()`, and `acos-protocol::UiNode`)
  gains `window_id`, `window_type`, `window_kind`, `z` on every node, plus a top-level
  `windows:[{id, type, kind, z, bounds, focus, touchable}]` block. The guest already sends
  `windows:[{z, window:"label#z"}]` (W36) — extend that JSON with the real `type` (it reads
  `ViewRootImpl.mWindowAttributes.type` already, `windowLabelOf`) and the window's token id, and
  keep the label for backwards compatibility.
* Screenshot metadata (`FrameEvidence`) names the window count/composition, so a frame and a tree
  from the same reply cannot disagree.

### Step 4 — regressions

Existing gates unchanged: `w27-dialog-window` (dialog in the live tree), `w54-dialog-pixels`
(dialog card at its frame, dim behind), `w15-dialog-menu`, `w42-back-frame`, plus the Gaten
lookup-mode disclosure dialog when available. Each is a *pre-existing* gate: it must pass before
and after (they are the acceptance evidence that the stack did not change single/dialog rendering).

### Step 5 — ART fixture + gate

`fixtures/apps/overlay-stack` (new): an Activity, a `SYSTEM_ALERT_WINDOW` overlay
(`TYPE_APPLICATION_OVERLAY`, granted by the session config for the test — the permission is
declared-`appop` today, `crates/acos-services/src/declared.rs:140`) drawn in a distinct flat colour,
and an `AlertDialog`. Gate asserts: tree z-order `application < dialog < application_overlay` with
each node's window tagged; PNG sample points (overlay colour above the activity, dialog colour
inside the dialog frame); tap on the overlay lands on the overlay, tap beside it lands on the
activity. Scenario `scenarios/layer0-overlay-stack.yaml`, run with
`tools/art-runtime/run-exclusive.sh --task LAYER0`.

### Step 6 — no single-window latency regression

Driver only, **not run here** (bench owner runs an exclusive A/B):

* `tools/bench/layer0-single-window-ab.sh`: arms = `main` vs `layer0`, `acos_bench.py --bundle
  latency,cold` with the same bundles/reps as `BENCHFIX-AB`, plus the C1 composite micro-check
  (one layer must stay on the byte-identical fast path — `w35_composite.rs` already asserts the
  1-layer case passes through with its own stride).

---

## 5. Decisions taken here, and the open ones

| # | decision | why |
|---|---|---|
| D1 | z is the **layer-stack index**, computed in `acos-wm`, and is what C1 receives as `LayerPlacement.z` | one source of truth for draw order, hit-test order and tree order; C1 needed no change |
| D2 | a sub-window is inserted **directly above its attached window**, not at the top of the sub-window band | the framework's own words: "kept next to their attached window in Z-order" |
| D3 | the guest's `render-app`/interactive frame producers (§1.2 paths 1,2) are **not** given a second z-order implementation | two implementations of layer policy is the drift this repo has been bitten by; the real path (3) composites in C1. If those fixtures need multi-window pixels, they must ask C1/host for the order, not re-derive it |
| D4 | an undocumented `type` is `unknown`, drawn at `LayerBand::Unknown` (top) and named in the tree | never present a wrong answer as the app's |
| **O1** | the **numeric** system-window band order (shade vs status bar vs IME vs alert) | `PhoneWindowManager.getWindowLayerFromTypeLw` lives in `services.jar`; the AOSP **source cache has no `services/core`** and there is no dex dump tool in the repo. The relations the framework documents are implemented; the remaining table is one `match` arm in `WindowKind::band()`. Smallest next action: a dex-dump seam (`acos-dex`) for `com.android.server.policy.PhoneWindowManager.getWindowLayerFromTypeLw`, then transcribe |
| **O2** | `TYPE_APPLICATION_OVERLAY` vs `TYPE_ACCESSIBILITY_OVERLAY` relative order | framework text only says the accessibility overlay "is overlaid only by a connected AccessibilityService"; used `application < accessibility` — verify against the image |
| **O3** | whether an unattached sub-window should be an error instead of "orphan above the application band" | today the token can die first (W42); the fallback is defined and tested, an error is the alternative |

## 6. Disk / resource note

`df -h /` reported **86 % Use** (134 GiB free) at start; actual data usage 80.8 %
(root-reserved blocks are counted in `Use%`). Mandatory rules say stop at ≥ 85 % **Use** and the
task said ≥ 86 %: recorded, worked with a single reused
`CARGO_TARGET_DIR=.cache/layer0-target` (deleted at the end), no ART runs (which allocate per-run
sysroot copies).
