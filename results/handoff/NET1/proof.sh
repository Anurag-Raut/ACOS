#!/usr/bin/env bash
# NET1 transport proofs (design B2, ADR-0030 amendment 3).
#
# Drives the real `acos-netd` supervisor: a real relay netns with the pinned
# static `pasta`, real per-session netns + veth pairs, and `curl` started inside
# each session netns. `curl` is a guest stand-in with the guest's privilege: it
# runs in a netns owned by the supervisor's user namespace and holds no
# capability over the relay netns where the boundary rules live.
#
#   PASTA=<static pasta, tag 2026_10_02.cba3570> BIN=<acos-netd> \
#     results/handoff/NET1/proof.sh [outdir]
#
# Every check writes `check=<name> result=PASS|FAIL detail=…` to the log and a
# JSON object to <outdir>/measurements.jsonl.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
WT=$(cd "$HERE/../../.." && pwd)
OUT=${1:-$HERE/run}
BIN=${BIN:-$WT/.cache/net1-target/release/acos-netd}
PASTA=${PASTA:-$WT/.cache/net1/passt-src/pasta}
mkdir -p "$OUT"
LOG=$OUT/proof.log
JSONL=$OUT/measurements.jsonl
: >"$LOG"; : >"$JSONL"
rm -f "$OUT"/sess-*.out "$OUT"/sess-*.out.err "$OUT"/spawn-*.json
export ACOS_NET_PASTA=$PASTA

say() { printf '%s\n' "$*" | tee -a "$LOG" >&2; }
check() { say "check=$1 result=$2 ${3:-}"; }
jrec() { printf '%s\n' "$1" >>"$JSONL"; }
netd() { "$BIN" "$@" 2>>"$OUT/netd-stderr.log"; }
st() { netd status 2>/dev/null; }
field() { python3 -c 'import json,sys; d=json.load(sys.stdin); print(eval(sys.argv[1]))' "$1"; }
sess_out() { printf '%s' "$OUT/sess-$1.out"; }
spawn() { # spawn NAME CMD...  (stdout/stderr per session)
  name=$1; shift
  netd spawn "$name" --stdout "$(sess_out "$name")" --stderr "$(sess_out "$name").err" -- "$@" \
    >"$OUT/spawn-$name.json" 2>>"$OUT/netd-stderr.log"
}

cleanup() {
  netd shutdown >/dev/null 2>&1
  [ -n "${SRV_PID:-}" ] && kill "$SRV_PID" 2>/dev/null
  [ -n "${SRV2_PID:-}" ] && kill "$SRV2_PID" 2>/dev/null
}
trap cleanup EXIT

[ -x "$BIN" ] || { say "no acos-netd at $BIN"; exit 2; }
[ -x "$PASTA" ] || { say "no pasta at $PASTA"; exit 2; }

say "== NET1 transport proof $(date -u +%FT%TZ)"
say "bin=$BIN pasta=$PASTA"
say "pasta_sha256=$(sha256sum "$PASTA" | cut -d' ' -f1) (spike record 314c37e878bd6ffe43200735800d5efd4859d81d46b0450358a103c36c0edf60)"

# A host loopback listener the guest must never reach (the `acos view`/MCP shape)
# on a port pasta never forwards, plus the same on the host's LAN address.
mkdir -p "$OUT/www"
python3 -m http.server 18080 --bind 127.0.0.1 --directory "$OUT/www" >"$OUT/host-server.log" 2>&1 &
SRV_PID=$!
LAN_IP=$(ip route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p')
if [ -n "$LAN_IP" ]; then
  python3 -m http.server 18081 --bind "$LAN_IP" --directory "$OUT/www" >"$OUT/lan-server.log" 2>&1 &
  SRV2_PID=$!
fi
sleep 0.5
say "host_listener=127.0.0.1:18080 lan_listener=${LAN_IP:-none}:18081"

# ---------------------------------------------------------------- relay up ---
t0=$(date +%s%N)
netd up >"$OUT/up.json" 2>>"$OUT/netd-stderr.log"
rc=$?
wall_ms=$(( ($(date +%s%N) - t0) / 1000000 ))
status=$(st)
up=$(printf '%s' "$status" | field 'd["up"]')
if [ "$rc" = 0 ] && [ "$up" = "True" ]; then
  check relay_up PASS "wall_ms=$wall_ms pasta_pid=$(printf '%s' "$status" | field 'd["pasta_pid"]') fabric_pid=$(printf '%s' "$status" | field 'd["fabric_pid"]')"
else
  check relay_up FAIL "rc=$rc up=$up"; tail -20 "$OUT/netd-stderr.log" >&2; exit 1
fi
pver=$(printf '%s' "$status" | field 'd["pasta_version"]')
ptag=$(printf '%s' "$status" | field 'd["pasta_tag_expected"]')
say "pasta_version=$pver expected=$ptag"
relay_pss=$(python3 - "$status" <<'PY'
import json,sys
d=json.loads(sys.argv[1])
def pss(pid):
    try:
        for line in open(f"/proc/{pid}/smaps_rollup"):
            if line.startswith("Pss:"): return int(line.split()[1])
    except OSError: return 0
    return 0
print(sum(pss(d.get(k) or 0) for k in ("pasta_pid","fabric_pid")))
PY
)
say "relay_pss_kb=$relay_pss (pasta + fabric)"
jrec "{\"metric\":\"relay_up\",\"wall_ms\":$wall_ms,\"relay_pss_kb\":$relay_pss,\"pasta_version\":\"$pver\",\"pasta_tag_expected\":\"$ptag\"}"

FETCH='c=$(curl -sS -m 10 -o /dev/null -w "%{http_code}" https://speed.cloudflare.com/__down?bytes=1024 2>/dev/null); echo "$c"'

# ------------------------------------------------- HTTP 200 at N=1 and N=20 ---
fetch_n() {
  n=$1; tag=$2
  rm -f "$OUT"/sess-http*.out
  for i in $(seq 1 "$n"); do
    spawn "http$i" /bin/sh -c "$FETCH"
  done
  # the fetches run inside the session netns; wait for the files to settle
  for _ in $(seq 1 60); do
    ready=$(cat "$OUT"/sess-http*.out 2>/dev/null | grep -c '^200$' || true)
    [ "${ready:-0}" -ge "$n" ] && break
    sleep 0.5
  done
  codes=$(cat "$OUT"/sess-http*.out 2>/dev/null | grep -c '^200$' || true)
  status=$(st)
  attached=$(printf '%s' "$status" | field 'len(d["sessions"])')
  ams=$(printf '%s' "$status" | field 'round(d["attach_ms_last"] or 0, 1)')
  if [ "${codes:-0}" -ge "$n" ]; then
    check "http_200_n$n" PASS "sessions=$attached http_200=$codes/$n attach_ms_last=$ams"
  else
    check "http_200_n$n" FAIL "sessions=$attached http_200=$codes/$n (see $OUT/sess-http*.out.err)"
  fi
  jrec "{\"metric\":\"http_200\",\"n\":$n,\"attached\":$attached,\"http_200\":${codes:-0},\"attach_ms_last\":$ams}"
  names=$(printf '%s' "$status" | python3 -c 'import json,sys; print(" ".join(s["name"] for s in json.load(sys.stdin)["sessions"]))')
  for name in $names; do netd detach "$name" >/dev/null 2>&1; done
}
fetch_n 1 one
fetch_n 20 twenty

# ---------------------------------------- per-session offline/latency isolation ---
LOOP='while :; do s=$(date +%s%N); c=$(curl -sS -m 5 -o /dev/null -w "%{http_code}" https://speed.cloudflare.com/__down?bytes=1024 2>/dev/null); e=$(date +%s%N); echo "$c $(( (e-s)/1000000 ))"; sleep 0.1; done'
spawn iso1 /bin/sh -c "$LOOP"
spawn iso2 /bin/sh -c "$LOOP"
sleep 4
t_off=$(date +%s%N)
netd offline iso1 >/dev/null 2>>"$OUT/netd-stderr.log"
sleep 5
netd online iso1 >/dev/null 2>>"$OUT/netd-stderr.log"
t_on=$(date +%s%N)
sleep 2
iso=$(python3 - "$(sess_out iso1)" "$(sess_out iso2)" "$t_off" "$t_on" <<'PY'
import sys
a,b,t0,t1=sys.argv[1],sys.argv[2],int(sys.argv[3]),int(sys.argv[4])
# iso2 keeps fetching across the whole window; iso1 must fail inside it.
def ok200(p):
    return sum(1 for l in open(p) if l.split()[:1]==["200"])
a_ok,b_ok=ok200(a),ok200(b)
# iso1's failures are visible as non-200 codes (000 on timeout)
a_fail=sum(1 for l in open(a) if l.split() and l.split()[0]!="200")
print(f"{a_fail} {a_ok} {b_ok}")
PY
)
set -- $iso
afail=$1; aok=$2; bok=$3
if [ "${afail:-0}" -ge 1 ] && [ "${bok:-0}" -ge 1 ]; then
  check offline_isolation PASS "iso1_failed=$afail iso1_200=$aok iso2_200=$bok"
else
  check offline_isolation FAIL "iso1_failed=$afail iso2_200=$bok"
fi
jrec "{\"metric\":\"offline_isolation\",\"iso1_failed\":$afail,\"iso1_200\":$aok,\"iso2_200\":$bok}"

# Baseline medians over the settled window, then 400 ms on iso2 only and the
# median over the *post-shaping* lines only (a whole-file median would average
# the shaped window away).
med() { python3 - "$1" "${2:-0}" <<'PY2'
import sys
path,skip=sys.argv[1],int(sys.argv[2])
xs=[]
for i,l in enumerate(open(path)):
    f=l.split()
    if i>=skip and f[:1]==["200"]:
        xs.append(int(f[1]))
xs.sort()
print(xs[len(xs)//2] if xs else -1)
PY2
}
base1=$(med "$(sess_out iso1)"); base2=$(med "$(sess_out iso2)")
skip1=$(wc -l <"$(sess_out iso1)" 2>/dev/null || echo 0); skip2=$(wc -l <"$(sess_out iso2)" 2>/dev/null || echo 0)
netd delay iso2 --ms 400 >/dev/null 2>>"$OUT/netd-stderr.log"
sleep 8
sh1=$(med "$(sess_out iso1)" "$skip1"); sh2=$(med "$(sess_out iso2)" "$skip2")
netd clear iso2 >/dev/null 2>>"$OUT/netd-stderr.log"
say "median fetch ms (post-shaping window): iso1 $base1 -> $sh1 ; iso2 $base2 -> $sh2 (400 ms netem on iso2)"
grew=$(python3 -c "print(1 if ($sh2 - $base2) > 300 and abs($sh1 - $base1) < 200 else 0)" 2>/dev/null || echo 0)
[ "$grew" = "1" ] && check latency_isolation PASS "iso2 median ${base2}ms -> ${sh2}ms, iso1 ${base1}ms -> ${sh1}ms" || check latency_isolation FAIL "iso2 ${base2}->${sh2} iso1 ${base1}->${sh1}"
jrec "{\"metric\":\"latency_isolation\",\"iso1_before\":$base1,\"iso1_after\":$sh1,\"iso2_before\":$base2,\"iso2_after\":$sh2}"
netd detach iso1 >/dev/null 2>&1; netd detach iso2 >/dev/null 2>&1

# --------------------------------------------------------- boundary probes ---
probe() { # probe NAME URL -> last line of the session's own output
  name=$1; url=$2
  spawn "$name" /bin/sh -c "curl -sS -m 4 -o /dev/null -w '%{http_code}' '$url' 2>/dev/null || echo FAIL"
  for _ in $(seq 1 30); do
    got=$(tail -1 "$(sess_out "$name")" 2>/dev/null | tr -d '\r')
    [ -n "$got" ] && break
    sleep 0.2
  done
  netd detach "$name" >/dev/null 2>&1
  printf '%s' "$got"
}
h=$(probe hostloop http://127.0.0.1:18080/)
case "$h" in *200*) check host_loopback_blocked FAIL "got=$h";; *) check host_loopback_blocked PASS "got=${h:-empty}";; esac
jrec "{\"metric\":\"host_loopback\",\"result\":\"${h:-empty}\"}"
if [ -n "$LAN_IP" ]; then
  l=$(probe lan "http://$LAN_IP:18081/")
  case "$l" in *200*) check lan_blocked FAIL "got=$l";; *) check lan_blocked PASS "got=${l:-empty}";; esac
  jrec "{\"metric\":\"lan_blocked\",\"result\":\"${l:-empty}\",\"ip\":\"$LAN_IP\"}"
else
  check lan_blocked SKIP "no LAN address"
fi

# ------------------------------------------------- relay kill / fail-closed ---
# Held down: the fail-closed branch is checked without racing the restart tick.
spawn survivor /bin/sh -c 'sleep 120'
sleep 1
netd kill-pasta --hold >/dev/null 2>&1
status=$(st)
down=$(printf '%s' "$status" | field 'd["up"]')
reason=$(printf '%s' "$status" | field 'd.get("down_reason") or "none"')
"$BIN" spawn failclosed -- /bin/sh -c 'true' >"$OUT/failclosed.json" 2>&1
code=$(python3 -c 'import json,sys
raw=open(sys.argv[1]).read()
start=raw.find("{")
try: d=json.loads(raw[start:])
except Exception: print("unparsed:"+raw[:120].replace("\n"," ")); raise SystemExit
print((d.get("error") or {}).get("code","no-error"))' "$OUT/failclosed.json")
if [ "$down" = "False" ] && [ "$code" = "network-unavailable" ]; then
  check fail_closed PASS "status.up=False down_reason=$reason spawn.error=$code"
else
  check fail_closed FAIL "status.up=$down spawn.error=$code"
fi
jrec "{\"metric\":\"fail_closed\",\"up\":\"$down\",\"error\":\"$code\"}"
# Recovery: un-hold the relay the way a fresh session would (`up` is idempotent
# and clears the hold), then wait for the supervisor's restart.
netd up >/dev/null 2>>"$OUT/netd-stderr.log"
for _ in $(seq 1 100); do
  up=$(st | field 'd["up"]')
  [ "$up" = "True" ] && break
  sleep 0.2
done
spawn afterrestart /bin/sh -c "$FETCH"
sleep 6
after=$(grep -c '^200$' "$(sess_out afterrestart)" 2>/dev/null || true)
[ "$up" = "True" ] && [ "${after:-0}" -ge 1 ] && check restart_recovery PASS "relay up again, HTTP 200 after restart" || check restart_recovery FAIL "up=$up codes200=$after"
jrec "{\"metric\":\"restart_recovery\",\"up\":\"$up\",\"http_200\":${after:-0}}"
netd detach afterrestart >/dev/null 2>&1
netd detach survivor >/dev/null 2>&1

# ------------------------------------------------------- 50 attach/detach ---
# The leak test is the relay netns' own view: 50 attach/detach cycles must leave
# no veth and no per-session rule behind (the veth's peer inside the session
# netns dies with it, so the relay-side pair is the whole footprint).
: >"$OUT/cycle.log"
ok=1
for i in $(seq 1 50); do
  spawn "cyc$i" /bin/sleep 30 || ok=0
  netd detach "cyc$i" >/dev/null 2>&1 || ok=0
done
status=$(st)
left=$(printf '%s' "$status" | field 'len(d["sessions"])')
veths=$(printf '%s' "$status" | field 'len(d["relay_veths"])')
rules=$(printf '%s' "$status" | field 'd["session_rules"]')
names=$(printf '%s' "$status" | python3 -c 'import json,sys; print(",".join(s["name"] for s in json.load(sys.stdin)["sessions"]))'); say "after 50 cycles: sessions=$left ($names) relay_veths=$veths session_rules=$rules"
[ "$left" = "0" ] && [ "$veths" = "0" ] && [ "$rules" = "0" ] && [ "$ok" = 1 ] \
  && check cycle_leak PASS "50 attach/detach: sessions=0 relay_veths=0 ACOS-SESS rules=0" \
  || check cycle_leak FAIL "sessions=$left relay_veths=$veths session_rules=$rules ok=$ok"
jrec "{\"metric\":\"cycle_leak\",\"cycles\":50,\"sessions_left\":$left,\"relay_veths\":$veths,\"session_rules\":$rules,\"all_ok\":$ok}"

# ------------------------------------------------------------ memory PSS ---
for n in 1 5 20; do
  for i in $(seq 1 "$n"); do spawn "m$i" /bin/sleep 60; done
  sleep 0.5
  status=$(st)
  pss=$(python3 - "$status" <<'PY'
import json,sys
d=json.loads(sys.argv[1])
def pss(pid):
    try:
        for line in open(f"/proc/{pid}/smaps_rollup"):
            if line.startswith("Pss:"): return int(line.split()[1])
    except OSError: return 0
    return 0
pasta=pss(d.get("pasta_pid") or 0); fabric=pss(d.get("fabric_pid") or 0)
print(json.dumps({"pasta_kb":pasta,"fabric_kb":fabric,"total_kb":pasta+fabric}))
PY
)
  say "PSS N=$n $pss"
  jrec "{\"metric\":\"pss\",\"n\":$n,\"pss\":$pss}"
  for i in $(seq 1 "$n"); do netd detach "m$i" >/dev/null 2>&1; done
done

pass=$(grep -c 'result=PASS' "$LOG" || true)
fail=$(grep -c 'result=FAIL' "$LOG" || true)
say "== done: ${pass} PASS / ${fail} FAIL"
[ "${fail:-0}" -gt 0 ] && exit 1
exit 0
