#!/usr/bin/env bash
#
# Measure the per-call overhead of the MCP server over the bare `acos` CLI.
#
# For N=50 iterations it times:
#   * bare:  `acos inspect <apk> --json` (process start + inspect)
#   * mcp:   one `tools/call` round trip through a live acos-mcp stdio session
#
# All timing is wall-clock milliseconds around one call, using `date +%s%N`.
# Output is machine-readable `key=value` lines plus a human summary.
#
# Usage: crates/acos-mcp/tests/bench-overhead.sh [iterations]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

MCP_BIN="${ACOS_MCP_BIN:-$ROOT/target/debug/acos-mcp}"
[ -x "$MCP_BIN" ] || MCP_BIN="$ROOT/target/release/acos-mcp"
ACOS="${ACOS_BIN:-$ROOT/target/release/acos}"
[ -x "$ACOS" ] || ACOS="$ROOT/target/debug/acos"
[ -x "$MCP_BIN" ] || { echo "acos-mcp not built (cargo build -p acos-mcp)" >&2; exit 1; }
[ -x "$ACOS" ] || { echo "acos not built (cargo build --release -p acos-cli)" >&2; exit 1; }

APK="$ROOT/fixtures/build/apps/hello-views.apk"
[ -f "$APK" ] || { echo "missing fixture $APK" >&2; exit 1; }

N="${1:-50}"
WARMUP=3
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
export ACOS_BIN="$ACOS"
export ACOS_ALLOWED_ROOTS="$ROOT"
export ACOS_MCP_QUIET=1

now_ms() { echo $(( $(date +%s%N) / 1000000 )); }

percentile() { # <file> <p>
  local file="$1" p="$2" n rank
  n=$(wc -l < "$file")
  rank=$(awk -v p="$p" -v n="$n" 'BEGIN { r = int(p * n / 100 + 0.999999); if (r < 1) r = 1; if (r > n) r = n; print r }')
  sed -n "${rank}p" "$file"
}

stats() { # <file> <label>
  local file="$1" label="$2"
  sort -n "$file" -o "$file"
  local n p50 p95 mean
  n=$(wc -l < "$file")
  p50=$(percentile "$file" 50)
  p95=$(percentile "$file" 95)
  mean=$(awk '{ s += $1 } END { printf "%.1f", s / NR }' "$file")
  echo "${label}_n=$n"
  echo "${label}_p50_ms=$p50"
  echo "${label}_p95_ms=$p95"
  echo "${label}_mean_ms=$mean"
}

# --- bare CLI -----------------------------------------------------------------
: > "$TMP/bare.txt"
for _ in $(seq 1 $((WARMUP + N))); do
  start=$(now_ms)
  "$ACOS" inspect "$APK" --json > /dev/null
  end=$(now_ms)
  echo $((end - start)) >> "$TMP/bare.txt"
done
tail -n "$N" "$TMP/bare.txt" > "$TMP/bare.tmp" && mv "$TMP/bare.tmp" "$TMP/bare.txt"

# --- MCP round trips ----------------------------------------------------------
coproc MCP { "$MCP_BIN"; }
mcp_in="${MCP[1]}"
mcp_out="${MCP[0]}"

send() { printf '%s\n' "$1" >&"$mcp_in"; }
recv() { IFS= read -r line <&"$mcp_out" || return 1; }

send '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"bench","version":"0"}}}'
recv > /dev/null
send '{"jsonrpc":"2.0","method":"notifications/initialized"}'

: > "$TMP/mcp.txt"
for i in $(seq 1 $((WARMUP + N))); do
  start=$(now_ms)
  send "{\"jsonrpc\":\"2.0\",\"id\":$((i + 100)),\"method\":\"tools/call\",\"params\":{\"name\":\"acos_inspect\",\"arguments\":{\"base\":{\"request_id\":\"bench_$i\"},\"apk\":\"$APK\"}}}"
  recv > /dev/null
  end=$(now_ms)
  echo $((end - start)) >> "$TMP/mcp.txt"
done
tail -n "$N" "$TMP/mcp.txt" > "$TMP/mcp.tmp" && mv "$TMP/mcp.tmp" "$TMP/mcp.txt"

# Close stdin so the server shuts down.
exec {mcp_in}>&-
wait "$MCP_PID" 2>/dev/null || true

stats "$TMP/bare.txt" "bare"
stats "$TMP/mcp.txt" "mcp"

bare_p50=$(percentile "$TMP/bare.txt" 50)
mcp_p50=$(percentile "$TMP/mcp.txt" 50)
bare_p95=$(percentile "$TMP/bare.txt" 95)
mcp_p95=$(percentile "$TMP/mcp.txt" 95)
echo "overhead_p50_ms=$((mcp_p50 - bare_p50))"
echo "overhead_p95_ms=$((mcp_p95 - bare_p95))"
echo "fixture=$APK"
echo "iterations=$N"
echo "mcp_bin=$MCP_BIN"
echo "acos_bin=$ACOS"
