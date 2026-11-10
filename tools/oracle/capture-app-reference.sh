#!/usr/bin/env bash
#
# capture-app-reference.sh — capture authoritative `art-reference` evidence for
# the fixture APKs on the API-34 emulator.
#
# For hello-views and hello-jni it records, under
# fixtures/reference/<app>/art-api34-x86_64/:
#   * am_start_*.txt        cold-start timings (ThisTime/TotalTime/WaitTime)
#   * screenshot_*.png      screencap PNGs
#   * ui_*.xml              uiautomator hierarchy dumps (texts/ids/bounds/classes)
#   * logcat_*.txt          lifecycle/native log lines (AcosFixture/AcosJniFixture)
#   * meminfo_*.txt         dumpsys meminfo PSS breakdown
#   * display.txt           dumpsys window display size/density
#   * observed-texts.txt    key node texts extracted from the UI dumps
#   * manifest.json         artifact list + device config + exact commands
#
# hello-views is driven through: type "World", tap Greet (expect
# "Hello, World! clicks: 1"), tap again (clicks: 2), rotate to force a
# recreate (verifies onSaveInstanceState), then Back.
#
# The emulator must already be running (tools/oracle/emulator.sh start).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SDK="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-/home/Qui/Android/Sdk}}"
ADB="$SDK/platform-tools/adb"
SERIAL="${ACOS_ORACLE_SERIAL:-emulator-5554}"
REF_ROOT="$ROOT/fixtures/reference"
APPS_DIR="$ROOT/fixtures/build/apps"
FIDELITY="art-reference"
SUB="art-api34-x86_64"
PY="$(command -v python3)"

HELLO_VIEWS_PKG="dev.acos.fixtures.helloviews"
HELLO_JNI_PKG="dev.acos.fixtures.hellojni"

log() { printf '[capture-app-ref] %s\n' "$*"; }
die() { printf '[capture-app-ref] error: %s\n' "$*" >&2; exit 1; }

"$ADB" -s "$SERIAL" get-state >/dev/null 2>&1 || die "no device at $SERIAL (start it: tools/oracle/emulator.sh start)"

# ---------------------------------------------------------------------------
# primitives
# ---------------------------------------------------------------------------
CMDLOG=""
run() { # log + run an adb command (stdout only)
    CMDLOG+="\$ $*"$'\n'
    "$@"
}
sh_cmd() { # run an adb shell command, logging it
    CMDLOG+="\$ adb -s $SERIAL shell $*"$'\n'
    "$ADB" -s "$SERIAL" shell "$@"
}

wait_resumed() { # <pkg> [timeout_s]
    local pkg="$1" timeout="${2:-30}" i=0
    while :; do
        if "$ADB" -s "$SERIAL" shell dumpsys window 2>/dev/null \
            | grep -q "mCurrentFocus=.*$pkg"; then
            return 0
        fi
        i=$((i + 1))
        [ "$i" -ge "$((timeout * 2))" ] && return 1
        sleep 0.5
    done
}

screencap() { # <dest>
    CMDLOG+="\$ adb -s $SERIAL exec-out screencap -p > $1"$'\n'
    "$ADB" -s "$SERIAL" exec-out screencap -p >"$1"
}

dump_ui() { # <dest> [retries]
    local dest="$1" retries="${2:-4}" i
    for i in $(seq 1 "$retries"); do
        CMDLOG+="\$ adb -s $SERIAL shell uiautomator dump /sdcard/window_dump.xml"$'\n'
        "$ADB" -s "$SERIAL" shell uiautomator dump /sdcard/window_dump.xml >/dev/null 2>&1 || true
        CMDLOG+="\$ adb -s $SERIAL exec-out cat /sdcard/window_dump.xml > $dest"$'\n'
        "$ADB" -s "$SERIAL" exec-out cat /sdcard/window_dump.xml >"$dest" 2>/dev/null || true
        if grep -q '<hierarchy' "$dest" 2>/dev/null; then
            return 0
        fi
        sleep 1
    done
    die "uiautomator dump failed for $dest"
}

node_center() { # <xml> <resource-id-suffix>  -> "x y"
    "$PY" - "$1" "$2" <<'PYEOF'
import re, sys
xml_path, suffix = sys.argv[1], sys.argv[2]
data = open(xml_path, encoding="utf-8", errors="replace").read()
pat = re.compile(r'<node\b[^>]*>')
for m in pat.finditer(data):
    tag = m.group(0)
    rid = re.search(r'resource-id="([^"]*)"', tag)
    if not rid or not rid.group(1).endswith(suffix):
        continue
    b = re.search(r'bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"', tag)
    if not b:
        continue
    x1, y1, x2, y2 = map(int, b.groups())
    print((x1 + x2) // 2, (y1 + y2) // 2)
    sys.exit(0)
sys.exit("node not found: " + suffix)
PYEOF
}

tap_id() { # <xml> <suffix>
    local xy
    xy="$(node_center "$1" "$2")" || die "node not found in $1: $2"
    CMDLOG+="\$ adb -s $SERIAL shell input tap $xy"$'\n'
    "$ADB" -s "$SERIAL" shell input tap $xy
}

ui_texts() { # <xml> -> newline list "class|resource-id|text|bounds"
    "$PY" - "$1" <<'PYEOF'
import re, sys, html
data = open(sys.argv[1], encoding="utf-8", errors="replace").read()
for m in re.finditer(r'<node\b[^>]*>', data):
    tag = m.group(0)
    def g(k):
        mm = re.search(k + r'="([^"]*)"', tag)
        return html.unescape(mm.group(1)) if mm else ""
    text = g("text")
    desc = g("content-desc")
    if text or desc:
        print("%s | %s | %s%s | %s" % (g("class"), g("resource-id"), text, ("/"+desc if desc else ""), g("bounds")))
PYEOF
}

ui_state_json() { # <xml> <dest.json>
    "$PY" - "$1" "$2" <<'PYEOF'
import json, sys, xml.etree.ElementTree as ET
root = ET.parse(sys.argv[1]).getroot()
nodes = []
for n in root.iter('node'):
    text = n.get('text', '')
    rid = n.get('resource-id', '')
    desc = n.get('content-desc', '')
    if not text and not rid and not desc:
        continue
    nodes.append({
        "class": n.get('class', ''),
        "resource_id": rid,
        "text": text,
        "content_desc": desc,
        "bounds": n.get('bounds', ''),
        "clickable": n.get('clickable', '') == 'true',
        "focused": n.get('focused', '') == 'true',
        "enabled": n.get('enabled', '') == 'true',
    })
with open(sys.argv[2], 'w') as f:
    json.dump(nodes, f, indent=2)
    f.write('\n')
PYEOF
}

assert_node_text() { # <xml> <resource-id-suffix> <expected>
    "$PY" - "$1" "$2" "$3" <<'PYEOF' || die "assertion failed: $2 text != '$3'"
import sys, xml.etree.ElementTree as ET
root = ET.parse(sys.argv[1]).getroot()
for n in root.iter('node'):
    if n.get('resource-id', '').endswith(sys.argv[2]) and n.get('text', '') == sys.argv[3]:
        sys.exit(0)
sys.exit(1)
PYEOF
}

assert_contains() { # <xml> <resource-id-suffix|-> <substring>
    "$PY" - "$1" "$2" "$3" <<'PYEOF' || die "assertion failed: '$3' not found in $1"
import sys, xml.etree.ElementTree as ET
root = ET.parse(sys.argv[1]).getroot()
suffix, needle = sys.argv[2], sys.argv[3]
for n in root.iter('node'):
    rid = n.get('resource-id', '')
    if suffix != '-' and not rid.endswith(suffix):
        continue
    if needle in n.get('text', ''):
        sys.exit(0)
sys.exit(1)
PYEOF
}

capture_common() { # <pkg> <tag> <outdir>
    local pkg="$1" tag="$2" out="$3"
    sh_cmd am force-stop "$pkg" >/dev/null 2>&1 || true
    sh_cmd logcat -c >/dev/null 2>&1 || true
    sleep 1
    CMDLOG+="\$ adb -s $SERIAL shell am start -W -n $pkg/.MainActivity -> am_start_initial.txt"$'\n'
    "$ADB" -s "$SERIAL" shell am start -W -n "$pkg/.MainActivity" >"$out/am_start_initial.txt" 2>&1 || true
    wait_resumed "$pkg" 30 || log "warning: $pkg did not report resumed"
    sleep 1
    screencap "$out/screenshot_initial.png"
    dump_ui "$out/ui_initial.xml"
    CMDLOG+="\$ adb -s $SERIAL shell dumpsys window displays > $out/display.txt"$'\n'
    "$ADB" -s "$SERIAL" shell dumpsys window displays >"$out/display.txt" 2>&1 || true
    CMDLOG+="\$ adb -s $SERIAL shell dumpsys meminfo $pkg > $out/meminfo_initial.txt"$'\n'
    "$ADB" -s "$SERIAL" shell dumpsys meminfo "$pkg" >"$out/meminfo_initial.txt" 2>&1 || true
    CMDLOG+="\$ adb -s $SERIAL shell logcat -d -v threadtime -s $tag:I > $out/logcat_initial.txt"$'\n'
    "$ADB" -s "$SERIAL" shell logcat -d -v threadtime -s "$tag:I" >"$out/logcat_initial.txt" 2>&1 || true
    ui_texts "$out/ui_initial.xml" >"$out/observed-texts-initial.txt"
    ui_state_json "$out/ui_initial.xml" "$out/ui_initial.json"
}

device_config_json() { # <outdir>
    local out="$1" size density locale fontscale night buildfp api abi
    size="$("$ADB" -s "$SERIAL" shell wm size 2>/dev/null | tr -d '\r' | tr '\n' ' ')"
    density="$("$ADB" -s "$SERIAL" shell wm density 2>/dev/null | tr -d '\r' | tr '\n' ' ')"
    locale="$("$ADB" -s "$SERIAL" shell getprop ro.product.locale 2>/dev/null | tr -d '\r')"
    [ -n "$locale" ] || locale="$("$ADB" -s "$SERIAL" shell getprop persist.sys.locale 2>/dev/null | tr -d '\r')"
    fontscale="$("$ADB" -s "$SERIAL" shell settings get system font_scale 2>/dev/null | tr -d '\r')"
    [ "$fontscale" = "null" ] && fontscale="1.0 (default; setting unset)"
    night="$("$ADB" -s "$SERIAL" shell cmd uimode night 2>/dev/null | tr -d '\r')"
    buildfp="$("$ADB" -s "$SERIAL" shell getprop ro.build.fingerprint 2>/dev/null | tr -d '\r')"
    api="$("$ADB" -s "$SERIAL" shell getprop ro.build.version.sdk 2>/dev/null | tr -d '\r')"
    abi="$("$ADB" -s "$SERIAL" shell getprop ro.product.cpu.abi 2>/dev/null | tr -d '\r')"
    "$PY" - "$out/device-config.json" "$size" "$density" "$locale" "$fontscale" "$night" "$buildfp" "$api" "$abi" <<'PYEOF'
import json, re, sys
path = sys.argv[1]
size, density, locale, fontscale, night, buildfp, api, abi = sys.argv[2:10]
m = re.search(r'(\d+)x(\d+)', size)
d = re.search(r'(\d+)', density)
doc = {
    "resolution": "%sx%s" % m.groups() if m else size.strip(),
    "density_dpi": int(d.group(1)) if d else None,
    "locale": locale or "(unset)",
    "font_scale": fontscale,
    "dark_light": night.replace("Night mode: ", ""),
    "build_fingerprint": buildfp,
    "api": api,
    "abi": abi,
    "serial": "emulator-5554",
    "emulator": "qemu-system-x86_64-headless, KVM, 2048 MB RAM, 4 cores, swiftshader_indirect",
    "raw_wm_size": size.strip(),
    "raw_wm_density": density.strip(),
}
with open(path, "w") as f:
    json.dump(doc, f, indent=2)
    f.write("\n")
PYEOF
}

write_manifest() { # <app> <outdir> <apk>
    "$PY" - "$REF_ROOT/$1/$SUB" "$3" "$1" "$FIDELITY" <<'PYEOF'
import hashlib, json, os, sys
outdir, apk, app, fidelity = sys.argv[1:5]

def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(1 << 20), b""):
            h.update(c)
    return h.hexdigest()

arts = []
for name in sorted(os.listdir(outdir)):
    if name == "manifest.json":
        continue
    p = os.path.join(outdir, name)
    if os.path.isfile(p):
        arts.append({"path": name, "size": os.path.getsize(p), "sha256": sha(p)})

manifest = {
    "fidelity": fidelity,
    "app": app,
    "device_dir": "art-api34-x86_64",
    "apk": os.path.relpath(apk, os.path.dirname(os.path.dirname(os.path.dirname(outdir)))) if os.path.exists(apk) else None,
    "apk_sha256": sha(apk) if os.path.exists(apk) else None,
    "artifacts": arts,
}
cmds = os.path.join(outdir, "commands.txt")
if os.path.exists(cmds):
    manifest["commands"] = open(cmds).read().splitlines()
dc = os.path.join(outdir, "device-config.json")
if os.path.exists(dc):
    manifest["device_config"] = json.load(open(dc))
with open(os.path.join(outdir, "manifest.json"), "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PYEOF
}

# ---------------------------------------------------------------------------
# hello-views
# ---------------------------------------------------------------------------
ref_hello_views() {
    local pkg="$HELLO_VIEWS_PKG" out="$REF_ROOT/hello-views/$SUB"
    rm -rf "$out"; mkdir -p "$out"
    CMDLOG=""
    log "installing hello-views.apk"
    CMDLOG+="\$ adb -s $SERIAL install -r -g fixtures/build/apps/hello-views.apk"$'\n'
    "$ADB" -s "$SERIAL" install -r -g "$APPS_DIR/hello-views.apk" >"$out/install.txt" 2>&1
    grep -q Success "$out/install.txt" || die "hello-views install failed"

    capture_common "$pkg" AcosFixture "$out"

    # --- interaction: type World + tap Greet ---
    tap_id "$out/ui_initial.xml" ":id/input"
    CMDLOG+="\$ adb -s $SERIAL shell input text World"$'\n'
    "$ADB" -s "$SERIAL" shell input text World
    sleep 1
    tap_id "$out/ui_initial.xml" ":id/btn"
    sleep 1
    screencap "$out/screenshot_after_click1.png"
    dump_ui "$out/ui_after_click1.xml"
    ui_texts "$out/ui_after_click1.xml" >"$out/observed-texts-after-click1.txt"
    ui_state_json "$out/ui_after_click1.xml" "$out/ui_after_click1.json"
    assert_node_text "$out/ui_after_click1.xml" ":id/result" "Hello, World! clicks: 1"

    # --- second tap ---
    tap_id "$out/ui_after_click1.xml" ":id/btn"
    sleep 1
    screencap "$out/screenshot_after_click2.png"
    dump_ui "$out/ui_after_click2.xml"
    ui_texts "$out/ui_after_click2.xml" >"$out/observed-texts-after-click2.txt"
    ui_state_json "$out/ui_after_click2.xml" "$out/ui_after_click2.json"
    assert_node_text "$out/ui_after_click2.xml" ":id/result" "Hello, World! clicks: 2"
    assert_node_text "$out/ui_initial.xml" ":id/result" "clicks: 0"
    assert_node_text "$out/ui_initial.xml" ":id/title" "Hello ACOS"

    # --- rotate to force recreate (onSaveInstanceState) ---
    CMDLOG+="\$ adb -s $SERIAL shell settings put system accelerometer_rotation 0"$'\n'
    "$ADB" -s "$SERIAL" shell settings put system accelerometer_rotation 0 >/dev/null 2>&1 || true
    CMDLOG+="\$ adb -s $SERIAL shell settings put system user_rotation 1"$'\n'
    "$ADB" -s "$SERIAL" shell settings put system user_rotation 1 >/dev/null 2>&1 || true
    sleep 3
    screencap "$out/screenshot_after_recreate.png"
    dump_ui "$out/ui_after_recreate.xml"
    ui_texts "$out/ui_after_recreate.xml" >"$out/observed-texts-after-recreate.txt"
    ui_state_json "$out/ui_after_recreate.xml" "$out/ui_after_recreate.json"
    assert_node_text "$out/ui_after_recreate.xml" ":id/result" "clicks: 2"
    CMDLOG+="\$ adb -s $SERIAL shell logcat -d -v threadtime -s AcosFixture:I > logcat_recreate.txt"$'\n'
    "$ADB" -s "$SERIAL" shell logcat -d -v threadtime -s AcosFixture:I >"$out/logcat_recreate.txt" 2>&1 || true
    CMDLOG+="\$ adb -s $SERIAL shell settings put system user_rotation 0"$'\n'
    "$ADB" -s "$SERIAL" shell settings put system user_rotation 0 >/dev/null 2>&1 || true
    sleep 2
    dump_ui "$out/ui_after_rotate_back.xml"
    ui_state_json "$out/ui_after_rotate_back.xml" "$out/ui_after_rotate_back.json"

    # --- back ---
    CMDLOG+="\$ adb -s $SERIAL shell input keyevent KEYCODE_BACK"$'\n'
    "$ADB" -s "$SERIAL" shell input keyevent KEYCODE_BACK >/dev/null 2>&1 || true
    sleep 2
    CMDLOG+="\$ adb -s $SERIAL shell logcat -d -v threadtime -s AcosFixture:I > logcat_final.txt"$'\n'
    "$ADB" -s "$SERIAL" shell logcat -d -v threadtime -s AcosFixture:I >"$out/logcat_final.txt" 2>&1 || true
    grep -o 'AcosFixture.*' "$out/logcat_final.txt" >"$out/lifecycle-summary.txt" || true
    "$PY" - "$out/lifecycle-summary.txt" <<'PYEOF' || die "hello-views lifecycle assertion failed"
import sys
lines = [l.split('AcosFixture: ')[-1].strip() for l in open(sys.argv[1]) if 'AcosFixture' in l]
if lines[:3] != ['onCreate', 'onStart', 'onResume']:
    sys.exit('cold-start order wrong: %r' % lines[:3])
for ev in ['onPause', 'onStop', 'onSaveInstanceState clicks=2', 'onDestroy']:
    if ev not in lines:
        sys.exit('missing lifecycle event: ' + ev)
if 'greet name=World clicks=1' not in lines or 'greet name=World clicks=2' not in lines:
    sys.exit('missing greet log lines')
sys.exit(0)
PYEOF
    screencap "$out/screenshot_after_back.png"

    device_config_json "$out"
    printf '%s' "$CMDLOG" >"$out/commands.txt"
    write_manifest hello-views "$out" "$APPS_DIR/hello-views.apk"
    log "hello-views artifacts: $(ls "$out" | wc -l) files"
}

# ---------------------------------------------------------------------------
# hello-jni
# ---------------------------------------------------------------------------
ref_hello_jni() {
    local pkg="$HELLO_JNI_PKG" out="$REF_ROOT/hello-jni/$SUB"
    rm -rf "$out"; mkdir -p "$out"
    CMDLOG=""
    log "installing hello-jni.apk"
    CMDLOG+="\$ adb -s $SERIAL install -r -g fixtures/build/apps/hello-jni.apk"$'\n'
    "$ADB" -s "$SERIAL" install -r -g "$APPS_DIR/hello-jni.apk" >"$out/install.txt" 2>&1
    grep -q Success "$out/install.txt" || die "hello-jni install failed"

    capture_common "$pkg" AcosJniFixture "$out"
    grep -o 'AcosJniFixture.*' "$out/logcat_initial.txt" >"$out/logcat-summary.txt" || true
    for needle in \
        'stringFromJNI=Hello from native C' \
        'add(20,22)=42' \
        'sumArray([1,2,3,4,5])=15' \
        'javaCallbackResult=42' \
        'voidCallback=from-native' \
        'registeredEcho=echo!' \
        'throwFromJNI caught=thrown from native'; do
        assert_contains "$out/ui_initial.xml" '-' "$needle"
    done

    # back out
    CMDLOG+="\$ adb -s $SERIAL shell input keyevent KEYCODE_BACK"$'\n'
    "$ADB" -s "$SERIAL" shell input keyevent KEYCODE_BACK >/dev/null 2>&1 || true
    sleep 1

    device_config_json "$out"
    printf '%s' "$CMDLOG" >"$out/commands.txt"
    write_manifest hello-jni "$out" "$APPS_DIR/hello-jni.apk"
    log "hello-jni artifacts: $(ls "$out" | wc -l) files"
}

mkdir -p "$REF_ROOT"
ref_hello_views
ref_hello_jni
log "done -> $REF_ROOT"
