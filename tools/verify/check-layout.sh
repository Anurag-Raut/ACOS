#!/usr/bin/env bash
#
# Repo-layout guard: keeps the tree tidy so it does not drift back.
#
#   1. tools/ holds no loose files (only README.md); scripts live in a topic folder.
#   2. The repo root holds only the allowed top-level markdown files.
#   3. No untracked, non-ignored junk at the repo root.
#   4. results/ tracks only reports/harness code/allow-listed inputs (no heavy
#      evidence), and out/ is not tracked; see results/README.md and the bottom
#      of .gitignore.
#   5. Every `docs|tools|research/...` path named in tools/README.md exists.
#
# Usage: tools/verify/check-layout.sh      (exit 0 = clean, 1 = violations listed)
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
fail=0
bad() { echo "check-layout: $*" >&2; fail=1; }

# 1. loose files in tools/
while IFS= read -r f; do
    [ "$f" = "tools/README.md" ] || bad "loose file in tools/: $f (move it into a topic folder, see tools/README.md)"
done < <(git ls-files 'tools/*' | awk -F/ 'NF==2')

# 2. root markdown allow-list
ALLOWED_ROOT_MD=" README.md AGENTS.md CLAUDE.md STATUS.md BENCHMARKS.md COMPATIBILITY.md DECISIONS.md EXPERIMENTS.md "
while IFS= read -r f; do
    case "$ALLOWED_ROOT_MD" in *" $f "*) ;; *) bad "unexpected root file: $f (docs belong under docs/)";; esac
done < <(git ls-files '*.md' | grep -v '/')

# 3. untracked junk at the root
while IFS= read -r f; do bad "untracked file at repo root: $f"; done \
    < <(git ls-files -o --exclude-standard | grep -v '/' || true)

# 4. results/ and out/: heavy or evidence-type files must not be tracked
while IFS= read -r f; do bad "tracked run output under out/: $f"; done < <(git ls-files out)
while IFS= read -r f; do
    case "$f" in
        results/parity/w193a/counter.png|results/render/corpus/*|results/aidl/*|results/wm/*|results/res/*|results/art-runtime/evidence/W31-artifacts/gaten-release-evidence/*) continue ;;
    esac
    case "$f" in
        *.png|*.log|*.err|*.marker|*.jsonl|*.gz|*.zst|*.tar) bad "tracked evidence file in results/: $f (keep it local; summarise in a .md)";;
    esac
    sz=$(stat -c %s "$f" 2>/dev/null || echo 0)
    [ "$sz" -le 2097152 ] || bad "tracked file over 2 MiB in results/: $f"
done < <(git ls-files results)

# 5. README paths exist
for idx in tools/README.md; do
    [ -f "$idx" ] || { bad "missing $idx"; continue; }
    while IFS= read -r p; do
        p="${p%[.,;:)]}"
        case "$p" in *'*'*|*'<'*|*'{'*) continue;; esac
        [ -e "$p" ] || bad "$idx names a missing path: $p"
    done < <(grep -o -E '`(docs|tools|research)/[A-Za-z0-9_./-]+`' "$idx" | tr -d '`' | sort -u)
done

[ "$fail" = 0 ] && echo "check-layout: ok"
exit "$fail"
