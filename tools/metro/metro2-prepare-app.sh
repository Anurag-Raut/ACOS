#!/usr/bin/env bash
#
# METRO2c — build the scratch Metro project the dev-loop drivers edit.
#
#   tools/metro/metro2-prepare-app.sh <dest>      # default .cache/metro1/app2
#
# `<dest>` is a hard-link farm of `.cache/rn-0.81.4/jsproject` (node_modules is
# shared, nothing is copied) whose edited files are REAL COPIES, so an edit
# never writes through to the shared cache. The fixture's single-file
# `index.js` is split the way React Native's own template is laid out:
#
#   App.js    exports the component (a Fast Refresh boundary)
#   index.js  registers it
#
# That split is what makes a component edit take RN's recommended path (Fast
# Refresh over `/hot`: same JS context, no teardown race) instead of a full
# reload; see results/handoff/METRO2/REPORT.md.
set -eu
WT=$(cd "$(dirname "$0")/../.." && pwd)
SRC=$WT/.cache/rn-0.81.4/jsproject
DEST=${1:-$WT/.cache/metro1/app2}
[ -d "$SRC" ] || { echo "missing $SRC (tools/build/build-rn-hello.sh fetch)" >&2; exit 2; }
rm -rf "$DEST"
mkdir -p "$(dirname "$DEST")"
cp -al "$SRC" "$DEST"
python3 - "$DEST" "$WT/fixtures/apps/rn-hello/index.js" <<'PY'
import os, sys
dest, fixture = sys.argv[1], sys.argv[2]
s = open(fixture).read()
i = s.index("function RnHello")
j = s.index("AppRegistry.registerComponent")
head, comp = s[:i], s[i:j]
head = head.replace("  AppRegistry,\n", "")
for name, body in (
    ("App.js", head + comp + "module.exports = {__esModule: true, default: RnHello};\n"),
    ("index.js", "const {AppRegistry} = require('react-native');\n"
                 "const App = require('./App').default;\n"
                 "AppRegistry.registerComponent('rnhello', () => App);\n"),
):
    p = os.path.join(dest, name)
    if os.path.exists(p):
        os.remove(p)          # break the hard link before writing
    open(p, "w").write(body)
print("prepared", dest)
PY
