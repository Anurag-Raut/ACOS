#!/usr/bin/env bash
#
# run-parity.sh -- run ONE ACOS scenario on ACOS ART and on the phone, then diff
# the two UI trees after every step.
#
#   tools/parity/run-parity.sh scenarios/counter.yaml
#   tools/parity/run-parity.sh scenarios/gaten-backstack.yaml --out results/parity/gaten-backstack
#
# What it does, in three separate phases so no lock is held longer than its job:
#
#   1. ACOS ART   `acos verify <scenario> --engine art` under the shared ART
#                 lock (`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`),
#                 writing the usual artifacts (`verdict.json`, `tree/<seq>.json`).
#   2. phone      the same scenario replayed on the device through
#                 `tools/phone/replay_scenario.py` (agent-device + `uiautomator
#                 dump`), under the shared phone flock, writing one tree per step.
#                 If the phone is not attached, the best recorded physical-device
#                 capture for that scenario is used instead and the report says
#                 so -- a missing phone is never a pass.
#   3. diff       `tools/parity/diff_trees.py` pairs the nodes of both trees per
#                 step (resource-id / text / content-desc / class+role), compares
#                 text, content-desc, enabled/checked/focused/selected,
#                 visibility and dp-normalised bounds, applies
#                 `tools/parity/allowlist.yaml`, and writes `report.json` +
#                 `report.md` under the report directory.
#
# Exit codes: 0 = compared, only documented platform differences; 1 = compared,
# unexpected differences (findings); 2 = usage/environment error; 3 = nothing to
# compare on one side (skipped, never a pass).
#
# Phone safety: only the scenario's own package (and for `com.gaten` the arm64
# build that matches the x86_64 APK ACOS runs) is installed; the package is
# restored to what was on the device before the run when it was already there;
# no device setting, account or security state is changed.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCENARIO=""
SERIAL="${ACOS_DEVICE:-ZA222WGJPP}"
OUT=""
ARTIFACTS=""
PHONE_SOURCE="auto"
PHONE_DIR=""
PHONE_APK=""
ALLOWLIST="$ROOT/tools/parity/allowlist.yaml"
TOLERANCE_DP="1.5"
REUSE_ACOS=0
KEEP_APPS=0
INSTALL=1
BIN="${ACOS_BIN:-$ROOT/.cache/parity1-target/release/acos}"
ART_BIN="${ACOS_ART_BIN:-$ROOT/.cache/parity1-target/release/acos-art}"
MEM="${ACOS_PARITY_MEM:-12G}"
TIME="${ACOS_PARITY_TIME:-900}"

usage() {
    sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    cat <<EOF

Options:
  --serial SERIAL          device serial (default \$ACOS_DEVICE or ZA222WGJPP)
  --out DIR                report directory (default results/parity/<scenario>)
  --acos-artifacts DIR     reuse/write ACOS artifacts (default .cache/parity/runs/<scenario>)
  --reuse-acos             do not run ACOS; diff the existing artifacts
  --phone-source SRC       auto | live | recorded-snapshot | recorded-observations
  --phone-dir DIR          recorded phone capture directory (with --phone-source)
  --phone-apk PATH         APK to install on the phone (default: scenario app,
                           with an arm64 sibling when the scenario names x86_64)
  --allowlist FILE         expected-difference allowlist (default tools/parity/allowlist.yaml)
  --tolerance-dp FLOAT     bounds tolerance in dp (default 1.5)
  --no-install             never install/uninstall; require the app already present
  --keep-apps              leave the app installed after the run
  --exclusive              take every ART slot (timing runs; not needed for parity)
  -h, --help               this help
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --serial) SERIAL="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --acos-artifacts) ARTIFACTS="$2"; shift 2 ;;
        --reuse-acos) REUSE_ACOS=1; shift ;;
        --phone-source) PHONE_SOURCE="$2"; shift 2 ;;
        --phone-dir) PHONE_DIR="$2"; shift 2 ;;
        --phone-apk) PHONE_APK="$2"; shift 2 ;;
        --allowlist) ALLOWLIST="$2"; shift 2 ;;
        --tolerance-dp) TOLERANCE_DP="$2"; shift 2 ;;
        --no-install) INSTALL=0; shift ;;
        --keep-apps) KEEP_APPS=1; shift ;;
        --exclusive) EXCLUSIVE=1; shift ;;
        --*) echo "run-parity: unknown option $1" >&2; exit 2 ;;
        *) SCENARIO="$1"; shift ;;
    esac
done

[ -n "$SCENARIO" ] || { echo "run-parity: a scenario YAML is required" >&2; exit 2; }
[ -f "$SCENARIO" ] || { echo "run-parity: no such scenario: $SCENARIO" >&2; exit 2; }
command -v python3 >/dev/null || { echo "run-parity: python3 is required" >&2; exit 2; }
python3 -c 'import yaml' 2>/dev/null || { echo "run-parity: PyYAML is required" >&2; exit 2; }

NAME="$(python3 -c "import sys,yaml;print((yaml.safe_load(open(sys.argv[1])) or {}).get('name') or '')" "$SCENARIO")"
[ -n "$NAME" ] || { echo "run-parity: the scenario has no name" >&2; exit 2; }
APP="$(python3 -c "import sys,yaml;print((yaml.safe_load(open(sys.argv[1])) or {}).get('app') or '')" "$SCENARIO")"
PKG="$(python3 -c "
import sys, yaml
doc = yaml.safe_load(open(sys.argv[1])) or {}
for step in doc.get('steps') or []:
    if isinstance(step, dict) and 'launch' in step:
        act = str((step['launch'] or {}).get('activity') or '')
        if '/' in act:
            print(act.split('/')[0]); break
        print(act.rsplit('.', 1)[0] if '.' in act else act); break
" "$SCENARIO")"

case "$APP" in
    /*) APP_PATH="$APP" ;;
    *)  APP_PATH="$ROOT/$APP" ;;
esac

OUT="${OUT:-$ROOT/results/parity/$NAME}"
ARTIFACTS="${ARTIFACTS:-$ROOT/.cache/parity/runs/$NAME}"
mkdir -p "$OUT" "$ARTIFACTS"

# Private runtime state: never the owner's ~/.local or the shared session socket.
export ACOS_FONT_DIR="${ACOS_FONT_DIR:-/home/Qui/Projects/acos/.cache/boot/android-34/fonts}"
export ACOS_ART_BIN="$ART_BIN"
export ACOS_RUNTIME_DIR="${ACOS_RUNTIME_DIR:-$ROOT/.cache/parity/runtime}"
export ACOS_SESSION_SOCKET="${ACOS_SESSION_SOCKET:-$ACOS_RUNTIME_DIR/session.sock}"
export ACOS_VIEW_TEST_DIR="${ACOS_VIEW_TEST_DIR:-$ROOT/.cache/parity/lv}"
mkdir -p "$ACOS_RUNTIME_DIR" "$ACOS_VIEW_TEST_DIR"

log() { echo "run-parity: $*" >&2; }

# ---------------------------------------------------------------------------
# 1. ACOS ART
# ---------------------------------------------------------------------------
if [ "$REUSE_ACOS" = "0" ]; then
    [ -x "$BIN" ] || { log "no acos binary at $BIN (build: cargo build -p acos-cli --release)"; exit 2; }
    [ -x "$ART_BIN" ] || { log "no acos-art binary at $ART_BIN (build: cargo build -p acos-art --release)"; exit 2; }
    log "ACOS ART: verify $SCENARIO"
    lock=(/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task PARITY1)
    [ "${EXCLUSIVE:-0}" = "1" ] && lock+=(--exclusive)
    if ! "${lock[@]}" -- "$ROOT/tools/guarded.sh" --mem "$MEM" --time "$TIME" -- \
            "$BIN" verify "$SCENARIO" --engine art --artifacts "$ARTIFACTS" --overwrite --json \
            >"$ARTIFACTS/verify.stdout.json" 2>"$ARTIFACTS/verify.stderr.log"; then
        log "the ACOS ART run did not pass; its artifacts are still diffed (a failing"
        log "side is reported, never hidden): $ARTIFACTS/verify.stderr.log"
    fi
else
    log "ACOS ART: reusing $ARTIFACTS"
fi
[ -f "$ARTIFACTS/verdict.json" ] || { log "no verdict.json in $ARTIFACTS: the ACOS side produced nothing to diff"; exit 3; }

# ---------------------------------------------------------------------------
# 2. phone
# ---------------------------------------------------------------------------
RECORDED_SNAPSHOT="$ROOT/fixtures/reference-device/$NAME"
RECORDED_OBS="$ROOT/results/phone/gaten-f344272/$NAME/phone"
device_attached() {
    command -v adb >/dev/null 2>&1 || return 1
    adb -s "$SERIAL" get-state >/dev/null 2>&1
}

if [ "$PHONE_SOURCE" = "auto" ]; then
    if device_attached; then
        PHONE_SOURCE="live"
    elif ls "$RECORDED_SNAPSHOT"/snapshot-*.json >/dev/null 2>&1; then
        PHONE_SOURCE="recorded-snapshot"
    elif [ -f "$RECORDED_OBS/observations.json" ]; then
        PHONE_SOURCE="recorded-observations"
    else
        log "phone $SERIAL is not attached and no recorded capture exists for $NAME: SKIPPED (not a pass)"
        exit 3
    fi
fi
log "phone source: $PHONE_SOURCE"

PHONE_OUT="$OUT/phone"
APK_FOR_PHONE=""
INSTALLED_BY_US=0
BACKUP_APK=""
PRE_VERSION=""

if [ "$PHONE_SOURCE" = "live" ]; then
    command -v adb >/dev/null 2>&1 || { log "adb is required for a live replay"; exit 2; }
    device_attached || { log "device $SERIAL is not attached: SKIPPED (not a pass)"; exit 3; }

    # Swap an x86_64 netive build for the arm64 build of the same app when the
    # scenario names one (Gaten ships per-ABI APKs).
    if [ -n "$PHONE_APK" ]; then
        APK_FOR_PHONE="$PHONE_APK"
    elif echo "$APP_PATH" | grep -q x86_64; then
        base_dir="$(dirname "$APP_PATH")"
        APK_FOR_PHONE="$(ls -1 "$base_dir"/*arm64*release*debugsigned.apk 2>/dev/null | head -1)"
        [ -n "$APK_FOR_PHONE" ] || APK_FOR_PHONE="$(ls -1 "$base_dir"/*arm64*.apk 2>/dev/null | head -1)"
        if [ -z "$APK_FOR_PHONE" ]; then
            log "the scenario names an x86_64 APK and no arm64 sibling exists:"
            log "  a live replay would test a different app build. SKIPPED (not a pass)"
            exit 3
        fi
    else
        APK_FOR_PHONE="$APP_PATH"
    fi
    [ -f "$APK_FOR_PHONE" ] || { log "phone APK not found: $APK_FOR_PHONE"; exit 2; }
    APK_SHA="$(sha256sum "$APK_FOR_PHONE" | cut -d' ' -f1)"

    PHONE_LOCK=/home/Qui/Projects/acos/.cache/phone.lock
    exec 9>>"$PHONE_LOCK"
    if ! flock -w 600 9; then
        log "the shared phone lock is busy (another agent is driving the device): SKIPPED"
        exit 3
    fi

    if [ "$INSTALL" = "1" ]; then
        PRE_VERSION="$(adb -s "$SERIAL" shell dumpsys package "$PKG" 2>/dev/null | sed -n 's/.*versionCode=\([0-9]*\).*/\1/p' | head -1)"
        if [ -n "$PRE_VERSION" ]; then
            # Leave the device as found: keep the installed APK, restore it later.
            BACKUP_APK="$ROOT/.cache/parity/backup-$PKG-$PRE_VERSION.apk"
            if [ ! -f "$BACKUP_APK" ]; then
                remote="$(adb -s "$SERIAL" shell pm path "$PKG" | head -1 | sed 's/package://' | tr -d '\r')"
                [ -n "$remote" ] && adb -s "$SERIAL" pull "$remote" "$BACKUP_APK" >/dev/null 2>&1
            fi
        fi
        log "installing $APK_FOR_PHONE on $SERIAL"
        # -r: replace in place; -d: the recorded build may be newer than the
        # one under test (Gaten versionCode 1322 vs f344272's 12).
        if ! adb -s "$SERIAL" install -r -d "$APK_FOR_PHONE" >"$OUT/install.log" 2>&1; then
            log "install failed; not touching $PKG (see $OUT/install.log)"
            exit 2
        fi
        INSTALLED_BY_US=1
    fi

    log "replaying $SCENARIO on $SERIAL"
    set +e
    python3 "$ROOT/tools/phone/replay_scenario.py" --source phone \
        --scenario "$SCENARIO" --serial "$SERIAL" --out "$PHONE_OUT" \
        --tree-dir "$PHONE_OUT" --no-device-settings >"$OUT/phone.log" 2>&1
    phone_rc=$?
    set -e
    [ -s "$PHONE_OUT/observations.json" ] || { log "the phone replay produced no observations; see $OUT/phone.log"; exit 3; }

    if [ "$INSTALLED_BY_US" = "1" ] && [ "$KEEP_APPS" = "0" ]; then
        if [ -n "$BACKUP_APK" ] && [ -f "$BACKUP_APK" ]; then
            log "restoring $PKG to the APK that was installed before this run"
            adb -s "$SERIAL" install -r "$BACKUP_APK" >"$OUT/restore.log" 2>&1 || \
                log "restore failed; the device now has $APK_FOR_PHONE installed (see $OUT/restore.log)"
        else
            log "uninstalling $PKG (it was not installed before this run)"
            adb -s "$SERIAL" uninstall "$PKG" >"$OUT/restore.log" 2>&1 || true
        fi
    fi
    exec 9>&-
    echo "$APK_SHA" > "$OUT/phone-apk.sha256"
else
    if [ -z "$PHONE_DIR" ]; then
        case "$PHONE_SOURCE" in
            recorded-snapshot) PHONE_DIR="$RECORDED_SNAPSHOT" ;;
            recorded-observations) PHONE_DIR="$RECORDED_OBS" ;;
        esac
    fi
    [ -d "$PHONE_DIR" ] || { log "no recorded phone capture at $PHONE_DIR"; exit 3; }
    PHONE_OUT="$PHONE_DIR"
fi

# ---------------------------------------------------------------------------
# 3. diff
# ---------------------------------------------------------------------------
DIFF_ARGS=(--scenario "$SCENARIO" --acos-artifacts "$ARTIFACTS"
           --phone-source "$PHONE_SOURCE" --phone-dir "$PHONE_OUT"
           --out "$OUT" --allowlist "$ALLOWLIST" --tolerance-dp "$TOLERANCE_DP")
if [ "$PHONE_SOURCE" = "live" ]; then
    DIFF_ARGS+=(--phone-apk "$APK_FOR_PHONE")
    [ -f "$OUT/phone-apk.sha256" ] && DIFF_ARGS+=(--phone-apk-sha256 "$(cat "$OUT/phone-apk.sha256")")
fi
python3 "$ROOT/tools/parity/diff_trees.py" "${DIFF_ARGS[@]}"
rc=$?
log "report: $OUT/report.md"
exit "$rc"
