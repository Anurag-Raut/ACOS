#!/usr/bin/env bash
set -uo pipefail
WT=/home/Qui/.paseo/worktrees/3oxa6xao/s3ab
cd "$WT/.cache/fb1/outside" || exit 2
env -i HOME="$HOME" USER="$USER" LANG="${LANG:-C.UTF-8}" PATH="$PATH" \
  XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-}" \
  ACOS_ALLOWED_ROOTS=/home/Qui ACOS_ARTIFACTS_DIR="$WT/.cache/fb1/outside/artifacts" \
  python3 -I "$WT/tools/release/mcp_smoke.py" \
    --mcp-bin "$WT/.cache/fb1/prefix/bin/acos-mcp" \
    --apk /home/Qui/Projects/acos/fixtures/build/apps/counter/base.apk \
    --mode counter --timeout 600 \
    > "$WT/.cache/fb1/mcp/counter.jsonl" 2> "$WT/.cache/fb1/mcp/counter.err"
echo "exit=$?"
