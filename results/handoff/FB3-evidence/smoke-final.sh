#!/usr/bin/env bash
set -u
W=/home/Qui/.t3/worktrees/acos/t3-d41b3b08-5ea0b20149a5a9a9
cd $W/.cache/fb3/run
run() { env -u ACOS_HOME ACOS_BIN=$W/.cache/fb3/prefix/bin/acos /home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task FB3 -- /home/Qui/Projects/acos/tools/guarded.sh --mem 10G --time 900 -- python3 -I $W/tools/release/mcp_smoke.py --mcp-bin $W/.cache/fb3/prefix/bin/acos-mcp --timeout 600 "$@"; }
run --apk $W/fixtures/build/apps/counter.apk --mode counter > $W/.cache/fb3/smoke-counter-final.jsonl 2> $W/.cache/fb3/smoke-counter-final.err; echo "COUNTER_EXIT $?"
run --apk $W/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk --mode gaten > $W/.cache/fb3/smoke-gaten-final.jsonl 2> $W/.cache/fb3/smoke-gaten-final.err; echo "GATEN_EXIT $?"
echo SMOKES_DONE
