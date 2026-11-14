#!/usr/bin/env bash
# F11 test double: an `acos` whose `verify` reply carries a huge error tail
# (the shape a failing RN verify produced: a java tail plus a native tail,
# repeated in the top-level error, the step error and the verdict failure).
#
# It answers --help/--version like the real CLI so the capability probe marks
# `verify` available, and prints $ACOS_STUB_JSON for everything else.
set -u

case "${1:-}" in
  --help)
    cat <<'HELP'
ACOS usage

Commands:
  inspect      Inspect an APK (static only)
  verify       Run a scenario and produce artifacts
  run          Run an APK

Options:
HELP
    exit 0
    ;;
  --version)
    printf 'acos 0.2.0\n'
    exit 0
    ;;
esac

# Record the argv a test wants to inspect (F10: how `flags` reach the CLI).
if [ -n "${ACOS_STUB_ARGV:-}" ]; then
  printf '%s\n' "$*" >>"$ACOS_STUB_ARGV"
fi

cat "${ACOS_STUB_JSON:?ACOS_STUB_JSON must point at the envelope}"
