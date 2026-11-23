#!/usr/bin/env bash
#
# Plain-bash tests for tools/verify/own-check.sh. No frameworks, no new deps.
# Usage: tools/tests/test-own-check.sh
#
# Each case runs the real tool against a throwaway git repo under .cache/tests/.
set -uo pipefail
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
TOOL="$REPO/tools/verify/own-check.sh"
SCRATCH="$REPO/.cache/tests/own-check.$$"
rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"
trap 'rm -rf "$SCRATCH"' EXIT

passes=0
failures=0
pass() { printf 'PASS %s\n' "$*"; passes=$((passes + 1)); }
fail() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }
check() { local desc="$1"; shift; if "$@"; then pass "$desc"; else fail "$desc"; fi; }

new_repo() {
    local d="$1"
    mkdir -p "$d/crates/a/src" "$d/tools" "$d/docs"
    (
        cd "$d" || exit 1
        git init -q
        git config user.email test@example.invalid
        git config user.name test
        printf 'hello\n' >crates/a/file.txt
        printf '[package]\n' >Cargo.toml
        printf '[package]\n' >crates/a/Cargo.toml
        printf 'pub mod a;\n' >crates/a/src/lib.rs
        printf 'status\n' >STATUS.md
        printf 'directives\n' >docs/OPERATING_DIRECTIVES.md
        printf '# agent resources\n' >docs/AGENT_RESOURCES.md
        printf '#!/bin/sh\n' >tools/kept.sh
        printf '*.tmp\n' >.gitignore
        git add -A
        git -c core.hooksPath=/dev/null commit -qm init
    )
}

R="$SCRATCH/repo1"
new_repo "$R"
last_out="$SCRATCH/last.out"
last_err="$SCRATCH/last.err"

run_oc() { # run_oc <repo> [args...]
    local repo="$1"
    shift
    ( cd "$repo" && ACOS_REPO_ROOT="$repo" "$TOOL" "$@" ) >"$last_out" 2>"$last_err"
}
has() { grep -q -- "$2" "$1"; }
has_line() { grep -qx -- "$2" "$1"; }

# -------------------------------------- 1. positive: owned modified + untracked
printf 'changed\n' >>"$R/tools/kept.sh"
printf '#!/bin/sh\n' >"$R/tools/new.sh"
printf 'ignored\n' >"$R/crates/a/notes.tmp"     # ignored: must not be listed
run_oc "$R" --own 'tools/*'
rc=$?
check "owned: exit 0"                         test "$rc" -eq 0
check "owned: PASS line printed"              has "$last_out" "own-check: PASS"
check "owned: owned file listed"              has "$last_out" "^  ok                tools/kept.sh$"
check "owned: untracked owned file listed"    has "$last_out" "^  ok                tools/new.sh$"
check "owned: ignored file not listed"        bash -c '! grep -q notes.tmp "$1"' _ "$last_out"

# ---------------------------------------------- 2. negative: outside ownership
printf 'code\n' >"$R/crates/a/src/main.rs"     # untracked, not in tools/*
run_oc "$R" --own 'tools/*'
rc=$?
check "outside: exit 1"                       test "$rc" -eq 1
check "outside: offender printed"             has "$last_out" "^  OUTSIDE-OWN       crates/a/src/main.rs$"
check "outside: FAIL summary printed"         has "$last_out" "own-check: FAIL"
check "outside: offender named in summary"    has "$last_out" "^  crates/a/src/main.rs$"

# ------------------------------ 3. negative: integrator-owned beats broad globs
printf 'x\n' >>"$R/STATUS.md"
run_oc "$R" --own '*'
rc=$?
check "integrator: STATUS.md flagged even with --own '*'" test "$rc" -eq 1
check "integrator: STATUS.md classified"      has "$last_out" "^  INTEGRATOR-OWNED  STATUS.md$"

printf 'x\n' >>"$R/docs/OPERATING_DIRECTIVES.md"
run_oc "$R" --own 'docs/*'
check "integrator: docs/OPERATING_DIRECTIVES.md flagged" test $? -eq 1
check "integrator: directives classified"     has "$last_out" "^  INTEGRATOR-OWNED  docs/OPERATING_DIRECTIVES.md$"

printf 'x\n' >>"$R/Cargo.toml"
run_oc "$R" --own '*'
check "integrator: root Cargo.toml flagged"   test $? -eq 1

printf 'x\n' >>"$R/crates/a/Cargo.toml"
run_oc "$R" --own 'crates/*'
check "integrator: crates/*/Cargo.toml flagged" test $? -eq 1
check "integrator: crate manifest classified" has "$last_out" "^  INTEGRATOR-OWNED  crates/a/Cargo.toml$"

printf 'pub mod b;\n' >>"$R/crates/a/src/lib.rs"
run_oc "$R" --own 'crates/*'
check "integrator: lib.rs flagged by filename" test $? -eq 1
check "integrator: lib.rs classified"         has "$last_out" "^  INTEGRATOR-OWNED  crates/a/src/lib.rs$"

# -------------------------------------------- 4. deletions and --base semantics
rm "$R/tools/new.sh"                            # deletion inside ownership
run_oc "$R" --own 'tools/*' --own 'STATUS.md' --own 'docs/*' --own 'Cargo.toml' \
    --own 'crates/*/Cargo.toml' --own 'crates/a/file.txt'
# Remaining offenders are the integrator files touched above; that is expected.
check "deletion: owned deletion not the offender" \
    bash -c '! grep -q "tools/new.sh" "$1"' _ "$last_out"

R2="$SCRATCH/repo2"
new_repo "$R2"
printf 'committed change\n' >>"$R2/tools/kept.sh"
( cd "$R2" && git add tools/kept.sh && git -c core.hooksPath=/dev/null commit -qm own-change )
run_oc "$R2" --own 'tools/*' --base HEAD~1
check "base: committed owned change passes"   test $? -eq 0
check "base: changed file listed"             has "$last_out" "tools/kept.sh"

printf 'committed change\n' >>"$R2/crates/a/file.txt"
( cd "$R2" && git add crates/a/file.txt && git -c core.hooksPath=/dev/null commit -qm outside-change )
run_oc "$R2" --own 'tools/*' --base HEAD~2
rc=$?
check "base: committed outside change fails"  test "$rc" -eq 1
check "base: committed offender printed"      has "$last_out" "^  OUTSIDE-OWN       crates/a/file.txt$"

# ------------------------------------------------ 5. clean tree and usage errors
R3="$SCRATCH/repo3"
new_repo "$R3"
run_oc "$R3" --own 'tools/*'
rc=$?
check "clean: no changes exits 0"             test "$rc" -eq 0
check "clean: reports no changed files"       has "$last_out" "no changed or untracked files"

run_oc "$R3" --base HEAD
check "usage: missing --own exits 2"          test $? -eq 2
run_oc "$R3" --own 'tools/*' --base does-not-exist
check "usage: bad --base exits 2"             test $? -eq 2
( cd "$R3" && "$TOOL" --own 'tools/*' --bogus ) >/dev/null 2>&1
check "usage: unknown flag exits 2"           test $? -eq 2

printf '\n%s passed, %s failed\n' "$passes" "$failures"
[ "$failures" -eq 0 ]
