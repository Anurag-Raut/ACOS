#!/usr/bin/env bash
# Register the ACOS MCP server with coding agents.
#
#   tools/release/register-mcp.sh [--scope user|local|project] [--acos-bin PATH]
#                                 [--acos-mcp-bin PATH] [--allowed-roots PATH[:PATH...]]
#                                 [--dry-run]
#
# What it does:
#   * `claude mcp add` (Claude Code) with the ACOS binaries and the roots an
#     agent may read APKs/scenarios from;
#   * prints the equivalent `mcpServers` JSON for every other MCP client.
#
# Why ACOS_ALLOWED_ROOTS defaults to $HOME: the server refuses every path
# argument that does not resolve under an allowed root (`UNSAFE_PATH`). An agent
# working in a *different* repository (for example ~/Projects/gaten) has to pass
# that repo's APK and scenario paths, so a single project root is not enough.
# $HOME covers every repository the user owns; narrow it with --allowed-roots if
# the host holds code that should stay out of reach.
#
# The script never registers anything without `--dry-run` being absent, and it
# never edits files.
set -euo pipefail

scope=user
acos_bin=""
acos_mcp_bin=""
allowed_roots="${ACOS_ALLOWED_ROOTS:-$HOME}"
dry_run=0
artifacts_dir=""

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --scope) scope=${2:?}; shift 2 ;;
    --acos-bin) acos_bin=${2:?}; shift 2 ;;
    --acos-mcp-bin) acos_mcp_bin=${2:?}; shift 2 ;;
    --allowed-roots) allowed_roots=${2:?}; shift 2 ;;
    --artifacts-dir) artifacts_dir=${2:?}; shift 2 ;;
    --dry-run) dry_run=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "register-mcp.sh: unknown argument $1" >&2; usage 2 ;;
  esac
done

case "$scope" in user|local|project) ;; *) echo "register-mcp.sh: --scope must be user, local or project" >&2; exit 2 ;; esac

# Locate the binaries: explicit flag, $ACOS_BIN/$ACOS_MCP_BIN, next to this
# script's checkout, then PATH.
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
if [ -z "$acos_bin" ]; then
  if [ -n "${ACOS_BIN:-}" ]; then acos_bin=$ACOS_BIN
  elif [ -x "$here/target/release/acos" ]; then acos_bin=$here/target/release/acos
  elif command -v acos >/dev/null 2>&1; then acos_bin=$(command -v acos)
  else echo "register-mcp.sh: cannot find the acos binary (pass --acos-bin)" >&2; exit 2; fi
fi
if [ -z "$acos_mcp_bin" ]; then
  if [ -n "${ACOS_MCP_BIN:-}" ]; then acos_mcp_bin=$ACOS_MCP_BIN
  elif [ -x "$here/target/release/acos-mcp" ]; then acos_mcp_bin=$here/target/release/acos-mcp
  elif command -v acos-mcp >/dev/null 2>&1; then acos_mcp_bin=$(command -v acos-mcp)
  else echo "register-mcp.sh: cannot find the acos-mcp binary (pass --acos-mcp-bin)" >&2; exit 2; fi
fi
for bin in "$acos_bin" "$acos_mcp_bin"; do
  [ -x "$bin" ] || { echo "register-mcp.sh: $bin is not executable" >&2; exit 2; }
  # A relative path would break the moment the client starts the server from
  # another directory, so resolve it now.
  bin=$(cd -- "$(dirname -- "$bin")" && pwd)/$(basename -- "$bin")
done
acos_bin=$(cd -- "$(dirname -- "$acos_bin")" && pwd)/$(basename -- "$acos_bin")
acos_mcp_bin=$(cd -- "$(dirname -- "$acos_mcp_bin")" && pwd)/$(basename -- "$acos_mcp_bin")

# The artifacts directory must exist before the server starts: it is added to
# the allowed roots, and a missing root cannot be canonicalised.
[ -n "$artifacts_dir" ] || artifacts_dir="$HOME/.acos-mcp-artifacts"
mkdir -p "$artifacts_dir"

echo "acos      : $acos_bin"
echo "acos-mcp  : $acos_mcp_bin"
echo "roots     : $allowed_roots"
echo "artifacts : $artifacts_dir"
echo "scope     : $scope"
echo

cmd=(claude mcp add --scope "$scope" acos
     --env "ACOS_BIN=$acos_bin"
     --env "ACOS_ALLOWED_ROOTS=$allowed_roots"
     --env "ACOS_ARTIFACTS_DIR=$artifacts_dir"
     -- "$acos_mcp_bin")

if [ "$dry_run" = 1 ]; then
  printf 'dry run: '
  printf '%q ' "${cmd[@]}"
  printf '\n\n'
elif command -v claude >/dev/null 2>&1; then
  # Replace any previous registration so the paths are never stale.
  claude mcp remove acos >/dev/null 2>&1 || true
  "${cmd[@]}"
  echo
  echo "registered; check with: claude mcp list"
  echo
else
  echo "claude CLI not found: not registered. Run this by hand when it is installed:" >&2
  printf '  '
  printf '%q ' "${cmd[@]}"
  printf '\n\n'
fi

cat <<JSON
Other MCP clients (Claude Desktop, Cursor, Cline, Continue, Zed, ...) accept an
mcpServers entry like this:

{
  "mcpServers": {
    "acos": {
      "command": "$acos_mcp_bin",
      "args": [],
      "env": {
        "ACOS_BIN": "$acos_bin",
        "ACOS_ALLOWED_ROOTS": "$allowed_roots",
        "ACOS_ARTIFACTS_DIR": "$artifacts_dir"
      }
    }
  }
}

Then ask the agent to call acos_help (or read the resource acos://guide) before
its first ACOS call: it returns the whole agent guide.
JSON
