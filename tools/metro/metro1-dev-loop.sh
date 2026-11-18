#!/usr/bin/env bash
#
# METRO1 — the React Native dev loop, end to end, on real ART.
#
# What this proves (and nothing more):
#   1. a **debug** RN 0.81 app in an ACOS session fetches its JS bundle from a
#      Metro dev server running on the *host* (port 8081) and renders the text
#      that bundle holds;
#   2. a JS source change plus a **reload** (Metro's own `POST /reload`, the same
#      broadcast the React Native CLI's `r` key sends over the `/message`
#      websocket) makes the app render the new text — same guest process.
#
# The network path is the v0.4.0 `network` module (ADR-0030): the relay opens
# host loopback port 8081 for this session, and `acos-netd`'s per-session
# forwarder binds `127.0.0.1:8081` inside the session netns (the `adb reverse`
# equivalent). The app asks for `localhost:8081` through RN's own
# `debug_http_host` preference — set by the debug variant's `MainApplication`.
#
# Usage: tools/metro/metro1-dev-loop.sh <outdir>
#
# Everything it starts (Metro, `acos-netd`, the guest session) it also stops.
set -u

HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../.." && pwd)
OUT=${1:-$WT/results/handoff/METRO1/run}
ACOS=${ACOS:-$WT/.cache/metro1-target/release/acos}
NETD=${ACOS_NETD_BIN:-$WT/.cache/metro1-target/release/acos-netd}
PASTA=${ACOS_NET_PASTA:-$WT/.cache/metro1/build/passt-src/pasta}
APP=${METRO_APP_DIR:-$WT/.cache/metro1/app2}
APK=$WT/fixtures/build/apps/rn-debug.apk
PORT=${METRO_PORT:-8081}
PKG=dev.acos.fixtures.rnhello

export ACOS_FONT_DIR=${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}
export ACOS_NET_PASTA=$PASTA
export ACOS_NETD_STATE=${ACOS_NETD_STATE:-$WT/.cache/metro1/netd-state}
export ACOS_RUNTIME_DIR=${ACOS_RUNTIME_DIR:-$WT/.cache/metro1/rt}
export ACOS_SESSION_SOCKET=${ACOS_SESSION_SOCKET:-$ACOS_RUNTIME_DIR/session.sock}
export ACOS_VIEW_TEST_DIR=${ACOS_VIEW_TEST_DIR:-$WT/.cache/lv}
export ACOS_ART_BIN=${ACOS_ART_BIN:-$WT/.cache/metro1-target/release/acos-art}

mkdir -p "$OUT" "$ACOS_NETD_STATE"
# The daemon refuses a runtime dir that group/other can reach (it holds the
# session socket).
mkdir -p "$ACOS_RUNTIME_DIR"
chmod 700 "$ACOS_RUNTIME_DIR"
LOG=$OUT/metro1.log
: >"$LOG"

note() { echo "$*" | tee -a "$LOG"; }

METRO_PID=""
SESSION=""

cleanup() {
    [ -n "$SESSION" ] && env -u ACOS_HOME "$ACOS" session destroy --session "$SESSION" --json \
        >"$OUT/session-destroy.json" 2>&1
    "$NETD" shutdown >"$OUT/netd-shutdown.log" 2>&1
    if [ -n "$METRO_PID" ]; then
        kill "$METRO_PID" 2>/dev/null
        wait "$METRO_PID" 2>/dev/null
    fi
}
trap cleanup EXIT

# --- preconditions ---------------------------------------------------------
[ -x "$ACOS" ] || { note "FAIL missing acos binary: $ACOS"; exit 2; }
[ -x "$NETD" ] || { note "FAIL missing acos-netd binary: $NETD"; exit 2; }
[ -x "$PASTA" ] || { note "FAIL missing pinned pasta: $PASTA (see results/handoff/NET3.md)"; exit 2; }
[ -f "$APK" ] || { note "FAIL missing debug APK: $APK (tools/build-rn-hello.sh debug)"; exit 2; }
[ -d "$APP" ] || { note "FAIL missing Metro project: $APP"; exit 2; }
note "pasta $("$PASTA" --version | head -1)"

# --- 1. Metro on the host --------------------------------------------------
if curl -s -m 2 -o /dev/null "http://127.0.0.1:$PORT/status"; then
    note "FAIL port $PORT already serves /status; refusing to drive someone else's Metro"
    exit 2
fi
( cd "$APP" && exec node node_modules/react-native/cli.js start --port "$PORT" ) \
    >"$OUT/metro.log" 2>&1 &
METRO_PID=$!
for _ in $(seq 1 120); do
    if curl -s -m 2 -o /dev/null "http://127.0.0.1:$PORT/status"; then break; fi
    sleep 0.5
done
if ! curl -s -m 2 -o /dev/null "http://127.0.0.1:$PORT/status"; then
    note "FAIL Metro did not answer /status"; tail -20 "$OUT/metro.log" | tee -a "$LOG"; exit 2
fi
note "metro up pid=$METRO_PID port=$PORT project=$APP"

# The bundle the guest will ask for, fetched by this same host curl: if this
# fails there is nothing to blame the guest for.
curl -s -m 120 -o "$OUT/bundle-host-probe.js" \
    -w 'host_bundle http=%{http_code} bytes=%{size_download}\n' \
    "http://127.0.0.1:$PORT/index.bundle?platform=android&dev=true&minify=false&app=rnhello&modulesOnly=false&runModule=true" \
    | tee -a "$LOG"

# --- 2. the relay, with the host port opened BEFORE the guest exists ------
# A debug RN app fetches its bundle during Application startup, so the opt-in
# cannot come after the launch: `acos-netd hostport add PORT` with no session is
# the relay-level opt-in, and the supervisor applies it to every session it
# attaches (crates/acos-netd/src/supervisor.rs, apply_relay_host_ports).
"$NETD" up >"$OUT/netd-up.log" 2>&1
rc=$?
note "netd_up rc=$rc pasta=$(grep -o '"pasta_version": *"[^"]*"' "$OUT/netd-up.log" | head -1)"
if [ "${METRO1_SKIP_HOSTPORT:-0}" = "1" ]; then
    # The negative arm of the test: without the opt-in the session must be
    # refused a host-loopback connection (NET3's `hostport_not_opted_in_refused`),
    # so the bundle fetch fails and no text appears. Never a pass by accident.
    note "hostport_add SKIPPED (METRO1_SKIP_HOSTPORT=1): the bundle fetch must fail"
else
    "$NETD" hostport add "$PORT" >"$OUT/netd-hostport-add.json" 2>&1
    rc=$?
    note "hostport_add rc=$rc $(tr -d '\n' <"$OUT/netd-hostport-add.json" | head -c 200)"
fi

# --- 3. launch the debug app (network:real) -------------------------------
env -u ACOS_HOME "$ACOS" launch --engine art "$APK" --service network:real \
    --keep --label metro1 --json >"$OUT/launch.json" 2>"$OUT/launch.err"
rc=$?
note "launch rc=$rc stage=$(python3 - "$OUT/launch.json" <<'PY'
import json,sys
try:
    d=json.load(open(sys.argv[1]))
except Exception as e:
    print(f"NO_JSON({e})"); raise SystemExit
r=d.get("result") or {}
print(r.get("stage") or d.get("status"))
PY
)"

env -u ACOS_HOME "$ACOS" session list --json >"$OUT/sessions.json" 2>&1
SESSION=$(python3 - "$OUT/sessions.json" <<'PY'
import json,sys
try:
    d=json.load(open(sys.argv[1]))
except Exception:
    print(""); raise SystemExit
rows=(d.get("result") or {}).get("sessions") or []
rows=[r for r in rows if r.get("label")=="metro1"] or rows
print((rows[0].get("session_id") or "") if rows else "")
PY
)
note "session=$SESSION"
[ -n "$SESSION" ] || { note "FAIL no session id"; exit 3; }

# --- 4. wait for the bundle text, twice, with a real edit in between ------
# `observe_text` prints the visible text of one observation, or nothing.
observe_text() {
    env -u ACOS_HOME "$ACOS" session observe --session "$SESSION" --text --json 2>>"$OUT/observe.err" \
        >"$OUT/observe-$1.json"
}

wait_for_text() {
    local want=$1 tag=$2 deadline=$3 t0
    t0=$(date +%s)
    while :; do
        observe_text "$tag"
        if grep -q -- "$want" "$OUT/observe-$tag.json" 2>/dev/null; then
            note "text_found '$want' after $(( $(date +%s) - t0 ))s (observe-$tag.json)"
            return 0
        fi
        [ $(( $(date +%s) - t0 )) -ge "$deadline" ] && break
        sleep 1
    done
    note "TIMEOUT '$want' after $(( $(date +%s) - t0 ))s"
    return 1
}

if ! wait_for_text "Hello ACOS" before 90; then
    note "--- guest log tail ---"
    env -u ACOS_HOME "$ACOS" session logs --session "$SESSION" --json >"$OUT/logs-before.json" 2>&1
    python3 - "$OUT/logs-before.json" <<'PY' | tail -40 | tee -a "$LOG"
import json,sys
try: d=json.load(open(sys.argv[1]))
except Exception as e: print(f"no logs: {e}"); raise SystemExit
lines=(d.get("result") or {}).get("lines") or (d.get("result") or {}).get("logs") or []
for l in lines[-40:]: print(l if isinstance(l,str) else json.dumps(l)[:300])
PY
    if [ "${METRO1_SKIP_HOSTPORT:-0}" = "1" ]; then
        note "RESULT negative=EXPECTED_FAIL stage=bundle_fetch (no host-port opt-in)"
        exit 0
    fi
    note "RESULT metro1=FAIL stage=bundle_fetch"
    exit 4
fi

# The JS source change a developer would make (same file Metro serves).
python3 - "$APP/index.js" <<'PY'
import sys
p=sys.argv[1]
s=open(p).read()
assert "Hello ACOS" in s, "index.js changed under us; nothing to edit"
open(p,"w").write(s.replace("Hello ACOS", "Hello Metro (reloaded)"))
print("edited", p)
PY
cp "$APP/index.js" "$OUT/index.js.after"
grep -n "Hello Metro" "$OUT/index.js.after" | tee -a "$LOG"

# Metro's reload broadcast: what `r` in the React Native CLI does.
curl -s -m 10 -o "$OUT/reload-response.txt" -w 'reload http=%{http_code}\n' \
    "http://127.0.0.1:$PORT/reload" | tee -a "$LOG"
note "reload_body=$(cat "$OUT/reload-response.txt")"

if ! wait_for_text "Hello Metro" after 90; then
    # Is the *server* the problem or the guest? Re-ask the same URL the app asks.
    curl -s -m 60 -o "$OUT/bundle-host-after-edit.js" \
        "http://127.0.0.1:$PORT/index.bundle?platform=android&dev=true&minify=false&app=rnhello&modulesOnly=false&runModule=true"
    if grep -q "Hello Metro (reloaded)" "$OUT/bundle-host-after-edit.js"; then
        note "host_bundle_after_edit served=NEW_TEXT (the server is right; the guest did not re-render it)"
    else
        note "host_bundle_after_edit served=OLD_TEXT (Metro did not rebuild)"
    fi
    note "metro_bundle_requests=$(grep -c 'BUNDLE  ./index.js' "$OUT/metro.log") packager_connects=$(grep -c 'Connection established' "$OUT/metro.log") packager_closes=$(grep -c 'Connection closed' "$OUT/metro.log")"
    env -u ACOS_HOME "$ACOS" session logs --session "$SESSION" --json >"$OUT/logs-reload.json" 2>&1
    python3 - "$OUT/logs-reload.json" <<'PY' | grep -iE "react|metro|bundle|dev|reload|error|exception" | tail -30 | tee -a "$LOG"
import json,sys
try: d=json.load(open(sys.argv[1]))
except Exception as e: print(f"no logs: {e}"); raise SystemExit
r=d.get("result") or {}
lines=r.get("lines") or r.get("logs") or []
for l in lines: print(l if isinstance(l,str) else json.dumps(l)[:300])
PY
    note "RESULT metro1=FAIL stage=reload"
    exit 5
fi

# --- 5. evidence -----------------------------------------------------------
env -u ACOS_HOME "$ACOS" session logs --session "$SESSION" --json >"$OUT/logs-after.json" 2>&1
env -u ACOS_HOME "$ACOS" session observe --session "$SESSION" --tree --json >"$OUT/tree-after.json" 2>&1
env -u ACOS_HOME "$ACOS" session observe --session "$SESSION" --screenshot "$OUT/frame-after.png" --json \
    >"$OUT/frame-after.json" 2>&1
"$NETD" status >"$OUT/netd-status.json" 2>&1
grep -o '"host_ports":\[[^]]*\]' "$OUT/netd-status.json" | head -1 | tee -a "$LOG"
note "RESULT metro1=PASS bundle=metro reload=metro"
