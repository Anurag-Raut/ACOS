# NET5 open finding — `offline` is not an offline an app can observe

**Status: OPEN, reproduces deterministically.** One step of the module's
app-level proof does not pass; everything before it does. Recorded here with the
evidence so the next track starts from the measurement instead of from the
symptom.

## What was asked

The brief's proof 4: *"offline control → app sees the loss, online again →
recovers"* — with the same guest process, because a relaunch is a new netns (and
a new relay row), so it would prove nothing about a mid-session control.

## What was built to ask it

`fixtures/apps/net-check` gained a `retry` button that re-runs the same real
HTTPS GET in the same process (the fixture previously fetched only in
`onCreate`). The scenario `scenarios/open-finding-offline.yaml` (kept out of
`scenarios/` while it fails) does:

```
launch (network:real) → status = HTTP 200
service_ctl {module: network, op: offline}      # PASSES
tap retry
assert_text status contains "Exception"          # FAILS: still "pending" after 60 s
service_ctl {module: network, op: online}
tap retry
assert_text status exact "HTTP 200"
```

## What the evidence says

| fact | evidence |
|---|---|
| the module's control really runs and really reaches the relay | step 3 `service_ctl` passes; the relay's own status reports `offline: true` for this session |
| the same retry with the module attached and **online** is instant | `scenarios/net-network-real.yaml` 9/9 + a retry-only variant: `fetch result=HTTP 200` again, same process, ~0 s |
| the failing fetch never reaches the resolver | with `ACOS_SPIKE_NET_DNS_LOG` set, the session logs **one** `getaddrinfo example.com` line (the first fetch) and nothing after the retry |
| the guest stays alive | the assertion polls the live tree 66 times over 60 s and reads the same `pending` text (`polls: 66, waited_ms: 60000`); no crash, no exit, `assert_no_crash` never reached |
| it is **not** the rule shape | three relay behaviours give the identical hang: `-j DROP` (NET3/NET4's), two `-j REJECT` rules (`--reject-with tcp-reset` for TCP and `icmp-port-unreachable` otherwise), and `ip link set <veth> down` |
| it is not the fixture's retry path | the retry works while online; the same tap handler fetches `HTTP 200` in the retry-only variant |

The three relay variants were tried and then **reverted**: `plan::offline_rules`
is NET3/NET4's single `iptables -w -I ACOS-SESS -i <veth> -j DROP` again, because
changing a shipped behaviour on a hypothesis that the experiment did not confirm
would be worse than the open finding. The revert is why `offline` has no
app-level witness today — the earlier behaviour only ever had a *status-level*
one (`session row offline=true`, NET3 §5).

## The narrowed hypothesis (not proven)

The retry does not ask dnsproxyd (no request logged) and does not log a `fetch
failed` line, so the fetch thread is inside `getResponseCode()` and stuck before
it can raise an error. With the network healthy the identical call returns in
milliseconds. The candidates, in order:

1. `HttpURLConnection`'s second request in this ART guest goes through a path that
   needs something the *guest* kernel does not provide once the session's only
   route is gone (a DNS lookup that bionic satisfies from a proxy it cannot
   connect to, or an `InetAddress` cache miss that blocks in `connect(2)` on the
   proxy socket).
2. The OKHttp connection pool's idle connection: the first fetch's socket was
   `disconnect()`ed, but the pool may still try to reuse a socket whose interface
   is down, and the retry blocks in the pool rather than in `connect`.
3. The guest's `AF_INET` socket layer in ACOS (native loader/shim) has a
   timeout-less wait on a socket error path that only triggers when the network
   is broken.

## Smallest next action

Instrument the fetch thread inside the guest (a stack dump on the retry, e.g.
`SIGQUIT`-style or a `Thread.getStackTrace()` log line in the fixture after 10 s
of `pending`) and read where it is blocked. That is one ART run and settles (1)
vs (2) vs (3) without touching the relay.

## Why it matters beyond this proof

An agent's *own* app will hit the same wall: an app that loses the network
mid-session must be able to observe it. Until the cause is known, ACOS can
emulate "no network at launch" (proven, 8/8 and 9/9) but not "the network goes
away" at app level.
