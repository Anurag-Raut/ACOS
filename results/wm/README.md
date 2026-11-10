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
  39 (lib) + 10 (codec_golden) + 4 (compositor) + 2 (fuzz)
+  6 (input) + 2 (multi_session) + 3 (replay) + 4 (vsync)
= 70 tests, 0 failed

$ cargo clippy -p acos-wm --all-targets -- -D warnings
=> clean

$ cargo run -p acos-wm --example replay_report
rows=109 ok=43 error=0 not-owned=57 transport=9

$ cargo bench -p acos-wm
| operation | p50 (ns) | p95 (ns) | mean (ns) |
|---|---:|---:|---:|
| inject → deliver → ack (ns) | 430.0 | 872.0 | 522.7 |
| relayout compute + reply (ns) | 861.0 | 1252.0 | 831.2 |
| compositor present 1080×2400 (ns) | 50122067.0 | 50477220.0 | 50133653.7 |
idle 5 s: timer arms 0, /proc/self/stat utime delta 0, stime delta 0 (see `bench.txt`)
```

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
compositor.rs SurfaceControl layer tree -> acos_render::Surface::present()
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
10. The **VM-dependent halves** of T-37/T-38 (delivering the callback queue
   through the real looper, and the framework's own `IRemoteInputConnection`
   proxy) are out of scope: this crate produces the typed queue and the
   `InputConnection` trait the native layer binds to.

## Tests

`cargo test -p acos-wm` (70 tests):

- `lib` — codec round-trips, wms frame/insets golden values, compositor damage,
  input routing/ack/ANR, vsync grid, IME typing, transport framing, replay.
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
