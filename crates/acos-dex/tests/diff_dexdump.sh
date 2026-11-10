#!/usr/bin/env bash
# Differential test: compare acos-dex's per-method instruction counts and opcode
# mnemonic sequences against the official Android SDK `dexdump -d`.
#
# Usage:
#   tests/diff_dexdump.sh [file.dex|file.apk ...]
#
# With no arguments it compares the checked-in fixtures. APK/JAR inputs have
# their classes*.dex entries extracted to a temp dir under /tmp (never into the
# repo).
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEXDUMP="${DEXDUMP:-/home/Qui/Android/Sdk/build-tools/35.0.0/dexdump}"
CANON="$CRATE_DIR/tests/dexdump_canon.py"

if [ ! -x "$DEXDUMP" ]; then
    echo "error: dexdump not found at $DEXDUMP" >&2
    exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Build the example once and locate its binary.
cargo build -q --manifest-path "$CRATE_DIR/Cargo.toml" -p acos-dex --example dexdump
WORKSPACE_ROOT="$(cd "$CRATE_DIR/../.." && pwd)"
DEXDUMP_BIN="$(find "$WORKSPACE_ROOT/target/debug/examples" -maxdepth 1 -name 'dexdump' -type f | head -1)"
if [ -z "$DEXDUMP_BIN" ]; then
    echo "error: could not locate built dexdump example" >&2
    exit 1
fi

TOTAL_METHODS=0
TOTAL_INSN=0
TOTAL_MISMATCH=0
FILES=0

compare_dex() {
    local dex="$1"
    local base
    base="$(basename "$dex")"
    local ours="$WORK/$base.ours"
    local theirs="$WORK/$base.theirs"
    local canon="$WORK/$base.canon"

    "$DEXDUMP_BIN" "$dex" --mnemonics > "$ours"
    "$DEXDUMP" -d "$dex" > "$WORK/$base.raw" 2>/dev/null
    python3 "$CANON" "$WORK/$base.raw" > "$canon"

    sort "$ours" -o "$ours"
    sort "$canon" -o "$canon"

    local methods insns
    methods=$(wc -l < "$ours")
    insns=$(awk '{s+=$5} END {print s+0}' "$ours")
    TOTAL_METHODS=$((TOTAL_METHODS + methods))
    TOTAL_INSN=$((TOTAL_INSN + insns))
    FILES=$((FILES + 1))

    if diff -u "$canon" "$ours" > "$WORK/$base.diff"; then
        printf 'OK   %-40s methods=%-5s instructions=%s\n' "$base" "$methods" "$insns"
    else
        local n
        n=$(grep -c '^[-+]M ' "$WORK/$base.diff" || true)
        TOTAL_MISMATCH=$((TOTAL_MISMATCH + n))
        printf 'FAIL %-40s mismatching-lines=%s\n' "$base" "$n"
        head -40 "$WORK/$base.diff"
    fi
}

# Expand APK/JAR inputs into their classes*.dex entries.
expand_input() {
    local path="$1"
    case "$path" in
        *.apk|*.jar)
            python3 - "$path" "$WORK" <<'PY'
import sys, zipfile, os
apk, work = sys.argv[1], sys.argv[2]
outdir = os.path.join(work, os.path.basename(apk) + ".dex")
os.makedirs(outdir, exist_ok=True)
with zipfile.ZipFile(apk) as z:
    for name in z.namelist():
        if name.startswith("classes") and name.endswith(".dex"):
            data = z.read(name)
            with open(os.path.join(outdir, name), "wb") as fh:
                fh.write(data)
            print(os.path.join(outdir, name))
PY
            ;;
        *)
            echo "$path"
            ;;
    esac
}

INPUTS=("$@")
if [ ${#INPUTS[@]} -eq 0 ]; then
    INPUTS=(
        "$CRATE_DIR/tests/data/fixtures.dex"
        "$CRATE_DIR/tests/data/fixtures_api19.dex"
        "$CRATE_DIR/tests/data/fixtures_invokecustom.dex"
    )
fi

for input in "${INPUTS[@]}"; do
    while IFS= read -r dex; do
        [ -n "$dex" ] && compare_dex "$dex"
    done < <(expand_input "$input")
done

echo "------------------------------------------------------------"
echo "files=$FILES methods=$TOTAL_METHODS instructions=$TOTAL_INSN mismatches=$TOTAL_MISMATCH"
if [ "$TOTAL_MISMATCH" -ne 0 ]; then
    exit 1
fi
