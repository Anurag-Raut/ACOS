#!/usr/bin/env bash
# End-to-end installed-prefix smoke test for ACOS system-app modules (v0.6.0 PR 5).
#
# 1. Installs ACOS into a private temporary prefix via tools/release/install.sh.
# 2. Verifies all four system-app APKs (plus com.android.webview.apk) exist
#    under <prefix>/share/acos/<ver>/.cache/boot/android-34/system-apps/ and are
#    recorded in MANIFEST.txt (with zero escaping symlinks).
# 3. From /tmp with `env -u ACOS_HOME`, launches a fixture APK via both the
#    installed `acos` CLI wrapper and the installed `acos-mcp` wrapper with
#    `system.settings` and `system.launcher` attached (and `system.permissions` /
#    `system.notifications` verified too), asserting clean first_frame launch
#    without GUEST_EXITED or fixtures/build/apps/ errors.
#
# Usage:
#   tools/art-runtime/run-exclusive.sh --task v060-installed-system-apps -- \
#       tools/guard/guarded.sh --mem 6G --time 300 -- \
#       tools/release/smoke-system-apps.sh [--from-target DIR]
set -euo pipefail

SELF_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd -- "$SELF_DIR/../.." && pwd)

FROM_TARGET=
while [ $# -gt 0 ]; do
    case "$1" in
        --from-target) FROM_TARGET=$2; shift 2 ;;
        --from-target=*) FROM_TARGET=${1#*=}; shift ;;
        -h|--help)
            sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            printf 'smoke-system-apps: unknown argument: %s\n' "$1" >&2
            exit 2
            ;;
    esac
done

if [ -z "$FROM_TARGET" ]; then
    CAND_TARGET="${CARGO_TARGET_DIR:-$REPO/.cache/target}"
    if [ -x "$CAND_TARGET/release/acos" ] \
        && [ -x "$CAND_TARGET/release/acos-art" ] \
        && [ -x "$CAND_TARGET/release/acos-mcp" ] \
        && [ -x "$CAND_TARGET/release/acos-netd" ] \
        && [ -x "$CAND_TARGET/release/acos-webview" ]; then
        FROM_TARGET="$CAND_TARGET"
    fi
fi

VERSION=$(awk -F'"' '/^\[workspace\.package\]/{w=1} w && /^version *=/{print $2; exit}' "$REPO/Cargo.toml")
[ -n "$VERSION" ] || { printf 'smoke-system-apps: failed to read VERSION\n' >&2; exit 1; }

TMP_ROOT=$(mktemp -d /tmp/acos-sysapps-smoke.XXXXXX)
TMP_PREFIX="$TMP_ROOT/prefix"
TMP_RUNTIME="$TMP_ROOT/runtime"
mkdir -p "$TMP_PREFIX" "$TMP_RUNTIME"
chmod 700 "$TMP_RUNTIME"

DAEMON_PID=
cleanup() {
    local rc=$?
    if [ -n "${DAEMON_PID:-}" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        kill -TERM "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
    rm -rf "$TMP_ROOT"
    exit "$rc"
}
trap cleanup EXIT INT TERM

printf '[smoke-system-apps] installing into private prefix %s\n' "$TMP_PREFIX"
INSTALL_ARGS=(--prefix "$TMP_PREFIX")
if [ -n "$FROM_TARGET" ]; then
    INSTALL_ARGS+=(--from-target "$FROM_TARGET")
fi
"$REPO/tools/release/install.sh" "${INSTALL_ARGS[@]}" >"$TMP_ROOT/install.log" 2>&1 || {
    cat "$TMP_ROOT/install.log" >&2 || true
    exit 1
}

DEST="$TMP_PREFIX/share/acos/$VERSION"
SYSAPPS_DIR="$DEST/.cache/boot/android-34/system-apps"
MANIFEST="$DEST/MANIFEST.txt"

[ -f "$MANIFEST" ] || { printf 'smoke-system-apps: missing %s\n' "$MANIFEST" >&2; exit 1; }

REQUIRED_APKS=(
    "com.android.webview.apk"
    "com.acos.launcher.apk"
    "com.android.settings.apk"
    "com.android.permissioncontroller.apk"
    "com.android.systemui.apk"
)

for pkg_apk in "${REQUIRED_APKS[@]}"; do
    rel=".cache/boot/android-34/system-apps/$pkg_apk"
    full="$SYSAPPS_DIR/$pkg_apk"
    [ -f "$full" ] && [ ! -L "$full" ] && [ -s "$full" ] || {
        printf 'smoke-system-apps: missing or invalid regular file %s\n' "$full" >&2
        exit 1
    }
    grep -Fxq "$rel" "$MANIFEST" || {
        printf 'smoke-system-apps: %s missing from %s\n' "$rel" "$MANIFEST" >&2
        exit 1
    }
    printf '[smoke-system-apps] verified %s in %s\n' "$rel" "$MANIFEST"
done

# Copy a fixture APK outside the checkout so no path resolves into the repo.
FIXTURE_APK=$(readlink -f -- "$REPO/fixtures/build/apps/settings-probe.apk" 2>/dev/null || true)
if [ -z "$FIXTURE_APK" ] || [ ! -f "$FIXTURE_APK" ]; then
    ACOS_APP_ONLY="settings-probe" "$REPO/tools/build/build-apps.sh"
    FIXTURE_APK=$(readlink -f -- "$REPO/fixtures/build/apps/settings-probe.apk")
fi
APP_APK="$TMP_ROOT/app.apk"
cp -f -- "$FIXTURE_APK" "$APP_APK"

# 1) Verify via installed `acos` CLI wrapper from /tmp with ACOS_HOME unset.
CLI_SOCK="$TMP_RUNTIME/cli.sock"
(
    cd /tmp
    exec env -u ACOS_HOME -u ACOS_BIN -u ACOS_SESSION_DAEMON \
        ACOS_RUNTIME_DIR="$TMP_RUNTIME" \
        ACOS_SESSION_SOCKET="$CLI_SOCK" \
        "$TMP_PREFIX/bin/acos" serve --socket "$CLI_SOCK" --owner-pid "$$" \
        >"$TMP_ROOT/cli-serve.log" 2>"$TMP_ROOT/cli-serve.err"
) &
DAEMON_PID=$!

for _ in $(seq 1 50); do
    if [ -S "$CLI_SOCK" ]; then
        break
    fi
    sleep 0.1
done
[ -S "$CLI_SOCK" ] || {
    printf 'smoke-system-apps: CLI daemon socket did not appear at %s\n' "$CLI_SOCK" >&2
    cat "$TMP_ROOT/cli-serve.err" >&2 || true
    exit 1
}

printf '[smoke-system-apps] running installed acos launch from /tmp (env -u ACOS_HOME)\n'
if ! (
    cd /tmp && env -u ACOS_HOME -u ACOS_BIN -u ACOS_SESSION_DAEMON \
        ACOS_RUNTIME_DIR="$TMP_RUNTIME" \
        ACOS_SESSION_SOCKET="$CLI_SOCK" \
        "$TMP_PREFIX/bin/acos" launch --engine art "$APP_APK" \
        --service system.settings:real \
        --service system.launcher:real \
        --service system.permissions:real \
        --service system.notifications:real \
        --json >"$TMP_ROOT/cli-launch.json" 2>"$TMP_ROOT/cli-launch.err"
); then
    printf 'smoke-system-apps: acos launch exited non-zero\n' >&2
    tail -n 10 "$TMP_ROOT/cli-serve.err" >&2 || true
    cat "$TMP_ROOT/cli-launch.err" >&2 || true
    python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(json.dumps(d.get("error") or d, indent=2))' "$TMP_ROOT/cli-launch.json" >&2 || cat "$TMP_ROOT/cli-launch.json" >&2 || true
    exit 1
fi

if grep -qE 'GUEST_EXITED|fixtures/build/apps/' "$TMP_ROOT/cli-launch.json" "$TMP_ROOT/cli-launch.err"; then
    printf 'smoke-system-apps: CLI launch hit forbidden error:\n' >&2
    cat "$TMP_ROOT/cli-launch.json" "$TMP_ROOT/cli-launch.err" >&2
    exit 1
fi

CLI_SID=$(python3 - "$TMP_ROOT/cli-launch.json" <<'PYEOF'
import json, sys
env = json.load(open(sys.argv[1]))
assert env.get("status") == "pass", f"CLI launch failed: {env}"
assert env.get("ok") is True, f"CLI launch not ok: {env}"
res = env.get("result") or {}
assert res.get("stage_reached") == "first_frame", f"stage_reached mismatch: {res}"
print(env.get("session_id") or res.get("session_id") or "")
PYEOF
)
[ -n "$CLI_SID" ] || { printf 'smoke-system-apps: missing session_id from CLI launch\n' >&2; exit 1; }

(
    cd /tmp && env -u ACOS_HOME -u ACOS_BIN -u ACOS_SESSION_DAEMON \
        ACOS_RUNTIME_DIR="$TMP_RUNTIME" \
        ACOS_SESSION_SOCKET="$CLI_SOCK" \
        "$TMP_PREFIX/bin/acos" session destroy --session "$CLI_SID" --json >/dev/null
)
kill -TERM "$DAEMON_PID" 2>/dev/null || true
wait "$DAEMON_PID" 2>/dev/null || true
DAEMON_PID=

# 2) Verify via installed `acos-mcp` wrapper from /tmp with ACOS_HOME unset.
printf '[smoke-system-apps] running installed acos-mcp launch from /tmp (env -u ACOS_HOME)\n'
python3 - "$TMP_PREFIX/bin/acos-mcp" "$APP_APK" "$TMP_RUNTIME" <<'PYEOF'
import json
import os
import select
import subprocess
import sys
import time

mcp_bin, app_apk, runtime_dir = sys.argv[1], sys.argv[2], sys.argv[3]
env = {k: v for k, v in os.environ.items() if k not in {
    "ACOS_HOME", "ACOS_BIN", "ACOS_SESSION_SOCKET", "ACOS_SESSION_DAEMON"
}}
env["ACOS_RUNTIME_DIR"] = runtime_dir

proc = subprocess.Popen(
    [mcp_bin],
    cwd="/tmp",
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    env=env,
    bufsize=0,
)
buf = b""
next_id = 1

def rpc(method, params=None, timeout=180.0):
    global next_id, buf
    rid = next_id
    next_id += 1
    frame = {"jsonrpc": "2.0", "id": rid, "method": method}
    if params is not None:
        frame["params"] = params
    proc.stdin.write((json.dumps(frame) + "\n").encode())
    proc.stdin.flush()
    deadline = time.monotonic() + timeout
    while True:
        while b"\n" not in buf:
            rem = deadline - time.monotonic()
            if rem <= 0:
                raise RuntimeError(f"timeout waiting for {method}")
            ready, _, _ = select.select([proc.stdout], [], [], rem)
            if not ready:
                raise RuntimeError(f"timeout waiting for {method}")
            chunk = os.read(proc.stdout.fileno(), 1 << 20)
            if not chunk:
                raise RuntimeError(f"acos-mcp closed stdout (rc={proc.poll()})")
            buf += chunk
        line, buf = buf.split(b"\n", 1)
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        if msg.get("id") == rid:
            assert "error" not in msg, f"{method} error: {msg}"
            return msg["result"]

def notify(method, params=None):
    frame = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        frame["params"] = params
    proc.stdin.write((json.dumps(frame) + "\n").encode())
    proc.stdin.flush()

sid = None
try:
    init = rpc("initialize", {
        "protocolVersion": "2025-03-26",
        "capabilities": {},
        "clientInfo": {"name": "smoke-system-apps", "version": "1"},
    })
    assert init["serverInfo"]["name"] == "acos-mcp", init
    notify("notifications/initialized")

    res = rpc("tools/call", {
        "name": "acos_launch",
        "arguments": {
            "apk": app_apk,
            "services": [
                {"id": "system.settings", "profile": "real"},
                {"id": "system.launcher", "profile": "real"},
                {"id": "system.permissions", "profile": "real"},
                {"id": "system.notifications", "profile": "real"},
            ],
        },
    })
    raw_text = res["content"][0]["text"]
    assert "GUEST_EXITED" not in raw_text, raw_text
    assert "fixtures/build/apps/" not in raw_text, raw_text
    env_reply = json.loads(raw_text)
    assert env_reply.get("status") == "pass", f"acos_launch failed: {env_reply}"
    assert env_reply.get("ok") is True, f"acos_launch not ok: {env_reply}"
    result = env_reply.get("result") or {}
    assert result.get("stage_reached") == "first_frame", result
    sid = env_reply.get("session_id") or result.get("session_id")
    assert sid, f"missing session_id: {env_reply}"

    destroy_res = rpc("tools/call", {
        "name": "acos_destroy",
        "arguments": {"session_id": sid},
    })
    destroy_env = json.loads(destroy_res["content"][0]["text"])
    assert destroy_env.get("status") == "pass", destroy_env
    sid = None
finally:
    if sid:
        try:
            rpc("tools/call", {"name": "acos_destroy", "arguments": {"session_id": sid}}, timeout=30.0)
        except Exception:
            pass
    try:
        proc.stdin.close()
    except OSError:
        pass
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    stderr_out = proc.stderr.read().decode(errors="replace")
    assert "GUEST_EXITED" not in stderr_out, stderr_out
    assert "fixtures/build/apps/" not in stderr_out, stderr_out
PYEOF

printf '[smoke-system-apps] PASS: installed prefix verified for all 4 system apps + acos & acos-mcp\n'
