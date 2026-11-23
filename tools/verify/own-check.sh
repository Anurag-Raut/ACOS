#!/usr/bin/env bash
#
# own-check.sh -- fail when a change touches files outside an agent's ownership.
#
# Usage:
#   tools/verify/own-check.sh --own <glob>... [--base <commit>]
#
# Lists every changed and untracked (non-ignored) file since --base (default:
# HEAD, i.e. working-tree changes only) and exits 1 if any path is:
#   * outside every --own glob, or
#   * integrator-owned: Cargo.toml, Cargo.lock, STATUS.md,
#     docs/OPERATING_DIRECTIVES.md, crates/*/Cargo.toml, **/lib.rs
#     (matched by filename only; lib.rs contents are not inspected).
#
# Globs use bash `case` pattern semantics: `*` also crosses `/`. A glob without
# wildcards matches an exact path or a directory prefix (e.g. `tools/tests`).
#
# Exit: 0 = all changed files owned; 1 = offenders (printed); 2 = usage error.
#
# Env: ACOS_REPO_ROOT  repo root override (used by tools/tests only).
set -uo pipefail

ROOT="${ACOS_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

usage() {
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

die() { printf 'own-check: %s\n' "$*" >&2; exit 2; }

owns=()
base="HEAD"
while [ $# -gt 0 ]; do
    case "$1" in
        --own)
            shift
            n=${#owns[@]}
            while [ $# -gt 0 ] && [ "${1#--}" = "$1" ]; do
                owns+=("$1"); shift
            done
            [ ${#owns[@]} -gt "$n" ] || die "--own needs at least one glob"
            ;;
        --base) [ $# -ge 2 ] || die "--base needs a commit"; base="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[ ${#owns[@]} -gt 0 ] || { usage >&2; die "--own <glob>... is required"; }
git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || die "not a git repo: $ROOT"
base_sha="$(git -C "$ROOT" rev-parse --verify --quiet "$base^{commit}")" \
    || die "cannot resolve --base $base"

is_integrator_owned() {
    local p="$1"
    case "$p" in
        Cargo.toml|Cargo.lock|STATUS.md|docs/OPERATING_DIRECTIVES.md) return 0 ;;
    esac
    [[ "$p" =~ ^crates/[^/]+/Cargo\.toml$ ]] && return 0
    [[ "$p" == "lib.rs" || "$p" == */lib.rs ]] && return 0
    return 1
}

matches_own() {
    local p="$1" g
    for g in "${owns[@]}"; do
        g="${g#./}"
        if [[ "$g" == *[\*\?\[]* ]]; then
            [[ "$p" == $g ]] && return 0
            # `dir/` means the directory subtree.
            [[ "$g" == */ && "$p" == "${g}"* ]] && return 0
        else
            [[ "$p" == "$g" || "$p" == "$g"/* ]] && return 0
        fi
    done
    return 1
}

mapfile -d '' -t files < <(
    {
        git -C "$ROOT" diff -z --name-only --diff-filter=ACMRDTUXB "$base_sha" --
        git -C "$ROOT" ls-files -z --others --exclude-standard
    } | LC_ALL=C sort -zu
)

ok=0
offenders=()
printf 'own-check: repo=%s base=%s (%s)\n' "$ROOT" "$base" "$base_sha"
printf 'own-check: own globs:'
printf ' %q' "${owns[@]}"
printf '\n'

if [ ${#files[@]} -eq 0 ]; then
    printf 'own-check: no changed or untracked files; PASS\n'
    exit 0
fi

printf 'own-check: changed/untracked files (%s):\n' "${#files[@]}"
for p in "${files[@]}"; do
    if is_integrator_owned "$p"; then
        printf '  INTEGRATOR-OWNED  %s\n' "$p"
        offenders+=("$p")
    elif matches_own "$p"; then
        printf '  ok                %s\n' "$p"
        ok=$((ok + 1))
    else
        printf '  OUTSIDE-OWN       %s\n' "$p"
        offenders+=("$p")
    fi
done

if [ ${#offenders[@]} -gt 0 ]; then
    printf 'own-check: FAIL -- %s offender(s) outside ownership or integrator-owned:\n' \
        "${#offenders[@]}"
    printf '  %s\n' "${offenders[@]}"
    exit 1
fi

printf 'own-check: PASS -- %s changed file(s), all within ownership\n' "$ok"
exit 0
