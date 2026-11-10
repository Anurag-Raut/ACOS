#!/usr/bin/env bash
#
# CI-style "no vacuous green" verification (HONESTY_AUDIT #11 / W94-A #1).
#
# Fixture-dependent tests skip loudly when a gitignored artifact is absent, so a
# plain `cargo test` on a fresh clone can pass with zero evidence. This script:
#
#   1. builds the fixture corpus (DEX programs + fixture APKs + NDK GLES lib),
#   2. exports ACOS_REQUIRE_FIXTURES=1, so any *remaining* missing artifact is a
#      hard test failure instead of a SKIP,
#   3. runs the fixture-dependent suite (the gate test plus every differential
#      and end-to-end test that consumes fixtures).
#
# Usage:
#   tools/verify-all.sh                 # build fixtures, then run the gate suite
#   tools/verify-all.sh --no-build      # assume fixtures exist (CI cache)
#   tools/verify-all.sh -- <cargo args> # extra args to the final `cargo test`
#
# Exits nonzero on the first missing fixture or failing test. See
# `results/audit/W94A.md` for the finding this closes.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BUILD=1
EXTRA=()
while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) BUILD=0; shift ;;
        --) shift; EXTRA=("$@"); break ;;
        *) echo "verify-all: unknown argument $1" >&2; exit 2 ;;
    esac
done

if [ "$BUILD" = 1 ]; then
    echo "verify-all: building fixtures (tools/build-fixtures.sh)"
    tools/build-fixtures.sh
    echo "verify-all: building fixture apps (tools/build-apps.sh)"
    tools/build-apps.sh
    if [ -x fixtures/jni/build.sh ]; then
        echo "verify-all: building the NDK GLES fixture"
        (cd fixtures/jni && ./build.sh)
    fi
fi

# From here on a missing fixture is a failure, not a SKIP.
export ACOS_REQUIRE_FIXTURES=1

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/.cache/target}"
export CARGO_TARGET_DIR="$TARGET_DIR"

# The fixture-dependent suite, one target per crate. Every one of these calls
# `have_fixture`, so a missing artifact fails instead of skipping.
FIXTURE_TESTS=(
    "acos-vm:fixtures_required"
    "acos-vm:ir_equivalence"
    "acos-render:corpus"
    "acos-cli:native_launch"
    "acos-mcp:native_session"
    "acos-runtime:provider_java"
    "acos-jni:gles_fixture"
)

echo "verify-all: running the fixture gate + fixture-dependent tests"
for spec in "${FIXTURE_TESTS[@]}"; do
    pkg="${spec%%:*}"
    target="${spec##*:}"
    echo "verify-all: cargo test -p $pkg --test $target"
    cargo test -p "$pkg" --test "$target" "${EXTRA[@]}"
done

echo "verify-all: OK (fixtures present, no test skipped)"
