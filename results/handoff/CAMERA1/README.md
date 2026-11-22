# CAMERA2 (v0.4.1) — Slice 1 Evidence Bundle

Branch: `camera2` (based on `14c38cc7e`, which merged `origin/main` `v0.4.0` with earlier `camera1c` WIP).
Design plan & Slice 2 architectural options: [`docs/CAMERA_PLAN.md`](../../../docs/CAMERA_PLAN.md).

## Summary of Results

| Acceptance Item | Verdict | Evidence |
|---|---|---|
| `docs/CAMERA_PLAN.md` (Camera2 path, gralloc/memfd, profiles, CameraX, `CAMERA` permission, slices, Slice 2 lettered options A/B/C) | **PASS** | [`docs/CAMERA_PLAN.md`](../../../docs/CAMERA_PLAN.md) |
| `cargo test --offline -p acos-services` (incl. `camera_module` 17/17, `module_kit` 21/21 incl. `hw.camera`, `modules_doc` 2/2) | **PASS** (759 passed, 0 failed, 2 pre-existing ignored) | [`acos-services-test.log`](acos-services-test.log) |
| `cargo test --offline -p acos-art --no-fail-fast` | **PASS\*** (261 passed, 1 runner-limitation fail\*, 1 pre-existing ignored) | [`acos-art-test.log`](acos-art-test.log) |
| ART positive scenario `scenarios/camera-probe.yaml` (`hw.camera:stub` + `system.permissions:real`) | **PASS** (14/14 steps, `ok=true`) | [`camera-probe/verdict.json`](camera-probe/verdict.json), [`art-scenarios.log`](art-scenarios.log) |
| ART negative scenario `scenarios/camera-off.yaml` (`hw.camera` off -> tombstone `UNSUPPORTED_ANDROID_API`) | **PASS** (6/6 steps, `ok=true`) | [`camera-off/verdict.json`](camera-off/verdict.json), [`camera-off.log`](camera-off.log) |
| `docs/MODULES.md` regenerated (not hand-edited) | **PASS** (`modules_doc` 2/2) | [`acos-services-test.log`](acos-services-test.log) |

\* `acos-art` `tests/spike_net_boundary.rs::guest_cannot_change_the_lan_drop_rules` fails in this harness with `bwrap: setting up uid map: Operation not permitted` under `NoNewPrivs: 1` (`cap-eff=0000000000201400`) — the same pre-existing runner limitation documented in [`results/handoff/HYGMERGE/README.md`](../HYGMERGE/README.md) §2. `spike_net_boundary.rs` was not modified or weakened; all other 261 tests in `acos-art` passed.

## Key ART Probe Outputs (Verbatim)

### Positive (`scenarios/camera-probe.yaml`, `camera-probe/tree/12.json`)

```text
READY mgr=present feat_cam=true feat_any=true
LIST count=1 ids=[0]
CHARS facing=BACK orient=90 level=LIMITED array=4000x3000 active=4000x3000 flash=false focal=4.38 jpeg=9(4000x3000) yuv=9 caps=[0]
UNKNOWN error=IllegalArgumentException: Camera id "99" does not exist (this stub serves "0")
OPEN error=SecurityException: Permission denial: android.permission.CAMERA is not granted
OPEN error=UnsupportedOperationException: unsupported Android API: android.hardware.ICameraService code 4 (hw.camera: connectDevice — opening a camera device and delivering frames to ImageReader/Surface requires Slice 2 (ICameraDeviceUser + gralloc YUV/BLOB buffer transport); see docs/CAMERA_PLAN.md)
```

### Negative (`scenarios/camera-off.yaml`, `camera-off/tree/4.json` + `camera-off/launcher.stderr.log`)

```text
READY mgr=present feat_cam=false feat_any=false
LIST error=UnsupportedOperationException: "media.camera" is provided by service module "hw.camera", which is not attached — attach it: acos_launch {"services": [{"id": "hw.camera", "profile": "real"}]}
```

`camera-off/launcher.stderr.log`:

```text
acos-binder-peer: UNSUPPORTED_ANDROID_API {"code":10,"descriptor":"android.hardware.ICameraService","error":"UNSUPPORTED_ANDROID_API","kind":"UNSUPPORTED_ANDROID_API","message":"\"media.camera\" is provided by service module \"hw.camera\", which is not attached — attach it: acos_launch {\"services\": [{\"id\": \"hw.camera\", \"profile\": \"real\"}]}","method":"code 10"}
acos-binder-peer: UNSUPPORTED_ANDROID_API {"code":5,"descriptor":"android.hardware.ICameraService","error":"UNSUPPORTED_ANDROID_API","kind":"UNSUPPORTED_ANDROID_API","message":"\"media.camera\" is provided by service module \"hw.camera\", which is not attached — attach it: acos_launch {\"services\": [{\"id\": \"hw.camera\", \"profile\": \"real\"}]}","method":"code 5"}
```

## Contents

- `acos-services-test.log` — full `cargo test --offline -p acos-services` run (759 passed, 0 failed).
- `acos-art-test.log` — full `cargo test --offline -p acos-art --no-fail-fast` run (261 passed, 1 documented runner-limitation failure).
- `art-scenarios.log` — `acos verify scenarios/camera-probe.yaml` pass (plus initial `camera-off` run showing the tombstone message text before updating `camera-off.yaml` assertion).
- `camera-off.log` — `acos verify scenarios/camera-off.yaml` pass.
- `camera-probe/` — full ART verify artifact bundle (`verdict.json`, `tree/`, `frames/`, `launcher.stderr.log`, `events.jsonl`).
- `camera-off/` — full ART verify artifact bundle (`verdict.json`, `tree/`, `frames/`, `launcher.stderr.log`, `events.jsonl`).
