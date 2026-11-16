set -x
export ACOS_ART_BIN=$PWD/.cache/rel3k2-target/release/acos-art
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
unset ACOS_HOME
ACOS=.cache/rel3k2-target/release/acos
APK=fixtures/build/apps/back-layers.apk
OUT=.cache/rel3k2/probe
rm -rf $OUT && mkdir -p $OUT
$ACOS serve --socket $PWD/$OUT/serve.sock --idle-timeout 120 --owner-pid $$ &
SERVER=$!
sleep 2
export ACOS_SESSION_SOCKET=$PWD/$OUT/serve.sock
$ACOS launch --keep --engine art --json "$APK" > $OUT/1-launch.json 2>$OUT/1-launch.err; echo "launch rc=$?"
$ACOS observe --tree --json > $OUT/2-tree.json 2>&1; echo "tree rc=$?"
$ACOS tap --id dialog --json > $OUT/3-tap-dialog.json 2>&1; echo "tap rc=$?"
$ACOS observe --tree --json > $OUT/4-tree-dialog.json 2>&1
$ACOS logs --tag AcosBack --min-priority V --json > $OUT/5-logs-dialog.json 2>&1
$ACOS key back --json > $OUT/6-back-dialog.json 2>&1; echo "back rc=$?"
$ACOS observe --tree --json > $OUT/7-tree-after-back.json 2>&1
$ACOS logs --tag AcosBack --min-priority V --json > $OUT/8-logs-after-back.json 2>&1
$ACOS tap --id input --json > $OUT/9-tap-input.json 2>&1; echo "tap input rc=$?"
$ACOS logs --tag AcosBack --min-priority V --json > $OUT/10-logs-input.json 2>&1
$ACOS key back --json > $OUT/11-back-ime.json 2>&1; echo "back2 rc=$?"
$ACOS observe --tree --json > $OUT/12-tree-after-back-ime.json 2>&1
$ACOS logs --tag AcosBack --min-priority V --json > $OUT/13-logs-after-back-ime.json 2>&1
$ACOS key back --json > $OUT/14-back-last.json 2>&1; echo "back3 rc=$?"
$ACOS logs --tag AcosBack --min-priority V --json > $OUT/15-logs-after-last.json 2>&1
$ACOS session destroy --session s_cli > $OUT/16-destroy.txt 2>&1
kill $SERVER 2>/dev/null
wait $SERVER 2>/dev/null
echo DONE
