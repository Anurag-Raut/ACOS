#!/usr/bin/env bash
# SYSUI4: the exact runs behind this directory (private runtime, shared ART lock).
set -euo pipefail
cd "$(dirname "$0")/../../.."
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/rt-sysui4
export ACOS_SESSION_SOCKET=$PWD/.cache/sysui4-session.sock
export ACOS_VIEW_TEST_DIR=$PWD/.cache/lv
export ACOS_ART_BIN=$PWD/.cache/sysui4-target/release/acos-art

for s in sysui1-permission-pregranted sysui1-permission-allow sysui1-permission-deny \
         sysui1-shade notif-perm-granted notif-perm-denied a11y-probe \
         mproc3-two-process gaten-backstack form-validation; do
    tools/art-runtime/run-exclusive.sh --task SYSUI4 -- \
        tools/guarded.sh --mem 12G --time 600 -- \
        env -u ACOS_HOME "$ACOS_ART_BIN" \
        .cache/sysui4-target/release/acos verify "scenarios/$s.yaml" \
        --engine art --json --artifacts "results/handoff/SYSUI4/$s" --overwrite
done

# the same arm with the host's ATMS trace
tools/art-runtime/run-exclusive.sh --task SYSUI4 -- \
    tools/guarded.sh --mem 12G --time 600 -- \
    env -u ACOS_HOME ACOS_ATMS_TRACE=1 .cache/sysui4-target/release/acos verify \
    scenarios/sysui1-permission-pregranted.yaml --engine art --json \
    --artifacts results/handoff/SYSUI4/pregranted-trace --overwrite
