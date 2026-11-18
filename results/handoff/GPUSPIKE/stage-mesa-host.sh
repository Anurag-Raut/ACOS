#!/usr/bin/env bash
set -euo pipefail
cd /home/Qui/.t3/worktrees/acos/t3-4410a241-c6878933394baf1b
V=/home/Qui/.t3/worktrees/acos/t3-4410a241-c6878933394baf1b/.cache/gpu-scratch/vendor/lib64
tools/art-gles.sh --tag mesa-host --egl-tag mesa --from-dir $V/egl --vulkan-icd $V/hw/vulkan.radeon.so --vulkan-tag radeon \
 --extra-lib $V/libgallium_dri.so --extra-lib $V/libLLVM22.so --extra-lib $V/libdrm_amdgpu.so --extra-lib $V/libdrm_radeon.so --extra-lib $V/libdrm_intel.so --extra-lib $V/libgbm_mesa.so --extra-lib $V/libgbm_mesa_wrapper.so --extra-lib $V/dri_gbm.so --extra-lib $V/libminigbm_gralloc_gbm_mesa.so \
 --extra-hw $V/hw/gralloc.gbm.so --extra-hw "$V/hw/android.hardware.graphics.mapper@4.0-impl.minigbm_gbm_mesa.so" \
 --prop ro.hardware.gralloc=gbm --prop gralloc.gbm.device=/dev/dri/renderD129 --render-node /dev/dri/renderD129 \
 --provider "Mesa 26.0.8 radeonsi/RADV + minigbm gralloc (Waydroid MAINLINE vendor image, Android 13)" \
 --version "Mesa 26.0.8; waydroid vendor_datetime=1792667170 (2026-10-22T11:06:10Z); LLVM 22" \
 --licence "MIT (Mesa, libdrm); Apache-2.0 WITH LLVM-exception (LLVM); BSD-3-Clause (minigbm)" \
 --source "/var/lib/waydroid/images/vendor.img sha256 c49c75c0e703bac7f1495e6db0e9232390a6539b3d732999dd601c63cb01bdd3"
