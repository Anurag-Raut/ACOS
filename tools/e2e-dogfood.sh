#!/usr/bin/env bash
#
# W46-A dogfood e2e: build nothing, run `acos launch` against the dogfood APKs
# and record exactly how far each launch gets, including every UNSUPPORTED_*
# error found on the ladder.
#
# This is a *observation* runner, not a gate: it never "retries until green"
# and never rewrites an app to hide a gap. It writes raw evidence under
# results/dogfood/launch/ and a ranked summary.
#
# Usage:
#   tools/e2e-dogfood.sh [name ...]        # default: all four dogfood apps
#
# Env:
#   ACOS=/path/to/acos                     # override the binary
#   ACOS_LAUNCH_TIMEOUT=300                # seconds (default 300)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/results/dogfood/launch"
TIMEOUT="${ACOS_LAUNCH_TIMEOUT:-300}"
ALL_APPS=(sqlite-crud canvas-shapes service-provider two-activity)

die() { echo "e2e-dogfood: error: $*" >&2; exit 1; }

# --- locate the acos binary ---------------------------------------------------
if [ -z "${ACOS:-}" ]; then
    for cand in \
        "$ROOT/.cache/target-w46a/release/acos" \
        "$ROOT/.cache/target-w46a/debug/acos" \
        "$ROOT/target/release/acos" \
        "$ROOT/target/debug/acos"; do
        if [ -x "$cand" ]; then ACOS="$cand"; break; fi
    done
fi
[ -n "${ACOS:-}" ] && [ -x "$ACOS" ] || die "no acos binary found; set ACOS=/path/to/acos"
echo "e2e-dogfood: acos = $ACOS"
"$ACOS" --version >/dev/null 2>&1 || true

mkdir -p "$OUT"

APPS=("$@")
if [ "${#APPS[@]}" -eq 0 ]; then APPS=("${ALL_APPS[@]}"); fi

for app in "${APPS[@]}"; do
    apk="$ROOT/fixtures/build/apps/$app.apk"
    [ -f "$apk" ] || die "missing APK: $apk (run tools/build-dogfood-apps.sh first)"

    echo "e2e-dogfood: [$app] acos launch (timeout ${TIMEOUT}s)"
    # A non-zero exit is expected while the runtime is partial; keep the JSON.
    "$ROOT/tools/guarded.sh" --mem 4G --time "$TIMEOUT" -- \
        "$ACOS" launch "$apk" --json > "$OUT/$app.json" 2> "$OUT/$app.stderr.txt"
    rc=$?
    echo "e2e-dogfood: [$app] exit=$rc"

    python3 - "$OUT/$app.json" "$OUT" "$app" "$rc" "$apk" <<'PY'
import json, sys, os, re

json_path, out_dir, app, rc, apk = sys.argv[1:6]
summary_txt = os.path.join(out_dir, app + ".summary.txt")
unsup_txt = os.path.join(out_dir, app + ".unsupported.txt")

raw = open(json_path, encoding="utf-8", errors="replace").read()
try:
    env = json.loads(raw)
except Exception as exc:                       # noqa: BLE001
    open(summary_txt, "w").write("PARSE_ERROR: %s\n" % exc)
    open(unsup_txt, "w").write("")
    print("e2e-dogfood: [%s] could not parse JSON: %s" % (app, exc))
    sys.exit(0)

result = env.get("result") or {}
stages = result.get("stages") or []
reached = result.get("reached_stage", "?")
status = env.get("status", "?")
err = env.get("error") or {}
err_code = err.get("code", "")

lines = []
lines.append("app:            %s" % app)
lines.append("apk:            %s" % apk)
lines.append("exit:           %s" % rc)
lines.append("envelope:       ok=%s status=%s" % (env.get("ok"), status))
lines.append("reached_stage:  %s" % reached)
lines.append("error.code:     %s" % err_code)
lines.append("classes_loaded: %s" % result.get("classes_loaded"))
lines.append("wall_ms:        %s" % result.get("wall_ms"))
lines.append("")
lines.append("ladder:")
for s in stages:
    line = "  [%-11s] %s" % (s.get("status", "?"), s.get("stage", "?"))
    lines.append(line)
    b = s.get("blocker")
    if b:
        lines.append("      code: %s" % b.get("code", ""))
        msg = (b.get("message") or "").splitlines()
        if msg:
            lines.append("      msg:  %s" % msg[0])

# Collect every UNSUPPORTED_* token anywhere in the envelope, with the path
# (stage / error) that introduced it, so duplicates are attributable.
found = []

def walk(node, path):
    if isinstance(node, dict):
        for k, v in node.items():
            walk(v, path + "." + str(k))
    elif isinstance(node, list):
        for i, v in enumerate(node):
            walk(v, path + "[%d]" % i)
    elif isinstance(node, str):
        for tok in re.findall(r"UNSUPPORTED_[A-Z0-9_]+", node):
            found.append((tok, path))

walk(env, "")

# Also the raw text: native tails and messages can carry tokens the JSON
# structure de-duplicates.
for tok in re.findall(r"UNSUPPORTED_[A-Z0-9_]+", raw):
    found.append((tok, "<raw>"))

uniq = {}
for tok, path in found:
    uniq.setdefault(tok, []).append(path)

lines.append("")
if uniq:
    lines.append("UNSUPPORTED_* found (%d distinct):" % len(uniq))
    for tok in sorted(uniq):
        lines.append("  %s" % tok)
else:
    lines.append("UNSUPPORTED_* found: none")

open(summary_txt, "w").write("\n".join(lines) + "\n")

u = ["# UNSUPPORTED_* for %s" % app, ""]
if uniq:
    for tok in sorted(uniq):
        u.append("- `%s`" % tok)
        paths = sorted(set(uniq[tok]))
        for p in paths[:6]:
            u.append("    - at `%s`" % p)
else:
    u.append("none (blocker may be JAVA_EXCEPTION or the ladder completed)")
open(unsup_txt, "w").write("\n".join(u) + "\n")

print("e2e-dogfood: [%s] reached=%s error=%s unsupported=%d" %
      (app, reached, err_code, len(uniq)))
PY
done

# --- aggregate ----------------------------------------------------------------
python3 - "$OUT" "${APPS[@]}" <<'PY'
import json, os, re, sys

out_dir = sys.argv[1]
apps = sys.argv[2:]
rows = []
for app in apps:
    p = os.path.join(out_dir, app + ".json")
    try:
        env = json.load(open(p, encoding="utf-8", errors="replace"))
    except Exception as exc:                   # noqa: BLE001
        rows.append({"app": app, "error": "parse: %s" % exc})
        continue
    result = env.get("result") or {}
    err = env.get("error") or {}
    raw = open(p, encoding="utf-8", errors="replace").read()
    unsup = sorted(set(re.findall(r"UNSUPPORTED_[A-Z0-9_]+", raw)))
    stages = result.get("stages") or []
    ok_stages = [s["stage"] for s in stages if s.get("status") == "ok"]
    blocked = next((s for s in stages if s.get("status") == "blocked"), None)
    rows.append({
        "app": app,
        "status": env.get("status"),
        "reached_stage": result.get("reached_stage"),
        "ok_stages": ok_stages,
        "blocked_stage": (blocked or {}).get("stage"),
        "blocker_code": (blocked or {}).get("blocker", {}).get("code") if blocked else None,
        "error_code": err.get("code"),
        "unsupported": unsup,
    })

json.dump(rows, open(os.path.join(out_dir, "SUMMARY.json"), "w"), indent=2)

md = ["# W46-A dogfood launch evidence", "",
      "`acos launch --json` per fixture. Partial results are honest partials.", "",
      "| app | reached | last ok stage | blocked stage | blocker | UNSUPPORTED_* |",
      "|---|---|---|---|---|---|"]
for r in rows:
    md.append("| %s | %s | %s | %s | %s | %s |" % (
        r.get("app"),
        r.get("reached_stage", "?"),
        (r.get("ok_stages") or ["-"])[-1],
        r.get("blocked_stage") or "-",
        r.get("blocker_code") or r.get("error_code") or "-",
        ", ".join("`%s`" % u for u in r.get("unsupported", [])) or "none",
    ))
open(os.path.join(out_dir, "SUMMARY.md"), "w").write("\n".join(md) + "\n")
print("e2e-dogfood: wrote %s/SUMMARY.md" % out_dir)
PY

echo "e2e-dogfood: evidence under $OUT"
