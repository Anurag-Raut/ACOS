#!/usr/bin/env bash
# FB1 live smoke: installed acos-mcp (scratch prefix) drives Gaten search by content_desc + acos_text.
set -uo pipefail
WT=/home/Qui/.paseo/worktrees/3oxa6xao/s3ab
cd "$WT/.cache/fb1/outside" || exit 2
env -i HOME="$HOME" USER="$USER" LANG="${LANG:-C.UTF-8}" PATH="$PATH" \
  XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-}" \
  ACOS_ALLOWED_ROOTS=/home/Qui ACOS_ARTIFACTS_DIR="$WT/.cache/fb1/outside/artifacts" \
  python3 -I "$WT/tools/release/mcp_smoke.py" \
    --mcp-bin "$WT/.cache/fb1/prefix/bin/acos-mcp" \
    --apk /home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk \
    --mode gaten-search --timeout 600 \
    > "$WT/.cache/fb1/mcp/gaten-search.jsonl" 2> "$WT/.cache/fb1/mcp/gaten-search.err"
echo "exit=$?"
