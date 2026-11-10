# W130-A — the pointer packet reaches Dart, the tap increments the counter, and the frame matches the phone once the system bars are masked

**Date:** 2026-11-11 (wave W130-A). **Base:** clean worktree of `HEAD`
(`47e1c38`, the W128-A commit plus its gdb breakpoints). Built with
`CARGO_TARGET_DIR=.cache/target-w130a`, `CARGO_PROFILE_DEV_DEBUG=0`,
`tools/guarded.sh`. **Not committed.** Engine offsets are for the pinned
`.cache/flutter-1/lib-x86_64/libflutter.so` (Flutter 3.47 x86_64, `.symtab` +
DWARF). Gate: `cargo test -p acos-jni --test flutter_hello`.

## Result at a glance

| | W128-A | W130-A |
|---|---|---|
| pointer reaches `PlatformDispatcher.onPointerDataPacket` | **no** | **yes** (`ACOS_DART_POINTER n=3`, then `n=2`) |
| tap hits the button | **no** (screen centre, and 90 px low) | **yes** (`pos=Offset(150.0, 240.0)`, `rect=(40,208)-(260,272)`) |
| counter after tap | `0` | **`1`** (`ACOS_DART_COUNTER counter=1`, `ACOS_DART_FRAME draw n=2 counter=1`) |
| frame PNGs | `w128a-frame-0/1.png` (both `counter: 0`) | **`w130a-frame-0.png`, `w130a-frame-tap.png`** (`counter: 0` / `counter: 1`) |
| device density | 2.625 (420 dpi, the ART emulator) | **2.5** (400 dpi, `fixtures/reference-device/device.json`) |
| status-bar padding | 126 px (48dp × 2.625) | **108 px** (measured in the reference PNG) |
| background geometry | 411×914 at y=0 | **432×960 at y=0** (reference: 432 wide) |
| button rect vs reference | ours y 226..289, ref y 208..271 (**18 px off**) | **identical: (40,208)-(259,271) in both** |
| unmasked `global_ssim` vs reference | 0.2929 | 0.3640 |
| **masked** `global_ssim` / `mean_ssim` (frame 0) | not measured | **0.9987 / 0.9999** |
| **masked** `global_ssim` / `mean_ssim` (tap frame) | not measured | **0.9968 / 0.9998** |
| `acos frame-diff --mask` | did not exist | **implemented** (repeatable, rectangular exclusion) |

## 1. Why the pointer never reached Dart

The chain from Java to Dart was traced end to end with
`tools/flutter-engine-gdb.py` (now **47** sites; the eight new pointer sites are
listed below). The first defect was in the **JNI carrier**, the second in the
**harness length argument**.

### 1.1 `GetDirectBufferAddress` returned a synthetic address

`ByteBuffer.allocateDirect` in ACOS is `VMRuntime.newNonMovableArray` +
`VMRuntime.addressOf`, i.e. a **synthetic** address
(`(0x8000_0000 | array_handle) << 32 | offset`) into the Java heap. The JNI
`GetDirectBufferAddress` fallback returned that value verbatim. The engine's
`flutter::DispatchPointerDataPacket` then does:

```
53058f  mov %rax,%r15                 # r15 = GetDirectBufferAddress(buffer)
53059d  movslq %ebp,%rcx              # rcx = position
5305a0  lea (%r15,%rcx,1),%rdx        # end = buffer + position
5305b5  call vector<unsigned char>::__init_with_size(begin=buffer, end, n=position)
```

`__init_with_size` is a `memcpy` out of that address. With the synthetic address
it faulted (`SIGSEGV` in libc) as soon as the length was non-zero. Fix
(`crates/acos-vm/src/natives/native_mem.rs`,
`crates/acos-vm/src/natives/jni_glue.rs`,
`crates/acos-jni/src/jni.rs`): `native_mem::host_pointer` resolves the backing
`byte[]`/`Vec<u8>` to its **real host pointer** (derived safely from the slice,
valid for the duration of the JNI call). JNI-created buffers in the
`direct_buffers` table keep their historic synthetic value, so
`tests/jni_slots.rs::direct_buffer_round_trips_through_native_mem` still passes.

### 1.2 the packet length argument was `0`

`FlutterJNI.dispatchPointerDataPacket(ByteBuffer, int position)` passes
`position` straight to the native side, and the native side treats it as the
**number of valid bytes** (`end = address + position`). The real Android
embedding passes `packet.position()` *after filling the buffer*
(`AndroidTouchProcessor`). W128-A passed `0`, so every packet arrived as an
empty vector and `Engine::DoDispatchPacket` took its `size < 0x120` early exit:

```
before:  CONV_LOOP begin=0 end=0      OUTSIZE_CMP rax=0     (no DartInvoke)
after:   CONV_LOOP begin=0x…5d50 end=0x…5f90 (576 bytes)  OUTSIZE_CMP rax=864
```

`PointerData` is **36 × 8 = 288 bytes** (`0x120`), confirmed independently by
the engine's own `div $0x120` record loop and the `cmp $0x120` output check; the
576-byte two-record packet matches. The harness now leaves `position` at the end
of `buildTap` and passes `packet.position()`.

### 1.3 the tap missed the button

The probe is `dart:ui`-only and does **not** apply a `devicePixelRatio`
transform, so its scene coordinates are the pixel grid: the button is
`(40, padding.top + 100) .. +220×64`. W128-A tapped at
`(WIDTH·DPR/2, (100+32)·DPR)` = (540, 132) in scene terms — 280 px right of the
button and 94 px above it. The tap is now the measured centre
`(150, STATUS_BAR_PX + 100 + 32) = (150, 240)` and the Dart handler divides the
packet by `dpr`, so the carrier value is `scene · dpr`.

### 1.4 the dispatcher is `DefaultPointerDataDispatcher`, not the smooth one

The trace shows `DefaultPointerDataDispatcher::DispatchPacket` →
`Engine::DoDispatchPacket` on the same thread (`tid=2`, the UI thread), before
`VsyncWaiter` is involved. No vsync resampling/frame callback is required for
delivery; the `SmoothPointerDataDispatcher` path is not on this build's code
path. `Shell::OnPlatformViewDispatchPointerDataPacket` is reached on the thread
that calls `FlutterJNI.dispatchPointerDataPacket`, and `DoDispatchPacket`
invokes `tonic::DartInvoke` directly.

Measured chain (`results/flutter/w130a-engine-gdb.log`, 47 sites installed,
byte-verified):

```
PTRJNI 1 tid=2 shell=0x… packet=0x4e08 count=576
PTRBUF 1 tid=2 buf=0x7fffe23e69f0 len=576
PTRSHELL 1 tid=2
DPD_DISPATCH 1 tid=2 ts=0
DODISPATCH 1 tid=2 begin=0x…5d50 end=0x…5f90          # 576 bytes
PTROUTSIZE 1 tid=2 bytes=864 records=3                # converter adds an "add"
PTRDARTINVOKE 1 tid=2 closure=0x7fffe6e63b00
…
PTRJNI 2 tid=2 count=576
PTRBUF 2 tid=2 buf=0x7fffe23fbe40 len=576
PTRSHELL 2 tid=2   DPD_DISPATCH 2   DODISPATCH 2
PTROUTSIZE 2 tid=2 bytes=576 records=2
PTRDARTINVOKE 2 tid=2
```

Dart side (engine log):

```
flutter: ACOS_DART_POINTER_EVENT change=PointerChange.add  pos=Offset(150.0, 240.0) rect=(0,0,0,0)           down=false
flutter: ACOS_DART_POINTER_EVENT change=PointerChange.down pos=Offset(150.0, 240.0) rect=(0,0,0,0)           down=false
flutter: ACOS_DART_POINTER_EVENT change=PointerChange.up   pos=Offset(150.0, 240.0) rect=(0,0,0,0)           down=false
flutter: ACOS_DART_FRAME draw n=1 counter=0 views=1 implicit=0 size=Size(1080.0, 2400.0) dpr=2.5 top=108.0
flutter: ACOS_DART_POINTER_EVENT change=PointerChange.down pos=Offset(150.0, 240.0) rect=(40,208,260,272) down=false
flutter: ACOS_DART_POINTER_EVENT change=PointerChange.up   pos=Offset(150.0, 240.0) rect=(40,208,260,272) down=true
flutter: ACOS_DART_COUNTER counter=1 at=Offset(150.0, 240.0)
flutter: ACOS_DART_FRAME draw n=2 counter=1 views=1 implicit=0 size=Size(1080.0, 2400.0) dpr=2.5 top=108.0
```

(The first down/up pair is the probe's own dispatch from `main()`, which runs
before the first frame has set `_buttonRect`; the harness `tapIncrement` after
frame 1 is the one that hits.)

## 2. `acos frame-diff --mask`

`DiffOptions` gained `mask: Vec<MaskRect>` (`MaskRect = [x0,y0,x1,y1]`,
exclusive). Masked pixels are excluded from every per-pixel statistic, from the
differing-pixel count/bounding box, from the block statistics, and from every
SSIM window that intersects the mask (windowed SSIM skips those windows via a
summed-area table; the single-window `global_ssim` accumulates only unmasked
pixels). New report fields: `masked_pixels`, `compared_pixels`, `mask`.

CLI:

```
acos frame-diff <reference> <candidate> \
  --mask 0,0,1080,108 --mask 0,2304,1080,2400 --json
```

`--mask` is repeatable; `x0>=x1`/`y0>=y1` is rejected at parse time.

## 3. Parity numbers (measured, not asserted from memory)

Reference: `fixtures/reference-device/flutter-hello/screenshot_initial.png`
(moto g35 5G, 1080×2400, **400 dpi → dpr 2.5**, Android 15, font scale 1.0).
Mask used: `[0,0,1080,108]` (system status bar) and `[0,2304,1080,2400]`
(navigation-bar band; it is black-on-black in both, so it does not move the
numbers — included for honesty). `masked_pixels = 220320`,
`compared_pixels = 2371680`.

| candidate | mask | global SSIM | mean SSIM | min SSIM | PSNR | differing | changed bbox |
|---|---|---|---|---|---|---|---|
| `w130a-frame-0.png` (`counter: 0`) | none | 0.3640 | 0.9620 | 0.0000 | 20.56 dB | 4.55 % | `[0,0,1080,248]` |
| `w130a-frame-0.png` | status+nav | **0.9987** | **0.9999** | 0.9447 | **51.01 dB** | **0.054 %** | `[41,135,184,248]` |
| `w130a-frame-tap.png` (`counter: 1`) | none | 0.3630 | 0.9618 | −0.5249 | 20.56 dB | 4.55 % | `[0,0,1080,248]` |
| `w130a-frame-tap.png` | status+nav | **0.9968** | **0.9998** | −0.5249 | **47.53 dB** | **0.057 %** | `[41,135,184,248]` |

Machine-readable copy: `results/flutter/w130a-parity.json` (written by the test).
The tap frame differs from frame 0 by **160 pixels**, bbox `[134,231,146,248]` —
exactly the `0`→`1` glyph inside the button; mean abs error 0.0056.

The unmasked `global_ssim` stays low because the status bar is a full-width band
of ~108 rows (4.5 % of the image) whose luma differs by ~106; that is a band the
ACOS window cannot draw, not a rasterisation defect. Masking it is the honest
comparison.

## 4. Residual deltas after masking (0.05 % of pixels)

`changed bbox = [41,135,184,248]` is **only text**: the "hello acos" title
(rows 135–168) and the button label (rows 231–248). Glyph geometry matches the
phone exactly:

| property | reference | W130-A frame |
|---|---|---|
| white-text bbox (title+label) | `x 41..182, y 135..247` | `x 41..182, y 135..247` |
| button purple bbox | `(40,208)-(259,271)` | `(40,208)-(259,271)` |
| background bbox | `x 0..431, y 108..959` (status bar covers 0..107) | `x 0..431, y 0..959` |
| best integer shift of the title crop | — | **(0,0)**; luma correlation **0.9896** |
| best integer shift of the label crop | — | **(0,0)**; luma correlation **0.9889** |

So there is **no residual position, size, or font-family error**. The remaining
difference is glyph antialiasing coverage: the phone's glyphs are slightly
thinner (frame 0 has 1305 pixels at luma > 200 vs the reference's 1080; mean
luma 60.8 vs 57.0 over the title crop). Both sides are grayscale (no ClearType),
the same Roboto glyphs at the same positions. The most plausible principled
cause is the **font binary/hinting revision**: the engine reads the API 34 boot
tree (`.cache/boot/android-34/fonts/Roboto-Regular.ttf`) while the reference is
an Android 15 (API 35) phone whose Roboto/hinting may differ. There is no
Flutter `Settings` flag for hinting (`hinting-engine` is a Skia/FontConfig
string, not an embedding option), and swapping the boot tree font would be a
separate, evidence-backed change. It is therefore reported, not papered over.

Two *were* fixed as principled causes and account for the 18 px / 21 px layout
error W128-A carried:

1. **density**: `device.json` says 400 dpi → dpr **2.5**, not the ART emulator's
   420 dpi → 2.625. The fixture's `physicalSize / dpr` background is therefore
   432×960, matching the reference's 432-wide background.
2. **status bar**: the reference's system band is rows 0..107, so
   `physicalPaddingTop` is **108 px**, not 48dp × 2.625 = 126. Every vertical
   coordinate (title 135, button 208) now matches to the pixel.

## 5. Reproduce

```bash
# 1. build (clean worktree of HEAD)
export CARGO_TARGET_DIR=$PWD/.cache/target-w130a CARGO_PROFILE_DEV_DEBUG=0
tools/flutter-hello-probe.sh        # rebuild flutter-probe.dex after Java edits
tools/flutter-hello-snapshot.sh     # rebuild libapp_probe.so after Dart edits
tools/guarded.sh --mem 10G --time 1800 -- \
  cargo test -p acos-jni --test flutter_hello -- --nocapture

# 2. the test itself asserts:
#    - ACOS_DART_POINTER n= reaches PlatformDispatcher.onPointerDataPacket
#    - ACOS_DART_COUNTER counter=1
#    - ACOS_DART_FRAME draw n=2 counter=1
#    - masked global SSIM >= 0.98 and mean SSIM >= 0.99 for both frames
#      (fixture-gated: skipped when the reference PNG is absent)
#    - frame-0 vs frame-tap has > 0 differing pixels

# 3. re-measure by hand
ACOS_BIN=$CARGO_TARGET_DIR/debug/acos tools/frame-diff \
  fixtures/reference-device/flutter-hello/screenshot_initial.png \
  results/flutter/w130a-frame-tap.png \
  --mask 0,0,1080,108 --mask 0,2304,1080,2400 --json

# 4. full pointer chain
ACOS_FLUTTER_W93A_CHILD=1 \
ACOS_FLUTTER_W93A_LIBDIR=$PWD/.cache/flutter-1/lib-x86_64 \
ACOS_FLUTTER_W93A_OUT=/tmp/acos-w130a-gdb \
ACOS_FLUTTER_W93A_APK=$PWD/fixtures/build/apps/flutter-hello.apk \
gdb --batch -x tools/flutter-engine-gdb.py -ex run --args \
  $CARGO_TARGET_DIR/debug/deps/flutter_hello-* \
  --exact flutter_child_worker --nocapture
```

## 6. Acceptance (W130-A worktree of `47e1c38`)

| check | result |
|---|---|
| `cargo test -p acos-jni` (all suites, incl. `flutter_hello` 2/2) | **pass** |
| `cargo test -p acos-render` (incl. new `diff::tests::mask_excludes_*`) | **pass** |
| `cargo test -p acos-vm --lib` (121) + `--test native_mem` + `--test render_views` | **pass** |
| `cargo clippy -p acos-render -p acos-cli -p acos-jni -p acos-vm --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `tools/diff-dexprogs.sh --corelib real` | **27 pass / 0 fail / 0 unsupported of 27** |
| engine breakpoints | 47 sites installed, byte-verified; pointer chain traced end to end |
| tap | **counter 0 → 1**, second frame shows `counter: 1` |
| masked parity | **0.9987 / 0.9968 global SSIM**, 0.9999 / 0.9998 mean SSIM |

## 7. Files

* `crates/acos-jni/tests/java/dev/acos/fixtures/flutterhello/FlutterProbe.java` —
  dpr 2.5, `STATUS_BAR_PX = 108`, tap at the measured button centre
  `(150, 240)`, `dispatchPointerDataPacket(packet, packet.position())`.
* `crates/acos-jni/tests/data/flutter_probe_main.dart` — `ACOS_DART_COUNTER`,
  per-event `ACOS_DART_POINTER_EVENT`, `counter`/`top` in the draw line.
* `crates/acos-jni/tests/data/flutter-probe.dex` — regenerated from the Java.
* `crates/acos-jni/tests/flutter_hello.rs` — `w130a-frame-0/tap.png` naming,
  pointer/counter/frame assertions, fixture-gated masked parity check +
  `w130a-parity.json`.
* `crates/acos-vm/src/natives/native_mem.rs` + `jni_glue.rs` — `host_pointer`.
* `crates/acos-jni/src/jni.rs` — `GetDirectBufferAddress` resolves the synthetic
  `addressOf` alias to its host backing store.
* `crates/acos-render/src/diff.rs` — `DiffOptions::mask`, masked per-pixel /
  block / SSIM statistics, `masked_pixels`/`compared_pixels`.
* `crates/acos-cli/src/frame_diff.rs` — repeatable `--mask x0,y0,x1,y1`.
* `tools/flutter-engine-gdb.py` — 8 pointer-path sites (`PTRBUF`,
  `DPD_DISPATCH`, `DODISPATCH`, `PTROUTSIZE`, `PTRDARTINVOKE`,
  `DARTINVOKEFIELD`), 47 total.
* `results/flutter/w130a-frame-0.png`, `w130a-frame-tap.png`,
  `w130a-parity.json`, `w130a-engine-gdb.log`.

---

# W137-A — productisation, part 1: Flutter detection, the semantics decoder, and the pointer packet

**Date:** 2026-11-11 (wave W137-A). **Base:** clean worktree of `HEAD`
(`db844bd`, the W130-A commit). Built with `CARGO_TARGET_DIR=.cache/target-w137a`,
`CARGO_PROFILE_DEV_DEBUG=0`, `tools/guarded.sh`. **Not committed.**

This wave starts turning the W128-A/W130-A **test harness** into a product
surface. The end goal (`acos launch <flutter.apk> --native` → frame in the
FrameStore → semantic tree → `acos tap` → second frame) needs the engine to run
in the confined child and a semantics decoder; this wave lands the
independently-testable half of that and records the exact wire format, so the
next wave can wire the driver without re-deriving it.

## 1. What landed

### 1.1 `acos-jni::flutter` — detection

`acos_jni::flutter::detect(apk)` / `detect_bytes(zip)` recognise a Flutter APK
**structurally** from its zip layout: `lib/<abi>/libflutter.so` (engine),
`lib/<abi>/libapp.so` (AOT snapshot) and `assets/flutter_assets/`. It returns
`FlutterApk { engine_abis, snapshot_abis, has_flutter_assets }`. A plain Android
APK is never misclassified (unit-tested).

`acos inspect` now carries a `flutter` object and a text line:

```
$ acos inspect fixtures/build/apps/flutter-hello.apk
  ...
  flutter: yes (engine abis ["x86_64"])
```

and for a framework APK:

```
$ acos inspect fixtures/build/apps/counter.apk
  ...
  (no flutter line; JSON `flutter.detected = false`)
```

### 1.2 `acos-jni::flutter::decode_semantics_update` — the semantic tree

Flutter has no `View` tree, so the agent tree must come from the engine's
**semantics** stream (`FlutterJNI.setSemanticsEnabled(true)` →
`nativeSetSemanticsEnabled`), delivered to the embedder as a custom
little-endian `ByteBuffer` and normally decoded by
`io.flutter.view.AccessibilityBridge`. That decoder was extracted from the
Flutter 3.47 embedding **inside the fixture APK**
(`dexdump -d classes.dex` → `AccessibilityBridge$SemanticsNode.updateWith`) and
ported byte-for-byte to Rust. The port is the authority-matched reader; it
consumes every field, so a future driver can decode real updates without a
second source of truth.

Wire format (`updateSemantics(buffer, strings, attrBuffers)`): repeatedly

```
i32 node_id
  i64 flags        # SemanticsFlag bitset (dart:ui bit positions)
  i32 actions      # SemanticsAction bitset
  i32 max_value_length, current_value_length
  i32 text_selection_base, text_selection_extent
  i32 platform_view_id, scroll_children, scroll_index, traversal_parent
  f32 scroll_position, scroll_extent_max, scroll_extent_min
  i32 role
  string identifier, label, attrs(label)
  string value, attrs(value)
  string increasedValue, attrs; decreasedValue, attrs
  string hint, attrs; tooltip; linkUrl; locale; minValue; maxValue
  i32 heading_level, text_direction
  f32 left, top, right, bottom
  f32 transform[16]; f32 hit_test_transform[16]
  i32 n; i32 children_in_traversal_order[n]
  i32 n; i32 children_in_hit_test_order[n]
  i32 n; i32 custom_action_ids[n]
```

`string` = `i32 index`; `-1` means null; otherwise `strings[index]`.
`attrs` = `i32 count`; `-1` means null; else `count × (i32 start, i32 end,
i32 kind, [kind==locale: i32 buffer_index])`.

`SemanticsNode` exposes `text()` (label → value), `bounds()`
(`[x0,y0,x1,y1]`), `clickable()` (`action::TAP`), `enabled()` and
`has_flag`/`has_action` over the full `dart:ui` bitsets. Flags/actions are
exported as bit constants in `flutter::flag` / `flutter::action`, so the ACOS
tree can be built without the engine vocabulary leaking.

### 1.3 `acos-jni::flutter::tap_packet` — the pointer packet

`tap_packet(x, y, timestamp_micros)` builds the exact two-record
`PointerData` (`down`+`up`, 36 LE field each, 576 bytes) that W130-A proved
reaches `PlatformDispatcher.onPointerDataPacket`. It is the product form of the
`FlutterProbe.buildTap` encoder; the caller applies `devicePixelRatio` and
passes `packet.len()` as the `position` argument (the W128-A empty-packet bug).

## 2. Tests

| suite | result |
|---|---|
| `cargo test -p acos-jni --lib` | **21 pass / 0 fail** (8 new: detection, decoder, truncation, tap packet) |
| `cargo clippy -p acos-jni -p acos-cli -p acos-mcp --all-targets --no-deps -- -D warnings` | **clean** |
| `cargo check --workspace --all-targets` | **clean** |
| `cargo test -p acos-jni --test flutter_hello` | **2 pass / 0 fail** — W130-A e2e reproduced unchanged (`global_ssim 0.9987 / 0.9968`, tap delta 160 px at `[134,231,146,248]`) |

## 3. Not done (honest boundary)

The driver and the product wiring are **not** finished in this wave. Still
missing, in dependency order:

1. **Run the engine in the confined child.** The W130-A child lives in the
   `flutter_hello` test, not in a product module. It must move to a
   `FlutterSession` the `acos-mcp` confined child owns, so `acos launch --native`
   publishes frames into the session `FrameStore`.
2. **Semantics plumbing.** `FlutterJNI.setSemanticsEnabled(true)` +
   `nativeSetSemanticsEnabled`, capture the `updateSemantics` buffer/strings/
   attribute buffers at the JNI boundary, feed `decode_semantics_update`, and
   map nodes to the ACOS tree (with `--text` lookup over `label`).
3. **Tap dispatch.** Call `FlutterJNI.dispatchPointerDataPacket` with
   `tap_packet` from the driver; `acos tap --text Increment` = semantics lookup
   → node centre → packet.
4. **CLI/MCP/session routing.** `acos launch`/`observe`/`tap`/`session` and the
   MCP tools must route a detected Flutter APK to the driver instead of the
   framework launch ladder.
5. **`acos parity --flutter`.** A preset for the `[0,0,1080,108]` status and
   `[0,2304,1080,2400]` navigation masks over the existing `acos frame-diff
   --mask` machinery (this part already works; only the preset and the
   tree-vs-uiautomator comparison across the process boundary are missing).
6. **Fixture-gated product e2e.** `launch → frame → tap → frame2` driven through
   the CLI/MCP (the W130-A test remains the evidence that the engine path works;
   it is not a product path).

This is recorded rather than papered over: `acos inspect` reports Flutter
honestly, and the launch ladder does **not** claim a Flutter pass.
