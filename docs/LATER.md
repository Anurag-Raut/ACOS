# Think-later list

Ideas that are not on any plan. Nothing here is scheduled; revisit when a measured need appears.

## Graphics (owner, 2026-11-18)
- **GPU-backed AHardwareBuffer on the real-GPU path (`mesa-host`)** — ~1–1.5 weeks. Lets screenshots and HARDWARE bitmaps work on the AMD/Intel GPU path (Mesa cannot import ACOS's memfd gralloc handles today). Seam: Mesa's `IMapper::getService` -> staged passthrough mapper + a GPU mode in `gralloc_shim.c`. Evidence: `results/handoff/GPUSPIKE/README.md` (branch `gpuspike`).
- **GPU-drawn (hwui hardware) app windows** — ~5–6 weeks. Closes HARDWARE-bitmap-in-View, RenderEffect, TextureView gaps; can run on SwiftShader or the real GPU. Conflicts with the 2026-11-12 compositor rule and weakens bit-exact frames; needs owner approval and a measured bottleneck. Cheaper interim for HARDWARE bitmaps in Views: copy-to-CPU patch in the software canvas (days; GLSPIKE4 option B).
