# Image ABI notebook (API-34 system image is ground truth; the AOSP tag is a navigation aid)

Append verified facts. Columns: Contract | reader/writer location in image | evidence | replay fixture.

| Contract | Location | Evidence | Fixture |
|---|---|---|---|
| `SurfaceControl.readFromParcel` client/handle use `readStrongBinder`; a null binder returns UNEXPECTED_NULL, aborting the parse | libandroid_runtime / libgui `SurfaceControl::readFromParcel` | W3: byte replay `research/art-feasibility/w3-leash/scripts/cascade.py`, `w3-cascade.txt` | `crates/acos-services/tests/leash_binder_nonnull.rs` |
| IWindowSession tail codes: grantInputChannel 35, setOnBackInvokedCallbackInfo 39, clearTouchableRegion 40, cancelDraw 41 | image framework dex (`acos-dex dexdump`) | W2 | `window_session_api34.rs` |
| ISurfaceComposer getMaxAcquiredBufferCount = 57; no getSchedulingPolicy | image libgui | L1/V3 | services tests |
| Cutout is a fixed `Rect[4]`; Display.Mode has no mVsyncRate | image framework dex | W2 | `displayinfo_api34.rs` |
| `Surface::lock()` leaves producer crop empty; consumer `lockAsync` returns -22 unless crop/dimensions set; `BufferQueueConsumer::acquireBuffer` returns NO_BUFFER_AVAILABLE instead of blocking | image libgui | G2 | `research/art-feasibility/g1-gralloc/probe_blast.c` |
