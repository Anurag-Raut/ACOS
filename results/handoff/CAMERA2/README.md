# CAMERA3 (v0.4.1) — `hw.camera` Slice 2 Evidence Bundle

Branch: `camera3`
Design plan: [`docs/CAMERA_PLAN.md`](../../../docs/CAMERA_PLAN.md) (§7 Option A chosen by owner).

## Summary of Results

| Acceptance Item | Verdict | Evidence |
|---|---|---|
| Real ART capture `scenarios/camera-capture.yaml` (`hw.camera:stub` + `system.permissions:real`, 640x480 `YUV_420_888` single-shot + 3-frame repeating via Option A frame-sink + `gralloc_shim.c`) | **PASS** (12/12 steps, `ok=true`, `wall_ms=5672`) | [`camera-capture/verdict.json`](camera-capture/verdict.json), [`camera-capture/logs/logcat.txt`](camera-capture/logs/logcat.txt) |
| Negative ART scenario `scenarios/negative/camera-capture-wrong-checksum.yaml` (asserts wrong CRC32 `0x00000000` after real capture of `0xdead8870`) | **FAIL** as expected (`status=fail`, `ASSERTION_FAILED`, steps 0..4 pass, step 5 fails) | [`camera-capture-wrong-checksum/verdict.json`](camera-capture-wrong-checksum/verdict.json) |
| Positive ART discovery & open/close `scenarios/camera-probe.yaml` (`hw.camera:stub`) | **PASS** (15/15 steps, `ok=true`, `wall_ms=8419`) | [`camera-probe/verdict.json`](camera-probe/verdict.json) |
| Negative ART off-mode Z `scenarios/camera-off.yaml` (`hw.camera` off: `getCameraIdList()` = `[]`, `openCamera`/`Capture` throw `UNSUPPORTED_ANDROID_API` naming `hw.camera` and `"profile": "stub"`) | **PASS** (10/10 steps, `ok=true`, `wall_ms=5627`) | [`camera-off/verdict.json`](camera-off/verdict.json) |
| `cargo test --offline -p acos-services --test camera_module --test module_kit --test modules_doc` | **PASS** (`camera_module` 20/20, `module_kit` 21/21, `modules_doc` 2/2) | [`cargo-tests.log`](cargo-tests.log) |
| `cargo test --offline -p acos-art --lib` | **PASS** (204/204 passed, 1 ignored) | — |

## Root-Cause Fix Applied on `camera3`

Disassembly of the image's `libgui.so` (`_ZN7android7Surface7performEiP13__va_list_tag` vs `Surface::dispatchConnect` / `Surface::dispatchDisconnect`) revealed an off-by-one constant bug in [`crates/acos-art/shim/render_shim.c`](../../../crates/acos-art/shim/render_shim.c):
- `ACOS_NATIVE_WINDOW_API_CONNECT` was `14` (which in `Surface::perform` is `dispatchDisconnect` calling vtable `+0x100`) instead of `13` (`dispatchConnect` calling vtable `+0x90`).
- `ACOS_NATIVE_WINDOW_API_DISCONNECT` was `15` instead of `14`.

Before the fix, `BufferQueueProducer` logged `disconnect: not connected (req=4)` and `dequeueBuffer: BufferQueue has no connected producer` (`rc=-19` / `-ENODEV`). Fixing `ACOS_NATIVE_WINDOW_API_CONNECT = 13` and `ACOS_NATIVE_WINDOW_API_DISCONNECT = 14` made single-shot and repeating `ImageReader` captures succeed end-to-end on real ART with zero warnings.

## Key Verbatim ART Logcat Output (`camera-capture/logs/logcat.txt`)

```text
I cameraprobe: READY mgr=present feat_cam=true feat_any=true
I cameraprobe: CAPTURE error=SecurityException: Permission denial: android.permission.CAMERA is not granted
I cameraprobe: CAPTURE opened id=0
I cameraprobe: CAPTURE image w=640 h=480 fmt=35 planes=3 ts=1000000000 crc32=0xdead8870 white=(235,128,128) green=(145,54,34) red=(81,90,240) black=(16,128,128) tag=(160,0,0,90) ramp=(127,128,128)
I cameraprobe: CAPTURE completed frame=0 ts=1000000000
I cameraprobe: CAPTURE closed id=0
I cameraprobe: REPEAT ok count=3 frames=[0, 1, 2] frames_inc=true ts_inc=true img_ts_inc=true crcs=[0xdead8870, 0xdfefda95, 0xdc292dba] tags=[160, 161, 162]
I cameraprobe: REPEAT closed id=0
```

## Contents (~9.4 MiB / 9.8 MB total, 0 dangling refs)

- `cargo-tests.log` — `camera_module`, `module_kit`, `modules_doc` test run.
- `camera-capture.verdict.json`, `camera-probe.verdict.json`, `camera-off.verdict.json`, `camera-capture-wrong-checksum.verdict.json` — CLI JSON envelopes.
- `camera-capture/` — full ART verify bundle (`verdict.json`, `logs/`, `launcher.stderr.log`, `events.jsonl`, `steps/`, `tree/`, `frames/*.png`).
- `camera-probe/`, `camera-off/`, `camera-capture-wrong-checksum/` — `verdict.json`, `logs/`, `launcher.stderr.log`, `events.jsonl`, `steps/`, `tree/`, and final UI `frames/*.png`.
