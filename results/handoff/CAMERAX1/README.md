# CAMERAX1 — AndroidX CameraX Verification on ACOS `hw.camera`

Branch: `camera3`
Dependency manifest: [`fixtures/apps/camerax-probe/DEPS.md`](../../../fixtures/apps/camerax-probe/DEPS.md) (37 artifacts, 5.95 MiB, SHA-256 verified via [`tools/fetch-camerax-deps.sh`](../../../tools/fetch-camerax-deps.sh))
Offline APK builder: [`tools/build-camerax-probe.sh`](../../../tools/build-camerax-probe.sh) (Gradle-free `aapt2` + `javac --release 11` + `d8` + `zipalign` + `apksigner`; hooked into [`tools/build-apps.sh`](../../../tools/build-apps.sh) via `ACOS_APP_ONLY=camerax-probe`)

## Summary of Results

| Check | Verdict | Wall / Notes | Evidence |
|---|---|---|---|
| 1. `scenarios/camerax-probe.yaml` (`ProcessCameraProvider` + `PreviewView` `PERFORMANCE` + `ImageAnalysis` `YUV_420_888` 640x480 + `ImageCapture` JPEG 640x480 + `Unbind`) | **PASS** (`ok=true`, `wall_ms=9691`, `peak_rss=55.3 MiB`) | Real ART; `crcs=[0xdead8870, 0xdfefda95, 0xdc292dba]`, `tags=[160, 161, 162]`, `BitmapFactory` `bmp=640x480 max_err=1 tol4=true`, `PreviewView` `STREAMING` + `assert_region` colorbars SHA-256 match, `open_devices=0`, `devices_opened=1`, `streams_created=3` | [`camerax-probe.verdict.json`](camerax-probe.verdict.json), [`camerax-probe/verdict.json`](camerax-probe/verdict.json), [`camerax-probe/logs/logcat.txt`](camerax-probe/logs/logcat.txt), [`camerax-probe/frames/6.png`](camerax-probe/frames/6.png) |
| 2. `scenarios/negative/camerax-probe-wrong.yaml` | **FAIL** (`ASSERTION_FAILED`, `wall_ms=2547`) | Asserts `crc32=0x00000000` against real `0xdead8870` | [`camerax-probe-wrong.verdict.json`](camerax-probe-wrong.verdict.json), [`camerax-probe-wrong/verdict.json`](camerax-probe-wrong/verdict.json) |
| 3a. Regression: `scenarios/camera-scripted.yaml` | **PASS** (`ok=true`, `wall_ms=5985`) | Existing Slice 3 scripted `set_frame` PNG/JPEG + `test_pattern` | [`camera-scripted.verdict.json`](camera-scripted.verdict.json) |
| 3b. Regression: `scenarios/camera-jpeg.yaml` | **PASS** (`ok=true`, `wall_ms=2544`) | Existing Slice 3 JPEG/BLOB capture | [`camera-jpeg.verdict.json`](camera-jpeg.verdict.json) |
| 3c. Regression: `scenarios/camera-preview.yaml` | **PASS** (`ok=true`, `wall_ms=3909`) | Existing Slice 3 `SurfaceView` preview | [`camera-preview.verdict.json`](camera-preview.verdict.json) |
| 3d. Regression: `scenarios/camera-capture.yaml` | **PASS** (`ok=true`, `wall_ms=5304`) | Existing Slice 2 YUV_420_888 capture | [`camera-capture.verdict.json`](camera-capture.verdict.json) |
| 3e. Regression: `scenarios/camera-probe.yaml` | **PASS** (`ok=true`, `wall_ms=8826`) | Existing Slice 1 discovery + permissions | [`camera-probe.verdict.json`](camera-probe.verdict.json) |
| 3f. Regression: `scenarios/camera-off.yaml` | **PASS** (`ok=true`, `wall_ms=5719`) | Existing `hw.camera` off tombstone | [`camera-off.verdict.json`](camera-off.verdict.json) |
| 3g. Negatives: `camera-capture-wrong-checksum`, `camera-jpeg-wrong-size`, `camera-preview-wrong-color`, `camera-scripted-wrong-checksum` | **4 FAIL** (`ASSERTION_FAILED`, `wall_ms=2542, 2534, 3250, 2549`) | All 4 fail as expected | [`camera-capture-wrong-checksum.verdict.json`](camera-capture-wrong-checksum.verdict.json), [`camera-jpeg-wrong-size.verdict.json`](camera-jpeg-wrong-size.verdict.json), [`camera-preview-wrong-color.verdict.json`](camera-preview-wrong-color.verdict.json), [`camera-scripted-wrong-checksum.verdict.json`](camera-scripted-wrong-checksum.verdict.json) |
| 4. `cargo test --offline -p acos-services --test camera_module --test module_kit --test modules_doc` | **PASS** (`camera_module` 21/21, `module_kit` 21/21, `modules_doc` 2/2) | Zero Rust changes required for `hw.camera` | [`cargo-tests.log`](cargo-tests.log) |

## Verbatim ART Logcat (`camerax-probe`)

```text
I cameraxprobe: READY feat_cam=true feat_any=true
I cameraxprobe: PREVIEW_STATE state=IDLE impl=PERFORMANCE
I cameraxprobe: PROVIDER ok cameras=1 has_back=true
I cameraxprobe: BIND_PREVIEW ok impl=PERFORMANCE cam=90
I cameraxprobe: PREVIEW_STATE state=STREAMING impl=PERFORMANCE
I cameraxprobe: ANALYSIS frame=1 w=640 h=480 fmt=35 planes=3 rot=90 ts=1000000000 crc32=0xdead8870 white=(235,128,128) green=(145,54,34) red=(81,90,240) black=(16,128,128) tag=(160,0,0,90) ramp=(127,128,128)
I cameraxprobe: ANALYSIS ok count=3 total=3 frames_inc=true ts_inc=true crcs=[0xdead8870, 0xdfefda95, 0xdc292dba] tags=[160, 161, 162]
I cameraxprobe: CAPTURE image w=480 h=640 fmt=256 ts=2466666652 bytes=13897 soi=true bmp=640x480 white=(255,255,255) green=(0,255,1) red=(254,0,0) black=(0,0,0) max_err=1 tol4=true
I cameraxprobe: UNBIND ok
I cameraxprobe: PREVIEW_STATE state=IDLE impl=PERFORMANCE
```
