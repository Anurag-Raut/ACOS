# NET1 — shared network relay, transport half (design B2): implementation report

**Branch** `net1` (from `netshare` `1249d7d6`). **Status: PARTIAL** — the transport
is implemented, measured and proven end to end; the ART-session wiring and the
`hostport` data path are not done (see §6 and §7).

Implements ADR-0030 amendment 3 (the owner's B2 decision) and the NETSHARE
recommendation, behind `ACOS_SPIKE_NET=1` (default off) with
`ACOS_NET_ARM=relay|base` selecting the arm.

## 1. What landed

| path | what |
|---|---|
| `crates/acos-netd/` | new crate: the relay supervisor (lib + `acos-netd` CLI) |
| `…/src/plan.rs` | the one plan: per-session addresses, the hardened `pasta` argv, the relay rule set, per-session control rules, the session shim |
| `…/src/supervisor.rs` | relay netns (fabric) + `pasta` supervision and restart, per-session netns/veth attach, detach, offline/delay/loss/host-port, `status` with the relay's own footprint, the fabric executor |
| `…/src/proto.rs` | one-JSON-per-line control protocol, structured error codes (`network-unavailable` = fail closed) |
| `…/src/client.rs` | the session API surface (`ensure_up`, `spawn`, `detach`, `offline`, `delay`, `host_port`, `status`), arm selection |
| `…/src/main.rs` | CLI: `up status spawn run detach offline online delay clear hostport kill-pasta shutdown` |
| `crates/acos-services/src/connectivity.rs` | `session_network(idx)` — the per-session address block, one source for `LinkProperties` **and** the transport |
| `crates/acos-netd/tests/plan_rules.rs` | 10 tests: hardening flags, rule set and order, per-session controls, the shim, the protocol, the arm switch |
| `results/handoff/NET1/proof.sh` | the live transport proof (below) |
| `results/handoff/NET1/ab-driver.sh` | the A/B driver, prepared and **not run** (refuses until the arm is wired) |

Shape, as decided:

```
acos-netd (supervisor, own userns; started by the first networked session)
 ├─ acos-netd fabric     relay netns: tap0 from pasta, every session's veth end,
 │                       all boundary + control rules
 ├─ pasta (pinned        the only NAT process on the host; 1 per host
 │   2026_10_02.cba3570)
 └─ per session: `unshare -n` child, own netns, one veth pair (10.163.<idx>.0/30)
```

The supervisor spawns the session because moving a veth end into a netns needs
`CAP_NET_ADMIN` in the user namespace that owns *that* netns, and capability only
flows downward from an ancestor user namespace: the relay netns and every session
netns must belong to the same user namespace, which only the supervisor can
guarantee. This is the spike's own model (`pasta` spawned the guest inside the
netns it had created), not a new constraint.

## 2. Proofs (all PASS, `run/proof.log`, `run/measurements.jsonl`)

`results/handoff/NET1/proof.sh` drives the real supervisor with the pinned static
`pasta` (sha256 `314c37e…0edf60`, identical to the spike's recorded binary) and
`curl` started *inside* each session netns as a guest stand-in (no capability in
the relay netns, same privilege as the guest).

```
check=relay_up              PASS  wall_ms=32  pasta_version=2026_10_02.cba3570
check=http_200_n1           PASS  sessions=1  http_200=1/1   attach_ms_last=17.1
check=http_200_n20          PASS  sessions=20 http_200=20/20 attach_ms_last=43.4
check=offline_isolation     PASS  iso1_failed=1 iso1_200=22 iso2_200=43
check=latency_isolation     PASS  iso2 96ms -> 1301ms (400 ms netem); iso1 125ms -> 82ms
check=host_loopback_blocked PASS  http://127.0.0.1:18080 -> 000 (unreachable)
check=lan_blocked           PASS  http://192.168.31.2:18081 -> 000 (unreachable)
check=fail_closed           PASS  status.up=False, spawn -> network-unavailable
check=restart_recovery      PASS  relay up again, HTTP 200 again
check=cycle_leak            PASS  50 attach/detach: sessions=0 relay_veths=0 ACOS-SESS rules=0
== 10 PASS / 0 FAIL
```

Memory (PSS of the network machinery: `pasta` + fabric; a session costs no
process of its own):

| N | pasta | fabric | total |
|---|---|---|---|
| 1 | 16.3 MiB | 0.62 MiB | **16.9 MiB** |
| 5 | 16.3 MiB | 0.62 MiB | **16.9 MiB** |
| 20 | 16.3 MiB | 0.64 MiB | **16.9 MiB** |

Flat from N=1 to 20, as the design predicted (26 MiB in the NETSHARE prototype;
`pasta`'s PSS varies 12.6–28.3 MiB across runs with traffic, so read *flat*, not
the absolute value). Attach 15–49 ms measured (17.1 ms at N=1, 43.4 ms at N=20):
the shim is `unshare` + one `ip -batch` + one `ip route show` exec, so this is
process-exec bound, **not** the 2 ms rtnetlink floor the design quotes. Making it
2 ms needs `rtnetlink` in the supervisor and no `unshare` exec for the shim
(see §7).

The leak check is the relay netns' own view (`relay_veths` from `ip -o link show`,
`session_rules` from `iptables -S ACOS-SESS`), not a bookkeeping count: the veth's
peer inside the session netns dies with the relay end, so the relay side is the
whole footprint.

## 3. Controls (per session, in the relay netns)

| control | mechanism | proven |
|---|---|---|
| offline/online | `iptables -I ACOS-SESS -i acosv<N> -j DROP` | yes (iso1 failed, iso2 unaffected) |
| latency/loss | `tc qdisc replace dev acosv<N> root netem delay … loss …` | yes (+1205 ms on iso2, iso1 unchanged) |
| host-port opt-in | pasta `-t <port>` (only when named) + `--dport` DROP for sessions that did not opt in | **code path only, not proven** (§6) |

## 4. The boundary

* `pasta` runs with `-t none -u none -T none -U none --no-splice
  --map-host-loopback none --no-map-gw` — measured leak-free against a host
  `127.0.0.1:18080` listener and a host LAN listener.
* RFC1918, link-local, CGNAT, host loopback (`127/8`, `0/8`) are DROPped for
  `-i acosv+` in the **relay** netns, after the conntrack accept so NATed
  TCP still returns. The relay netns is owned by the supervisor's user namespace;
  the guest is a nested user namespace with no capability over it, and seccomp
  keeps `AF_NETLINK`/`SO_MARK` denied (SPIKE-NET2), so the guest cannot see or
  change the rules (moved from "policy the guest could undo" to "netns the guest
  cannot touch", per ADR-0030 amendment 3).
* Fail closed: while the relay is down every attach answers
  `{"error":{"code":"network-unavailable"}}`; the supervisor restarts `pasta` on
  the next tick and the sessions recover (both directions proven; `kill-pasta
  --hold` exists so the down branch is testable without racing the restart).

## 5. `dnsproxyd`

Unchanged from the spike (`acos_art::spike_net::serve_dnsproxyd`), which already
serves the responder in the three `/dev/socket` producers the launcher owns
(`session.rs`, `launch.rs`, `app.rs`). **The fourth producer
(`zygote_prod::prepare_session`, `<plan>/<tag>/sock`) is not wired** — it is the
one the production zygote child mounts, and it is NET2's integration point, not
transport.

## 6. Not done (honest list)

1. **acos-art wiring.** No `acos-art` path calls `acos_netd`, so `ACOS_NET_ARM=relay`
   is not yet a live arm for ART sessions: `acos_art::spike_net::wrap` still
   produces the BASE arm (per-session pasta, unchanged and still working). The
   integration is one function — `plan::shim + Request::Spawn` replacing
   `spike_net::wrap` — but it changes how the guest's stdio reaches the supervisor
   (`Request::Spawn` needs the client's `stdout`/`stderr` fds passed with
   `SCM_RIGHTS`, or `acos-art` switches to per-session log files). That is the
   architectural seam between "transport" and "module", and it is why this is
   PARTIAL rather than DONE.
2. **Host-port opt-in data path.** The gate rules and the `-t <port>` forward are
   implemented, and `hostport add` restarts `pasta` with the port; the end-to-end
   proof (a guest fetching `http://169.254.2.1:8081/` from an opted-in session and
   being refused from a non-opted-in one) has not been run. The per-session
   `REDIRECT` the ADR names is the alternative if pasta's forward turns out not to
   reach the guest on the gateway address.
3. **2 ms attach.** Exec-based shim; `rtnetlink` in the supervisor removes the
   `ip`/`unshare` execs (and would let the supervisor create the session netns
   itself instead of `unshare -n`).
4. **A/B.** Prepared (`ab-driver.sh`), not run, and it refuses to run until (1)
   lands — an `off`-vs-`off` bench would be a false pass.

## 7. Smallest next action

Wire `Request::Spawn` into the ART launcher (`crates/acos-art/src/session.rs` /
`launch.rs`) behind `ACOS_NET_ARM`, passing the guest's stdio; then run
`results/handoff/NET1/proof.sh` with a real `net-check` session and hand
`ab-driver.sh` to the bench owner.

## 8. Reproduce

```sh
# 1. pinned pasta (static), sha256 must be 314c37e878bd6ffe…0edf60
git clone --depth 1 --branch 2026_10_02.cba3570 https://passt.top/passt .cache/net1/passt-src
(cd .cache/net1/passt-src && make clean && make -j8 static VERSION=2026_10_02.cba3570)

# 2. the supervisor
CARGO_TARGET_DIR=$PWD/.cache/net1-target cargo build --release --offline -p acos-netd
CARGO_TARGET_DIR=$PWD/.cache/net1-target cargo test  --release --offline -p acos-netd   # 10 tests

# 3. the live transport proof (≈90 s, no sudo, no kernel change)
ACOS_NET_PASTA=$PWD/.cache/net1/passt-src/pasta \
  BIN=$PWD/.cache/net1-target/release/acos-netd \
  results/handoff/NET1/proof.sh results/handoff/NET1/run

# 4. the control surface by hand
export ACOS_NET_PASTA=$PWD/.cache/net1/passt-src/pasta
.cache/net1-target/release/acos-netd up
.cache/net1-target/release/acos-netd spawn s1 -- /bin/sh -c 'curl -sS -o /dev/null -w "%{http_code}\n" https://example.com'
.cache/net1-target/release/acos-netd offline s1     # then `online s1`
.cache/net1-target/release/acos-netd delay s1 --ms 400
.cache/net1-target/release/acos-netd detach s1
.cache/net1-target/release/acos-netd shutdown
```

The supervisor keeps its socket and logs in `/run/user/<uid>/acos-netd/`
(`netd.log`, `fabric.log`, `pasta.log`); `shutdown` removes the relay netns and
`pasta`.

## 9. Environment / cleanliness

No sudo, no kernel change, no writes outside the worktree except
`/run/user/<uid>/acos-netd/` (the supervisor's socket + logs; removed on
`acos-netd shutdown`). `pasta` was rebuilt from the pinned tag
(`.cache/net1/passt-src`, static, sha256 matches the spike). Disk stayed 82–88 %
(the host sat at 88 % when this started); the build target is deleted after the
run. Host RAM peak during the proofs: < 3 GB.
