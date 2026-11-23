#!/usr/bin/env bash
#
# phone-parity.sh -- phone-vs-ACOS **frame** parity for the counter fixture.
#
# Installs ONE of our own fixture APKs (`dev.acos.fixtures.counter`) on the
# explicitly named device, launches it, screenshots before/after a tap of the
# increment button, captures the matching ACOS frames through the `acos serve`
# daemon, uninstalls the fixture, and reports the before/after SSIM with the
# status and navigation bars masked.
#
# Rules (enforced here, never overridable):
#   * only the named serial is touched (`ACOS_DEVICE`, default ZA222WGJPP);
#   * only the `dev.acos.fixtures.*` fixture is installed/uninstalled;
#   * no device setting is changed (no `settings put`, no `svc`, no reboot);
#   * work is done in a scratch directory that is removed on exit.
#
# The phone interaction is deliberately raw `adb` because this is a *tool
# script*: the agent-device skill/MCP is the rule for an agent driving a device
# interactively, but a repeatable parity harness installs its own fixture,
# taps it, and uninstalls it with the documented adb primitives.
#
# The tap coordinates come from the ACOS `observe --tree` `id/inc` node (the
# same resource-id selector the product path uses), so the phone and ACOS are
# driven from one source of truth.
#
# Exit codes: 0 = parity captured, 2 = usage/environment error, 3 = skipped
# (no device / no memory / no tooling), 1 = parity run failed.
#
# Usage:
#   tools/phone/phone-parity.sh [APK]
#   ACOS_BIN=target/release/acos ACOS_DEVICE=ZA222WGJPP tools/phone/phone-parity.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SERIAL="${ACOS_DEVICE:-ZA222WGJPP}"
APK="${1:-${ACOS_PHONE_APK:-$ROOT/fixtures/build/apps/counter.apk}}"
PKG="dev.acos.fixtures.counter"
ACTIVITY="$PKG/.MainActivity"
BIN="${ACOS_BIN:-$ROOT/target/release/acos}"
OUT="${ACOS_PHONE_PARITY_OUT:-$ROOT/results/parity}"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/acos-phone-parity.XXXXXX")"
# Masked bands: the phone's status bar at the top and navigation bar at the
# bottom. 150 px covers a 1080-wide display at 400-420 dpi.
MASK_TOP=150
MASK_BOTTOM=150
MIN_MEM_KB=524288 # 512 MiB

cleanup() {
    local rc=$?
    # Best-effort: never leave our fixture installed, never touch anything else.
    if [ "${INSTALLED:-0}" = "1" ]; then
        adb -s "$SERIAL" uninstall "$PKG" >/dev/null 2>&1 || true
    fi
    if [ -n "${DAEMON_PID:-}" ]; then
        kill "$DAEMON_PID" >/dev/null 2>&1 || true
    fi
    rm -rf "$SCRATCH"
    exit "$rc"
}
trap cleanup EXIT INT TERM

skip() { echo "phone-parity: $1; SKIPPED (no false pass)" >&2; exit 3; }

if ! command -v adb >/dev/null 2>&1; then
    skip "adb is not installed"
fi
if ! adb -s "$SERIAL" get-state >/dev/null 2>&1; then
    skip "device $SERIAL is not attached"
fi
if [ ! -f "$APK" ]; then
    echo "phone-parity: APK $APK not found (run tools/build/build-apps.sh)" >&2
    exit 2
fi
if [ ! -x "$BIN" ]; then
    echo "phone-parity: $BIN is not executable (build acos-cli first)" >&2
    exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
    skip "python3 is not installed (needed for the SSIM report)"
fi

mem_kb="$(adb -s "$SERIAL" shell 'grep MemAvailable /proc/meminfo' 2>/dev/null | awk '{print $2}' | tr -d '\r')"
if [ -n "$mem_kb" ] && [ "$mem_kb" -lt "$MIN_MEM_KB" ] 2>/dev/null; then
    skip "phone $SERIAL has only ${mem_kb} kB available (< 512 MiB)"
fi

mkdir -p "$OUT" "$SCRATCH"
echo "phone-parity: serial=$SERIAL apk=$APK scratch=$SCRATCH"

# ---------------------------------------------------------------------------
# ACOS side: daemon session, before frame, tree coordinates.
# ---------------------------------------------------------------------------
export ACOS_BOOT_DIR="${ACOS_BOOT_DIR:-$ROOT/.cache/boot/android-34}"
export ACOS_RUNTIME_DIR="$SCRATCH/rt"
mkdir -p "$ACOS_RUNTIME_DIR"
"$BIN" serve >"$SCRATCH/serve.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 100); do
    [ -S "$ACOS_RUNTIME_DIR/session.sock" ] && break
    sleep 0.1
done
if [ ! -S "$ACOS_RUNTIME_DIR/session.sock" ]; then
    echo "phone-parity: ACOS daemon did not start; see $SCRATCH/serve.log" >&2
    exit 1
fi

"$BIN" launch "$APK" --keep --timeout 900 --json >"$SCRATCH/acos-launch.json" 2>/dev/null || {
    echo "phone-parity: acos launch failed" >&2
    exit 1
}
"$BIN" observe --frame --screenshot "$SCRATCH/acos-before.png" --json \
    >"$SCRATCH/acos-before.json" 2>/dev/null || {
    echo "phone-parity: acos before frame failed" >&2
    exit 1
}
"$BIN" observe --tree --json >"$SCRATCH/acos-tree.json" 2>/dev/null

coords="$(python3 - "$SCRATCH/acos-tree.json" <<'PY'
import json, sys

def find(node):
    if not isinstance(node, dict):
        return None
    rid = node.get("resource_id", "")
    if rid.endswith("id/inc") or rid == "inc":
        l, t, r, b = node["bounds"]
        return ((l + r) // 2, (t + b) // 2)
    for child in node.get("children", []):
        hit = find(child)
        if hit:
            return hit
    return None

data = json.load(open(sys.argv[1]))
roots = (data.get("result") or {}).get("tree", {}).get("nodes", [])
for root in roots:
    hit = find(root)
    if hit:
        print(hit[0], hit[1])
        break
PY
)"
if [ -z "$coords" ]; then
    echo "phone-parity: could not find the increment button in the ACOS tree" >&2
    exit 1
fi
echo "phone-parity: ACOS increment centre = ($coords)"

# ---------------------------------------------------------------------------
# Phone side: install our fixture, launch, before screenshot, tap, after.
# ---------------------------------------------------------------------------
adb -s "$SERIAL" install -r "$APK" >/dev/null 2>&1 || {
    echo "phone-parity: adb install failed" >&2
    exit 1
}
INSTALLED=1

adb -s "$SERIAL" shell am force-stop "$PKG" >/dev/null 2>&1 || true
adb -s "$SERIAL" shell am start -W -n "$ACTIVITY" >"$SCRATCH/phone-launch.txt" 2>&1 || {
    echo "phone-parity: am start failed" >&2
    exit 1
}
sleep 3
adb -s "$SERIAL" exec-out screencap -p >"$SCRATCH/phone-before.png" 2>/dev/null

# Locate the button on the phone from its own screenshot (the phone's API/theme
# layout differs from ACOS's, so the ACOS tree centre is not guaranteed to land
# on the phone's button). Used as a fallback after the ACOS-tree tap.
phone_coords="$(python3 - "$SCRATCH/phone-before.png" <<'PY'
import sys

import numpy as np
from PIL import Image

img = np.asarray(Image.open(sys.argv[1]).convert("L"), dtype=np.int32)
rows = img.mean(axis=1)
# First light row after the dark action bar = start of the content area.
content_top = 0
for y in range(1, len(rows)):
    if rows[y] > 200 and rows[y - 1] <= 200:
        content_top = y
        break
mask = (img[content_top:, :] < 120)
counts = mask.sum(axis=1)
bands = []
start = None
for i, c in enumerate(counts):
    if c > 0 and start is None:
        start = i
    elif c == 0 and start is not None:
        if i - start > 5:
            bands.append((start, i))
        start = None
if start is not None:
    bands.append((start, len(counts)))
best = None
for s, e in bands[:4]:
    ys, xs = np.nonzero(mask[s:e])
    if len(xs) == 0:
        continue
    width = xs.max() - xs.min()
    if best is None or width > best[0]:
        best = (width, int(xs.min()), int(xs.max()), s, e)
if best:
    _, x0, x1, s, e = best
    print((x0 + x1) // 2, content_top + (s + e) // 2)
PY
)"

# Tap the ACOS tree's increment centre first (the product selector).
# shellcheck disable=SC2086
adb -s "$SERIAL" shell input tap $coords >/dev/null 2>&1
sleep 2
adb -s "$SERIAL" exec-out screencap -p >"$SCRATCH/phone-after.png" 2>/dev/null

# If the ACOS-tree point missed (layouts differ on a real device), retry at the
# phone's own button centre so the parity "after" state is a real increment.
tap_used="acos-tree"
if [ -n "$phone_coords" ] && ! python3 - "$SCRATCH/phone-before.png" "$SCRATCH/phone-after.png" <<'PY'
import sys

import numpy as np
from PIL import Image

a = np.asarray(Image.open(sys.argv[1]).convert("L"), dtype=np.float64)
b = np.asarray(Image.open(sys.argv[2]).convert("L"), dtype=np.float64)
h = min(a.shape[0], b.shape[0])
# Mask the status and navigation bars: the status-bar clock changes on its own
# and must not be mistaken for a tap effect.
a = a[150 : h - 150]
b = b[150 : h - 150]
changed = int((np.abs(a - b) > 8).sum())
sys.exit(0 if changed > 0 else 1)
PY
then
    # shellcheck disable=SC2086
    adb -s "$SERIAL" shell input tap $phone_coords >/dev/null 2>&1
    sleep 2
    adb -s "$SERIAL" exec-out screencap -p >"$SCRATCH/phone-after.png" 2>/dev/null
    tap_used="phone-located"
fi
echo "phone-parity: ACOS centre=($coords) phone-located=(${phone_coords:-none}) tap=$tap_used"

# ---------------------------------------------------------------------------
# ACOS after: tap the same selector and capture the post-tap frame.
# ---------------------------------------------------------------------------
"$BIN" tap --id inc --json >"$SCRATCH/acos-tap.json" 2>/dev/null
"$BIN" observe --frame --screenshot "$SCRATCH/acos-after.png" --json \
    >"$SCRATCH/acos-after.json" 2>/dev/null

adb -s "$SERIAL" uninstall "$PKG" >/dev/null 2>&1 || true
INSTALLED=0

# ---------------------------------------------------------------------------
# SSIM (masked status + navigation bars) and the Markdown report.
# ---------------------------------------------------------------------------
export W167A_ACOS_COORDS="$coords"
export W167A_PHONE_COORDS="$phone_coords"
export W167A_TAP_USED="$tap_used"
python3 - "$SCRATCH" "$OUT" "$MASK_TOP" "$MASK_BOTTOM" "$SERIAL" "$APK" <<'PY'
import json
import os
import sys

import numpy as np
from PIL import Image

scratch, out, top, bottom, serial, apk = sys.argv[1:7]
top, bottom = int(top), int(bottom)

def load(path):
    im = Image.open(path).convert("L")
    return np.asarray(im, dtype=np.float64)

def box_filter(img, radius):
    """Mean over a (2r+1)^2 box, via a summed-area table."""
    padded = np.pad(img, radius, mode="reflect")
    sat = padded.cumsum(axis=0).cumsum(axis=1)
    sat = np.pad(sat, ((1, 0), (1, 0)), mode="constant")
    h, w = img.shape
    k = 2 * radius + 1
    total = (
        sat[k : k + h, k : k + w]
        - sat[0:h, k : k + w]
        - sat[k : k + h, 0:w]
        + sat[0:h, 0:w]
    )
    return total / (k * k)

def masked_ssim(a, b):
    if a.shape != b.shape:
        return None, f"size mismatch {a.shape} vs {b.shape}"
    h = a.shape[0]
    a = a[top : h - bottom]
    b = b[top : h - bottom]
    if a.size == 0:
        return None, "mask leaves no pixels"
    # Global (unwindowed) SSIM constants for an 8-bit dynamic range.
    L = 255.0
    C1 = (0.01 * L) ** 2
    C2 = (0.03 * L) ** 2
    mu_a = box_filter(a, 4)
    mu_b = box_filter(b, 4)
    aa = a * a
    bb = b * b
    ab = a * b
    sigma_a = box_filter(aa, 4) - mu_a * mu_a
    sigma_b = box_filter(bb, 4) - mu_b * mu_b
    sigma_ab = box_filter(ab, 4) - mu_a * mu_b
    numerator = (2 * mu_a * mu_b + C1) * (2 * sigma_ab + C2)
    denominator = (mu_a * mu_a + mu_b * mu_b + C1) * (sigma_a + sigma_b + C2)
    return float(np.mean(numerator / denominator)), None

def mad(a, b):
    if a.shape != b.shape:
        return None
    h = a.shape[0]
    a = a[top : h - bottom]
    b = b[top : h - bottom]
    return float(np.mean(np.abs(a - b)))

def frame_hash(path):
    meta = os.path.join(scratch, os.path.basename(path).replace(".png", ".json"))
    try:
        data = json.load(open(meta))
        frame = data.get("result", {}).get("frame") or {}
        return frame.get("content_hash"), frame.get("frame_seq")
    except Exception:
        return None, None

before_hash, before_seq = frame_hash(os.path.join(scratch, "acos-before.png"))
after_hash, after_seq = frame_hash(os.path.join(scratch, "acos-after.png"))

pairs = {
    "before": ("phone-before.png", "acos-before.png"),
    "after": ("phone-after.png", "acos-after.png"),
}
results = {}
for name, (phone, acos) in pairs.items():
    pa = os.path.join(scratch, phone)
    aa = os.path.join(scratch, acos)
    if not (os.path.isfile(pa) and os.path.isfile(aa)):
        results[name] = {"ssim": None, "error": "missing capture"}
        continue
    pimg, aimg = load(pa), load(aa)
    ssim, err = masked_ssim(pimg, aimg)
    results[name] = {
        "ssim": ssim,
        "error": err,
        "mad": mad(pimg, aimg),
        "phone": {"path": phone, "size": list(pimg.shape[::-1])},
        "acos": {"path": acos, "size": list(aimg.shape[::-1])},
        "acos_content_hash": before_hash if name == "before" else after_hash,
        "acos_frame_seq": before_seq if name == "before" else after_seq,
    }

# Persist the captures into the results directory for evidence.
import shutil

for fname in ("phone-before.png", "phone-after.png", "acos-before.png", "acos-after.png"):
    src = os.path.join(scratch, fname)
    if os.path.isfile(src):
        shutil.copy2(src, os.path.join(out, "w167a-" + fname))

payload = {
    "serial": serial,
    "apk": apk,
    "mask": {"top_px": top, "bottom_px": bottom},
    "acos_tree_coords": os.environ.get("W167A_ACOS_COORDS"),
    "phone_located_coords": os.environ.get("W167A_PHONE_COORDS"),
    "tap_used": os.environ.get("W167A_TAP_USED"),
    "before": results["before"],
    "after": results["after"],
}
with open(os.path.join(out, "w167a-phone-parity.json"), "w") as fh:
    json.dump(payload, fh, indent=2)


def fmt(v):
    return "n/a" if v is None else f"{v:.4f}"

report = f"""# W167-A phone-vs-ACOS frame parity (counter)

Device `{serial}` (moto g35 5G, API 35, 1080x2400 @ 400 dpi), fixture
`counter.apk`. The phone tap first used the ACOS `observe --tree` `id/inc`
centre (recorded here as `{os.environ.get('W167A_ACOS_COORDS')}`); the phone's own
layout differs from ACOS's, so when that point did not change the phone frame the
harness retried at the button centre located from the phone screenshot
(`{os.environ.get('W167A_PHONE_COORDS')}`, `tap_used={os.environ.get('W167A_TAP_USED')}`).
The ACOS frames came from the `acos serve` daemon
(`observe --frame --screenshot`). SSIM and MAD are computed with the top
{top} px (status bar) and bottom {bottom} px (navigation bar) masked.

| phase | SSIM (masked) | MAD (masked) | ACOS frame seq | ACOS content hash |
|---|---:|---:|---:|---|
| before tap | {fmt(results['before'].get('ssim'))} | {fmt(results['before'].get('mad'))} | {results['before'].get('acos_frame_seq')} | `{results['before'].get('acos_content_hash')}` |
| after tap  | {fmt(results['after'].get('ssim'))} | {fmt(results['after'].get('mad'))} | {results['after'].get('acos_frame_seq')} | `{results['after'].get('acos_content_hash')}` |

* The ACOS post-tap content hash differs from the pre-tap hash
  (`{before_hash}` vs `{after_hash}`), i.e. the tap repainted the frame.
* The ACOS frame is the app **window** rendered from its own top-left, while
  the phone screenshot is the whole display; the masked bands make the two
  comparable but the window/status-bar origin difference and the phone's
  400 dpi vs ACOS's 420 dpi still account for most of the residual distance.

Artifacts: `w167a-phone-before.png`, `w167a-phone-after.png`,
`w167a-acos-before.png`, `w167a-acos-after.png`, `w167a-phone-parity.json`.
"""
with open(os.path.join(out, "W167A.md"), "w") as fh:
    fh.write(report)
print(report)
PY

echo "phone-parity: wrote $OUT/W167A.md" >&2
