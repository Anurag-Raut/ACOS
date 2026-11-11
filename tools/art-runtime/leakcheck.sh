#!/usr/bin/env bash
#
# leakcheck.sh -- before/after snapshot of everything an ART run may leave behind.
#
# The 2026-11-11 OOM (results/art-runtime/R1.md) and the S3 orphan trial both showed
# the same shape of failure: the run ended, the host did not go back to its earlier
# state. This tool is the cheap, repeatable check for that:
#
#   processes   dalvikvm64 / bwrap / acos-art / SessionMain (matched on comm or
#               cmdline), identified by pid **and** start time so pid reuse cannot
#               hide or invent a leak
#   cgroups     systemd scopes under /sys/fs/cgroup with a memory ceiling
#               (MemoryMax) that belong to an acos/guarded run
#   scratch     per-run directories under the repo: target/tmp/guarded.*,
#               .cache/tmp, .cache/acos-*, .cache/art/data/session/*,
#               .cache/art/data/runs/* (S4 per-run data roots)
#   sockets     listening AF_UNIX/AF_INET sockets owned by a matched process
#   memfds      memfd: file descriptors held by a matched process
#
# Usage:
#   tools/art-runtime/leakcheck.sh snapshot STATE_FILE
#   tools/art-runtime/leakcheck.sh check    STATE_FILE
#   tools/art-runtime/leakcheck.sh watch    STATE_FILE -- CMD [ARGS...]
#
# `check` prints one line per leaked item and exits 1 (0 = clean). `watch` takes a
# snapshot, runs CMD under tools/guarded.sh defaults supplied by the caller, checks
# again, and exits 1 on a leak (otherwise the command's own status). STATE_FILE is
# scratch; put it under .cache/, never /tmp.
#
# Env:
#   ACOS_LEAKCHECK_ROOTS  colon-separated extra roots to scan for scratch dirs
#                         (e.g. a test's own --data dir under .cache/)
#
# Examples:
#   tools/art-runtime/leakcheck.sh watch .cache/h1/before.run -- \
#       tools/guarded.sh --mem 4G --time 300 -- ./target/debug/acos-art render-app \
#           --apk fixtures/build/apps/counter.apk --out .cache/h1/counter
#   tools/art-runtime/leakcheck.sh snapshot .cache/h1/s0.tsv
#   tools/art-runtime/leakcheck.sh check    .cache/h1/s0.tsv
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

usage() { sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

mode="${1:-}"
state="${2:-}"
if [ -z "$mode" ] || [ -z "$state" ]; then usage >&2; exit 2; fi
case "$mode" in
    snapshot|check|watch) ;;
    -h|--help|help) usage; exit 0 ;;
    *) echo "leakcheck: unknown mode '$mode' (snapshot|check|watch)" >&2; exit 2 ;;
esac
if [ "$mode" = watch ]; then
    shift 2
    [ "${1:-}" = "--" ] && shift
    if [ $# -eq 0 ]; then echo "leakcheck: watch needs -- CMD" >&2; exit 2; fi
fi
mkdir -p "$(dirname "$state")"

# One scanner for both sides of the comparison, so the snapshot and the check can
# never disagree about what "the same process" means.
scan() {
    # stdin is the heredoc, never the caller's: a scanner that read the caller's
    # script would silently "scan" garbage (seen while developing this tool).
    ACOS_LEAKCHECK_ROOT="$ROOT" python3 - <<'PY'
import glob, os, re, sys

root = os.environ["ACOS_LEAKCHECK_ROOT"]
extra = [p for p in os.environ.get("ACOS_LEAKCHECK_ROOTS", "").split(":") if p]
PATTERNS = ("dalvikvm64", "bwrap", "acos-art", "SessionMain")

def read(path, mode="r"):
    try:
        with open(path, mode) as f:
            return f.read()
    except OSError:
        return None

# ---- processes ------------------------------------------------------------
procs = []
for d in glob.glob("/proc/[0-9]*"):
    pid = os.path.basename(d)
    comm = read(d + "/comm")
    stat = read(d + "/stat")
    if comm is None or stat is None:
        continue
    comm = comm.strip()
    try:  # fields after "(comm)": 1 ppid, ... 20 starttime
        rest = stat[stat.rindex(")") + 2:].split()
        ppid, start, st = rest[1], rest[19], rest[0]
    except (ValueError, IndexError):
        continue
    raw = read(d + "/cmdline", "rb") or b""
    # One record per procedure: a `bash -c '<script>'` cmdline contains literal
    # newlines and would otherwise split into fake "extra" state lines.
    cmdline = (raw.replace(b"\0", b" ").decode("utf-8", "replace")
                  .replace("\n", " ").replace("\t", " ").strip())
    if not any(p in comm or p in cmdline for p in PATTERNS):
        continue
    # Every bwrap on a desktop is not ours: flatpak/snap wrappers come and go
    # constantly and would make every check "leak". An ART bwrap always names a
    # path inside this repo (sysroot, data, shim).
    if comm == "bwrap" and root not in cmdline:
        continue
    procs.append((pid, start, ppid, st, comm, cmdline))

pids = {p[0] for p in procs}

# ---- listening sockets owned by a matched process -------------------------
inode_owner = {}
memfds = []
for pid in pids:
    for fd in glob.glob(f"/proc/{pid}/fd/*"):
        try:
            target = os.readlink(fd)
        except OSError:
            continue
        if target.startswith("socket:["):
            inode_owner[target[8:-1]] = pid
        elif "memfd:" in target:
            memfds.append((pid, os.path.basename(fd), target))

listeners = []
for path, proto in (("/proc/net/tcp", "tcp"), ("/proc/net/tcp6", "tcp6")):
    text = read(path)
    if not text:
        continue
    for line in text.splitlines()[1:]:
        f = line.split()
        if len(f) > 9 and f[3] == "0A" and f[9] in inode_owner:  # 0A = LISTEN
            listeners.append((proto, f[1], inode_owner[f[9]], f[9]))
text = read("/proc/net/unix")
if text:
    for line in text.splitlines()[1:]:
        f = line.split()
        if len(f) >= 7 and f[3] == "00010000" and f[6] in inode_owner:  # SO_ACCEPTCON
            listeners.append(("unix", f[7] if len(f) > 7 else "-", inode_owner[f[6]], f[6]))

# ---- systemd memory scopes ----------------------------------------------
cgroups = []
cg_root = "/sys/fs/cgroup"
want = re.compile(r"(acos|guarded|acos-art|systemd-run)", re.I)
for dirpath, dirnames, filenames in os.walk(cg_root):
    depth = dirpath.count(os.sep) - cg_root.count(os.sep)
    if depth >= 7:
        dirnames[:] = []
    if "memory.max" in filenames and want.search(dirpath):
        cgroups.append((dirpath, (read(dirpath + "/memory.max") or "?").strip()))

# ---- per-run scratch directories ----------------------------------------
scratch = []
for pat in ("target/tmp/*", ".cache/tmp/*", ".cache/acos-*",
            ".cache/art/data/session/*", ".cache/art/data/runs/*"):
    scratch += glob.glob(os.path.join(root, pat))
for r in extra:
    scratch += glob.glob(os.path.join(r, "*"))
scratch = sorted({os.path.realpath(p) for p in scratch if os.path.isdir(p)})

# ---- emit ---------------------------------------------------------------
for pid, start, ppid, st, comm, cmdline in sorted(procs, key=lambda p: int(p[0])):
    print(f"proc\t{pid}:{start}\tppid={ppid}\tstate={st}\tcomm={comm}\tcmd={cmdline}")
for path, memmax in sorted(cgroups):
    print(f"cgroup\t{path}\tmemory.max={memmax}")
for path in scratch:
    print(f"scratch\t{path}")
for proto, addr, pid, inode in sorted(listeners):
    print(f"socket\t{proto}\taddr={addr}\tpid={pid}\tinode={inode}")
for pid, fd, target in sorted(memfds):
    print(f"memfd\t{pid}\t{fd}\t{target}")
PY
}

in_list() {  # $1 type, $2 key, $3 snapshot -> 0 when the item was already present
    cut -f1,2 "$3" | grep -qxF "$1"$'\t'"$2"
}

snapshot() {
    local out="$1" tmp="$1.cur"
    scan > "$tmp" || { echo "leakcheck: scan failed" >&2; exit 3; }
    mv "$tmp" "$out"
    printf 'leakcheck: snapshot %s (%s items) -> %s\n' \
        "$(date -Is)" "$(wc -l < "$out")" "$out"
}

do_check() {
    local snap="$1" cur="$1.cur"
    if [ ! -f "$snap" ]; then
        echo "leakcheck: no snapshot at $snap (run the snapshot mode first)" >&2
        exit 2
    fi
    scan > "$cur" || { echo "leakcheck: scan failed" >&2; exit 3; }
    # A state file we did not write is a bug in this tool, not a leak report.
    bad="$(grep -vcE $'^(proc|cgroup|scratch|socket|memfd)\t' "$cur" || true)"
    if [ "${bad:-0}" -ne 0 ]; then
        echo "leakcheck: refusing to interpret $cur ($bad unrecognised line(s)); first: $(head -1 "$cur")" >&2
        exit 3
    fi
    leaks=0
    local type key rest
    while IFS=$'\t' read -r type key rest; do
        [ -z "$type" ] && continue
        in_list "$type" "$key" "$snap" && continue
        # A scratch dir that no longer exists was another process's live working
        # directory when we scanned (concurrent agent, the wrapper's own
        # target/tmp/guarded.*): transient, not a leak. A real leak persists.
        if [ "$type" = scratch ] && [ ! -e "$key" ]; then continue; fi
        leaks=$((leaks + 1))
        case "$type" in
            proc)    echo "LEAK process pid/start=$key $rest" ;;
            cgroup)  echo "LEAK cgroup $key $rest" ;;
            scratch) echo "LEAK scratch dir $key" ;;
            socket)  echo "LEAK listening socket $key $rest" ;;
            memfd)   echo "LEAK memfd process=$key $rest" ;;
            *)       echo "LEAK $type $key $rest" ;;
        esac
    done < "$cur"
    if [ "$leaks" -ne 0 ]; then
        echo "leakcheck: FAIL — $leaks leak(s) versus $snap" >&2
        rm -f "$cur"
        return 1
    fi
    echo "leakcheck: clean — nothing new versus $snap ($(wc -l < "$cur") items now)" >&2
    rm -f "$cur"
    return 0
}

case "$mode" in
    snapshot) snapshot "$state" ;;
    check)    do_check "$state" ;;
    watch)
        snapshot "$state"
        "$@"
        rc=$?
        echo "leakcheck: watched command exited rc=$rc" >&2
        do_check "$state" || exit 1
        exit "$rc"
        ;;
esac
