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
#   6. New files (added since the merge base with $ACOS_LAYOUT_BASE, default
#      origin/main) obey the commit policy in AGENTS.md "What may be committed":
#      no binaries/images anywhere, no run output (repro.sh, artifacts/,
#      verdict/logs/captures), and under results/ only `.md` reports. A path
#      the owner approved goes in tools/verify/layout-allow.txt (one glob per line).
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

# 6. commit policy for NEW files (see AGENTS.md "What may be committed")
base_ref="${ACOS_LAYOUT_BASE:-origin/main}"
if base=$(git merge-base HEAD "$base_ref" 2>/dev/null); then
    allow_file=tools/verify/layout-allow.txt
    allowed() {
        [ -f "$allow_file" ] || return 1
        local g
        while IFS= read -r g; do
            case "$g" in ''|'#'*) continue;; esac
            # shellcheck disable=SC2254
            case "$1" in $g) return 0;; esac
        done < "$allow_file"
        return 1
    }
    while IFS= read -r f; do
        allowed "$f" && continue
        case "$f" in
            *.png|*.jpg|*.jpeg|*.gif|*.webp|*.bmp|*.ico|*.apk|*.aab|*.jar|*.aar|*.dex|*.so|*.a|*.o|*.zip|*.gz|*.zst|*.tar|*.xz|*.7z|*.bin|*.img|*.mp4|*.webm|*.mov|*.wav|*.mp3|*.ttf|*.otf|*.wasm|*.class)
                bad "new binary file: $f (generate it at build/test time instead, or get owner approval in $allow_file)";;
        esac
        case "$f" in
            */repro.sh|repro.sh|*/artifacts/*|*/verdict.json|*.verdict.json|*/events.jsonl|*/logcat.txt)
                bad "new run output: $f (keep run output local; summarise in one .md report)";;
        esac
        case "$f" in
            results/*.md) ;;
            results/*) bad "new non-report file under results/: $f (results/ takes one .md report per task; archive raw evidence outside git)";;
        esac
    done < <(git diff --name-only --diff-filter=A "$base" HEAD)
else
    echo "check-layout: note: no merge base with $base_ref; skipped new-file policy (rule 6)" >&2
fi

[ "$fail" = 0 ] && echo "check-layout: ok"
exit "$fail"
