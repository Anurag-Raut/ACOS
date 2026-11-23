#!/usr/bin/env bash
#
# accept-bundle.sh -- run one command and record a verifiable evidence bundle.
#
# Usage:
#   tools/verify/accept-bundle.sh --task NAME -- <cmd...>
#
# Creates results/bundles/<task>-<UTCstamp>/ containing:
#   bundle.txt   task, base commit, `git status --porcelain`, source digest
#                (before/after), image-manifest digest, command, cwd, exit code,
#                UTC start/finish, run-log path+sha256, and a
#                SOURCE_CHANGED_DURING_RUN marker when the command edited sources.
#   run.log      combined stdout+stderr of the command.
#
# The source digest covers `git diff HEAD` limited to crates/ tools/ scenarios/
# docs/ plus the contents of untracked, non-ignored files in those same dirs.
# results/, fixtures/, research/ and .cache/ are deliberately excluded so that
# command output does not make the evidence look dirty.
#
# This tool NEVER launches ART itself. Callers that do wrap it:
#   tools/art-runtime/run-exclusive.sh --task NAME -- \
#       tools/verify/accept-bundle.sh --task NAME -- tools/guard/guarded.sh --mem 8G <cmd>
#
# Env: ACOS_REPO_ROOT  repo root override (used by tools/tests only).
#
# Exit: the command's exit code (0..255). A set SOURCE_CHANGED_DURING_RUN marker
# is reported on stderr and in bundle.txt; the caller decides whether to trust it.
set -uo pipefail

ROOT="${ACOS_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

usage() {
    sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

die() { printf 'accept-bundle: %s\n' "$*" >&2; exit 2; }

task=""
while [ $# -gt 0 ]; do
    case "$1" in
        --task) [ $# -ge 2 ] || die "--task needs a value"; task="$2"; shift 2 ;;
        --)     shift; break ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument before --: $1" ;;
    esac
done

[ -n "$task" ] || { usage >&2; die "--task NAME is required"; }
case "$task" in
    *[!A-Za-z0-9._-]*) die "task name must match [A-Za-z0-9._-]+ (got: $task)" ;;
esac
[ $# -gt 0 ] || { usage >&2; die "no command given after --"; }

command -v git >/dev/null 2>&1 || die "git not found"
command -v sha256sum >/dev/null 2>&1 || die "sha256sum not found"
git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || die "not a git repo: $ROOT"

SRC_DIRS=(crates tools scenarios docs)

source_digest() {
    local root="$1"
    {
        # Tracked changes (staged + unstaged) relative to HEAD, source dirs only.
        git -C "$root" -c core.quotepath=false diff HEAD --no-color --no-ext-diff \
            --no-textconv --binary -- "${SRC_DIRS[@]}"
        # Untracked, non-ignored files: path + mode + content hash, sorted.
        git -C "$root" ls-files --others --exclude-standard -z -- "${SRC_DIRS[@]}" \
            | LC_ALL=C sort -z \
            | while IFS= read -r -d '' f; do
                  printf 'untracked %s ' "$f"
                  stat -c '%a' -- "$root/$f" 2>/dev/null || printf '?'
                  printf ' '
                  sha256sum -- "$root/$f" 2>/dev/null | cut -d' ' -f1 || printf 'missing'
                  printf '\n'
              done
    } | sha256sum | cut -d' ' -f1
}

# Cheap image identity: READY (api/abi/source/extracted_at) plus the generated
# sysroot MANIFEST (sorted path + size + sha256[:16]); fall back to a sorted
# path+size listing if the manifest is absent.
image_manifest_digest() {
    local sys="$1/.cache/art/sysroot"
    [ -d "$sys" ] || { printf 'none'; return; }
    {
        [ -f "$sys/READY" ] && cat "$sys/READY"
        printf '\n--manifest--\n'
        if [ -f "$sys/MANIFEST.txt" ]; then
            cat "$sys/MANIFEST.txt"
        else
            ( cd "$sys" && find . -type f -printf './%P\t%s\n' | LC_ALL=C sort )
        fi
    } | sha256sum | cut -d' ' -f1
}

base_commit="$(git -C "$ROOT" rev-parse HEAD)" || die "cannot resolve HEAD"
status_start="$(git -C "$ROOT" status --porcelain)"
src_before="$(source_digest "$ROOT")"
img_digest="$(image_manifest_digest "$ROOT")"

sysroot="$ROOT/.cache/art/sysroot"
if [ -d "$sysroot" ]; then
    if [ -f "$sysroot/MANIFEST.txt" ]; then
        img_source="MANIFEST.txt"
        img_files="$(grep -c '^\./' "$sysroot/MANIFEST.txt" 2>/dev/null || true)"
    else
        img_source="find"
        img_files="$(find "$sysroot" -type f | wc -l | tr -d ' ')"
    fi
    img_ready="$(tr '\n' ' ' < "$sysroot/READY" 2>/dev/null | sed 's/ *$//')"
else
    img_source="none"
    img_files="0"
    img_ready="none"
fi

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
bundle="$ROOT/results/bundles/$task-$stamp"
n=1
while [ -e "$bundle" ]; do
    bundle="$ROOT/results/bundles/$task-$stamp-$n"
    n=$((n + 1))
done
mkdir -p "$bundle" || die "cannot create $bundle"
bundle_txt="$bundle/bundle.txt"
log="$bundle/run.log"
log_rel="${log#"$ROOT"/}"
cmd_str="$(printf '%q ' "$@")"
cmd_str="${cmd_str% }"
cwd="$(pwd -P)"
started="$(date -u +%FT%TZ)"

{
    printf 'bundle_version: 1\n'
    printf 'task: %s\n' "$task"
    printf 'base_commit: %s\n' "$base_commit"
    printf '\n--- git status --porcelain (start) ---\n'
    if [ -n "$status_start" ]; then printf '%s\n' "$status_start"; else printf '(clean)\n'; fi
    printf -- '--- end git status ---\n'
    printf 'source_digest_before: %s\n' "$src_before"
    printf 'image_manifest_digest: %s\n' "$img_digest"
    printf 'image_manifest_source: %s\n' "$img_source"
    printf 'image_manifest_files: %s\n' "$img_files"
    printf 'image_ready: %s\n' "$img_ready"
    printf 'command: %s\n' "$cmd_str"
    printf 'cwd: %s\n' "$cwd"
    printf 'started_utc: %s\n' "$started"
} >"$bundle_txt"

"$@" 2>&1 | tee "$log"
rc=${PIPESTATUS[0]}
finished="$(date -u +%FT%TZ)"
log_sha="$(sha256sum "$log" | cut -d' ' -f1)"
src_after="$(source_digest "$ROOT")"

{
    printf 'command_exit_code: %s\n' "$rc"
    printf 'finished_utc: %s\n' "$finished"
    printf 'run_log: %s\n' "$log_rel"
    printf 'run_log_sha256: %s\n' "$log_sha"
    printf 'source_digest_after: %s\n' "$src_after"
    if [ "$src_before" = "$src_after" ]; then
        printf 'source_status: OK\n'
    else
        printf 'source_status: SOURCE_CHANGED_DURING_RUN\n'
        printf '\n--- git status --porcelain (end) ---\n'
        git -C "$ROOT" status --porcelain
        printf -- '--- end git status ---\n'
    fi
} >>"$bundle_txt"

if [ "$src_before" != "$src_after" ]; then
    printf 'accept-bundle: WARNING: SOURCE_CHANGED_DURING_RUN (source digest %s -> %s)\n' \
        "$src_before" "$src_after" >&2
fi
printf 'accept-bundle: bundle=%s exit_code=%s source_status=%s\n' \
    "${bundle#"$ROOT"/}" "$rc" \
    "$([ "$src_before" = "$src_after" ] && printf OK || printf SOURCE_CHANGED_DURING_RUN)" >&2

exit "$rc"
