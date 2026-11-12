#!/usr/bin/env bash
#
# bench-release.sh -- build the release binaries, then run a benchmark harness
# with them (W29). Release is the only profile that can produce a verdict.
#
#   tools/art-runtime/bench-release.sh [--target-dir DIR] [--no-build] [--check]
#                                      [--script PATH] [--mem SIZE] [--time SECS]
#                                      [-- <args passed to the bench script>]
#
# Examples
#   tools/art-runtime/bench-release.sh --check
#       Build (unless --no-build) and print the provenance stamp for
#       target/release/{acos,acos-art}: profile, full sha256, verdict eligibility,
#       host gate. No ART guest is started.
#   tools/art-runtime/bench-release.sh --script tools/art-runtime/bench-session.sh \
#       -- --n 1 --modes full --reps 5 --out results/bench/w29-session
#       Build release, then run bench-session.sh --profile release with those args.
#   tools/art-runtime/bench-release.sh --script tools/art-runtime/bench-n.sh -- art 10
#
# Build: CARGO_TARGET_DIR=<target-dir> tools/guarded.sh --mem ... -- cargo build
# --release -p acos-cli -p acos-art  (the same guarded path the harnesses use).
#
# Release-only rule (BENCHMARKS.md section 11): this wrapper refuses to hand a
# debug binary to a harness. It exports ACOS_BENCH_PROFILE=release and passes
# --target-dir/--profile to bench-session.sh; bench-n.sh and bench.sh already
# read target/release and are refused here when --target-dir is custom.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 1

usage() { sed -n '2,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
NO_BUILD=0
CHECK=0
SCRIPT="$ROOT/tools/art-runtime/bench-session.sh"
MEM="${ACOS_BENCH_MEM:-12G}"
TIME="${ACOS_BENCH_TIME:-1200}"
PASSTHRU=()

while [ $# -gt 0 ]; do
    case "$1" in
        --target-dir) TARGET_DIR="$2"; shift 2 ;;
        --no-build)   NO_BUILD=1; shift ;;
        --check)      CHECK=1; shift ;;
        --script)     SCRIPT="$2"; shift 2 ;;
        --mem)        MEM="$2"; shift 2 ;;
        --time)       TIME="$2"; shift 2 ;;
        --)           shift; PASSTHRU=("$@"); break ;;
        -h|--help)    usage; exit 0 ;;
        *) echo "bench-release: unknown argument: $1" >&2; exit 2 ;;
    esac
done

case "$TARGET_DIR" in
    /*) BIN_ROOT="$TARGET_DIR" ;;
    *)  BIN_ROOT="$ROOT/$TARGET_DIR" ;;
esac
ACOS="$BIN_ROOT/release/acos"
ACOS_ART="$BIN_ROOT/release/acos-art"

echo "bench-release: repo $ROOT" >&2
echo "bench-release: profile release, target dir $TARGET_DIR" >&2

# ---- build (guarded; same memory ceiling as the harnesses) ----
if [ "$NO_BUILD" -eq 0 ]; then
    if ! CARGO_TARGET_DIR="$TARGET_DIR" "$ROOT/tools/guarded.sh" --mem "$MEM" --time "$TIME" -- \
            cargo build --release -p acos-cli -p acos-art; then
        echo "bench-release: release build failed; no benchmark was run" >&2
        exit 3
    fi
fi
for b in "$ACOS" "$ACOS_ART"; do
    if [ ! -x "$b" ]; then
        echo "bench-release: $b missing (build it with this script, or --target-dir)" >&2
        exit 3
    fi
done

# ---- provenance stamp (also used by --check) ----
STAMP_JSON="$(python3 -I - "$ROOT" "$ACOS" "$ACOS_ART" <<'PY'
import json
import sys
sys.path.insert(0, sys.argv[1] + "/tools")
import benchlib
root, acos, acos_art = sys.argv[1], sys.argv[2], sys.argv[3]
stamp = benchlib.provenance(
    root, [acos, acos_art], profile="release",
    gate=benchlib.host_gate(root, check_lock=False), engine="art")
print(json.dumps(stamp, indent=2, sort_keys=True))
PY
)"
STAMP_RC=$?
if [ "$STAMP_RC" -ne 0 ] || [ -z "$STAMP_JSON" ]; then
    echo "bench-release: could not stamp release binaries" >&2
    exit 3
fi
printf '%s\n' "$STAMP_JSON"

ELIGIBLE="$(printf '%s' "$STAMP_JSON" | python3 -I -c 'import json,sys; print("1" if json.load(sys.stdin)["verdict_eligible"] else "0")')"
PROFILE="$(printf '%s' "$STAMP_JSON" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["profile"])')"
if [ "$PROFILE" != "release" ] || [ "$ELIGIBLE" != "1" ]; then
    echo "bench-release: REFUSING: build profile is '$PROFILE' and/or binaries are not" >&2
    echo "bench-release: verdict-eligible (a debug binary can never produce a verdict)." >&2
    exit 5
fi

if [ "$CHECK" -eq 1 ]; then
    echo "bench-release: check ok: release binaries stamped, no benchmark run" >&2
    exit 0
fi

[ -x "$SCRIPT" ] || { echo "bench-release: bench script not executable: $SCRIPT" >&2; exit 2; }

# bench-session.sh understands --target-dir/--profile; the other harnesses read
# target/release directly, so a custom target dir is refused for them.
case "$(basename "$SCRIPT")" in
    bench-session.sh)
        PASSTHRU=(--profile release --target-dir "$TARGET_DIR" "${PASSTHRU[@]}")
        ;;
    *)
        if [ "$TARGET_DIR" != "target" ]; then
            echo "bench-release: $(basename "$SCRIPT") only reads target/release;" >&2
            echo "bench-release: rerun with --target-dir target (or use bench-session.sh)" >&2
            exit 2
        fi
        ;;
esac

export ACOS_BENCH_PROFILE=release
echo "bench-release: running $SCRIPT ${PASSTHRU[*]}" >&2
exec "$SCRIPT" "${PASSTHRU[@]}"
