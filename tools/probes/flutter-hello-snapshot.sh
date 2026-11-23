#!/usr/bin/env bash
#
# Build the W93-A instrumented AOT snapshot:
#
#   .cache/flutter-1/lib-x86_64/libapp_probe.so
#
# It is the same `dart:ui`-only fixture as `fixtures/apps/flutter-hello/lib/
# main.dart` plus `print("ACOS_DART_MAIN entered")` at the top of `main()`, so a
# confined run logs a hard proof that the engine executed the app snapshot.
#
# It reuses `.cache/flutter-1/engine/{linux64/gen_snapshot,patched/...}` and the
# fixture's `package_config.json`; nothing is downloaded.
#
# Usage: tools/probes/flutter-hello-snapshot.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CACHE="$ROOT/.cache/flutter-1"
PATCHED="$CACHE/engine/patched/flutter_patched_sdk_product"
APP_DIR="$ROOT/fixtures/apps/flutter-hello"
SRC="$ROOT/crates/acos-jni/tests/data/flutter_probe_main.dart"
OUT="$CACHE/lib-x86_64/libapp_probe.so"
WORK="$ROOT/target/tmp/flutter-hello-snapshot"
DART_SDK="${DART_SDK:-/opt/dart-sdk}"

die() { echo "flutter-hello-snapshot: error: $*" >&2; exit 1; }

DART_AOT="$DART_SDK/bin/dartaotruntime"
FRONTEND="$DART_SDK/bin/snapshots/frontend_server_aot.dart.snapshot"
GEN="$CACHE/engine/linux64/gen_snapshot"

[ -x "$DART_AOT" ] || die "dartaotruntime not found: $DART_AOT"
[ -f "$FRONTEND" ] || die "frontend_server snapshot not found: $FRONTEND"
[ -x "$GEN" ] || die "gen_snapshot not found: $GEN"
[ -f "$PATCHED/platform_strong.dill" ] || die "patched SDK not found: $PATCHED"
[ -f "$SRC" ] || die "probe Dart source not found: $SRC"

rm -rf "$WORK"
mkdir -p "$WORK" "$(dirname "$OUT")"

"$DART_AOT" "$FRONTEND" \
    --sdk-root "$PATCHED" \
    --platform "$PATCHED/platform_strong.dill" \
    --target=flutter --aot --tfa --link-platform \
    --packages "$APP_DIR/package_config.json" \
    --output-dill "$WORK/app.dill" \
    "$SRC" >/dev/null
[ -f "$WORK/app.dill" ] || die "Dart kernel not produced"

"$GEN" --snapshot_kind=app-aot-elf --elf="$OUT" --strip "$WORK/app.dill" >/dev/null
[ -f "$OUT" ] || die "libapp_probe.so not produced"
echo "flutter-hello-snapshot: -> $OUT ($(stat -c '%s' "$OUT") bytes)"
