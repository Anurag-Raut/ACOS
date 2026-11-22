#!/usr/bin/env bash
#
# Fetch and SHA-256-verify the CameraX Maven dependency closure into
# `.cache/maven-camerax/` (from Google Maven and Maven Central only).
#
# Usage:
#   tools/fetch-camerax-deps.sh               # download missing + verify all sha256
#   tools/fetch-camerax-deps.sh --verify-only # offline check used by build scripts
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/fixtures/apps/camerax-probe/DEPS.md"
CACHE_DIR="$ROOT/.cache/maven-camerax"

VERIFY_ONLY=0
if [ "${1:-}" = "--verify-only" ]; then
    VERIFY_ONLY=1
elif [ $# -gt 0 ]; then
    echo "usage: $0 [--verify-only]" >&2
    exit 2
fi

[ -f "$MANIFEST" ] || { echo "fetch-camerax-deps: missing manifest $MANIFEST" >&2; exit 1; }

if [ "$VERIFY_ONLY" = 1 ] && [ ! -d "$CACHE_DIR" ]; then
    cat >&2 <<MSG
camerax-probe: error: missing CameraX Maven cache at $CACHE_DIR
Run: tools/fetch-camerax-deps.sh
(Requires one-time network access to dl.google.com and repo1.maven.org, ~6 MiB total, SHA-256 verified against fixtures/apps/camerax-probe/DEPS.md.)
MSG
    exit 1
fi

mkdir -p "$CACHE_DIR"

python3 - "$MANIFEST" "$CACHE_DIR" "$VERIFY_ONLY" <<'PY'
import hashlib, os, re, sys, urllib.request

manifest_path, cache_dir, verify_only = sys.argv[1], sys.argv[2], sys.argv[3] == "1"

row_re = re.compile(
    r"^\|\s*`([^`]+)`\s*\|\s*`([^`]+)`\s*\|\s*`([^`]+)`\s*\|\s*(\d+)\s*\|\s*`([0-9a-f]{64})`\s*\|\s*`([^`]+)`\s*\|\s*(https://\S+)\s*\|$"
)

entries = []
with open(manifest_path, "r", encoding="utf-8") as f:
    for line in f:
        m = row_re.match(line.strip())
        if m:
            entries.append(m.groups())

if not entries:
    print(f"fetch-camerax-deps: no artifact rows parsed from {manifest_path}", file=sys.stderr)
    sys.exit(1)

missing = []
for coord, ver, ext, size_s, expected_sha, fname, url in entries:
    expected_size = int(size_s)
    dest = os.path.join(cache_dir, fname)
    if not os.path.isfile(dest):
        if verify_only:
            missing.append(fname)
            continue
        print(f"fetch-camerax-deps: downloading {fname} ({expected_size} B)")
        with urllib.request.urlopen(url, timeout=60) as resp:
            data = resp.read()
        with open(dest + ".part", "wb") as out:
            out.write(data)
        os.rename(dest + ".part", dest)
    with open(dest, "rb") as inp:
        data = inp.read()
    actual_sha = hashlib.sha256(data).hexdigest()
    if len(data) != expected_size or actual_sha != expected_sha:
        if verify_only:
            print(
                f"camerax-probe: error: corrupted or mismatched artifact {dest}\n"
                f"  expected size={expected_size} sha256={expected_sha}\n"
                f"  actual   size={len(data)} sha256={actual_sha}\n"
                f"Re-run: rm -rf {cache_dir} && tools/fetch-camerax-deps.sh",
                file=sys.stderr,
            )
            sys.exit(1)
        os.remove(dest)
        print(
            f"fetch-camerax-deps: SHA-256 mismatch for {fname}: expected {expected_sha}, got {actual_sha}",
            file=sys.stderr,
        )
        sys.exit(1)

if missing:
    print(
        f"camerax-probe: error: missing {len(missing)} artifact(s) in {cache_dir} (first: {missing[0]})\n"
        f"Run: tools/fetch-camerax-deps.sh",
        file=sys.stderr,
    )
    sys.exit(1)

print(f"fetch-camerax-deps: verified {len(entries)} artifacts in {cache_dir}")
PY
