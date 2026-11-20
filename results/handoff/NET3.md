# NET3 — the shared relay, finished: host ports, the N=5 flake, supervisor identity, guest cgroup

**Branch** `net3` (from `net2` `d645d25e`). **Status: PARTIAL** — every acceptance
criterion has a passing proof except the two that are only partly mine: the
`acos_net` verb is proven through a live `acos serve` daemon but is **not
advertised over MCP** (MOD1's descriptor surface), and the relay arm's session
*boot* is 4.7–12.6 s vs 0.17–2.2 s without the network module — an open
performance regression with a measured, narrowed cause (below). The tree-binding
failure NET2 handed over is root-caused and fixed; 3 consecutive N=5 runs pass
5/5 scenarios / 10 steps.

Everything stays behind `ACOS_SPIKE_NET=1` (default **off**) with
`ACOS_NET_ARM=relay|base`. `off` is byte-identical to before.

## 1. Orchestrator decision (a): pasta `-T` cannot do it — measured, then the ADR path

Measured first, as instructed (`results/handoff/NET3/exp-pasta-T.sh`, transcript
`run/pasta-T/experiment.log`). Topology: the relay netns (`A`, where `pasta
--netns` joins) and a session netns (`B`) joined to it by a veth, exactly design
B2 without the supervisor; the host listener is `127.0.0.1:18081` in the init
netns.

| `-T` form | from the target ns (A) | from the session ns (B) | pasta log |
|---|---|---|---|
| `-T 18081` | **200** | **refused** | — |
| `-T 169.254.2.15/18081` | refused | refused | `Listening address not allowed for -T` |
| `-T 10.163.1.1/18081` | refused | refused | `Listening address not allowed for -T` |

Control: a plain listener bound inside A is `200` from A and `000` from B, so the
probe machinery is honest. Why: on the pinned tag a `-T` rule is served by a
listening socket pasta binds **in its own target namespace** and only for
loopback → loopback flows (`fwd_nat_from_splice()` returns `PIF_NONE` for
anything else, `fwd.c`); and any explicit address other than loopback is refused
at parse time. So no `-T` form can give "an opted-in session reaches
`127.0.0.1:<port>` on the host" in the relay topology.

**Therefore the fallback, proven end to end** (the ADR's per-session gate, plus
the forwarder the orchestrator named):

```
pasta --map-host-loopback 169.254.2.1     (the relay's own gateway; the only
                                           path from a session to host 127.0.0.1)
ACOS-SESS, per session at attach:
  ... -d 169.254.2.1/32 -p tcp -j REJECT --reject-with tcp-reset
  ... -d 169.254.2.1/32 -j REJECT                                  (default deny)
per opted-in (session, port):
  -I ACOS-SESS ... -d 169.254.2.1/32 -p tcp --dport P -j ACCEPT    (an ACCEPT in
  this chain ends the packet's traversal, so it never reaches the relay's
  link-local/CGNAT drops below it)
per opted-in (session, port): one `acos-netd forward` that `setns`es into the
  session netns and binds 127.0.0.1:P there, dialing 169.254.2.1:P
```

The forwarder is what makes an app's `localhost` work (Metro's
`ws://localhost:8081`, the thing `adb reverse` gives a phone); it exists only for
opted-in ports, is a child of the supervisor, and dies with the session. `REJECT
--reject-with tcp-reset` rather than `DROP`: an un-opened port must be *refused*,
and a silent drop left the guest hanging in SYN retries (measured 4.5 s of boot
time). `pasta` is never restarted by a control (`hostport` is two rule changes and
one process), so the opt-in is hot.

### Proof (real session netns, real host listener, `run/proof.log`)

```
check=relay_up PASS "pasta_version": "2026_10_02.cba3570"
check=hostport_opted_in PASS localhost=200 map=200
check=hostport_not_opted_in_refused PASS localhost=000000 map=000000 gateway=000000
check=other_host_loopback_ports_refused PASS localhost=000000 map=000000 gateway=000000
check=hostport_revoke PASS before=200 after=000000 forwarders_after_del=0
check=identity …
check=stale_supervisor_replaced PASS old_pid=2125723 new_pid=2130636 …
check=relay_up_again "up": true
check=guest_cgroup_enforced PASS … memory.max=1073741824 … python_exit=137 …
check=cgroup_limit_is_the_scope PASS outside-any-scope: unguarded-ok 1572864000
check=no_leak "relay_veths": [] "session_rules": 0 forwarders=0
```

Note the three refusals: a session without the opt-in cannot reach the port, an
opted-in session cannot reach a *different* host-loopback port, and nothing extra
listens on the session's gateway (`gateway=000`).

## 2. The N=5 tree-binding flake: root-caused and fixed

**Cause (from NET2's own evidence).** The three failing runs' session stderr ended
with

```
acos-netd: {"error":{"code":"io","message":"Broken pipe (os error 32)"},"ok":false}
```

That line is the **relay proxy's** (`acos-netd run`) own control-socket error. The
proxy then exited `1`, and the launcher — whose `Child` *is* the proxy — reported
it as `GUEST_EXITED … status exit status: 1` while binding the tree. The guest had
in fact logged `fetch result=HTTP 200` and was still alive. So the failure was a
**misattributed control-plane error**, not a guest death, and it landed randomly at
N=5 because the supervisor's per-connection handlers run concurrently.

**Fix (class, not symptom).**
* the proxy never turns its own failure into the guest's status: it re-connects and
  retries the status query, detaches best-effort, and reports `136` only when the
  session is really gone without a status;
* the supervisor logs *why* a connection ended (it used to return silently), never
  closes a live connection on a transient read error, and retries `EINTR`;
* `shutdown` now ends the supervisor process (`std::process::exit`) instead of
  leaving a live socket with a dead relay behind — a second latent wedge;
* `serve()` takes a `flock` so two clients racing a first start cannot unlink a
  live supervisor's socket.

**Result:** `run/n5x3` — three consecutive N=5 runs, relay arm, real ART:

```
rep1: pass=5/5 steps10=5/5
rep2: pass=5/5 steps10=5/5
rep3: pass=5/5 steps10=5/5
```

No `Broken pipe`, no `GUEST_EXITED` in any of the 15 sessions. **Control arm**
(`run/n5-netoff`): the same five runs with the network module off reach step 4 and
fail the expected `HTTP 200` assertion with an honest `SecurityException` — i.e.
tree binding at N=5 is fine without the relay, so the flake was the relay's.

**Residual (open, measured, not fixed).** One of 18 sessions still bound a *stale*
tree (`assert_text` read `pending` 1 ms after the guest logged `HTTP 200`), and the
first N=5 run of a freshly linked binary failed three captures with `TIMEOUT:
strict settle timed out (probe-floor)` (`probe_seq 5, tree_age_ms 1565` vs `40` in
a passing run). Both point at the same thing: in the relay arm the ART guest's
**boot** is slow — `session_start_ms` 4.7 s (N=1), 9.3–12.6 s (N=5), against
0.17–2.2 s with the network module off — while the relay's own attach is 25–65 ms
and a trivial `/bin/sh` guest through the same proxy takes 96–135 ms. So the cost
is inside the ART guest's boot in the session netns, not in the relay's spawn
path. NET2's N=5 runs reported 254–354 ms, so this is a regression that must be
chased before Metro. Leading candidates (not yet tested): the guest's first frame
wait interacting with the extra netns, or `dnsproxyd` being contacted during boot
(the DNS log shows one `getaddrinfo example.com` at 222 ms, so DNS itself is fine).

## 3. Supervisor identity in `Status`; a stale supervisor is replaced cleanly

`StatusInfo.supervisor` (and the new cheap `Identity` request) carries the
supervisor's pid, `/proc/self/exe`, a **content build id** (FNV-1a 64 of the
binary), its crate version and start time. `client::connect()` compares the
running supervisor with the binary *it* would start (`ACOS_NETD_BIN`, else the
`acos-netd` next to the caller): same path + different hash, or a binary that no
longer exists, is stale. The client then sends `Shutdown`, waits, and escalates to
SIGTERM/SIGKILL **only** for a supervisor that did not exit (a pre-NET3 build),
guarded by `/proc/<pid>/exe` so a recycled pid cannot be hit; then it starts the
current build. Two more fixes fell out of this: the supervisor is always started
from the `acos-netd` binary rather than `current_exe()` (`<acos> serve` is the MCP
daemon, not the relay), and `shutdown` ends the process.

Proof: `check=stale_supervisor_replaced PASS old_pid=… new_pid=… build 87b8… ->
c4eb…` (the copy at the same path is modified, i.e. a "rebuild"). The transition
from a pre-NET3 supervisor needs the escalation once — it is implemented and was
exercised (`acos-netd: stale supervisor 1496339 did not exit on shutdown;
signalling it`).

## 4. The run's cgroup scope reaches the guest

`Request::Spawn` gains `cgroup` (the caller's `/proc/self/cgroup` `0::…` line);
the supervisor writes the guest pid into `<mount><path>/cgroup.procs` **before the
session counts as attached**, reads `/proc/<pid>/cgroup` back to verify, and fails
the spawn closed if either step fails. `acos-netd run` (the proxy, which is inside
the launcher's scope) supplies it.

Proof: inside a `guarded.sh --mem 1G` run the guest printed
`cgroup=/user.slice/…/run-p2130764-…scope` (the guard's own scope),
`memory_max=1073741824`; a 1.5 GiB allocation inside it was OOM-killed
(`python_exit=137`, and the host-side watcher read `oom_kill 1` from the scope's
`memory.events`), while the identical allocation outside any scope completed
(`unguarded-ok 1572864000`). On real ART the guest lands in `acos-art`'s
per-session scope (`memory.max=2048MiB`), which is the scope that bounds it in
production.

## 5. `acos_net` through a live `acos serve` daemon, and through MCP

`results/handoff/NET3/serve-net.py` starts `acos serve --engine art` on a private
socket, launches `net-check` through the daemon's own newline-JSON protocol, then
drives the verb and records every frame verbatim (`run/serve/serve-net.log`):

```
check=daemon_launch PASS
check=daemon_status_reports_offline PASS session row offline=true after `offline`
check=daemon_delay_reported PASS session row delay_ms=400
check=daemon_hostport_reported PASS host_ports contains 18081
check=mcp_lists_acoustic_tools PASS 19 tools
check=mcp_acos_net_not_advertised PASS tools/call -> 'unknown tool `acos_net`'
== 6 PASS / 0 FAIL
```

This found and fixed a real integration bug: the daemon's interactive launch named
its relay session `art-<pid>` (unaddressable) while `acos_net` resolves
`acos-<session id>`, so every control answered `SESSION_NOT_FOUND`. The engine now
passes `ACOS_ART_SESSION_ID` to the launcher (`start_with_zygote(…, Some(id))`) and
`launch.rs` names the relay session with it; a one-shot `verify` keeps
`art-<pid>`. `run/serve-netoff/` keeps the earlier transcript, where the same
controls correctly answered `SESSION_NOT_FOUND` because the session had no network
module — the fail-closed answer, not a fake pass.

**MCP gap:** `acos_net` is not in `acos-mcp`'s descriptors, so MCP answers
`{"code":-32602,"message":"unknown tool `acos_net`"}`. That surface belongs to MOD1
(each module advertises its control ops); this report does not invent it.

## 6. Acceptance criteria

| # | criterion | verdict | evidence |
|---|---|---|---|
| 1 | host-port end to end: opted-in session reaches host `127.0.0.1:18081`; no opt-in refused; other host-loopback ports unreachable | **PASS** | `run/proof.log`: `hostport_opted_in PASS localhost=200 map=200`, `hostport_not_opted_in_refused PASS`, `other_host_loopback_ports_refused PASS`, `hostport_revoke PASS` |
| 2 | N=5 flake root-caused; 3 consecutive N=5 runs 5/5 all 10 steps | **PASS (with an open perf residual)** | cause: proxy EPIPE reported as the guest's exit (`run/n5b`→`run/n5x3`); `run/n5x3/rep{1,2,3}` each `pass=5/5 steps10=5/5`; control `run/n5-netoff` |
| 3 | supervisor identity in Status; a client replaces a stale supervisor cleanly | **PASS** | `run/proof.log`: `identity … "build_id": "c4eb265b66cc2330"`, `stale_supervisor_replaced PASS old_pid=… new_pid=…` |
| 4 | the run's cgroup scope reaches `Request::Spawn`; guest stays in its memcg (relay arm) | **PASS** | `run/proof.log`: `guest_cgroup_enforced PASS … memory.max=1073741824 … python_exit=137`; ART sessions land in `acos-art`'s `memory.max=2048MiB` scope (`run/n5x3/*/art-frames/session-exit.json`) |
| 5 | `acos_net` driven through a live `acos serve` daemon and through MCP once; transcripts saved | **PARTIAL** | `run/serve/serve-net.log`: 5/5 daemon actions PASS; MCP call returns `unknown tool acos_net` (not advertised — MOD1). The session-id bug this exposed is fixed |

Unit tests: `cargo test -p acos-netd` → 11/11 (`forward` round trip +
the new gate/shim/argv assertions).

## 7. Smallest next actions

1. **The relay arm's session boot (perf, blocks Metro).** 4.7–12.6 s vs 0.17–2.2 s.
   Bisect with `ACOS_ART_STAGE_TIMING=1` inside the guest boot (`stage()` markers
   exist; none fire on this path yet) between `shim exec` and the ready line, and
   A/B `--map-host-loopback` on/off plus `ip link set lo up` on/off: those are the
   only two differences the session netns gained.
2. **`acos_net` in MCP descriptors** (MOD1's manifest work) — the verb works; only
   the advertisement is missing. The `net_ctl` error text mentions `acos net up`,
   a CLI verb that does not exist; say `acos-netd up` instead.
3. **The residual stale-tree bind** (1 in 18): likely the same boot latency; if it
   survives (1), make `assert_text`'s first sample rule out a tree older than the
   request (`tree_age_ms` is already reported).

## 8. Reproduce

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_NET_PASTA=$PWD/.cache/net3/passt-src/pasta      # pinned 2026_10_02.cba3570
export ACOS_NETD_BIN=$PWD/.cache/net3-target/release/acos-netd
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
CARGO_TARGET_DIR=$PWD/.cache/net3-target \
  tools/guarded.sh --mem 12G --time 900 -- cargo build --release --offline \
  -p acos-netd -p acos-art -p acos-cli -p acos-mcp

results/handoff/NET3/exp-pasta-T.sh                       # why -T cannot work (~20 s)
results/handoff/NET3/proof-hostport.sh                    # host ports, identity, cgroup (~90 s)
ACOS_NET_PASTA=… tools/art-runtime/run-exclusive.sh --task NET3 -- \
  tools/guarded.sh --mem 20G --time 1100 -- \
  bash results/handoff/NET3/n5-driver.sh $PWD/results/handoff/NET3/run/n5x3 3 5
# acos_net through the daemon + MCP:
ACOS_SPIKE_NET=1 ACOS_NET_ARM=relay ACOS_SESSION_SOCKET=$PWD/.cache/net3/session.sock \
  tools/art-runtime/run-exclusive.sh --task NET3 -- tools/guarded.sh --mem 10G --time 600 -- \
  python3 results/handoff/NET3/serve-net.py $PWD/results/handoff/NET3/run/serve
```

`acos-netd forward --listen 127.0.0.1:P --target 169.254.2.1:P` is the forwarder;
`acos-netd identity` prints the supervisor's identity; `acos-netd shutdown` now
stops the process.

## 9. Environment / cleanliness

No sudo, no kernel change. No writes outside the worktree except the supervisor's
own state directory (`/run/user/1000/acos-netd/`), removed by `acos-netd shutdown`.
Disk 79 % → 86 % during the ART runs and cleaned back (run artifacts are pruned
below, and the build target is deleted at the end of the turn). Host RAM peak
during the N=5 runs: 5 concurrent ART guests inside a 20 GiB guard, host stayed
under ~24 GiB used. `pasta` was rebuilt from the pinned tag; sha256
`314c37e878bd6ffe43200735800d5efd4859d81d46b0450358a103c36c0edf60`, identical to
NET1/NET2 and the spike.
