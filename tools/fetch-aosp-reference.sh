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
# `libs/sensor` + `services/sensorservice` are the native sensor service the
# `sensors` module serves (ISensorServer/ISensorEventConnection wire, the
# `Sensor::flatten` layout and the `ASensorEvent` packet the BitTube carries).
clone native $B/frameworks/native libs/binder libs/binder/ndk include/binder libs/ui libs/gui \
  libs/sensor services/sensorservice
clone base $B/frameworks/base core/jni libs/hwui libs/androidfw core/java/android/os core/java/com/android/internal/os
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
clone connectivity $B/packages/modules/Connectivity framework/src/android/net
clone wifi $B/packages/modules/Wifi framework/java/android/net/wifi
clone bluetooth $B/packages/modules/Bluetooth framework/java/android/bluetooth
clone permission $B/packages/modules/Permission framework-s/java/android/app/role
du -sh "$ROOT"/*
