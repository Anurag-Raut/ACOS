#!/usr/bin/env bash
cd /home/Qui/.paseo/worktrees/3oxa6xao/perf-p9-fast-launch || exit 2
OUT=/home/Qui/Projects/acos/.cache/p9-tests
mkdir -p "$OUT"
export CARGO_TARGET_DIR=/home/Qui/Projects/acos/.cache/p9-target CARGO_BUILD_JOBS=4
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
cargo test -p acos-cli -p acos-services --offline >"$OUT/cli-services.log" 2>&1
echo "acos-cli+acos-services rc=$?"
grep -c "^test result: ok" "$OUT/cli-services.log" | sed 's/^/  ok-suites=/'
grep -E "^test result: FAILED" "$OUT/cli-services.log" | sed 's/^/  /'
cargo test -p acos-art --lib --bins --offline >"$OUT/art-lib.log" 2>&1
echo "acos-art lib+bins rc=$?"
grep -E "^test result" "$OUT/art-lib.log" | sed 's/^/  /'
