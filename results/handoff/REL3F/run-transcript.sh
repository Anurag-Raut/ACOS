#!/usr/bin/env bash
# REL3F manual transcript: `acos guide`, `acos session list` text mode, and a
# selector error in the human form. Run from anywhere; paths are derived from
# this file. Intended to be run under
#   tools/art-runtime/run-exclusive.sh --task REL3F -- tools/guarded.sh --mem 12G --time 600 -- <this>
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WT="$(cd "$HERE/../../.." && pwd)"
ACOS="$WT/.cache/rel3f-target/debug/acos"
RT="$WT/.cache/rel3f-transcript-runtime"
OUT="$HERE"
TRANSCRIPT="$OUT/transcript.txt"

rm -rf "$RT"
mkdir -p "$RT"
chmod 700 "$RT"
export ACOS_RUNTIME_DIR="$RT"
export ACOS_SESSION_IDLE_SECS=30
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_MCP_QUIET=1

say() { printf '$ %s\n' "$*" >>"$TRANSCRIPT"; }
run() {
    say "$@"
    "$@" >>"$TRANSCRIPT" 2>&1
    local rc=$?
    printf '[rc %s]\n\n' "$rc" >>"$TRANSCRIPT"
    return 0
}

: >"$TRANSCRIPT"
printf 'REL3F manual transcript, %s\nworktree: %s\n\n' "$(date -u +%FT%TZ)" "$WT" >>"$TRANSCRIPT"

# ---------------------------------------------------------------------------
# 1. The agent guide from the CLI (same text as MCP acos_help).
# ---------------------------------------------------------------------------
say "env -u ACOS_HOME $ACOS guide workflow | head -20"
env -u ACOS_HOME "$ACOS" guide workflow 2>/dev/null | head -20 >>"$TRANSCRIPT"
printf '\n[full text: guide-workflow.txt; $ acos guide workflow]\n\n' >>"$TRANSCRIPT"
env -u ACOS_HOME "$ACOS" guide workflow >"$OUT/guide-workflow.txt" 2>"$OUT/guide-workflow.err"
printf 'guide workflow: %s bytes, %s lines, stderr %s bytes\n\n' \
    "$(wc -c <"$OUT/guide-workflow.txt")" "$(wc -l <"$OUT/guide-workflow.txt")" \
    "$(wc -c <"$OUT/guide-workflow.err")" >>"$TRANSCRIPT"

say "env -u ACOS_HOME $ACOS help | tail -4"
env -u ACOS_HOME "$ACOS" help 2>&1 | tail -4 >>"$TRANSCRIPT"
printf '\n' >>"$TRANSCRIPT"

# ---------------------------------------------------------------------------
# 2. A real session daemon: `session list` text mode shows count + rows.
# ---------------------------------------------------------------------------
env -u ACOS_HOME "$ACOS" serve --idle-timeout 30 >"$OUT/serve.log" 2>&1 &
SERVE_PID=$!
for _ in $(seq 1 200); do
    [ -S "$RT/session.sock" ] && break
    sleep 0.1
done
run env -u ACOS_HOME "$ACOS" session list

# An ART session carries the agent label, age and guest RSS that the list prints.
say "env -u ACOS_HOME $ACOS launch fixtures/build/apps/counter.apk --engine art --keep --label demo --json"
( cd "$WT" && env -u ACOS_HOME "$ACOS" launch fixtures/build/apps/counter.apk --engine art --keep --label demo --json ) >"$OUT/launch.json" 2>"$OUT/launch.err"
printf '[rc %s] %s\n\n' "$?" "$(head -c 400 "$OUT/launch.json" | tr '\n' ' ')" >>"$TRANSCRIPT"

run env -u ACOS_HOME "$ACOS" session list

# 3. `destroy` takes the id positionally (and `--session` still works).
run env -u ACOS_HOME "$ACOS" session destroy s_cli --json
run env -u ACOS_HOME "$ACOS" session list

kill -TERM "$SERVE_PID" 2>/dev/null
wait "$SERVE_PID" 2>/dev/null

# ---------------------------------------------------------------------------
# 3. A selector error: human form, never Rust Debug.
# ---------------------------------------------------------------------------
cat >"$OUT/wait-for-human-selector.yaml" <<'YAML'
version: 1
name: wait-for-human-selector
description: wait_for on a text the fixture never renders
app: fixtures/build/apps/hello-views.apk
engine: reference-only
fallback: forbid
api_level: 34
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
steps:
  - launch:
      activity: dev.acos.fixtures.helloviews/.MainActivity
      clear_state: true
  - wait_for:
      selector: {text: {exact: Nonexistent}}
      timeout_ms: 50
YAML
say "env -u ACOS_HOME $ACOS verify wait-for-human-selector.yaml --apk fixtures/build/apps/hello-views.apk --reference fixtures/reference/hello-views/art-api34-x86_64 --artifacts <scratch> --json"
( cd "$WT" && env -u ACOS_HOME "$ACOS" verify "$OUT/wait-for-human-selector.yaml" \
    --apk fixtures/build/apps/hello-views.apk \
    --reference fixtures/reference/hello-views/art-api34-x86_64 \
    --artifacts "$RT/verify-artifacts" --json ) >"$OUT/selector-error.stdout.json" 2>"$OUT/selector-error.stderr.txt"
printf '[rc %s]\nstdout error.message: ' "$?" >>"$TRANSCRIPT"
python3 - "$OUT/selector-error.stdout.json" >>"$TRANSCRIPT" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1]))
print(doc.get("error", {}).get("message", "<no error>"))
PY
printf 'stderr (human summary):\n' >>"$TRANSCRIPT"
cat "$OUT/selector-error.stderr.txt" >>"$TRANSCRIPT"
printf '\n' >>"$TRANSCRIPT"

# Evidence checks the orchestrator can grep.
if grep -q 'text=="Nonexistent"' "$OUT/selector-error.stderr.txt" &&
   ! grep -q 'Selector {' "$OUT/selector-error.stderr.txt"; then
    printf 'CHECK selector human form: PASS\n' >>"$TRANSCRIPT"
else
    printf 'CHECK selector human form: FAIL\n' >>"$TRANSCRIPT"
fi
