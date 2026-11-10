# `acos-wm` — window manager, compositor, input dispatcher, IME, vsync

This directory holds the acceptance evidence for `crates/acos-wm`, the
VM-independent Rust replacement for `system_server`'s WindowManagerService,
SurfaceFlinger, input dispatcher, IME service and Choreographer vsync source
(BACKLOG tasks **T-28/T-29/T-30/T-34/T-36/T-37/T-38**, VM-independent halves).

- Source: `crates/acos-wm/`
- Artifacts: this directory
  - `parcelable_layouts.txt` — DEX-decoded field order for every WMS codec
  - `replay_coverage.txt` — per-row replay table for both measured traces
  - `bench.txt` — p50/p95 and idle-CPU measurement
  - `test-output.txt`, `clippy-output.txt` — exact acceptance output

```text
$ cargo test -p acos-wm
  44 (lib) + 10 (codec_golden) + 4 (compositor) + 2 (fuzz)
+  6 (input) + 2 (multi_session) + 3 (replay) + 4 (vsync)
= 75 tests, 0 failed
  (the 44 lib tests include `damage_matches_full`, a 600-case proptest)

$ cargo test -p acos-render
  63 (lib) + 14 (contract) + 2 (golden) + 1 (hello_views_metrics)
+  4 (present_damage: hash equality + immutability)
= 84 tests, 0 failed
  (`present_damage` is a 600-case proptest)

$ cargo clippy -p acos-wm -p acos-render --all-targets -- -D warnings
=> clean

$ cargo run -p acos-wm --example replay_report
rows=109 ok=43 error=0 not-owned=57 transport=9

$ cargo bench -p acos-wm
| operation | p50 (ns) | p95 (ns) |
|---|---:|---:|
| legacy compose+to_pixmap, 1 opaque layer | 49 767 019 | 50 461 404 |
| compose, 1 opaque layer (full damage) | 203 957 | 247 339 |
| present, 1 opaque layer (full damage) | 749 940 | 952 473 |
| compose, 1 opaque layer (200×100 damage) | 1 533 | 1 583 |
| present, 1 opaque layer (200×100 damage) | 19 928 | 22 403 |
| compose, app + translucent IME (full) | 384 458 | 439 682 |
| present, app + translucent IME (full) | 923 869 | 1 075 977 |
| compose, app + IME (200×100 damage) | 1 593 | 1 624 |
| present, app + IME (200×100 damage) | 20 018 | 22 372 |
| present, no change (skipped) | 20 | 20 |
idle 5 s: timer arms 0 (see `bench.txt`)
```

## Compositor performance (follow-up)

The compositor was the 50 ms hot path: `compose` allocated a fresh full-frame
`Bitmap`, erased it, blitted every layer pixel-by-pixel through
`get_pixel`/`set_pixel` float blending, then copied the whole thing again in
`Bitmap::to_pixmap`. It now:

- renders directly into the surface's working pixmap (no per-present
  allocation, no `to_pixmap` copy);
- tracks a **damage rect** per mutation (old ∪ new coverage) and re-composites
  only that region — the full frame only on first present / `invalidate`;
- has an **opaque row-memcpy** fast path and an integer premultiplied
  `source-over` path, both row-wise over raw bytes with the damage clip applied;
- **skips** invisible/zero-alpha layers and layers fully covered by an opaque
  layer above them.

The damage path is proven **pixel-exact** against a naive full composite by the
600-case `damage_matches_full` proptest and the `damage_tracking_keeps_*`,
`removed_layer_area_is_repainted` and `negative_position_and_crop_stay_in_bounds`
tests (overlapping translucent layers, crop, alpha, negative positions,
create/remove).

| target (release, p50) | before | after | verdict |
|---|---:|---:|---|
| 1 opaque full-screen layer, first present | 49.3 ms | **0.75 ms** | ≤ 3 ms ✅ |
| 1 opaque layer, repeat present, 200×100 damage | 49.3 ms | **0.020 ms** (compose **1.5 µs**) | ≤ 0.2 ms ✅ |
| no-change present | ~0 | **20 ns** | ~0 ✅ |
| app + translucent IME, full present | 51.5 ms | **0.94 ms** | ≤ 4 ms ✅ |
| app + IME, repeat present, 200×100 damage | 51.5 ms | **0.020 ms** (compose **1.6 µs**) | ≤ 0.5 ms ✅ |

The repeat-present target was blocked by `acos_render::Surface::present`,
which re-hashed the **whole** 1080×2400 buffer (`xxh3_128`) and cloned it
(`Arc::new(pixmap.clone())`) on every frame. `Surface::present_damage` now
re-hashes only the rows that intersect the caller's damage and re-folds a
per-row xxh3 Merkle root (so the reported `content_hash` is still over the
whole buffer), and double-buffers the pixels so a previous `Frame` stays
immutable without a full clone. The compositor's own damage-limited work is
**1.5 µs**; the end-to-end repeat present dropped from **0.385 ms to
0.020 ms** (≈20×), while a full present still reports its honest full-buffer
cost (≈0.42 ms at the `Surface` level, 0.75 ms end-to-end including the full
composite).

After the fix the full end-to-end composite (0.75 ms) is well below
`acos-render`'s own ~4–5 ms hello-views render, so the compositor no longer
moves the fresh-screenshot metric.

One deliberate per-commit cost: `set_buffer` scans an alpha-declared buffer once
to learn whether it is fully opaque (`detect_opaque`), which is what enables the
memcpy fast path. It short-circuits on the first transparent pixel and costs
~0.54 ms for a fully-opaque 1080×2400 Rgba8888 frame (bench rows *commit … no-alpha
declared* vs *… Rgba8888*); a transaction `opaque` hint would remove it.

## Design

Pure Rust, event-driven, no threads and no periodic timers. The crate is
`#![forbid(unsafe_code)]` **everywhere**: even the `AF_UNIX`/`SOCK_SEQPACKET`
channel and the clock seam use the safe `rustix` wrappers, so the
"minimal fd/epoll exception" the brief allowed was not needed.

```text
display.rs    the real 1080x2400@420 geometry + the ten reference InsetsSources
codec.rs      typed Parcel codecs whose field order is the DEX writeToParcel order
wms.rs        IWindowSession/IWindowManager window state machine + frame/insets math
callbacks.rs  the outgoing IWindow callback queue (resized, insetsControlChanged, …)
compositor.rs SurfaceControl layer tree -> damage-tracked compose -> Surface::present()
input.rs      InputDispatcher: hit-testing, injection, finishInputEvent acks, ANR
ime.rs        IInputMethodManager + IRemoteInputConnection + InputConnection trait
vsync.rs      DisplayEventReceiver/Choreographer generator (boot-anchored 60 Hz grid)
facade.rs     SessionWm: the one object the native layer and agent API use
replay.rs     drives the measured binder_calls_{launch,interaction}.tsv
```

**No wakeups when idle.** The vsync generator arms a deadline only while a
client has an outstanding `scheduleVsync`; the input dispatcher arms an ANR
deadline only while an event is unacked. Every arm/disarm goes through the
`Clock` seam, so the counting test (`clock.arm_count() == 0`) and the 5-second
`/proc/self/stat` bench both prove the resting state is timer-free.

**Damage-tracked compositing.** The compositor owns one persistent output
buffer (the surface pixmap) and writes into it in place. Each layer mutation
records the union of the layer's old and new display-space coverage in a
pending damage rect; `present` re-composites only that region from the
background plus every layer clipped to it. Because compositing is per-pixel
local, this is provably pixel-identical to a full composite — the exact property
`damage_matches_full` (600 cases) checks after every random mutation.

## Public API (native layer + agent)

```rust
// one composed stack per session
SessionWm::new(display) / SessionWm::with_clock(display, Arc<dyn Clock>)
  .add_window(IWindow, LayoutParams, requestedVisibleTypes) -> AddWindowResult
  .relayout(window, LayoutParams, syncSeq)                  -> RelayoutResult
  .finish_drawing(window, Bitmap, frameNumber, causeId)     -> Option<u64 /*frameSeq*/>
  .remove_window(window)
  .capture(after_seq) / .capture_fresh(after_seq)           -> FrameView
  .tap(x, y, causeId) / .swipe(...) / .key(code, causeId)   -> InputOutcome
  .type_text(imeClient, text, causeId)                      -> InputOutcome
  .ack(seq)                                                 -> AckRecord (latency)
  .pump_vsync() / .pump_anr()
  .set_ui_tree_provider(Arc<dyn UiTreeProvider>)            // T-40 handoff hook
  .register_input_connection(BinderId, Arc<dyn InputConnection>)
  .bind_text_field(imeClient, connection, EditorInfo)
```

`AddWindowResult` / `RelayoutResult` carry the typed out-params:
`InputChannel`, `InsetsState`, `InsetsSourceControl$Array`, `outFrame` /
`ClientWindowFrames`, `MergedConfiguration`, `SurfaceControl`,
`InsetsSourceControl$Array`, `syncSeqId`.

## Derived real numbers and their source

All numbers below are MEASURED in
`fixtures/reference/hello-views/art-api34-x86_64/display.txt` (the ART emulator
capture), not chosen:

| value | number | source line |
|---|---|---|
| display frame | `[0,0][1080,2400]` | `Display: mDisplayId=0` / `DisplayFrames w=1080 h=2400` |
| app window frame | `[0,128][1080,2337]` | `mAppBounds=Rect(0, 128 - 1080, 2337)` and `nonDecorFrame=[0,128][1080,2337]` |
| status bar inset | top **128** | `type=statusBars frame=[0,0][1080,128]` |
| navigation bar inset | bottom **63** | `type=navigationBars frame=[0,2337][1080,2400]` |
| IME (hidden, visible frame) | `[0,2274][1080,2400]`, height **126** | `type=ime … visibleFrame=[0,2274][1080,2400]` |
| display cutout | `[0,0][1080,128]` | `type=displayCutout frame=[0,0][1080,128]` |
| mandatory gestures | top 160, bottom 84 | `mandatorySystemGestures` `[0,0][1080,160]`, `[0,2316][1080,2400]` |
| system gestures | left 78, right 78 | `systemGestures` `[0,0][78,2400]`, `[1002,0][1080,2400]` |
| density | 420 dpi / 2.625 | `420dpi`, `device-config.json` |
| refresh | 60.000004 Hz, interval 16 666 667 ns | `display.txt` / mode |
| InsetsSourceControl positions | ime `(0,128)` hint bottom 883; status `(0,0)` hint top 128; nav `(0,2274)` hint bottom 63 | `Control map:` block |
| rounded corners | 4× radius 28 | `mRoundedCorners` |
| privacy indicator bounds | `[827,0][1043,128]`, rotation 0 | `mPrivacyIndicatorBounds` |
| cutout path | `M 507,64 a 33,33 0 1 0 66,0 33,33 0 1 0 -66,0 Z @left` | `mDisplayCutout` |

The frame computation honours fixed sizes and otherwise returns the app bounds;
`relayout` returns `RELAYOUT_RES_FIRST_TIME` on the first call only, and the
same stable `SurfaceControl` handle afterwards.

### Codec field order

`src/codec.rs` transcribes `writeToParcel` for `Rect`, `Point`, `Insets`,
`LocaleList`, `TextUtils` (CharSequence), `WindowConfiguration`,
`Configuration`, `MergedConfiguration`, `ClientWindowFrames`, `SurfaceControl`,
`DisplayCutout$ParcelableWrapper`, `CutoutPathParserInfo`, `RoundedCorner`,
`RoundedCorners`, `PrivacyIndicatorBounds`, `DisplayShape`, `InsetsSource`,
`InsetsSourceControl`, `InsetsSourceControl$Array`, `InsetsState`,
`InsetsFrameProvider`, `WindowManager$LayoutParams`, `InputChannel`, `EditorInfo`,
`InputBindResult` and `InputConnectionCommandHeader`. Regenerate the evidence:

```text
cargo run -p acos-wm --example derive_layouts   # -> results/wm/parcelable_layouts.txt
```

## Replay coverage

`results/wm/replay_coverage.txt` is the full per-row table. Summary over the 109
non-empty rows of `binder_calls_launch.tsv` + `binder_calls_interaction.tsv`:

| status | rows | meaning |
|---|---:|---|
| `ok` | 43 | WM-owned method driven through `SessionWm`, typed reply/delivery |
| `error` | 0 | — |
| `not-owned` | 57 | interface belongs to another ACOS component (PMS/ATMS/display/…) |
| `transport` | 9 | `onTransact` envelopes, not methods |

Every WM-owned, non-transport row replays `ok` (asserted by `tests/replay.rs`):

- **IWindowManager**: `openSession`, `useBLAST`, `getCurrentAnimatorScale`,
  `hasNavigationBar`, `isInTouchMode`, `setInTouchMode`
- **IWindowSession**: `addToDisplayAsUser`, `relayout`, `finishDrawing`,
  `setOnBackInvokedCallbackInfo`, `updateRequestedVisibleTypes`,
  `onRectangleOnScreenRequested`, `remove`
- **IWindow** callbacks: `dispatchAppVisibility`, `windowFocusChanged`,
  `insetsControlChanged`, `resized`, `showInsets`, `hideInsets`,
  `executeCommand`, `closeSystemDialogs`
- **IME**: `addClient`, `startInputOrWindowGainedFocus`, `show/hideSoftInput`,
  `isImeTraceEnabled`, `getImeTrackerService`, `reportPerceptibleAsync`,
  `removeImeSurfaceFromWindowAsync`, `IImeTracker.onRequest{Show,Hide}` /
  `onProgress` / `onShown` / `onHidden`, `IInputMethodSession.viewClicked` /
  `updateSelection`, `IInputMethodClient.setActive` / `onBindMethod` /
  `reportFullscreenMode`, `IRemoteInputConnection.{commitText,
  getSurroundingText, setComposingRegion, clearMetaKeyStates,
  finishComposingText, getTextBeforeCursor, deleteSurroundingText}`

The relayout reply is decoded back through the derived layout in
`replay::decode_relayout_reply` (result + `ClientWindowFrames` +
`MergedConfiguration` + `SurfaceControl` + `InsetsState` + controls + bundle
marker).

## Gaps and declared divergences

1. **`SurfaceControl.nativeWriteToParcel`** is a libgui native, not DEX bytecode.
   The Java prefix (`String8 name`, width, height, non-null marker) is derived;
   ACOS substitutes an 8-byte layer handle for the native pointer and the ACOS
   native layer implements the matching `nativeReadFromParcel`.
2. **`InputChannel.nativeWriteToParcel`** is likewise native. The ACOS layout is
   `present`, `String8 name`, `int32 channelHandle`, token binder; the native
   layer resolves the handle to the real `SOCK_SEQPACKET` fd held by
   `transport::InputChannelRegistry`.
3. **`IWindow.windowFocusChanged`** is *not* in the API-34 derived `IWindow`
   table (focus is folded into `resized`/visibility). ACOS keeps it as an
   explicit code-17 extension; the replay sees it only via the services list,
   not the trace.
4. **`InsetsState.InternalInsetsType` constants** are inlined by `d8` and are not
   recoverable from the DEX field table; the values in `codec::inset_type` are
   the documented public constants (INFERRED).
5. **`DisplayShape`** is written as absent: the capture exposes only its integer
   hash, not the string spec.
6. **`EditorInfo`** nested `AutofillId`, `Bundle`, `SurroundingText`,
   `targetInputMethodUser` are encoded as null through the documented static
   writers; the manager needs none of them for V1 typing.
7. **IME**: ACOS plays the IME service role in-process (no external
   `InputMethodService`), which is the declared V1 divergence. When no
   `InputConnection` is registered, `startInputOrWindowGainedFocus` returns
   `NO_IME`. `showSoftInput` still answers honestly.
8. **`relayout` out-`Bundle`** is written as null (marker `0`); the framework
   reader accepts a null bundle.
9. **Layer transform hints** (`setTransform`) are stored on the layer and in
   the typed transaction, but the V1 software composite rasterises the layer
   axis-aligned (identity); rotation/crop state is recorded for the native layer
   and the frame-timeline.
10. **`acos-render` damage-aware commit — implemented.** The repeat-present
    target (≤ 0.2 ms; § Compositor performance) was blocked by
    `Surface::present`, which re-hashed the full 1080×2400 buffer and cloned it
    on every frame. `acos-render` now provides:

    ```rust
    // acos_render::Surface
    pub fn present_damage(&mut self, cause_id: i64, damage: Option<Rect>,
                          unsupported_ops: Vec<String>) -> u64;
    pub fn pixmap(&self) -> &Pixmap;
    pub fn pixmap_mut(&mut self) -> &mut Pixmap;
    ```

    `present()` is kept and is exactly `present_damage(cause_id, None, ops)`.
    `damage == None` means "unknown / whole frame" and pays the honest
    full-buffer cost; `Some(rect)` re-hashes only the rows intersecting `rect`.
    The full-buffer `content_hash` is a Merkle fold — per-row xxh3-128, then a
    streaming xxh3-128 over the row digests — so a damage-limited present
    reports exactly the same hash as a full present of the same pixels (proven
    by the 600-case `present_damage_hash_equals_full_present` proptest in
    `crates/acos-render/tests/present_damage.rs`). `seq`, `cause_id`, `class`,
    timestamps, `unsupported_ops` and the `NO_FRESH_FRAME` semantics are
    unchanged.

    Previous frames stay immutable without a per-frame full clone: the surface
    double-buffers, moving the just-presented pixels into the frame's
    `Arc<Pixmap>` and working in the other buffer, which is synced over the
    damage region. The other buffer is the previous frame's buffer reclaimed
    with `Arc::try_unwrap`; if a handed-out frame still holds it, it is copied
    first (Arc copy-on-write). In the steady state where the caller does not
    retain frames across presents, no full clone is paid. Measured at the
    `Surface` level (release, 1080×2400, p50 over 60 runs): repeat present with
    a 200×100 damage rect **0.018 ms** (was 0.39 ms), full present **0.416 ms**
    (honest, unchanged work).
11. The **VM-dependent halves** of T-37/T-38 (delivering the callback queue
   through the real looper, and the framework's own `IRemoteInputConnection`
   proxy) are out of scope: this crate produces the typed queue and the
   `InputConnection` trait the native layer binds to.

## Tests

`cargo test -p acos-wm` (75 tests):

- `lib` — codec round-trips, wms frame/insets golden values, compositor
  damage-tracking (`damage_matches_full` 600-case proptest +
  move/remove/negative-crop exactness), input routing/ack/ANR, vsync grid, IME
  typing, transport framing, replay.
- `codec_golden.rs` — explicit golden bytes (`Rect`, `Point`, `Insets`,
  `ClientWindowFrames`) + round-trip stability for all parcelables + the
  `parcelable_layouts.txt` coverage check.
- `compositor.rs` — `finishDrawing` → fresh frame with cause, frame_seq
  monotonic, two-window opaque z-order, measured app bounds.
- `input.rs` — hit-test → deliver → ack latency, focus, swipe
  DOWN/MOVE/UP, `type_text` → `InputConnection`, idle no-wakeups, ANR.
- `vsync.rs` — zero idle wakeups, deterministic 60 Hz sequence, multiple
  clients share one deadline, pipe-record shape.
- `multi_session.rs` — two concurrent `SessionWm`s never observe each other's
  events or frames.
- `fuzz.rs` — proptest: 64 random operation sequences never panic and keep the
  vsync/ack invariants.
- `replay.rs` — every WM-owned trace row replays `ok`; AIDL layouts present.
