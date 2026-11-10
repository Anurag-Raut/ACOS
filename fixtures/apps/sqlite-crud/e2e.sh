#!/usr/bin/env bash
# e2e-style observation for sqlite-crud: build is separate, this only launches and
# records how far the ACOS runtime gets (see tools/e2e-dogfood.sh).
# Usage: fixtures/apps/sqlite-crud/e2e.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
exec "$ROOT/tools/e2e-dogfood.sh" sqlite-crud
