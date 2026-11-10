#!/usr/bin/env bash
# Test double for the `acos` binary: answers `--help`/`--version` immediately
# (so the capability probe succeeds and `inspect` becomes "available"), then
# sleeps on any real command so cancellation/timeout can be exercised.
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

sleep "${ACOS_TEST_SLEEP_SECS:-30}"
exit 0
