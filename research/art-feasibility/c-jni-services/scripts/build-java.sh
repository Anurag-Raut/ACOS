#!/usr/bin/env bash
# build-java.sh -- compile the probe classes with the host JDK and d8 them into
# a single classes.dex, exactly like fixtures/ does.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXPDIR="$(cd "$HERE/.." && pwd)"
ACOS_ROOT="$(cd "$EXPDIR/../../.." && pwd)"
SCRATCH="$ACOS_ROOT/.cache/art-exp-c"
mkdir -p "$SCRATCH/java/classes" "$ACOS_ROOT/.cache/art-exp/data/dex/artjni"

D8="${D8:-$HOME/Android/Sdk/build-tools/35.0.0/d8}"
[ -x "$D8" ] || D8="$(ls -d "$HOME"/Android/Sdk/build-tools/*/d8 2>/dev/null | tail -1)"
command -v javac >/dev/null || { echo "javac missing" >&2; exit 1; }

javac -Xlint:-options --release 11 -encoding UTF-8 -d "$SCRATCH/java/classes" \
      "$EXPDIR"/java/dev/acos/artprobe/*.java
# d8 needs a jar or class files; use the class dir
"$D8" --min-api 34 --output "$SCRATCH/java/classes" "$SCRATCH/java/classes"/*.class \
      >/dev/null 2>&1 || "$D8" --min-api 34 --output "$SCRATCH/java/classes" \
      $(find "$SCRATCH/java/classes" -name '*.class')
cp -f "$SCRATCH/java/classes/classes.dex" "$ACOS_ROOT/.cache/art-exp/data/dex/artjni/classes.dex"
echo "built $(stat -c%s "$ACOS_ROOT/.cache/art-exp/data/dex/artjni/classes.dex") byte dex"
