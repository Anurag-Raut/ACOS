#!/usr/bin/env bash
#
# status-check.sh -- fail when STATUS.md's recorded HEAD is stale.
#
# `STATUS.md` starts with a header naming the commit it was written at, e.g.
#
#   **As of 2026-11-11, HEAD `2c682c7`** (...)
#
# Docs drift: a status file can claim a state that is no longer true. This
# checker makes the claim falsifiable: the recorded short-sha must name an
# ancestor of the current `git HEAD`, within `ACOS_STATUS_MAX_LAG` commits
# (default 3). A missing/unknown/non-ancestor/too-far-behind hash exits 1.
#
# Usage:
#   tools/verify/status-check.sh [STATUS.md]
#
# Env:
#   ACOS_STATUS_MAX_LAG   allowed commits behind (default 3)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

STATUS="${1:-STATUS.md}"
MAX_LAG="${ACOS_STATUS_MAX_LAG:-3}"

if [ ! -f "$STATUS" ]; then
    echo "status-check: FAIL: $STATUS not found" >&2
    exit 1
fi

# First `HEAD \`<sha>\`` in the header (first 15 lines) is the claim.
claimed="$(head -n 15 "$STATUS" | grep -m1 -oE 'HEAD `[0-9a-f]{7,40}`' | grep -oE '[0-9a-f]{7,40}' || true)"
if [ -z "$claimed" ]; then
    echo "status-check: FAIL: no 'HEAD \`<sha>\`' claim in the first 15 lines of $STATUS" >&2
    exit 1
fi

head_sha="$(git rev-parse HEAD)"

if [ "$claimed" = "$head_sha" ]; then
    echo "status-check: OK: $STATUS HEAD $claimed == git HEAD"
    exit 0
fi

if ! git cat-file -e "${claimed}^{commit}" 2>/dev/null; then
    echo "status-check: FAIL: $STATUS names unknown commit $claimed" >&2
    exit 1
fi

if ! git merge-base --is-ancestor "$claimed" "$head_sha" 2>/dev/null; then
    echo "status-check: FAIL: $STATUS HEAD $claimed is not an ancestor of git HEAD $head_sha" >&2
    exit 1
fi

behind="$(git rev-list --count "$claimed..$head_sha")"
if [ "$behind" -gt "$MAX_LAG" ]; then
    echo "status-check: FAIL: $STATUS HEAD $claimed is $behind commits behind git HEAD $head_sha (max $MAX_LAG)" >&2
    exit 1
fi

echo "status-check: OK: $STATUS HEAD $claimed is $behind commit(s) behind git HEAD $head_sha (<= $MAX_LAG)"
