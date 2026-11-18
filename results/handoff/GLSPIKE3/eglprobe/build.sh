#!/usr/bin/env bash
# Build the EGL client probe (diagnostic only; no GL code, no rendering).
#   results/handoff/GLSPIKE3/eglprobe/build.sh
# Then run it in the guest (see ../README.md §8):
#   acos-art run --dex …/eglprobe/classes.dex --class EglProbe
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SDK="${ANDROID_HOME:-/home/Qui/Android/Sdk}"
AJ="$SDK/platforms/android-34/android.jar"
D8="$SDK/build-tools/${BUILD_TOOLS_VERSION:-35.0.0}/d8"
rm -rf "$here/classes" && mkdir -p "$here/classes"
javac --release 11 -cp "$AJ" -d "$here/classes" "$here/EglProbe.java"
"$D8" --lib "$AJ" --min-api 26 --output "$here" "$here/classes/EglProbe.class"
echo "built $here/classes.dex"
