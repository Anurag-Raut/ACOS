#!/usr/bin/env bash
# e2e-style observation for two-activity: build is separate, this only launches and
# records how far the ACOS runtime gets (see tools/e2e-dogfood.sh).
# Usage: fixtures/apps/two-activity/e2e.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
exec "$ROOT/tools/e2e-dogfood.sh" two-activity
