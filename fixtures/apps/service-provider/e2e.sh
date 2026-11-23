#!/usr/bin/env bash
# e2e-style observation for service-provider: build is separate, this only launches and
# records how far the ACOS runtime gets (see tools/verify/e2e-dogfood.sh).
# Usage: fixtures/apps/service-provider/e2e.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
exec "$ROOT/tools/verify/e2e-dogfood.sh" service-provider
