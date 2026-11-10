#!/usr/bin/env bash
# Test double that detects concurrent execution with `mkdir` as an atomic lock.
# Each invocation appends `start` (lock acquired) or `overlap` (lock held) to
# $ACOS_TEST_MARKER, then emits a valid ACOS JSON envelope.
set -u

case "${1:-}" in
  --help)
    printf 'Usage: fake <COMMAND>\n\nCommands:\n  inspect  fake inspect\n  run-dex  fake run-dex\n'
    exit 0
    ;;
  --version)
    printf 'fake 0.1.0\n'
    exit 0
    ;;
esac

marker="${ACOS_TEST_MARKER:?ACOS_TEST_MARKER must be set}"
lock="$marker.lock"
if mkdir "$lock" 2>/dev/null; then
  printf 'start\n' >> "$marker"
  sleep "${ACOS_TEST_SLEEP_SECS:-0.3}"
  rmdir "$lock"
else
  printf 'overlap\n' >> "$marker"
fi

printf '%s' '{"request_id":"req_fake","ok":true,"status":"pass","backend":"acos-native","fidelity":"static-analysis","capabilities_used":["apk.inspect"],"error":null,"result":{"fake":true},"metrics":{"wall_ms":1}}'
