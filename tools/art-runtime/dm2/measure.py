#!/usr/bin/env python3
"""measure.py -- D-M2a: measured facts for the M2 (per-session memory) diagnosis.

Diagnosis only: this harness never touches the ART engine or the product code.
It holds N (`--n`) real `acos serve --engine art` sessions of
`fixtures/build/apps/counter.apk`, settles 60 s after the LAST first frame (the
exact W57/M2 state: `tools/art-runtime/d2_warm.py` semantics), and then dumps the
actual process trees and their `/proc/PID/smaps` so the per-session cost can be
split into mapping groups and into shared vs private pages.

Two phases, each run inside its own `run-exclusive.sh` + `guarded.sh`
acquisition (one ART run per acquisition, the host lock is never held across
reps):

  --phase run --n K --rep R     hold K sessions, settle, dump processes + smaps
  --phase analyze --dir DIR     group the dumps and write analysis.json/.md

The engine/harness decisions stay in `d2_warm.py` (this script imports its
`Serve`, `WarmSampler`, `descendants`, `launch_session`, `survivors`, host gate
and cgroup helpers; it does not change its verdict behaviour).

Grouping (analysis):
  one rule per mapping pathname (libs, boot image, framework jar/dex, dalvik
  spaces, JIT cache, linear alloc, malloc, memfd/ashmem, fonts, resources, icu,
  stacks, acos-art host/compositor, other). Every group reports Rss, Pss,
  Private_Dirty, Private_Clean, Shared_Clean, Shared_Dirty, Swap and the Pss
  components. Across sessions, `sum(Pss)` is the system-wide unique-page total
  for that group (each page shared by k mappings contributes size/k to each of
  the k processes, so the k contributions add to the page size once) and
  `sum(Rss) - sum(Pss)` is the shared remainder. Where the kernel makes
  `/proc/PID/pagemap` PFNs unreadable (this host: ptrace_scope=1, no
  CAP_SYS_ADMIN), that is the stated substitute for pagemap+kpagecount.

Scratch lives under the --outdir (never /tmp). `--keep-scratch` keeps the serve
scratch dir for evidence; the raw smaps dumps are always gzipped.
"""

import argparse
import gzip
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ART = HERE.parent
REPO = ART.parent.parent
sys.path.insert(0, str(REPO / "tools"))
sys.path.insert(0, str(ART))

import benchlib  # noqa: E402
import d2_warm  # noqa: E402  (imported: Serve, sampler, launch, host gate)

MiB = 1024.0 * 1024.0
APK = d2_warm.APK


# ---------------------------------------------------------------- /proc helpers
def read_text(path):
    try:
        return Path(path).read_text(errors="replace")
    except OSError:
        return ""


def uid():
    return d2_warm.utc()


def ns_of(pid):
    out = {}
    for name in ("mnt", "pid", "net", "user", "uts", "ipc", "cgroup", "time"):
        try:
            out[name] = os.readlink("/proc/%d/ns/%s" % (pid, name))
        except OSError:
            out[name] = None
    return out


def ns_inodes(ns):
    out = {}
    for k, v in ns.items():
        m = re.search(r"\[(\d+)\]", v or "")
        out[k] = int(m.group(1)) if m else None
    return out


def cgroup_of(pid):
    for line in read_text("/proc/%d/cgroup" % pid).splitlines():
        if line.startswith("0::"):
            return line[3:].strip()
    return None


def status_fields(pid):
    out = {}
    for line in read_text("/proc/%d/status" % pid).splitlines():
        k, _, rest = line.partition(":")
        if k in ("Name", "Threads", "VmRSS", "VmSize", "RssAnon", "RssFile",
                 "RssShmem", "VmSwap", "NSpid", "Uid", "CapEff"):
            out[k] = rest.strip()
    return out


ROLLUP_KEYS = ("Size", "Rss", "Pss", "Pss_Anon", "Pss_File", "Pss_Shmem",
               "Shared_Clean", "Shared_Dirty", "Private_Clean", "Private_Dirty",
               "Referenced", "Anonymous", "Swap", "SwapPss", "Locked",
               "AnonHugePages", "ShmemPmdMapped", "FilePmdMapped")


def rollup(pid):
    out = dict.fromkeys(ROLLUP_KEYS, 0)
    for line in read_text("/proc/%d/smaps_rollup" % pid).splitlines():
        k, _, rest = line.partition(":")
        if k in out:
            try:
                out[k] = int(rest.split()[0])
            except (ValueError, IndexError):
                pass
    return out


SMAPS_FIELDS = ("Size", "KernelPageSize", "MMUPageSize", "Rss", "Pss",
                "Pss_Dirty", "Shared_Clean", "Shared_Dirty", "Private_Clean",
                "Private_Dirty", "Referenced", "Anonymous", "KSM", "LazyFree",
                "AnonHugePages", "ShmemPmdMapped", "FilePmdMapped", "Shared_Hugetlb",
                "Private_Hugetlb", "Swap", "SwapPss", "Locked", "Pss_Anon",
                "Pss_File", "Pss_Shmem")
HEADER_RE = re.compile(r"^([0-9a-f]+)-([0-9a-f]+) (\S+) (\S+) (\S+) (\d+)\s*(.*)$")


def parse_smaps(pid):
    """[(header dict, {field: KiB})] from /proc/PID/smaps."""
    rows = []
    cur = None
    for line in read_text("/proc/%d/smaps" % pid).splitlines():
        m = HEADER_RE.match(line)
        if m:
            if cur is not None:
                rows.append(cur)
            start, end, perms, offset, dev, inode, path = m.groups()
            size = (int(end, 16) - int(start, 16)) // 1024
            cur = ({"start": int(start, 16), "end": int(end, 16), "perms": perms,
                    "offset": int(offset, 16), "dev": dev, "inode": int(inode),
                    "path": path.strip(), "vsize_kb": size},
                   dict.fromkeys(SMAPS_FIELDS, 0))
            continue
        if cur is None:
            continue
        k, _, rest = line.partition(":")
        if k in SMAPS_FIELDS:
            try:
                cur[1][k] = int(rest.split()[0])
            except (ValueError, IndexError):
                pass
    if cur is not None:
        rows.append(cur)
    return rows


def fds_of(pid):
    out = {"count": 0, "memfd": [], "gralloc": [], "ashmem": []}
    try:
        entries = os.listdir("/proc/%d/fd" % pid)
    except OSError:
        return out
    out["count"] = len(entries)
    for e in entries:
        try:
            target = os.readlink("/proc/%d/fd/%s" % (pid, e))
        except OSError:
            continue
        if not ("memfd:" in target or "ashmem" in target or "dmabuf" in target):
            continue
        rec = {"fd": int(e), "target": target}
        try:
            st = os.stat("/proc/%d/fd/%s" % (pid, e))
            rec["size_bytes"] = st.st_size
            rec["inode"] = st.st_ino
            rec["dev"] = st.st_dev
        except OSError:
            pass
        out["memfd"].append(rec)
        if "acos-gralloc" in target:
            out["gralloc"].append(rec)
        elif "ashmem" in target:
            out["ashmem"].append(rec)
    return out


def all_pids():
    for name in os.listdir("/proc"):
        if name.isdigit():
            yield int(name)


def ancestry(root):
    """{pid: ppid} for root and all its descendants (walks /proc once)."""
    pm = d2_warm.parent_map()
    keep = {root}
    changed = True
    while changed:
        changed = False
        for pid, pp in pm.items():
            if pp in keep and pid not in keep:
                keep.add(pid)
                changed = True
    return {p: pm.get(p) for p in keep}


def tree_text(root, records, title):
    kids = {}
    for pid, rec in records.items():
        kids.setdefault(rec["ppid"], []).append(pid)
    for v in kids.values():
        v.sort()
    lines = []
    seen = set()

    def walk(pid, depth):
        if pid in seen:
            return
        seen.add(pid)
        r = records.get(pid) or {}
        rr = r.get("rollup") or {}
        lines.append("%s%s[%d] %s  rss=%.1f pss=%.1f pd=%.1f  cg=%s  threads=%s"
                     % ("    " * depth, "└─ " if depth else "", pid,
                        r.get("comm", "?"), rr.get("Rss", 0) / 1024.0,
                        rr.get("Pss", 0) / 1024.0,
                        rr.get("Private_Dirty", 0) / 1024.0,
                        r.get("cgroup", "?"), r.get("threads", "?")))
        for c in kids.get(pid, []):
            walk(c, depth + 1)

    lines.append(title)
    walk(root, 0)
    return "\n".join(lines)


# ---------------------------------------------------------------- grouping
ART_LIB_PREFIXES = ("libart", "libdexfile", "libprofile", "libnativebridge",
                    "libnativeloader", "libsigchain", "libdt_fd_forward",
                    "libadbconnection", "libopenjdkjvm", "libunwindstack")
GROUPS = (
    "art runtime libs",
    "other .so",
    "boot image (boot*.art/.oat/.vdex)",
    "app dexopt (.art/.oat/.vdex)",
    "framework jar/dex",
    "dalvik heap: main/region space",
    "dalvik heap: zygote space",
    "dalvik heap: large object space",
    "dalvik heap: non-moving space",
    "dalvik heap: other spaces",
    "dalvik jit code cache",
    "dalvik linear alloc + other dalvik anon",
    "malloc (scudo/libc_malloc/[heap])",
    "gralloc memfd buffers",
    "other memfd/ashmem",
    "fonts (.ttf/.otf/.ttc)",
    "resources (apk)",
    "icu (.dat)",
    "stacks",
    "acos-art host/compositor (anon)",
    "other",
)


def group_of(path, role):
    p = path or ""
    low = p.lower()
    base = os.path.basename(low)
    anon = p.startswith("[anon:") or p.startswith("[heap") or p in ("", "[stack]")
    # dalvik spaces first: their names contain "dalvik"
    if "dalvik" in low or "scudo" in low or "libc_malloc" in low:
        # ART names its image/dex mappings [anon:dalvik-<file>]: those are the
        # boot image / dexopt copies, not dalvik-heap spaces.
        if re.search(r"\.(art|oat|vdex)\s*\]?$", low):
            if os.path.basename(low.split("]")[0]).startswith("boot"):
                return "boot image (boot*.art/.oat/.vdex)"
            return "app dexopt (.art/.oat/.vdex)"
        if "jit-code-cache" in low:
            return "dalvik jit code cache"
        if "zygote" in low:
            return "dalvik heap: zygote space"
        if "large object" in low or "live objects" in low:
            return "dalvik heap: large object space"
        if "non moving" in low or "non-moving" in low:
            return "dalvik heap: non-moving space"
        if "main space" in low or "region space" in low or "allocspace" in low:
            return "dalvik heap: main/region space"
        if "linear" in low or "rosalloc" in low or "thread" in low \
                or "card table" in low or "mark stack" in low or "mod union" in low \
                or "live bitmap" in low or "mark-bitmap" in low \
                or "allocation stack" in low or "baker" in low or "gc " in low:
            return "dalvik linear alloc + other dalvik anon"
        if low.startswith("[anon:scudo") or "libc_malloc" in low or low.startswith("[heap"):
            return "malloc (scudo/libc_malloc/[heap])"
        if anon:
            return "dalvik linear alloc + other dalvik anon"
    if "acos-gralloc" in low:
        return "gralloc memfd buffers"
    if "/memfd:" in low or low.startswith("memfd:") or "ashmem" in low \
            or "dmabuf" in low:
        return "other memfd/ashmem"
    if base.endswith(".so") or ".so." in base or base.endswith(".so (deleted)"):
        if base.startswith(ART_LIB_PREFIXES) or "/libart" in low:
            return "art runtime libs"
        return "other .so"
    if base.endswith((".art", ".oat", ".vdex")):
        if base.startswith("boot") or "/boot" in low:
            return "boot image (boot*.art/.oat/.vdex)"
        return "app dexopt (.art/.oat/.vdex)"
    if base.endswith((".jar", ".dex")) or low.endswith(".jar (deleted)"):
        return "framework jar/dex"
    if base.endswith(".apk") or ".apk " in base:
        return "resources (apk)"
    if base.endswith((".ttf", ".otf", ".ttc")):
        return "fonts (.ttf/.otf/.ttc)"
    if base.endswith(".dat") and "icu" in low:
        return "icu (.dat)"
    if low.startswith("[stack") or "/stack" in low or low.startswith("[vdso") \
            or low.startswith("[vvar") or low.startswith("[anon:stack") \
            or "stack_and_tls" in low or "signal" in low:
        return "stacks"
    if low.startswith("[anon:scudo") or "libc_malloc" in low or low == "[heap]":
        return "malloc (scudo/libc_malloc/[heap])"
    if anon or low.startswith("[anon"):
        if role == "launcher":
            return "acos-art host/compositor (anon)"
        return "other"
    if role == "launcher":
        return "acos-art host/compositor (anon)"
    return "other"


def role_of(comm, exe, pid, daemon_pid):
    if pid == daemon_pid:
        return "daemon"
    if comm == "bwrap":
        return "sandbox"
    if comm == "acos-art":
        return "launcher"
    if comm in ("fixtures.counter", "dalvikvm64") or "dalvikvm" in (exe or ""):
        return "guest"
    return "other"


# ---------------------------------------------------------------- run phase
def dump_process(pid, daemon_pid, evidence_dir):
    rec = {"pid": pid, "ppid": None, "comm": d2_warm.comm_of(pid)}
    try:
        stat = read_text("/proc/%d/stat" % pid)
        rec["ppid"] = int(stat.rsplit(")", 1)[1].split()[1])
    except (IndexError, ValueError):
        pass
    try:
        argv = Path("/proc/%d/cmdline" % pid).read_bytes().split(b"\0")
        rec["argv"] = [a.decode("utf-8", "replace") for a in argv if a]
    except OSError:
        rec["argv"] = []
    rec["argv_str"] = " ".join(rec["argv"])
    rec["cgroup"] = cgroup_of(pid)
    rec["cgroups"] = [l for l in read_text("/proc/%d/cgroup" % pid).splitlines() if l]
    ns = ns_of(pid)
    rec["namespaces"] = ns
    rec["ns_inodes"] = ns_inodes(ns)
    st = status_fields(pid)
    rec["status"] = st
    rec["threads"] = int(st.get("Threads", 0) or 0)
    rec["rss_anon_kb"] = int(str(st.get("RssAnon", "0 kB")).split()[0] or 0) \
        if st.get("RssAnon") else 0
    rec["rss_shmem_kb"] = int(str(st.get("RssShmem", "0 kB")).split()[0] or 0) \
        if st.get("RssShmem") else 0
    try:
        rec["exe"] = os.readlink("/proc/%d/exe" % pid)
    except OSError:
        rec["exe"] = ""
    rec["role"] = role_of(rec["comm"], rec["exe"], pid, daemon_pid)
    r = rollup(pid)
    rec["rollup"] = r
    rec["fds"] = fds_of(pid)
    rec["smaps_entries"] = len(parse_smaps(pid))

    d = evidence_dir / ("pid-%d" % pid)
    d.mkdir(parents=True, exist_ok=True)
    raw = read_text("/proc/%d/smaps" % pid).encode()
    with gzip.open(d / "smaps.gz", "wb", compresslevel=6) as fh:
        fh.write(raw)
    for name, text in (("maps", read_text("/proc/%d/maps" % pid)),
                       ("smaps_rollup", read_text("/proc/%d/smaps_rollup" % pid)),
                       ("status", read_text("/proc/%d/status" % pid)),
                       ("cmdline", rec["argv_str"] + "\n"),
                       ("cgroup", "\n".join(rec["cgroups"]) + "\n"),
                       ("namespaces", "\n".join("%s -> %s" % (k, v)
                                                for k, v in ns.items()) + "\n")):
        (d / name).write_text(text)
    return rec


def run_phase(args):
    tag = "n%d-rep%d" % (args.n, args.rep)
    scratch = Path(args.outdir) / ("scratch-%s" % tag)
    if scratch.exists():
        shutil.rmtree(scratch)
    (scratch / "sessions").mkdir(parents=True)
    evidence = Path(args.evidence) / tag
    if evidence.exists():
        shutil.rmtree(evidence)
    raw_dir = evidence / "raw"
    raw_dir.mkdir(parents=True, exist_ok=True)
    doc = {"schema": "acos-dm2-measure-run/1", "utc": uid(), "phase": "run",
           "n": args.n, "rep": args.rep, "settle_s": args.settle,
           "engine": "art", "apk": str(APK),
           "apk_sha256": benchlib.sha256_file(APK),
           "git_head": subprocess.run(
               ["git", "-C", str(REPO), "rev-parse", "HEAD"],
               capture_output=True, text=True).stdout.strip(),
           "binary": args.acos, "binary_sha256": benchlib.sha256_file(args.acos),
           "host_before": d2_warm.host_now(), "launches": [], "ok": False}
    art_bin = str(Path(args.acos).with_name("acos-art"))
    doc["acos_art"] = art_bin
    doc["acos_art_sha256"] = benchlib.sha256_file(art_bin)
    serve = daemon = sampler = None
    t0 = time.time()
    try:
        probe = benchlib.binary_provenance(args.acos)
        doc["profile"] = probe.get("profile")
        doc["verdict_eligible_binary"] = probe.get("verdict_eligible")
        if not probe.get("verdict_eligible"):
            raise RuntimeError("refusing a non-release binary: %s"
                               % probe.get("not_eligible_reason"))
        serve = d2_warm.Serve(args.acos, scratch, max_sessions=max(args.n + 2, 8))
        job = benchlib.cgroup_path(serve.proc.pid)
        if job is None:
            raise RuntimeError("cannot resolve the job cgroup of acos serve")
        doc["job_cgroup"] = str(job)
        daemon = serve.client()
        daemon.call("ping", {})
        sampler = benchlib.ChurnSampler(
            job, sampler_pid=os.getpid(), root_pid=os.getpid(),
            scope_parent=Path(job).parent,
            artifacts=[str(scratch / "sessions")])
        sampler.start()
        time.sleep(args.bare_settle)
        win = sampler.window(time.time() - args.bare_settle + 1.0)
        doc["bare"] = {"settle_s": args.bare_settle,
                       "net_anon_shmem_mib": (win["last"]["net_anon_shmem_mib"]
                                              if win and win.get("last") else None)}
        session_pids = {}
        for i in range(args.n):
            before = d2_warm.descendants(serve.proc.pid)
            sid, base = d2_warm.launch_session(daemon, i, "dm2")
            r = daemon.call("acos_launch", {"base": base, "apk": str(APK)})
            env = r["envelope"]
            value = env.get("result") or {}
            new = sorted(d2_warm.descendants(serve.proc.pid) - before)
            session_pids[i] = new
            doc["launches"].append({
                "i": i, "session_id": sid, "launch_ms": round(r["wall_ms"], 3),
                "ok": bool(env.get("ok")),
                "error": (env.get("error") or {}).get("code")
                if env.get("error") else None,
                "frame_seq": (value.get("frame") or {}).get("frame_seq")
                if isinstance(value.get("frame"), dict) else None,
                "first_frame_at": time.time() if env.get("ok") else None,
                "pids": new,
                "result_keys": sorted(value.keys()),
                "heap": value.get("heap"),
            })
            if not env.get("ok"):
                break
        firsts = [s["first_frame_at"] for s in doc["launches"] if s.get("first_frame_at")]
        if len(firsts) != args.n:
            raise RuntimeError("%d/%d sessions reached a first frame"
                               % (len(firsts), args.n))
        doc["last_first_frame_utc"] = uid()
        deadline = max(firsts) + args.settle
        while time.time() < deadline:
            time.sleep(min(1.0, max(0.0, deadline - time.time())))
        settled_from = deadline - 5.0
        sw = sampler.window(settled_from)
        doc["settled_sampler"] = {
            "net_anon_shmem_mib": sw["net_anon_shmem_mib"] if sw else None,
            "last": sw["last"] if sw else None,
            "samples": sw["samples"] if sw else 0}
        total = (sw["last"]["net_anon_shmem_mib"] if sw and sw.get("last") else None)
        # Every cgroup that carries this job: the guarded scope plus each guest
        # scope the launcher made (the same set the sampler sums).
        cg_paths = [job] + [Path(c) for c in (sampler.extras or [])]
        cg_rows = {}
        for cgp in cg_paths:
            st = benchlib.read_memstat(cgp) or {}
            cg_rows[str(cgp)] = {
                "anon_mib": round(st.get("anon", 0) / MiB, 3),
                "file_mib": round(st.get("file", 0) / MiB, 3),
                "shmem_mib": round(st.get("shmem", 0) / MiB, 3),
                "pagetables_mib": round(st.get("pagetables", 0) / MiB, 3),
                "kernel_mib": round(st.get("kernel", 0) / MiB, 3),
                "anon_shmem_mib": round((st.get("anon", 0) + st.get("shmem", 0))
                                        / MiB, 3),
                "memory_current_mib": round(
                    (benchlib.read_cgroup_int(cgp, "memory.current") or 0) / MiB, 3),
                "memory_peak_mib": round(
                    (benchlib.read_cgroup_int(cgp, "memory.peak") or 0) / MiB, 3),
            }
        tot = {k: round(sum(v[k] for v in cg_rows.values()), 3)
               for k in ("anon_mib", "shmem_mib", "anon_shmem_mib", "file_mib",
                         "pagetables_mib", "kernel_mib", "memory_current_mib",
                         "memory_peak_mib")}
        tot["cgroups"] = cg_rows
        tot["job_cgroup"] = str(job)
        doc["cgroup_settled"] = tot
        doc["total_anon_shmem_mib"] = total
        doc["incremental_mib_per_session"] = (
            round((total - doc["bare"]["net_anon_shmem_mib"]) / args.n, 3)
            if total is not None and doc["bare"]["net_anon_shmem_mib"] is not None else None)

        # ---- the dump: every process of the daemon's tree, at settle ----
        tree = ancestry(serve.proc.pid)
        records = {}
        for pid in sorted(tree):
            if not Path("/proc/%d" % pid).exists():
                continue
            records[pid] = dump_process(pid, serve.proc.pid, raw_dir)
        doc["processes"] = list(records.values())
        # session attribution: pid sets recorded per launch (+ any later child)
        pid_session = {}
        for i, pids in session_pids.items():
            for p in pids:
                pid_session[p] = i
        # children spawned after the launch snapshot follow their parent
        changed = True
        while changed:
            changed = False
            for pid, rec in records.items():
                if pid not in pid_session and rec.get("ppid") in pid_session:
                    pid_session[pid] = pid_session[rec["ppid"]]
                    changed = True
        doc["pid_session"] = {str(k): v for k, v in sorted(pid_session.items())}
        doc["trees"] = {}
        for i in range(args.n):
            roots = [p for p, s in pid_session.items() if s == i
                     and (records.get(p) or {}).get("ppid") == serve.proc.pid]
            if not roots:
                roots = [p for p, s in pid_session.items() if s == i]
            doc["trees"][str(i)] = tree_text(
                min(roots) if roots else 0, records,
                "session %d" % i)
        doc["tree_host"] = tree_text(serve.proc.pid, records,
                                     "acos serve (daemon) tree")
        # per-session process anon+shmem (M2 attribution)
        per = []
        for i in range(args.n):
            pids = [p for p, s in pid_session.items() if s == i
                    and p in records]
            vals = [(records[p]["rss_anon_kb"] + records[p]["rss_shmem_kb"]) / 1024.0
                    for p in pids]
            per.append({"i": i, "pids": pids,
                        "process_anon_shmem_mib": round(sum(vals), 3)})
        doc["per_session"] = per
        # guest heap properties (the launcher writes them into the session data dir)
        # The heap table the launcher bound into the sandbox: the run-exclusive
        # data dir (ACOS_ART_DATA) holds the /data/acos.properties it ro-binds.
        # It is deleted with the acquisition, so it is copied here, at settle.
        props = {}
        art_data = os.environ.get("ACOS_ART_DATA")
        if art_data:
            for p in sorted(Path(art_data).glob("*.properties")):
                props[str(p)] = read_text(p)
            doc["art_data"] = str(art_data)
        doc["guest_props"] = props
        doc["ok"] = True
    except Exception as exc:  # noqa: BLE001 -- record it, never fake a row
        doc["error"] = "%s: %s" % (type(exc).__name__, exc)
    finally:
        if sampler is not None:
            sampler.stop()
            doc["sample_count"] = len(sampler.samples)
            doc["sampler_errors"] = sampler.errors[:5]
            doc["sampler_summary"] = sampler.summary()
            with open(evidence / "samples.json", "w") as f:
                json.dump(sampler.samples, f)
        if daemon is not None:
            try:
                for s in doc["launches"]:
                    daemon.call("acos_destroy", {"base": {
                        "request_id": "dm2-destroy-%s" % s["session_id"],
                        "session_id": s["session_id"]}})
            except Exception:  # noqa: BLE001
                pass
            try:
                daemon.close()
            except Exception:  # noqa: BLE001
                pass
        if serve is not None:
            doc["serve_rc"] = serve.close()
        # Session metadata is written when the session stops: keep the heap table
        # and the frame geometry the launcher recorded before scrubbing scratch.
        live = {}
        for p in sorted(scratch.glob("sessions/*/art-frames/live-session.json")):
            try:
                j = json.loads(read_text(p))
            except ValueError:
                j = {}
            live[str(p.relative_to(scratch))] = {
                "heap": j.get("heap"),
                "steps": [{"cmd": st.get("cmd"), "ok": st.get("ok"),
                           "width": st.get("width"), "height": st.get("height"),
                           "buffer_generation": st.get("buffer_generation")}
                          for st in (j.get("steps") or [])][:4],
                "protocol": j.get("protocol"),
                "capture_source": j.get("capture_source")}
        if live:
            doc["live_session"] = live
            (evidence / "live-session-heap.json").write_text(json.dumps(live, indent=1))
        time.sleep(0.5)
        doc["survivors"] = d2_warm.survivors(scratch)
        doc["leftover_scan"] = scan_leftovers(scratch)
        doc["host_after"] = d2_warm.host_now()
        doc["wall_s"] = round(time.time() - t0, 3)
        doc["utc_finished"] = uid()
        with open(evidence / "run.json", "w") as f:
            json.dump(doc, f, indent=2)
        print("dm2: wrote %s ok=%s n=%d total=%s incr=%s survivors=%d"
              % (evidence / "run.json", doc.get("ok"), args.n,
                 doc.get("total_anon_shmem_mib"),
                 doc.get("incremental_mib_per_session"), len(doc["survivors"])))
        if args.keep_scratch:
            doc["scratch_kept"] = str(scratch)
        else:
            shutil.rmtree(scratch, ignore_errors=True)
    return doc


def scan_leftovers(scratch):
    """Any ART guest / bwrap / acos-art still naming this rep's scratch."""
    out = []
    for pid in all_pids():
        cmd = read_text("/proc/%d/cmdline" % pid).replace("\0", " ")
        if str(scratch) in cmd and pid != os.getpid():
            out.append({"pid": pid, "comm": d2_warm.comm_of(pid), "cmd": cmd[:240]})
    return out


# ---------------------------------------------------------------- analysis
def metric_sum(rows):
    keys = ("Rss", "Pss", "Private_Dirty", "Private_Clean", "Shared_Clean",
            "Shared_Dirty", "Swap", "Size", "Pss_Anon", "Pss_File", "Pss_Shmem")
    return {k: sum(r[1].get(k, 0) for r in rows) / 1024.0 for k in keys}


def new_session(rec):
    return {"pid": rec["pid"], "role": rec["role"], "comm": rec["comm"],
            "groups": {}, "rss_anon_shmem_mib":
                round((rec["rss_anon_kb"] + rec["rss_shmem_kb"]) / 1024.0, 3),
            "rollup_mib": {k: round(rec["rollup"].get(k, 0) / 1024.0, 3)
                           for k in ("Rss", "Pss", "Private_Dirty",
                                     "Private_Clean", "Shared_Clean",
                                     "Shared_Dirty", "Swap", "Pss_Anon",
                                     "Pss_File", "Pss_Shmem", "Anonymous")}}


def analyse_evidence(root):
    root = Path(root)
    runs = []
    for p in sorted(root.glob("n*-rep*/run.json")):
        d = json.loads(p.read_text())
        d["_dir"] = str(p.parent)
        runs.append(d)
    doc = {"schema": "acos-dm2-analysis/1", "utc": uid(), "runs": {}}
    for run in runs:
        n = run["n"]
        tag = "n%d-rep%d" % (n, run["rep"])
        recs = {r["pid"]: r for r in run["processes"]}
        pid_session = {int(k): v for k, v in run.get("pid_session", {}).items()}
        sessions = []
        group_total = {}
        files = {}      # (sessions' sessions) path -> per-entry aggregate
        buffers = []
        for i in range(n):
            pids = [p for p, s in pid_session.items() if s == i and p in recs]
            per_group = {}
            buf_fds = []
            heap = {}
            for pid in pids:
                rec = recs[pid]
                role = rec["role"]
                for hdr, m in parse_smaps_file(Path(run["_dir"]) / "raw" /
                                               ("pid-%d" % pid) / "smaps.gz"):
                    g = group_of(hdr["path"], role)
                    per_group.setdefault(g, []).append((hdr, m))
                    key = (hdr["path"] or ("[anon role=%s]" % role))
                    f = files.setdefault(key, {"path": key, "role": role,
                                               "inode": hdr["inode"],
                                               "sessions": set(), "pids": 0,
                                               "rows": []})
                    f["sessions"].add(tag)
                    f["pids"] += 1
                    f["rows"].append((hdr, m))
                    if "gralloc" in (hdr["path"] or ""):
                        heap.setdefault("gralloc_maps", 0)
                        heap["gralloc_maps"] += 1
                for fd in rec["fds"]["gralloc"]:
                    buf_fds.append({"pid": pid, "role": role, **fd})
            groups = {g: metric_sum(rows) for g, rows in per_group.items()}
            # per-session dalvik heap detail (Rss vs reserved VSS)
            for g, rows in per_group.items():
                if g.startswith("dalvik heap"):
                    heap[g] = {"mappings": len(rows),
                               "reserved_mib": round(sum(r[0]["vsize_kb"] for r in rows)
                                                     / 1024.0, 3),
                               "rss_mib": round(sum(r[1]["Rss"] for r in rows)
                                                / 1024.0, 3),
                               "pss_mib": round(sum(r[1]["Pss"] for r in rows)
                                                / 1024.0, 3),
                               "private_dirty_mib": round(
                                   sum(r[1]["Private_Dirty"] for r in rows) / 1024.0, 3)}
            inodes = {}
            for fd in buf_fds:
                e = inodes.setdefault(fd.get("inode"), {
                    "inode": fd.get("inode"), "size_bytes": fd.get("size_bytes"),
                    "fds": [], "roles": set()})
                e["fds"].append({"pid": fd["pid"], "fd": fd["fd"],
                                 "role": fd["role"]})
                e["roles"].add(fd["role"])
            buf_summary = {
                "unique_memfds": len(inodes),
                "total_mib": round(sum(v["size_bytes"] or 0 for v in inodes.values())
                                   / MiB, 3),
                "fds_total": len(buf_fds),
                "per_inode": [{"inode": v["inode"], "size_mib": round(
                    (v["size_bytes"] or 0) / MiB, 3),
                    "fds": len(v["fds"]), "roles": sorted(v["roles"]),
                    "pids": sorted({f["pid"] for f in v["fds"]})}
                    for v in inodes.values()],
                "mapped_at_settle": sum(1 for hdr, m in
                                        (r for g, rows in per_group.items()
                                         if g == "gralloc memfd buffers"
                                         for r in rows)),
            }
            per_proc = []
            for pid in pids:
                rec = recs[pid]
                pg = {}
                for hdr, m in parse_smaps_file(Path(run["_dir"]) / "raw" /
                                               ("pid-%d" % pid) / "smaps.gz"):
                    pg.setdefault(group_of(hdr["path"], rec["role"]), []).append(
                        (hdr, m))
                per_proc.append({
                    "pid": pid, "comm": rec["comm"], "role": rec["role"],
                    "ppid": rec["ppid"], "cgroup": rec["cgroup"],
                    "threads": rec["threads"],
                    "rollup_mib": new_session(rec)["rollup_mib"],
                    "groups": {g: metric_sum(rows) for g, rows in pg.items()},
                    "rss_anon_shmem_mib": round(
                        (rec["rss_anon_kb"] + rec["rss_shmem_kb"]) / 1024.0, 3),
                })
            sess = {"tag": tag, "i": i, "n": n, "pids": pids,
                    "buffers_summary": buf_summary,
                    "processes": [{"pid": p["pid"], "comm": p["comm"],
                                   "role": p["role"],
                                   "rss_anon_shmem_mib": p["rss_anon_shmem_mib"]}
                                  for p in per_proc],
                    "per_process": per_proc,
                    "groups": groups, "heap": heap,
                    "buffers": buf_fds,
                    "rss_anon_shmem_mib": round(sum(
                        (recs[p]["rss_anon_kb"] + recs[p]["rss_shmem_kb"])
                        / 1024.0 for p in pids), 3),
                    "rollup_mib": {k: round(sum(recs[p]["rollup"].get(k, 0) for p in pids)
                                            / 1024.0, 3)
                                   for k in ("Rss", "Pss", "Private_Dirty",
                                             "Private_Clean", "Shared_Clean",
                                             "Shared_Dirty", "Swap", "Pss_Anon",
                                             "Pss_File", "Pss_Shmem")}}
            sessions.append(sess)
            for g, v in groups.items():
                a = group_total.setdefault(g, dict.fromkeys(v, 0.0))
                for k, x in v.items():
                    a[k] += x
        # group totals over all N sessions of this run
        for g, a in group_total.items():
            for k in a:
                a[k] = round(a[k], 3)
        per_session_groups = {g: {k: round(v / n, 3) for k, v in a.items()}
                              for g, a in group_total.items()}
        unique = {g: round(a.get("Pss", 0.0), 3) for g, a in group_total.items()}
        shared = {g: round(a.get("Rss", 0.0) - a.get("Pss", 0.0), 3)
                  for g, a in group_total.items()}
        doc["runs"][tag] = {
            "n": n,
            "per_session_groups": per_session_groups,
            "unique_mib": unique,
            "shared_mib": shared, "rep": run["rep"], "ok": run.get("ok"),
            "binary_sha256": run.get("binary_sha256"),
            "acos_art_sha256": run.get("acos_art_sha256"),
            "git_head": run.get("git_head"),
            "settle_s": run.get("settle_s"),
            "total_anon_shmem_mib": run.get("total_anon_shmem_mib"),
            "incremental_mib_per_session": run.get("incremental_mib_per_session"),
            "cgroup_settled": run.get("cgroup_settled"),
            "bare_anon_shmem_mib": (run.get("bare") or {}).get("net_anon_shmem_mib"),
            "per_session_process_anon_shmem_mib": run.get("per_session"),
            "daemon": next(({"pid": p["pid"], "comm": p["comm"],
                             "threads": p["threads"],
                             "rss_anon_shmem_mib": round(
                                 (p["rss_anon_kb"] + p["rss_shmem_kb"]) / 1024.0, 3),
                             "rollup_mib": {k: round(p["rollup"].get(k, 0) / 1024.0, 3)
                                            for k in ("Rss", "Pss", "Private_Dirty")}}
                            for p in run["processes"] if p["role"] == "daemon"), None),
            "trees": run.get("trees"),
            "sessions": sessions,
            "groups_total": group_total,
            "host_before": run.get("host_before"),
            "host_after": run.get("host_after"),
            "survivors": run.get("survivors"),
            "leftover_scan": run.get("leftover_scan"),
            "guest_props": run.get("guest_props"),
            "pid_session": run.get("pid_session"),
            "error": run.get("error"),
        }
    doc["files"] = condensed_files(runs)
    return doc


def parse_smaps_file(path):
    rows = []
    cur = None
    try:
        lines = gzip.open(path, "rt", errors="replace").read().splitlines()
    except (OSError, EOFError):
        return rows
    for line in lines:
        m = HEADER_RE.match(line)
        if m:
            if cur is not None:
                rows.append(cur)
            start, end, perms, offset, dev, inode, pathname = m.groups()
            cur = ({"start": int(start, 16), "end": int(end, 16), "perms": perms,
                    "offset": int(offset, 16), "dev": dev, "inode": int(inode),
                    "path": pathname.strip(),
                    "vsize_kb": (int(end, 16) - int(start, 16)) // 1024},
                   dict.fromkeys(SMAPS_FIELDS, 0))
            continue
        if cur is None:
            continue
        k, _, rest = line.partition(":")
        if k in SMAPS_FIELDS:
            try:
                cur[1][k] = int(rest.split()[0])
            except (ValueError, IndexError):
                pass
    if cur is not None:
        rows.append(cur)
    return rows


def condensed_files(runs):
    """Per file-backed pathname: system-wide Rss vs Pss (one row per run set)."""
    out = {}
    for run in runs:
        tag = "n%d" % run["n"]
        recs = {r["pid"]: r for r in run["processes"]}
        pid_session = {int(k): v for k, v in run.get("pid_session", {}).items()}
        for pid, rec in recs.items():
            if pid not in pid_session:
                continue  # daemon / not a session process
            for hdr, m in parse_smaps_file(Path(run["_dir"]) / "raw" /
                                           ("pid-%d" % pid) / "smaps.gz"):
                if not hdr["path"] or hdr["path"].startswith("["):
                    continue
                e = out.setdefault(hdr["path"], {"path": hdr["path"], "runs": {}})
                r = e["runs"].setdefault(tag, {
                    "mappings": 0, "Rss": 0, "Pss": 0, "Private_Dirty": 0,
                    "Private_Clean": 0, "Shared_Clean": 0, "Shared_Dirty": 0,
                    "Pss_Anon": 0, "Pss_File": 0, "Pss_Shmem": 0, "Swap": 0,
                    "group": group_of(hdr["path"], rec["role"]),
                    "roles": set()})
                r["mappings"] += 1
                r["roles"].add(rec["role"])
                for k in ("Rss", "Pss", "Private_Dirty", "Private_Clean",
                          "Shared_Clean", "Shared_Dirty", "Pss_Anon",
                          "Pss_File", "Pss_Shmem", "Swap"):
                    r[k] += m.get(k, 0)
    flat = []
    for path, e in out.items():
        row = {"path": path, "runs": {}}
        for tag, r in e["runs"].items():
            row["runs"][tag] = {k: (round(v / 1024.0, 3) if isinstance(v, int)
                                    else (sorted(v) if isinstance(v, set) else v))
                                for k, v in r.items()}
        flat.append(row)
    return sorted(flat, key=lambda r: -r["runs"].get("n5", r["runs"].get("n1", {}))
                  .get("Rss", 0))


def marginal(run_a, run_b, div):
    a, b = run_a["groups_total"], run_b["groups_total"]
    out = {}
    for g in sorted(set(a) | set(b)):
        ga, gb = a.get(g, {}), b.get(g, {})
        out[g] = {k: round((gb.get(k, 0.0) - ga.get(k, 0.0)) / div, 3)
                  for k in ("Rss", "Pss", "Private_Dirty", "Shared_Clean",
                            "Shared_Dirty", "Pss_Anon", "Pss_Shmem")}
    return out


def render_analysis(doc):
    runs = doc["runs"]

    def run(n):
        cands = [v for k, v in runs.items() if v["n"] == n and v.get("ok")]
        return cands[0] if cands else None

    lines = ["# D-M2a measured mapping breakdown -- %s" % doc["utc"], ""]
    lines.append("## Runs")
    lines.append("")
    lines.append("| run | sessions | total anon+shmem MiB | incremental MiB/session | cgroup anon MiB | cgroup shmem MiB | survivors |")
    lines.append("|---|---|---|---|---|---|---|")
    for tag, r in sorted(runs.items()):
        cg = r.get("cgroup_settled") or {}
        lines.append("| %s | %d | %s | %s | %s | %s | %d |" % (
            tag, r["n"], r.get("total_anon_shmem_mib"),
            r.get("incremental_mib_per_session"), cg.get("anon_mib"),
            cg.get("shmem_mib"), len(r.get("survivors") or [])))
    lines.append("")
    for n in (1, 5, 10):
        r = run(n)
        if not r:
            continue
        lines.append("## N=%d: top mapping groups by Private_Dirty" % n)
        lines.append("")
        lines.append("Per-session = sum over the %d sessions / %d; unique = sum(Pss) "
                     "across sessions (each shared page counted once); shared = "
                     "sum(Rss) - sum(Pss)." % (n, n))
        lines.append("")
        lines.append("| group | Rss/session | Pss/session | Private_Dirty/session | Private_Clean/session | Shared_Clean/session | Shared_Dirty/session | unique MiB | shared MiB |")
        lines.append("|---|---|---|---|---|---|---|---|---|")
        rows = sorted(r["per_session_groups"].items(),
                      key=lambda kv: -kv[1].get("Private_Dirty", 0))
        for g, v in rows:
            lines.append("| %s | %.2f | %.2f | %.2f | %.2f | %.2f | %.2f | %.2f | %.2f |"
                         % (g, v.get("Rss", 0), v.get("Pss", 0),
                            v.get("Private_Dirty", 0), v.get("Private_Clean", 0),
                            v.get("Shared_Clean", 0), v.get("Shared_Dirty", 0),
                            r["unique_mib"].get(g, 0), r["shared_mib"].get(g, 0)))
        lines.append("")
        for sess in r["sessions"]:
            b = sess.get("buffers_summary") or {}
            lines.append("* session %d: process anon+shmem %.2f MiB; gralloc memfds: "
                         "%s unique, %.2f MiB, %s fds, owners %s; mapped at settle: %s"
                         % (sess["i"], sess.get("rss_anon_shmem_mib"),
                            b.get("unique_memfds"), b.get("total_mib", 0),
                            b.get("fds_total"),
                            [x["roles"] for x in b.get("per_inode", [])],
                            b.get("mapped_at_settle")))
        lines.append("")
    if run(1) and run(5):
        lines.append("## Marginal per session, per group (MiB/session)")
        lines.append("")
        m54 = marginal(run(1), run(5), 4.0)
        m105 = marginal(run(5), run(10), 5.0) if run(10) else {}
        lines.append("| group | (N5-N1)/4 Pss | (N5-N1)/4 Rss | (N5-N1)/4 Private_Dirty | (N10-N5)/5 Pss | (N10-N5)/5 Rss |")
        lines.append("|---|---|---|---|---|---|")
        for g in sorted(m54, key=lambda g: -m54[g]["Pss"]):
            a = m54[g]
            b = m105.get(g, {})
            lines.append("| %s | %.2f | %.2f | %.2f | %s | %s |" % (
                g, a["Pss"], a["Rss"], a["Private_Dirty"],
                "%.2f" % b["Pss"] if b else "-",
                "%.2f" % b["Rss"] if b else "-"))
        lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------- CLI
def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--phase", choices=("run", "analyze", "self-test"))
    ap.add_argument("--acos")
    ap.add_argument("--n", type=int, default=1)
    ap.add_argument("--rep", type=int, default=0)
    ap.add_argument("--outdir", default=str(REPO / ".cache/dm2"))
    ap.add_argument("--evidence",
                    default=str(REPO / "results/art-runtime/evidence/D-M2"))
    ap.add_argument("--settle", type=float, default=60.0)
    ap.add_argument("--bare-settle", type=float, default=10.0)
    ap.add_argument("--keep-scratch", action="store_true")
    ap.add_argument("--dir", help="evidence dir for --phase analyze")
    args = ap.parse_args(argv)
    if args.phase == "self-test":
        return self_test()
    if args.phase == "run":
        if not args.acos:
            ap.error("--acos is required")
        if "/release/" not in args.acos.replace(os.sep, "/"):
            print("dm2: refusing a non-release binary path %s" % args.acos,
                  file=sys.stderr)
            return 2
        Path(args.outdir).mkdir(parents=True, exist_ok=True)
        d = run_phase(args)
        return 0 if d.get("ok") else 1
    root = Path(args.dir or args.evidence)
    doc = analyse_evidence(root)
    with gzip.open(root / "analysis.json.gz", "wt") as f:
        json.dump(doc, f, indent=1)
    md = render_analysis(doc)
    with open(root / "analysis.md", "w") as f:
        f.write(md + "\n")
    print("dm2: wrote %s and analysis.md" % (root / "analysis.json.gz"))
    for tag, r in sorted(doc["runs"].items()):
        print("  %-10s n=%-2d total=%s incr=%s ok=%s"
              % (tag, r["n"], r.get("total_anon_shmem_mib"),
                 r.get("incremental_mib_per_session"), r.get("ok")))
    return 0


def self_test():
    """Classifier checks only; no ART."""
    ok = True

    def check(name, cond):
        nonlocal ok
        ok = ok and bool(cond)
        print("%s %s" % ("PASS" if cond else "FAIL", name))

    check("art lib", group_of("/system/lib64/libart.so", "guest") == "art runtime libs")
    check("other so", group_of("/system/lib64/libc.so", "guest") == "other .so")
    check("boot", group_of("/system/framework/arm64/boot.art", "guest")
          == "boot image (boot*.art/.oat/.vdex)")
    check("app dexopt", group_of("/data/dalvik-cache/x86_64/App.oat", "guest")
          == "app dexopt (.art/.oat/.vdex)")
    check("framework jar", group_of("/apex/x/javalib/core-oj.jar", "guest")
          == "framework jar/dex")
    check("region space", group_of("[anon:dalvik-main space (region space)]", "guest")
          == "dalvik heap: main/region space")
    check("large object", group_of("[anon:dalvik-large object space]", "guest")
          == "dalvik heap: large object space")
    check("jit", group_of("[anon:dalvik-jit-code-cache]", "guest")
          == "dalvik jit code cache")
    check("linear", group_of("[anon:dalvik-LinearAlloc]", "guest")
          == "dalvik linear alloc + other dalvik anon")
    check("scudo", group_of("[anon:scudo:primary]", "guest")
          == "malloc (scudo/libc_malloc/[heap])")
    check("heap", group_of("[heap]", "guest") == "malloc (scudo/libc_malloc/[heap])")
    check("gralloc", group_of("/memfd:acos-gralloc (deleted)", "guest")
          == "gralloc memfd buffers")
    check("font", group_of("/system/fonts/Roboto-Regular.ttf", "guest")
          == "fonts (.ttf/.otf/.ttc)")
    check("icu", group_of("/system/usr/icu/icudt72l.dat", "guest") == "icu (.dat)")
    check("apk", group_of("/data/app/x/base.apk", "guest") == "resources (apk)")
    check("stack", group_of("[stack]", "guest") == "stacks")
    check("launcher anon", group_of("", "launcher") == "acos-art host/compositor (anon)")
    print("dm2 self-test: %s" % ("ok" if ok else "FAILED"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
