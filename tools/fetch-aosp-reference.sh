#!/usr/bin/env bash
# Recreate the read-only AOSP source reference used by agents (docs/AOSP_REFERENCE.md).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)/.cache/aosp"; TAG=${AOSP_TAG:-android-14.0.0_r75}
mkdir -p "$ROOT"; cd "$ROOT"
clone(){ n=$1; url=$2; shift 2
  if [ ! -d "$n" ]; then
    git clone -q --depth 1 --branch "$TAG" --filter=blob:none --sparse "$url" "$n"
  fi
  # `add`, not `set`: an existing cache keeps its other paths and gains the
  # new ones (LAYER1 added services/core/java/com/android/server/policy + /wm
  # for the real getWindowLayerFromTypeLw table).
  git -C "$n" sparse-checkout add "$@"; }
B=https://android.googlesource.com/platform
clone native $B/frameworks/native libs/binder libs/binder/ndk include/binder libs/ui libs/gui
clone base $B/frameworks/base core/jni libs/hwui libs/androidfw core/java/android/os core/java/com/android/internal/os \
  services/core/java/com/android/server/policy \
  services/core/java/com/android/server/wm
clone art $B/art runtime libartbase libnativeloader libnativebridge
clone bionic $B/bionic linker libc/bionic libc/include
clone libbase $B/system/libbase .
clone core $B/system/core libutils libcutils liblog property_service
du -sh "$ROOT"/*
