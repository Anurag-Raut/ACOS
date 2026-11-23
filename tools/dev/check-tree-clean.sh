#!/usr/bin/env bash
#
# check-tree-clean.sh -- a test suite must leave the checkout's TRACKED files
# byte-identical.
#
#   tools/dev/check-tree-clean.sh [--base FILE]
#
# Tests used to publish their evidence into the tracked `results/` tree
# (`results/flutter/`, `results/rn/`, `results/render/`, `results/vm/`,
# `results/aidl/`), so a `cargo test --workspace` run rewrote committed evidence
# and left `git status` dirty; the SCENHYG suite check is what caught it. The
# writers now go through `acos_test_support::test_artifacts_dir`
# (`$ACOS_TEST_ARTIFACTS` or `<checkout>/.cache/test-artifacts/<suite>`).
#
# What this checks: no *tracked* file under `results/`, `scenarios/`, `docs/`,
# `crates/`, `fixtures/` or `tools/` is modified, deleted or added by the suite.
# Untracked scratch under `.cache/` is fine and is not reported. A deliberate
# edit to tracked files is not a test side effect, so pass `--base FILE` with a
# `git status --porcelain` snapshot taken before the suite: then only *new*
# changes are failures.
#
# Exit 0: clean. Exit 1: the suite changed tracked files (each one printed).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT" || exit 2

BASE=""
while [ $# -gt 0 ]; do
    case "$1" in
        --base) BASE="$2"; shift 2 ;;
        -h|--help) sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "check-tree-clean: unknown argument $1" >&2; exit 2 ;;
    esac
done

# Tracked paths a test suite must never rewrite. Untracked files (`??`) and
# ignored paths are not test side effects on committed evidence.
now="$(git status --porcelain --untracked-files=no -- results scenarios docs crates fixtures tools DECISIONS.md EXPERIMENTS.md)"

if [ -n "$BASE" ]; then
    [ -f "$BASE" ] || { echo "check-tree-clean: no snapshot at $BASE" >&2; exit 2; }
    before="$(cat "$BASE")"
    # Compare lines: anything in `now` that was not already in `before` is new.
    changed="$(comm -13 <(printf '%s\n' "$before" | sort) <(printf '%s\n' "$now" | sort) | sed '/^$/d')"
else
    changed="$now"
fi

if [ -n "$changed" ]; then
    echo "check-tree-clean: the suite changed tracked files:" >&2
    printf '%s\n' "$changed" >&2
    echo "" >&2
    echo "Point the writer at acos_test_support::test_artifacts_dir (never results/)." >&2
    exit 1
fi
echo "check-tree-clean: clean ($(git rev-parse --short HEAD), $(git status --porcelain --untracked-files=no | wc -l) tracked change(s) present before/after)"
exit 0
