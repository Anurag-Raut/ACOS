#!/usr/bin/env bash
#
# Produce a small, human-readable MCP session transcript against the real
# acos-mcp binary: initialize, tools/list (truncated), inspect, run-dex.
#
# Usage: crates/acos-mcp/tests/transcript.sh
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

MCP_BIN="${ACOS_MCP_BIN:-$ROOT/target/debug/acos-mcp}"
[ -x "$MCP_BIN" ] || MCP_BIN="$ROOT/target/release/acos-mcp"
ACOS="${ACOS_BIN:-$ROOT/target/release/acos}"
[ -x "$ACOS" ] || ACOS="$ROOT/target/debug/acos"

APK="$ROOT/fixtures/build/apps/hello-views.apk"
DEX="$ROOT/fixtures/build/dexprogs/01_hello/classes.dex"

export ACOS_BIN="$ACOS"
export ACOS_ALLOWED_ROOTS="$ROOT"
export ACOS_MCP_QUIET=1

coproc MCP { "$MCP_BIN"; }
mcp_in="${MCP[1]}"
mcp_out="${MCP[0]}"

send() { printf '%s\n' "$1" >&"$mcp_in"; }

# roundtrip <label> <request-json> [truncate-chars]
roundtrip() {
  local label="$1" request="$2" limit="${3:-0}" response count
  echo "--> $label"
  send "$request"
  IFS= read -r response <&"$mcp_out" || true
  if [ "$limit" -gt 0 ] && [ "${#response}" -gt "$limit" ]; then
    count=$(grep -o '"name":"acos_' <<<"$response" | wc -l || true)
    if [ "$count" -gt 0 ]; then
      echo "<-- ${response:0:$limit}… (truncated; response is ${#response} bytes, $count tools)"
    else
      echo "<-- ${response:0:$limit}… (truncated; response is ${#response} bytes)"
    fi
  else
    echo "<-- $response"
  fi
  echo
}

echo "# ACOS MCP session transcript"
echo
echo "server: $MCP_BIN"
echo "backend: $ACOS"
echo

roundtrip "initialize (protocolVersion 2025-06-18)" \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"transcript","version":"0"}}}'

send '{"jsonrpc":"2.0","method":"notifications/initialized"}'

roundtrip "tools/list" \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' 320

roundtrip "tools/call acos_inspect" \
  "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"acos_inspect\",\"arguments\":{\"base\":{\"request_id\":\"tr_inspect\"},\"apk\":\"$APK\"}}}" 900

roundtrip "tools/call acos_run_dex" \
  "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"tools/call\",\"params\":{\"name\":\"acos_run_dex\",\"arguments\":{\"base\":{\"request_id\":\"tr_rundex\"},\"path\":\"$DEX\",\"main\":\"Main\"}}}" 900

roundtrip "tools/call acos_verify (unimplemented -> unsupported)" \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"acos_verify","arguments":{"base":{"request_id":"tr_verify"},"scenario":"s.yaml","artifacts":"out"}}}' 700

exec {mcp_in}>&-
wait "$MCP_PID" 2>/dev/null || true
