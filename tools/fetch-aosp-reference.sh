#!/usr/bin/env bash
# Recreate the read-only AOSP source reference used by agents (docs/AOSP_REFERENCE.md).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)/.cache/aosp"; TAG=${AOSP_TAG:-android-14.0.0_r75}
mkdir -p "$ROOT"; cd "$ROOT"
clone(){ n=$1; url=$2; shift 2; [ -d "$n" ] && return 0
  git clone -q --depth 1 --branch "$TAG" --filter=blob:none --sparse "$url" "$n"; git -C "$n" sparse-checkout set "$@"; }
# `add` widens an existing checkout (the tree is a partial clone, so the blobs
# of a newly added path are fetched on demand). `--skip-checks` is needed for a
# single file path in cone mode, which is how the client classes below are kept
# out of huge surrounding directories.
add(){ n=$1; shift; [ -d "$n" ] || return 0; git -C "$n" sparse-checkout add --skip-checks "$@"; }
B=https://android.googlesource.com/platform
clone native $B/frameworks/native libs/binder libs/binder/ndk include/binder libs/ui libs/gui
clone base $B/frameworks/base core/jni libs/hwui libs/androidfw core/java/android/os core/java/com/android/internal/os core/java/android/webkit
# `libs/sensor` + `services/sensorservice` are the native sensor service the
# `sensors` module serves (ISensorServer/ISensorEventConnection wire, the
# `Sensor::flatten` layout and the `ASensorEvent` packet the BitTube carries).
clone native $B/frameworks/native libs/binder libs/binder/ndk include/binder libs/ui libs/gui \
  libs/sensor services/sensorservice
clone base $B/frameworks/base core/jni libs/hwui libs/androidfw core/java/android/os core/java/com/android/internal/os \
  services/core/java/com/android/server/policy \
  services/core/java/com/android/server/wm
clone art $B/art runtime libartbase libnativeloader libnativebridge
clone bionic $B/bionic linker libc/bionic libc/include
clone libbase $B/system/libbase .
clone core $B/system/core libutils libcutils liblog property_service
# Service-client classes behind the tombstone table (ADR-0029 item 3). The
# per-name decision in `acos-services/src/module.rs::TOMBSTONES` is made from
# these files, so re-run this after a re-decision.
add base core/java/android/app core/java/android/content core/java/android/net \
  core/java/android/os core/java/android/view core/java/android/webkit \
  core/java/android/media/session media/java/android/media/MediaRouter.java \
  core/java/android/bluetooth core/java/android/nfc nfc/java/android/nfc \
  telephony/java/android/telephony hardware/java/android/hardware \
  core/java/android/hardware
# LAYER1: the real getWindowLayerFromTypeLw table (WindowManagerPolicy) for an existing cache.
add base services/core/java/com/android/server/policy services/core/java/com/android/server/wm
# MODSENS: the native sensor service for an existing cache.
add native libs/sensor services/sensorservice
# MODTAIL2: the service-manager server and its AIDL, for
# `ServiceHost`'s `registerForNotifications`/`unregisterForNotifications` and the
# `waitForService` shim (`ServiceManagerShim`), plus the bluetooth module's own
# AIDL (every `IBluetooth` method is `oneway … SynchronousResultReceiver`).
add native cmds/servicemanager libs/binder/aidl/android/os libs/binder/include/binder
add bluetooth android/app/aidl/android/bluetooth service/aidl/android/bluetooth
add base core/java/android/os/ServiceManager.java core/java/android/os/IServiceManager.aidl
clone connectivity $B/packages/modules/Connectivity framework/src/android/net
clone wifi $B/packages/modules/Wifi framework/java/android/net/wifi
clone bluetooth $B/packages/modules/Bluetooth framework/java/android/bluetooth
clone permission $B/packages/modules/Permission framework-s/java/android/app/role
# CAMERA1: the camera service client/server (`frameworks/av`) and the camera_metadata
# format + tags (`system/media`), for the `hw.camera` module's `CameraMetadata` codec
# and the ICameraService AIDL. Clone-or-widen: safe on an existing cache.
clone av $B/frameworks/av camera/aidl camera/include camera/camera2 camera/CameraMetadata.cpp camera/CameraBase.cpp camera/ndk services/camera/libcameraservice/api2 camera/cameraserver
clone media $B/system/media camera
du -sh "$ROOT"/*
