#!/usr/bin/env bash
#
# Plain-bash tests for tools/accept-bundle.sh. No frameworks, no new deps.
# Usage: tools/tests/test-accept-bundle.sh
#
# Each case runs the real tool against a throwaway git repo under .cache/tests/
# (scratch stays out of the shared tree, per OPERATING_DIRECTIVES §5).
set -uo pipefail
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
TOOL="$REPO/tools/accept-bundle.sh"
SCRATCH="$REPO/.cache/tests/accept-bundle.$$"
rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"
trap 'rm -rf "$SCRATCH"' EXIT

passes=0
failures=0
pass() { printf 'PASS %s\n' "$*"; passes=$((passes + 1)); }
fail() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }
check() { # check <description> <command...>
    local desc="$1"
    shift
    if "$@"; then pass "$desc"; else fail "$desc"; fi
}
has() { grep -q -- "$2" "$1"; }        # has <file> <BRE pattern>
has_re() { grep -qE -- "$2" "$1"; }      # has_re <file> <ERE pattern>
has_text() { grep -qF -- "$2" "$1"; }    # has_text <file> <literal text>
has_line() { grep -qx -- "$2" "$1"; }

new_repo() {
    local d="$1"
    mkdir -p "$d/crates/a/src" "$d/tools" "$d/docs"
    (
        cd "$d" || exit 1
        git init -q
        git config user.email test@example.invalid
        git config user.name test
        printf 'hello\n' >crates/a/file.txt
        printf '[workspace]\n' >Cargo.toml
        printf 'status\n' >STATUS.md
        printf 'directives\n' >docs/OPERATING_DIRECTIVES.md
        printf '# agent resources\n' >docs/AGENT_RESOURCES.md
        printf '#!/bin/sh\n' >tools/kept.sh
        printf '*.tmp\n' >.gitignore
        git add -A
        git -c core.hooksPath=/dev/null commit -qm init
    )
}

bundle_for() { # bundle_for <repo> <task>
    find "$1/results/bundles" -mindepth 1 -maxdepth 1 -type d -name "$2-*" | head -1
}

R="$SCRATCH/repo1"
new_repo "$R"
printf 'untracked\n' >"$R/tools/new-untracked.sh"

# ---------------------------------------------------------------- 1. positive
out="$SCRATCH/1.out"
err="$SCRATCH/1.err"
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" --task t-ok -- true ) >"$out" 2>"$err"
rc=$?
B="$(bundle_for "$R" t-ok)"
TXT="$B/bundle.txt"
check "positive: exit code 0"                 test "$rc" -eq 0
check "positive: bundle dir <task>-<UTCstamp>" bash -c '[[ "$1" =~ /t-ok-[0-9]{8}T[0-9]{6}Z$ ]]' _ "$B"
check "positive: task recorded"               has_line "$TXT" "task: t-ok"
check "positive: base commit recorded"        has "$TXT" "^base_commit: $(git -C "$R" rev-parse HEAD)\$"
check "positive: start status shows untracked" has "$TXT" '^?? tools/new-untracked.sh$'
check "positive: command recorded"            has_line "$TXT" "command: true"
check "positive: cwd recorded"                has "$TXT" "^cwd: $(cd "$R" && pwd -P)\$"
check "positive: UTC start recorded"          has_re "$TXT" '^started_utc: [0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'
check "positive: UTC finish recorded"         has_re "$TXT" '^finished_utc: [0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$'
check "positive: exit code recorded"          has_line "$TXT" "command_exit_code: 0"
check "positive: image digest none (no .cache)" has_line "$TXT" "image_manifest_digest: none"
check "positive: source digest stable"        bash -c '[ "$(sed -n "s/^source_digest_before: //p" "$1")" = "$(sed -n "s/^source_digest_after: //p" "$1")" ]' _ "$TXT"
check "positive: source_status OK"            has_line "$TXT" "source_status: OK"
check "positive: no SOURCE_CHANGED marker"    bash -c '! grep -q SOURCE_CHANGED_DURING_RUN "$1"' _ "$TXT"
LOG="$B/run.log"
check "positive: run.log exists"              test -f "$LOG"
check "positive: run.log sha256 recorded"     has_line "$TXT" "run_log_sha256: $(sha256sum "$LOG" | cut -d' ' -f1)"
check "positive: run.log path recorded"       has_line "$TXT" "run_log: results/bundles/$(basename "$B")/run.log"

# ------------------------------------------- 2. negative: source changed in run
out="$SCRATCH/2.out"
err="$SCRATCH/2.err"
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" --task t-change -- \
    bash -c 'printf "mutated\n" >> crates/a/file.txt; echo changed' ) >"$out" 2>"$err"
rc=$?
B="$(bundle_for "$R" t-change)"
TXT="$B/bundle.txt"
check "negative/change: tool exits with command rc 0" test "$rc" -eq 0
check "negative/change: marker present"       has_line "$TXT" "source_status: SOURCE_CHANGED_DURING_RUN"
check "negative/change: digests differ"       bash -c '[ "$(sed -n "s/^source_digest_before: //p" "$1")" != "$(sed -n "s/^source_digest_after: //p" "$1")" ]' _ "$TXT"
check "negative/change: end status recorded"  has_text "$TXT" '--- git status --porcelain (end) ---'
check "negative/change: warning on stderr"    has "$err" "SOURCE_CHANGED_DURING_RUN"

# --------------------------- 2b. negative: untracked source changed in run
out="$SCRATCH/2b.out"
err="$SCRATCH/2b.err"
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" --task t-untr -- \
    bash -c 'printf "untracked-mutated\n" >> tools/new-untracked.sh' ) >"$out" 2>"$err"
rc=$?
B="$(bundle_for "$R" t-untr)"
TXT="$B/bundle.txt"
check "negative/untracked: tool exits 0"      test "$rc" -eq 0
check "negative/untracked: marker present"    has_line "$TXT" "source_status: SOURCE_CHANGED_DURING_RUN"

# ------------- 2c. control: results/ and ignored files must not dirty sources
out="$SCRATCH/2c.out"
err="$SCRATCH/2c.err"
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" --task t-noise -- \
    bash -c 'mkdir -p results; printf "noise\n" > results/noise.txt; \
             printf "noise\n" > crates/a/notes.tmp' ) >"$out" 2>"$err"
rc=$?
B="$(bundle_for "$R" t-noise)"
TXT="$B/bundle.txt"
check "control: results/ + ignored writes exit 0" test "$rc" -eq 0
check "control: results/ + ignored writes stay OK" has_line "$TXT" "source_status: OK"

# ------------------------------------- 3. negative: nonzero command exit code
out="$SCRATCH/3.out"
err="$SCRATCH/3.err"
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" --task t-fail -- \
    bash -c 'echo to-stdout; echo to-stderr >&2; exit 7' ) >"$out" 2>"$err"
rc=$?
B="$(bundle_for "$R" t-fail)"
TXT="$B/bundle.txt"
check "negative/exit: tool propagates rc 7"   test "$rc" -eq 7
check "negative/exit: exit code recorded"     has_line "$TXT" "command_exit_code: 7"
check "negative/exit: stdout captured in log" has "$B/run.log" '^to-stdout$'
check "negative/exit: stderr captured in log" has "$B/run.log" '^to-stderr$'
check "negative/exit: stdout teed through"    has "$out" '^to-stdout$'
check "negative/exit: failures do not dirty source" has_line "$TXT" "source_status: OK"

# ---------------------------------------------- 4. image manifest digest recipe
mkdir -p "$R/.cache/art/sysroot/system/lib64"
printf 'android_api=34\nabi=x86_64\nsource_image=fake\n' >"$R/.cache/art/sysroot/READY"
printf './README\t3\tabcdef0123456789\n./system/lib64/libx.so\t42\t0123456789abcdef\n' \
    >"$R/.cache/art/sysroot/MANIFEST.txt"
printf 'libx\n' >"$R/.cache/art/sysroot/system/lib64/libx.so"
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" --task t-img -- true ) >"$SCRATCH/4.out" 2>"$SCRATCH/4.err"
rc=$?
B="$(bundle_for "$R" t-img)"
TXT="$B/bundle.txt"
expected="$( { cat "$R/.cache/art/sysroot/READY"; printf '\n--manifest--\n'; \
    cat "$R/.cache/art/sysroot/MANIFEST.txt"; } | sha256sum | cut -d' ' -f1 )"
check "image: exit 0"                         test "$rc" -eq 0
check "image: digest matches documented recipe" has_line "$TXT" "image_manifest_digest: $expected"
check "image: manifest source recorded"       has_line "$TXT" "image_manifest_source: MANIFEST.txt"
check "image: manifest file count recorded"   has_line "$TXT" "image_manifest_files: 2"
check "image: READY identity recorded"        has "$TXT" '^image_ready: android_api=34 abi=x86_64 source_image=fake$'
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" --task t-img2 -- true ) >/dev/null 2>&1
B2="$(bundle_for "$R" t-img2)"
check "image: digest deterministic across runs" \
    bash -c 'test "$(sed -n "s/^image_manifest_digest: //p" "$1")" = "$(sed -n "s/^image_manifest_digest: //p" "$2")"' \
    _ "$TXT" "$B2/bundle.txt"

# -------------------------------------------------------------- 5. usage errors
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" -- true ) >/dev/null 2>&1
check "usage: missing --task exits 2"         test $? -eq 2
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" --task bad/name -- true ) >/dev/null 2>&1
check "usage: bad task name exits 2"          test $? -eq 2
( cd "$R" && ACOS_REPO_ROOT="$R" "$TOOL" --task t-nocmd -- ) >/dev/null 2>&1
check "usage: missing command exits 2"        test $? -eq 2

printf '\n%s passed, %s failed\n' "$passes" "$failures"
[ "$failures" -eq 0 ]
