# NET2 — the relay wired into real ART sessions: implementation report

**Branch** `net2` (from `net1` `744172d4`). **Status: PARTIAL** — the relay is
wired, proven on real ART at N=1 and N=5, and the controls work; the host-port
opt-in data path is **not** proven (a real finding: `pasta -t`/`-T` are opposite
directions, see §5) and 3 of 5 concurrent ART sessions failed a *later* harness
stage after a successful HTTP 200 (§4.3, needs triage).

Everything stays behind `ACOS_SPIKE_NET=1` (default **off**) with
`ACOS_NET_ARM=relay|base`. `off` is byte-identical to before; `base` is NET1's
per-session `pasta`.

## 1. What landed

| path | what |
|---|---|
| `crates/acos-art/src/spike_net.rs` | `arm()`, `net_session_name()`, `netd_bin()`, `relay_wrap()`: the relay arm's command is `acos-netd run <name> -- bwrap …`. `wrap()` (per-session `pasta`) is now the **base** arm only. |
| `crates/acos-art/src/lib.rs` | `Sandbox::net_session(id)`; `command()` dispatches on `ACOS_NET_ARM`. A bad arm value is a structured error, never a silent switch. Host-side `staging` steps stay out of the relay (no network needed; an attach costs a netns + veth). |
| `crates/acos-art/src/session.rs` | the session route names its relay session after its session id (`acos-<id>`) |
| `crates/acos-art/src/zygote_prod.rs` | **the 4th `dnsproxyd` producer**: `prepare_session` serves `<plan>/<tag>/sock` (the directory a production zygote child mounts as `/dev/socket`) and holds the responder until the next session |
| `crates/acos-netd/src/proto.rs` | `Spawn` gains `fds` (count) and `owner_pid`; new `Session { name }` light query; `SessionInfo` gains `exit`/`signal` |
| `crates/acos-netd/src/lib.rs` | the control socket reads with `recvmsg` and keeps `SCM_RIGHTS` descriptors (a plain `read` silently drops them) |
| `crates/acos-netd/src/supervisor.rs` | `spawn_with_fds` (the guest's stdio **is** the caller's: `dup` + `Stdio::from`), `owner_pid` lifetime rule (a dead launcher detaches the session in `tick`), per-session reaping on `Session` so an exit status is never "unknown", `session(name)` accessor |
| `crates/acos-netd/src/client.rs` | `spawn_stdio`, `session`, `wait_exit`, `request_with_fds` (`sendmsg` + `SCM_RIGHTS`) |
| `crates/acos-netd/src/main.rs` | `run` is the launcher's stdio proxy: passes fds 0/1/2, binds the session to its own pid, waits for the guest, detaches, exits with the guest's status; `spawn` keeps the log-file form for CLI use |
| `crates/acos-netd/src/plan.rs` | `relay_pasta_args`: host ports are `-T <port>` (**guest → host**), not `-t` (host → guest); no duplicate `-T`/`-U` specs |
| `crates/acos-protocol/src/error.rs` | `ErrorCode::NetworkUnavailable` (`NETWORK_UNAVAILABLE`, class `host_guard`, retryable, with a hint): the fail-closed answer of an API call while the relay is down |
| `crates/acos-cli/src/engine/serve.rs` | **`acos_net` serve verb**: `status` / `offline` / `online` / `delay` / `hostport`, addressed by session id (`acos-<id>`), host-side only (the relay owns the rules; the guest is never asked) |
| `crates/acos-netd/tests/plan_rules.rs` | the host-port direction test now asserts `-T` |
| `results/handoff/NET2/proof-controls.sh` | the control proof on the wired proxy (below) |

### Why a proxy process, not a direct `Request::Spawn` call

The supervisor must spawn the guest (it owns the user namespace that holds the
session netns), so the launcher's `Child` cannot be the guest. `acos-netd run`
is that `Child`: it passes **its own** stdin/stdout/stderr over the control
socket with `SCM_RIGHTS`, so the guest writes logcat and the interaction
protocol into exactly the pipes the launcher already reads — capture is
identical, not re-implemented. It exits with the guest's status, `SIGKILL`ing it
(or the launcher dying) tears the session down, and `Request::Spawn`'s
`owner_pid` makes the supervisor detach a session whose owner is gone.

Cost: one extra small process per networked session (the proxy). The transport
itself is still flat (NET1: 16.9 MiB from N=1 to 20; measured here at 25–34 ms
attach).

## 2. Proofs

All commands below use `.cache/net2-target/release/{acos,acos-art,acos-netd}`,
the pinned static `pasta` (sha256 `314c37e878bd6ffe…0edf60`, identical to NET1
and the spike), and `ACOS_SPIKE_NET=1 ACOS_NET_ARM=relay`; ART runs went through
`tools/art-runtime/run-exclusive.sh --task NET2`.

| # | check | result | evidence |
|---|---|---|---|
| 1 | relay arm is really live for ART | **PASS** | `/run/user/1000/acos-netd/netd.log`: `session art-<pid> attached idx=1 addr=10.163.1.2/30 … attach_ms=28.9` |
| 2 | guest stdio through `SCM_RIGHTS` (stdout, stderr, stdin) + exit status | **PASS** | `run/stdio.log`: guest `stdout-works`, `stderr-works`, `http=200`, `run_exit=7` |
| 3 | `net-check` on real ART, N=1, relay | **PASS** | `run/n1b.log`: `verify: pass (ok=true, backend=acos-native, fidelity=art-reference) … steps: 10` |
| 4 | N=5 concurrent ART sessions, relay | **PASS for the network half, PARTIAL for the scenario** | `run/n5b/`: `netd.log` shows idx=1..5 concurrently; all five logcat files carry `fetch result=HTTP 200` **and** `internet=true validated=true iface=wlan0 addrs=1 dns=1 dnsList=1.1.1.1 routes=1`; 2 of 5 completed all 10 steps, 3 exited 1 *while binding the tree* after the 200 (§4.3) |
| 5 | offline flips only that session | **PASS** | `run/controls/proof.log`: `check=offline_isolation PASS offline_session_failed=6 offline_session_200=4 peer_200=12` |
| 6 | host-loopback boundary | **PASS** | `check=host_loopback_blocked PASS host_loopback=000 unopted_port=000` |
| 7 | host-port opt-in end to end | **FAIL** | `check=hostport_optin FAIL opted_in=000 not_opted_in=000` — §5 |
| 8 | relay kill → network unavailable → restart → HTTP 200 | **PASS** | `check=fail_closed_restart PASS before=200 during=000000 recovered=200 relay_down=yes spawn=network-unavailable` |
| 9 | no leak after teardown | **PASS** | `check=no_leak "relay_veths": [] "session_rules": 0` |
| 10 | unit tests (plan, rules, protocol, shim) | **PASS** | `cargo test -p acos-netd`: `test result: ok. 10 passed; 0 failed` |

`proof-controls.sh` drives the same `acos-netd run` proxy the launcher uses, so
checks 5–9 exercise the wired path (fd-passing stdio, owner-bound lifetime), not
NET1's harness.

## 3. Controls on the live session API

`acos_net` (serve verb, host-side):

```json
{"tool":"acos_net","args":{"action":"offline"}}            // this session only
{"tool":"acos_net","args":{"action":"delay","ms":400,"loss_pct":1}}
{"tool":"acos_net","args":{"action":"hostport","port":8081}} // Metro opt-in
{"tool":"acos_net","args":{"action":"status"}}               // relay + this session's row
```

The session id maps to the relay session by one shared rule
(`acos_art::spike_net::net_session_name`), so `offline` on a session id addresses
the guest's own netns. A relay that is down answers
`NETWORK_UNAVAILABLE` (fail closed, retryable) — never a silent offline. Not
driven end to end in this increment: the verb is compiled, its client calls are
the ones `proof-controls.sh` proves, but it was not exercised through a live
`acos serve` daemon (no time left in the turn). The MCP tool advertisement
(`acos_service_ctl`-style) is **not** added: that surface belongs to MOD1 and the
generic `forward_session_tool` path already forwards unknown names to the daemon.

## 4. Honest list

### 4.1 Host-port opt-in is not proven (finding: `-t` vs `-T`)

`pasta --help` (the pinned tag): `-t, --tcp-ports SPEC  TCP port forwarding **to
namespace**` (host → guest), `-T, --tcp-ns SPEC  TCP port forwarding **to init
namespace**` (guest → host). NET1 used `-t <port>`, i.e. the wrong direction for
Metro; NET2 switched to `-T <port>` and removed a duplicate `-T none` that made
`pasta` fail to start. With `-T 18081` the guest still gets
`connect: connection refused` on its gateway address (10.163.1.1:18081), so
`-T` alone does not open the path either — most likely the spec needs the
addressing form (`[ADDR[%IFACE]/]PORTS`, e.g. the gateway address) or the
ADR-sanctioned alternative (a per-session `REDIRECT` on that session's veth) is
required. **This is a design question for the orchestrator**, and it is the one
blocking item for METRO1.

### 4.2 `-T none` duplicate was a real bug

The relay did not start at all with two `-T none`/`-U none` specs
(`pasta.log`: `Couldn't listen on requested ports` → `the relay netns did not
come up`). Fixed; `up` is clean.

### 4.3 3 of 5 concurrent ART sessions failed *after* a 200

The three failures are `GUEST_EXITED … the guest ended while binding the tree
(status exit status: 1)`, and their logcat carries `fetch result=HTTP 200` plus
the full validated-`LinkProperties` line, so the network half passed. The base
arm at N=5 cannot be compared as-is: without per-run `--artifacts` dirs 4 of 5
runs fail earlier with `ARTIFACTS_INVALID` (they share `out/net-check`).
Candidate causes to try first: the tree-binding stage races the session's end
through the proxy (the guest's stdout pipe closing), or plain contention at 5
concurrent verifies. Smallest next action: rerun `run/n5b` twice with
`--artifacts` per run and `ACOS_ART_STAGE_TIMING=1`, then diff the stage table of
a passing and a failing run.

### 4.4 A stale supervisor wedges the relay after a rebuild

`Client::ensure_up` adopts any live supervisor, but a supervisor whose binary
was replaced cannot exec its fabric (`unshare: failed to execute …acos-netd
(deleted)`), so every attach answers `network-unavailable` until it is killed.
This bit me twice during the proofs (documented in `run/` logs). Fix: the
supervisor reports its own exe/boot identity in `Status`, and the client replaces
a supervisor whose binary is gone. Not done (out of turn budget).

### 4.5 Per-session memcg on the relay arm

The proxy is inside the run's cgroup scope; the guest (spawned by the
supervisor) is not. `--mem`/`memory.max` therefore bounds the launcher and the
proxy but not the guest on the relay arm. Fix: `Request::Spawn` takes the scope's
`cgroup.procs` path and the supervisor writes the guest pid into it.

### 4.6 2 ms attach

Unchanged from NET1: 25–34 ms measured (exec-based shim). Accepted for v0.4.0.

## 5. A/B

`results/handoff/NET1/ab-driver.sh` is now runnable against the wired build: its
precondition (`grep -rl "acos_netd::client" crates/acos-art/src`) is satisfied
(`spike_net.rs`), so it no longer refuses; `NET1_AB_RUN=1` is still the bench
owner's switch. **No A/B was run here** (no exclusive bench from a coding turn).

## 6. Reproduce

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_NET_PASTA=$PWD/.cache/net2/pasta          # pinned static pasta
export ACOS_NETD_BIN=$PWD/.cache/net2-target/release/acos-netd
CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=$PWD/.cache/net2-target \
  tools/guarded.sh --mem 12G --time 900 -- cargo build --release --offline -p acos-netd -p acos-art -p acos-cli

# transport + controls (≈2 min, no ART)
results/handoff/NET2/proof-controls.sh

# the ART proof (N=1), through the shared lock
ACOS_SPIKE_NET=1 ACOS_NET_ARM=relay tools/art-runtime/run-exclusive.sh --task NET2 -- \
  tools/guarded.sh --mem 12G --time 600 -- env -u ACOS_HOME \
  .cache/net2-target/release/acos verify --engine art \
  --apk fixtures/build/apps/net-check.apk --scenario scenarios/net-check.yaml
```

## 7. Environment / cleanliness

No sudo, no kernel change, no writes outside the worktree except
`/run/user/1000/acos-netd/` (the supervisor's socket + logs, removed by
`acos-netd shutdown`). Disk 83–85 % throughout; the build target is deleted at
the end of the turn. Host RAM peak < 6 GB (5 concurrent ART sessions), no
exclusive runs.
