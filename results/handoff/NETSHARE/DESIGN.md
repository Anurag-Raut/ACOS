# NETSHARE — one shared network relay for every ACOS session

Branch `netshare` (from `t3/spike-net-session-2-gate`, HEAD bf0420b1). Design note +
measured prototypes; **no product code was changed**. Evidence in `raw/`, prototype
sources in `proto/`.

## Verdict

**B2 — one shared *relay netns* served by one `pasta`, with each session keeping its own
netns joined by a veth pair.** Fixed cost 26 MiB once per host, **~0 MiB per session**
(measured flat from 1 to 20 sessions), attach **2 ms** (floor), per-session
online/offline and latency/loss proven working on a live session, guest holds no
capability over the netns that holds the rules. ~2 weeks, all of option A's plumbing
and none of its TCP stack. Option A stays the upgrade path if pasta's fixed 24 MiB or
its ~15–20 MB/s aggregate ceiling ever becomes a problem; the plumbing is identical, only
the NAT process is swapped.

The owner's original shape ("all sessions join one pre-built netns, one pasta") is
measured too (**B1**, same cost) and **cannot satisfy the per-session controls
requirement** — inside a single netns there is no per-session identity to match on
(§B1). B2 is B1 plus a per-session netns and a veth; that is the whole difference.

## What was measured (and how)

Reproduce: `NETSHARE_PASTA=/path/to/pasta` (build it from the pinned tag:
`git clone --depth 1 --branch 2026_10_02.cba3570 https://passt.top/passt && make -j8`), then
`python3 run_matrix.py --out <jsonl> --arms base,b-shared,b-veth,a-tap --sizes 1,5,20`
and `python3 controls_b.py`, both under `unshare --user --map-root-user`. The pasta binary
itself is not committed.

Prototype harness: one supervisor (own userns, host netns) + a "fabric" shell in the
relay netns + one `pasta`; sessions are real `bwrap` sandboxes (`--unshare-pid/ipc/uts`,
`--cap-drop ALL`, `--clearenv`, no `--unshare-net`) running `dig` (UDP, proves the
datapath end-to-end) and `curl` of a 20 MB Cloudflare file (TCP/TLS, 1–3 reps).
`attach_ms` = request → guest's first packet path usable, measured inside the guest.
PSS = `smaps_rollup` of the **network machinery only** (pasta + fabric/relay; guests are
identical in every arm and excluded). `unshare --user --map-root-user`, no sudo, no
kernel change; `pasta 2026_10_02.cba3570` built from the pinned tag as in the spike.
Every measurement cell ran under `run-exclusive.sh --exclusive` + `guarded.sh`; host RAM
stayed under 25 % and disk at 78–83 % (the host sat at 83 % before the run — scratch was
6 MiB). `raw/measurements.jsonl` holds the 15 kept cells; one further BASE cell was
deleted after fixing a harness bug in that arm (`--unshare-net` was still in its bwrap,
throughput 0), noted here for honesty.

| option | shape | procs | attach | PSS/session | PSS total 1→20 | throughput 1 sess | aggregate @20 | per-session controls | effort |
|---|---|---|---|---|---|---|---|---|---|
| **BASE** (spike today) | 1 pasta per session | **N** | 11.6–17.2 ms (N≤5), **887 ms** (N=20) | **23.9–24.7 MiB** | 24 → **494 MiB** | 11.8–12.0 MB/s | **30.6 MB/s** | yes (in-session netfilter) | done |
| **B1** | one netns, one pasta, sessions join it | 2 | 2.3–4.3 ms | **~0** (flat 24.5→24.3 MiB) | 24.5 → 24.3 MiB | 12.7 MB/s | 14.8 MB/s | **impossible** | 1 wk |
| **B2** (recommended) | one relay netns + pasta; per-session netns + veth | 2 | 27–40 ms prototype, **2 ms floor** | **~0** (flat 26.8→26.3 MiB) | 26.8 → 26.3 MiB | 11.4–12.9 MB/s | 18.2 MB/s | **yes** (rule/netem on the veth) | 2 wk |
| **A** | one userspace NAT owning one TAP fd per session netns | **1** | 0.5–3.8 ms | **~0.2 MiB** (7.0→3.5 MiB total, Python) | 7.0 → 3.5 MiB | n/a (**no TCP**) | n/a | yes (per-fd, in process) | **4–8 wk** |
| **C** | seccomp user-notify socket broker | 1 | n/a (**not prototyped**) | ~0 | ~0 | n/a | n/a | yes (per-fd) | 3–6 wk, high risk |

Throughput at N=20 is per-session mean × 20 concurrent flows (4 MB each); the host's
single-flow ceiling is ~12 MB/s, so the N=20 column measures the relay's parallelism, not
link capacity. B1/B2 funnel all sessions through one single-threaded `pasta`: **~15–18 MB/s
aggregate vs ~30 MB/s with 20 pastas**. For page loads, API calls and file fetches in a
dev tool that is not a limitation; for 20 simultaneous bulk downloads it is the one
measured regression of the shared design.

## The options

### BASE — one pasta per session (the spike branch, for reference)
Measured 23.9–24.7 MiB PSS each, and **887 ms attach at N=20** (20 pasta forks + 20 ×
18 `iptables` execs serialised). This is the cost the owner asked to remove. It is also
the only arm that scales throughput with N, and the only one with zero fixed cost.

### B1 — one shared netns, one pasta, sessions join it
Cheapest possible: 24.5 MiB once, 2–4 ms attach, no per-session process or fd.
**Fails the brief**: inside one netns every session has the same addresses, the same
interfaces and the same uid (all unprivileged userns mappings collapse to the owner's
host uid, so `iptables -m owner --uid-owner` cannot tell sessions apart, and the guest
cannot be given a per-session source address without `CAP_NET_ADMIN` in that netns,
which is exactly what we must not give it). Controls would have to be re-implemented as
a socket-level proxy — i.e. option C. Keep it only as the "no controls needed" mode.

### B2 — recommended
```
supervisor (own userns, host netns)
 ├─ pasta --netns <relay netns> --config-net -I tap0 -a 169.254.2.15/24 -g 169.254.2.1
 │        -t none -u none -T none -U none --no-splice --map-host-loopback none
 └─ relay netns ("fabric", owns tap0 + every session's veth end + all rules)
      ├─ FORWARD → ACOS-SESS (per-session rules) → conntrack → LAN/DROP → tap0 ACCEPT
      └─ per session: veth pair, one end moved into the session's own netns (10.163.N.0/30)
```
* **Controls**: `iptables -I ACOS-SESS -i vN -j DROP` = offline; `tc qdisc add dev vN root
  netem delay 400ms` = latency (loss/har the same way: `REDIRECT` to a per-session proxy
  listening on the relay's loopback). Nothing inside the guest participates, so it works
  on a live session mid-request.
* **Measured** (`raw/controls-summary.json`, 2 sessions, one relay, one pasta):

  | | session 1 (offline 8 s) | session 1 (netem window) | session 2 (session 1 offline) | session 2 (own netem 400 ms) |
  |---|---|---|---|---|
  | median HTTP time | 1111 ms, then `rc=28` timeouts | 67 ms (unaffected) | 84 ms, all 200 | **1275 ms** (vs 72 ms baseline) |

  Each control moved exactly one session, and only for its window.
* **Boundary is better than the spike's**: the rules live in the relay netns, owned by
  the supervisor's userns; the guest is a nested userns with `--cap-drop ALL`, so it can
  neither flush nor see them (the spike had to prove the guest *couldn't* undo rules in a
  netns it was root in). Host loopback/LAN: `-t/-u/-T/-U none` (both directions of pasta
  port mirroring — the spike's `-T none -U none` is not enough, pasta's `-t auto`
  auto-scan still appears in `--netns` mode and must be turned off too), `--no-splice`,
  `--map-host-loopback none`, plus in-relay DROP for RFC1918/link-local/CGNAT/gateway.
  Fail-closed: if pasta is not up, the relay has no `tap0` and refuses to attach sessions.
* **Attach cost**: 27–40 ms measured in the prototype, of which ~25 ms is prototype glue
  (bash fifo round trip + 4 `ip` process execs per session); the same four netlink
  operations measured directly take **2 ms** (`veth_attach_ms=2`), so a Rust `acos-netd`
  using rtnetlink is a ~1–3 ms attach.
* **Reuses from the spike**: the pasta binary + build record + licence
  (`research/spike-net/artifacts/passt-build.txt`), the hardened flag list, the
  `sandbox_flags()` change (drop `--unshare-net`), the `dnsproxyd` responder (unchanged —
  it lives outside the netns), the `connectivity`/`LinkProperties` constants (each
  session still has its own netns and address, so parity stays per-session),
  `research/spike-net/probe.c` re-runs as the reachability check, and the seccomp
  `AF_INET` gate is untouched.
* **Risks**: one `pasta` is a single point of failure for *all* sessions (mitigation:
  supervisor restarts it; a dead pasta = every session offline, which is honest), the
  ~15–18 MB/s aggregate ceiling, and pasta runs outside the sandbox (accepted by the
  owner's "security takes a back seat" decision, and the spike already had this).

### A — one userspace NAT process, one TAP fd per session
Prototyped far enough to measure the parts that decide the architecture: the session
shim creates the TAP inside its own netns (`open("/dev/net/tun")` + `TUNSETIFF`), passes
the fd to the single relay over `SCM_RIGHTS`, and the relay answers ARP, ICMP echo and
NATs UDP per session. **Verified**: `dig @1.1.1.1 speed.cloudflare.com` inside the guest
returned `162.159.140.220` through that relay (raw UDP NAT), one process for N sessions,
7.0 MiB total at N=1, 3.5 MiB at N=20 (Python interpreter; a Rust/C relay is 1–3 MiB),
**+0.17 MiB per session**, attach 3.8 → 0.5 ms.
**Not prototyped: TCP** — and that *is* the design: a multi-tenant TCP stack (passt's
`tcp.c`/`tcp_splice.c`/`tcp_buf.c` ≈ 4 kLOC of a ~12 kLOC tap stack; smoltcp is not
vendored and would still need a NAT/flow layer). So A today is a measured *floor* plus a
4–8 week estimate, not a shippable option. It is the right long-term answer if we ever
want per-session egress identities, per-flow shaping, or to stop shipping a C NAT
process — and it shares every piece of B2's plumbing (relay netns, per-session netns,
supervisor, control API), so choosing B2 now does not close that door.

### C — socket-level broker (seccomp user-notify)
`SECCOMP_RET_USER_NOTIF` on `socket`/`connect`/`bind`/`getsockopt`, broker opens the real
host socket, returns it to the guest via `SECCOMP_IOCTL_NOTIF_ADDFD` (a socketpair whose
far end the broker splices, or an `AF_UNIX` socket the broker relays). Zero per-session
RAM, trivially per-session controls, no netns and no pasta at all. **Not prototyped**: it
needs a full socket-semantics emulation (`getpeername`, `getsockopt`/`SO_*`,
`shutdown(SHUT_WR)` half-close, `TCP_NODELAY`, `MSG_*`, non-blocking + `epoll`
interaction, `sendfile`/`splice`) and a per-connection relay thread or `io_uring` loop; a
round trip per `connect` is cheap but every `setsockopt` an app makes becomes our
problem, and Android code does use them (WebView, OkHttp, exoplayer). 3–6 weeks with the
highest risk of silent app-incompatibility — and it buys nothing B2 does not already have
except "no pasta", which B2 keeps as the proven, pinned binary we already ship.

## Recommendation

Implement **B2** as `acos-netd` (Rust, one per host, started by the first networked
session and supervised): own a userns + relay netns, run the pinned `pasta` with the
hardened flags, create one veth pair per session, install the boundary once, expose
`attach(session) / offline(on|off) / delay(ms, jitter, loss) / har(on|off) / stats`.
Default stays **`off`** (ADR-0030 item 6) until the A/B gate runs, but note that with B2
the memory axis of that gate is no longer the question it was: +26 MiB once per host,
0 per session, 2 ms attach — the gate is now only about cold/warm launch.

Smallest first step: land the relay supervisor behind `ACOS_SPIKE_NET=1` (same gate as
the spike) replacing `spike_net::wrap()` with "attach to the relay", keep the per-session
pasta path selectable as the fallback arm, and re-run the spike's boundary probe + the
`net-check` scenario against it.

## Open questions for the owner

1. **Aggregate throughput ceiling.** One `pasta` is one epoll loop: ~15–18 MB/s across 20
   concurrent sessions vs ~30 MB/s with 20 pastas. Acceptable for a dev tool, or does
   that alone push us to A?
2. **Blast radius of one relay.** A dead/blocked relay takes every session offline at
   once (fail-closed). Accept + supervisor restart, or do you want per-session isolation
   of failures (only possible in BASE/A)?
3. **Boundary relocation.** The LAN/host-loopback rules move from each session's netns
   into the relay's, i.e. from "policy the guest could undo" to "netns the guest holds no
   capability over". Confirm ADR-0030's §3 wording is rewritten that way.
4. **Host-port opt-in (Metro 8081).** With one shared pasta, a named host port is exposed
   to the *relay* netns, i.e. **every** session can reach it. Do we want that (simple) or a
   per-session `REDIRECT` in the relay that only named sessions get (one rule, cheap)?
5. **Where does the control API live?** A new `acos-netd` host daemon, or inside the
   existing host broker that `acos view`/MCP already need? B2 needs one long-lived
   process either way; I recommend the existing broker to avoid a fourth daemon.
6. **Keep BASE as a fallback arm for one release?** Cheap (the code is the spike's) and
   it is the only way to A/B against a known-good transport.

## Appendix — raw numbers

```
arm                        N  attach_ms  pss/sess_MiB  pss_total_MiB  procs  tp_MB/s  tp_max  bytes    ready_s
BASE-pasta-per-session     1     11.6      23.92          23.92         1     11.80   12.51  20 000 000   5.19
BASE-pasta-per-session     1     15.6      26.56          26.56         1     12.00   12.56  20 000 000   5.08
BASE-pasta-per-session     5     17.2      24.51         122.56         5      3.82    7.56  20 000 000  18.20
BASE-pasta-per-session    20    887.2      24.73         494.51        20      1.53    9.20   4 000 000   7.01
B-shared (B1)              1      3.0      24.52          24.52         2     12.66   12.72  20 000 000   4.82
B-shared (B1)              5      3.9       4.91          24.57         2      4.32   10.35  20 000 000  16.11
B-shared (B1)             20      4.3       1.21          24.25         2      0.74    0.94   4 000 000   6.83
B-veth (B2)                1     27.5      26.76          26.76         2     11.37   12.81  20 000 000   5.58
B-veth (B2)                5     39.9       5.43          27.15         2      3.87    7.07  20 000 000  16.50
B-veth (B2)               20     37.2       1.32          26.31         2      0.91    3.39   4 000 000   7.54
A-tap-relay                1      3.8       6.99           7.00         1      n/a     n/a      n/a        n/a
A-tap-relay                5      1.1       0.88           4.41         1      n/a     n/a      n/a        n/a
A-tap-relay               20      0.5       0.17           3.48         1      n/a     n/a      n/a        n/a
veth attach floor (4 netlink ops, no glue): 2 ms
```
`pss/sess` for the shared arms is `total/N` and therefore misleading at small N; the row to
read is `pss_total_MiB`, which is flat 24–27 MiB from 1 to 20 sessions. Kernel-side memory
(veth pair + netns per session, ~tens of KiB slab) is not in PSS and was not measured.

Not measured, stated so nobody quotes it: B2 with a real ART session and the production
`dnsproxyd` path at N=20 (the spike's `net-check` on this branch is the smallest next
action), pasta's per-flow memory at 20 × concurrent flows, and option C end to end.
