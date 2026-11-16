#!/usr/bin/env bash
# Recreate the read-only AOSP source reference used by agents (docs/AOSP_REFERENCE.md).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)/.cache/aosp"; TAG=${AOSP_TAG:-android-14.0.0_r75}
mkdir -p "$ROOT"; cd "$ROOT"
clone(){ n=$1; url=$2; shift 2; [ -d "$n" ] && return 0
  git clone -q --depth 1 --branch "$TAG" --filter=blob:none --sparse "$url" "$n"; git -C "$n" sparse-checkout set "$@"; }
B=https://android.googlesource.com/platform
clone native $B/frameworks/native libs/binder libs/binder/ndk include/binder libs/ui libs/gui
clone base $B/frameworks/base core/jni libs/hwui libs/androidfw core/java/android/os core/java/com/android/internal/os core/java/android/webkit
clone art $B/art runtime libartbase libnativeloader libnativebridge
clone bionic $B/bionic linker libc/bionic libc/include
clone libbase $B/system/libbase .
clone core $B/system/core libutils libcutils liblog property_service
du -sh "$ROOT"/*
