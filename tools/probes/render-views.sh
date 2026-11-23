#!/usr/bin/env bash
# W101-A: produce honest, pipeline-generated render evidence.
#
# Runs the real Android framework at the Java level (no Activity launch):
#   LayoutInflater.inflate(layout XML) -> View.measure/layout -> View.draw
#   -> RecordingCanvas/RenderNode display list -> HardwareRenderer -> acos-render
# and scores each frame against the ART API-34 reference with the same
# `acos_render::diff` engine `acos frame-diff` uses.
#
# Then re-runs the hand-authored corpus so `corpus-metrics.json` records which
# scenes are `pipeline-generated` vs `hand-authored` (per-scene `provenance`).
#
# Requirements: the host `javac`/`d8` (SDK) toolchain, the built fixture APKs
# (`tools/build/build-fixtures.sh`), the API-34 boot classpath + framework-res and the
# Android fonts under `.cache/boot/android-34`. Missing fixtures make the tests
# skip loudly (set ACOS_REQUIRE_FIXTURES=1 to hard-fail instead).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

: "${CARGO_TARGET_DIR:=$ROOT/.cache/target-w101a}"
export CARGO_TARGET_DIR
export CARGO_PROFILE_DEV_DEBUG=0

FONT_DIR="${ACOS_FONT_DIR:-$ROOT/.cache/boot/android-34/fonts}"
if [[ -d "$FONT_DIR" ]]; then
  export ACOS_FONT_DIR="$FONT_DIR"
fi

echo "== pipeline-generated frames (real View.draw -> RecordingCanvas -> acos-render) =="
cargo test -p acos-vm --test render_views -- --nocapture

echo
echo "== hand-authored corpus (re-stamped with per-scene provenance) =="
cargo test -p acos-render --test corpus -- --nocapture

echo
echo "wrote:"
echo "  results/render/corpus/corpus-pipeline-metrics.json   (real pipeline)"
echo "  results/render/corpus/corpus-metrics.json            (+ provenance)"
echo "  results/render/pipeline/*.png                        (pipeline frames)"
