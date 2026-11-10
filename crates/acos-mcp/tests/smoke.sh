#!/usr/bin/env bash
#
# Manual MCP conformance smoke test: drives the real acos-mcp binary with
# printf'd JSON-RPC lines and checks the responses with grep. No jq, no python.
#
# Usage:  crates/acos-mcp/tests/smoke.sh
# Env:    ACOS_MCP_BIN (default: target/debug/acos-mcp, else target/release)
#         ACOS_BIN     (default: target/release/acos, else target/debug)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

MCP_BIN="${ACOS_MCP_BIN:-$ROOT/target/debug/acos-mcp}"
if [ ! -x "$MCP_BIN" ]; then
  MCP_BIN="$ROOT/target/release/acos-mcp"
fi
if [ ! -x "$MCP_BIN" ]; then
  echo "smoke: acos-mcp binary not found; run: cargo build -p acos-mcp" >&2
  exit 1
fi

ACOS="${ACOS_BIN:-$ROOT/target/release/acos}"
if [ ! -x "$ACOS" ]; then
  ACOS="$ROOT/target/debug/acos"
fi
if [ ! -x "$ACOS" ]; then
  echo "smoke: acos binary not found; run: cargo build --release -p acos-cli" >&2
  exit 1
fi

export ACOS_BIN="$ACOS"
export ACOS_ALLOWED_ROOTS="$ROOT"
export ACOS_MCP_QUIET=1

APK="$ROOT/fixtures/build/apps/hello-views.apk"
DEX="$ROOT/fixtures/build/dexprogs/01_hello/classes.dex"
if [ ! -f "$APK" ] || [ ! -f "$DEX" ]; then
  echo "smoke: fixtures are missing; run tools/build-apps.sh and tools/build-fixtures.sh" >&2
  exit 1
fi

responses="$(
  {
    printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}'
    printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
    printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"ping","params":{}}'
    printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}'
    printf '%s\n' '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"acos_capabilities","arguments":{}}}'
    printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"acos_inspect\",\"arguments\":{\"base\":{\"request_id\":\"smoke_inspect\"},\"apk\":\"$APK\"}}}"
    printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"tools/call\",\"params\":{\"name\":\"acos_run_dex\",\"arguments\":{\"base\":{\"request_id\":\"smoke_rundex\"},\"path\":\"$DEX\",\"main\":\"Main\"}}}"
  } | "$MCP_BIN"
)"

echo "$responses"

fail=0
check() {
  if ! grep -q "$1" <<<"$responses"; then
    echo "smoke: FAIL missing: $1" >&2
    fail=1
  fi
}

check '"serverInfo":{"name":"acos-mcp"'
check '"result":{}'
check '"name":"acos_inspect"'
check '"command":"inspect"'
check '"available":true'
check '"status":"pass"'
check '"fidelity":"static-analysis"'
check '"fidelity":"native-runtime"'
check 'Hello, ACOS!'

if [ "$fail" -ne 0 ]; then
  echo "smoke: FAILED" >&2
  exit 1
fi
echo "smoke: OK"
