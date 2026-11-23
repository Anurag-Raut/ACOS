#!/usr/bin/env bash
# K5b keystore suite: fidelity + cross-session adversarial + default-off.
#
# Vehicles, and why:
#   * items 1-2 run as a plain ART guest (`acos-art run --class KeystoreProbe`):
#     real ART, real bwrap sandbox, real per-run `/data/app-data` bind, real
#     landlock, stdout captured. Two of them inside ONE lock acquisition are two
#     concurrent sessions with separate run roots and separate data binds.
#     (`acos launch --engine art` goes through the session daemon, which fixes a
#     guest's environment when the daemon starts, so a per-call mode/keystore
#     switch cannot be delivered there; that is a harness limit, not a keystore
#     one, and the app-session path is still measured in phase 3.)
#   * item 3 runs the probe APK through `acos launch --engine art` with no
#     keystore requested: a real app session must keep the honest
#     `AndroidKeyStore not found` failure.
#
# Every phase runs on real ART under `tools/art-runtime/run-exclusive.sh` and
# `tools/guard/guarded.sh`. Output: <out>/{fidelity,xsession,default-off}/
# Exit 0 only when every check passed and nothing leaked.
set -uo pipefail
WT=/home/Qui/.paseo/worktrees/3oxa6xao/k5
OUT="${1:-$WT/.cache/k5b/keystore-suite}"
PHASE="${2:-all}"
ACOS="$WT/.cache/k5-target/release/acos"
ACOS_ART="$WT/.cache/k5-target/release/acos-art"
APK="$WT/fixtures/build/apps/keystore-probe.apk"
PKG=dev.acos.fixtures.keystoreprobe
PROBE_DEX="$WT/.cache/k5b/probe-guest/classes.dex"
ART_DATA="$WT/.cache/art/data"
# `acos-art run` writes its per-run roots under ACOS_ART_DATA; the phases
# inspect them from the host, so pin it here instead of relying on the caller.
export ACOS_ART_DATA="${ACOS_ART_DATA:-$ART_DATA}"
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_BOOT_DIR
cd "$WT" || exit 9
mkdir -p "$OUT"
PROGRESS="$OUT/progress.log"
SUMMARY="$OUT/summary.tsv"

# Compile the probe guest: the fixture's harness + the production provider it
# installs (the exact SoftwareKeyStore.java LaunchMain compiles), one dex.
build_probe_dex() {
    local out; out=$(dirname "$PROBE_DEX")
    local sdk=/home/Qui/Android/Sdk
    local android_jar="$sdk/platforms/android-34/android.jar"
    rm -rf "$out/classes" && mkdir -p "$out/classes" "$out"
    javac -nowarn -source 11 -target 11 -cp "$android_jar" -d "$out/classes" \
        "$WT"/fixtures/apps/keystore-probe/src/dev/acos/fixtures/keystoreprobe/KeystoreProbe.java \
        "$WT"/crates/acos-art/guest/java/dev/acos/art/SoftwareKeyStore.java \
        || return 1
    "$sdk/build-tools/35.0.0/d8" --lib "$android_jar" --min-api 34 \
        --output "$out" $(find "$out/classes" -name '*.class') || return 1
    rm -rf "$out/classes"
    [ -f "$PROBE_DEX" ]
}

summarise() {
    python3 "$WT/tools/keystore-k5b/summarise.py" "$1" "$2"
}

# The stdout of an `acos-art run` is the guest's stdout; the KEYPROBE records
# are extracted from it (a truncated line cannot pass: it must parse).
phase_fidelity() {
    local dir="$OUT/fidelity" since; since=$(date +%s)
    build_probe_dex || return 1
    rm -rf "$dir"; mkdir -p "$dir"
    # The run root is removed at teardown, so the host-side keystore file is
    # measured while the guest is alive: start it, poll for the file, copy it.
    ( "$ACOS_ART" run --dex "$PROBE_DEX" --class dev.acos.fixtures.keystoreprobe.KeystoreProbe \
        fidelity /data/app-data/$PKG/files $PKG \
        > "$dir/stdout.log" 2> "$dir/stderr.log" ) &
    local g_pid=$!
    local store=""
    for _ in $(seq 1 600); do
        store=$(find "$ACOS_ART_DATA/runs" -type f -name 'entries.properties' 2>/dev/null | head -1)
        [ -n "$store" ] && break
        sleep 0.1
    done
    local mode="" leaky=""
    if [ -n "$store" ]; then
        mode=$(stat -c '%a' "$store")
        mkdir -p "$dir/keystore" && cp -a "$store" "$dir/keystore/"
        leaky=$(find "$ACOS_ART_DATA/runs" -name 'entries.properties' -perm /077 2>/dev/null | head -1)
    fi
    wait "$g_pid"
    local rc=$?
    echo "fidelity run rc=$rc (store=$store mode=$mode)" >> "$PROGRESS"
    awk '/^KEYPROBE \{/{ sub(/^KEYPROBE /, ""); print }' "$dir/stdout.log" > "$dir/fidelity.jsonl"
    [ $rc -eq 0 ] || { echo "[k5b] FAIL fidelity: guest rc=$rc"; tail -5 "$dir/stderr.log"; return 1; }
    local rcf=0
    if [ -n "$store" ]; then
        echo -e "keystore-file-mode\t$mode\tapp-data/$PKG/acos-keystore/entries.properties" >> "$SUMMARY"
        [ "$mode" = "600" ] || { echo "[k5b] FAIL keystore file mode $mode (expected 600)"; rcf=1; }
        [ -n "$leaky" ] && { echo "[k5b] FAIL keystore file group/world accessible: $leaky"; rcf=1; }
    else
        echo "[k5b] FAIL fidelity: the guest wrote no keystore file"; rcf=1
    fi
    summarise "$dir/fidelity.jsonl" fidelity || rcf=1
    return $rcf
}

phase_xsession() {
    local dir="$OUT/xsession" since; since=$(date +%s)
    build_probe_dex || return 1
    rm -rf "$dir"; mkdir -p "$dir/a" "$dir/b"
    # Session A: own run root (`--data a`), mints key k_a + a private file and
    # prints its paths. `create` holds for ~30 s after writing, so A is ALIVE
    # while the driver snapshots its app-data and while session B runs.
    ( "$ACOS_ART" run --dex "$PROBE_DEX" --class dev.acos.fixtures.keystoreprobe.KeystoreProbe \
        create /data/app-data/$PKG/files $PKG \
        > "$dir/a/stdout.log" 2> "$dir/a/stderr.log" ) &
    local a_pid=$!
    local priv="" bin_priv=""
    for _ in $(seq 1 240); do
        priv=$(find "$ACOS_ART_DATA/runs" -type f -name 'private-a.txt' 2>/dev/null | head -1)
        [ -n "$priv" ] && break
        sleep 0.5
    done
    if [ -z "$priv" ]; then
        echo "[k5b] FAIL xsession: session A never wrote private-a.txt"
        kill "$a_pid" 2>/dev/null; wait "$a_pid" 2>/dev/null
        return 1
    fi
    # A's app-data root (RUNS/<id>/app-data/<pkg>) and its keystore file.
    local appdata keyfile
    appdata=$(dirname "$(dirname "$(dirname "$priv")")")
    keyfile="$appdata/acos-keystore/entries.properties"
    bin_priv="/data/app-data/$PKG/files/$(basename "$(dirname "$priv")")/private-a.txt"
    # Copy A's app-data OUT of the live run root, inside the locked shell and
    # while A is still running: `acos-art run` removes its run root at teardown,
    # which is what made K5b's post-exit measurement impossible.
    mkdir -p "$dir/a/appdata"
    cp -a "$appdata/." "$dir/a/appdata/" 2> "$dir/a/copy.err"
    local copy_rc=$?
    printf '%s\n%s\n%s\n' "$priv" "$keyfile" "$bin_priv" > "$dir/a/paths"
    local mode=""
    [ -f "$keyfile" ] && mode=$(stat -c '%a' "$keyfile")
    echo -e "xsession-a-live-copy\t$copy_rc\t$dir/a/appdata" >> "$SUMMARY"
    echo -e "xsession-a-keystore-mode\t$mode\t$keyfile" >> "$SUMMARY"
    echo "[k5b] session A alive=$a_pid material=$priv keys=$keyfile copy_rc=$copy_rc mode=$mode" \
        | tee -a "$PROGRESS"

    local rcx=0
    [ $copy_rc -eq 0 ] || { echo "[k5b] FAIL xsession: cannot copy A's app-data out"; rcx=1; }
    [ -f "$dir/a/appdata/acos-keystore/entries.properties" ] \
        || { echo "[k5b] FAIL xsession: A's copied app-data has no keystore file"; rcx=1; }
    [ "$mode" = "600" ] || { echo "[k5b] FAIL xsession: A's keystore mode '$mode' (expected 600)"; rcx=1; }

    # Session B: a *second* concurrent guest with its own run root and its own
    # `/data/app-data` bind. Targets: A's host path, A's stored key file, and
    # the path a peer app would name inside its own sandbox.
    "$ACOS_ART" run --dex "$PROBE_DEX" --class dev.acos.fixtures.keystoreprobe.KeystoreProbe \
        probe /data/app-data/$PKG/files $PKG \
        "$priv" "$keyfile" "$bin_priv" \
        > "$dir/b/stdout.log" 2> "$dir/b/stderr.log"
    local rc=$?
    echo "xsession B rc=$rc" >> "$PROGRESS"
    [ $rc -eq 0 ] || { echo "[k5b] FAIL xsession: B guest rc=$rc"; tail -5 "$dir/b/stderr.log"; return 1; }
    awk '/^KEYPROBE \{/{ sub(/^KEYPROBE /, ""); print }' "$dir/b/stdout.log" > "$dir/probe.jsonl"
    # B's own report is only meaningful while A is still alive: "absent" must
    # mean isolated, not deleted.
    if kill -0 "$a_pid" 2>/dev/null; then
        echo -e "xsession-a-alive-during-b\tyes\tpid=$a_pid" >> "$SUMMARY"
    else
        echo -e "xsession-a-alive-during-b\tno\tpid=$a_pid" >> "$SUMMARY"
        echo "[k5b] FAIL xsession: A had already exited while B probed"; rcx=1
    fi
    wait "$a_pid"; local a_rc=$?
    echo "xsession A rc=$a_rc" >> "$PROGRESS"
    [ $a_rc -eq 0 ] || { echo "[k5b] FAIL xsession: A guest rc=$a_rc"; rcx=1; }
    summarise "$dir/probe.jsonl" xsession || rcx=1
    return $rcx
}

# Item 3: with no keystore requested, a real app session keeps the honest
# failure. `acos launch` does not surface guest stdout, so the app's own checks
# are read from the session log buffer (`acos logs --tag KEYPROBE`).
phase_default_off() {
    local dir="$OUT/default-off"; rm -rf "$dir"; mkdir -p "$dir/runtime"
    export ACOS_RUNTIME_DIR="$dir/runtime" ACOS_SESSION_IDLE_SECS=90
    "$ACOS" session destroy > /dev/null 2>&1
    env -u ACOS_ART_KEYSTORE "$ACOS" launch "$APK" --engine art --keep --json \
        > "$dir/launch.json" 2> "$dir/launch.err"
    local rc=$?
    echo "default-off launch rc=$rc" >> "$PROGRESS"
    local rco=0
    "$ACOS" logs --session s_cli --tag KEYPROBE --json > "$dir/logs.json" 2> "$dir/logs.err" \
        || rco=1
    "$ACOS" session destroy > /dev/null 2>&1
    python3 "$WT/tools/keystore-k5b/extract.py" "$dir/logs.json" "$dir/default_off.jsonl" default-off \
        || rco=1
    [ $rc -eq 0 ] || rco=1
    [ -f "$dir/default_off.jsonl" ] && summarise "$dir/default_off.jsonl" default-off || rco=1
    return $rco
}

run_phase() {
    local name="$1" mem="$2" secs="$3"
    echo "[k5b] === phase $name ==="
    "$WT/tools/art-runtime/run-exclusive.sh" --task K5B -- \
        "$WT/tools/guard/guarded.sh" --mem "$mem" --time "$secs" -- \
        env ACOS_K5B_INLOCK=1 "$WT/tools/keystore-k5b/run.sh" "$OUT" "$name"
    local rc=$?
    if [ $rc -eq 0 ]; then echo -e "$name\tPASS\t$OUT/$name" >> "$SUMMARY"
    else echo -e "$name\tFAIL\t$OUT/$name" >> "$SUMMARY"; fi
    return $rc
}

fail=0
case "$PHASE" in
    all)
        [ -f "$APK" ] || { echo "[k5b] missing probe APK: $APK"; exit 2; }
        [ -x "$ACOS" ] || { echo "[k5b] missing acos: $ACOS"; exit 2; }
        [ -x "$ACOS_ART" ] || { echo "[k5b] missing acos-art: $ACOS_ART"; exit 2; }
        build_probe_dex || { echo "[k5b] cannot build the probe dex"; exit 2; }
        : > "$SUMMARY"
        echo "started_utc=$(date -u +%FT%TZ)" > "$PROGRESS"
        run_phase fidelity 8G 300 || fail=1
        run_phase xsession 10G 420 || fail=1
        run_phase default_off 8G 300 || fail=1
        echo; echo "[k5b] summary:"; cat "$SUMMARY"
        if [ $fail -ne 0 ]; then echo "[k5b] RESULT: FAIL"; exit 1; fi
        echo "[k5b] RESULT: PASS"
        ;;
    fidelity)
        if [ "${ACOS_K5B_INLOCK:-0}" = 1 ]; then phase_fidelity || fail=1
        else run_phase fidelity 8G 300 || fail=1; fi ;;
    xsession)
        if [ "${ACOS_K5B_INLOCK:-0}" = 1 ]; then phase_xsession || fail=1
        else run_phase xsession 10G 420 || fail=1; fi ;;
    default_off)
        if [ "${ACOS_K5B_INLOCK:-0}" = 1 ]; then phase_default_off || fail=1
        else run_phase default_off 8G 300 || fail=1; fi ;;
    *) echo "[k5b] unknown phase: $PHASE" >&2; exit 2 ;;
esac
if [ "$PHASE" != all ]; then
    if [ $fail -ne 0 ]; then echo "[k5b] RESULT ($PHASE): FAIL"; exit 1; fi
    echo "[k5b] RESULT ($PHASE): PASS"
fi
