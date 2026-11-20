# WVKILL — who SIGKILLed the shared engine when a session ended

Branch `wvkill`, from `wvscope` (`81693fbf`). Scope: `crates/acos-webview/src/cdp.rs`
(+ one additive field in `engineStatus`), two test files, one diagnostic tool.

## Status

| item | state |
|---|---|
| 1. sender identified | **identified by elimination + a failing/passing test pair** (§1); one caveat stated honestly (§1.3) |
| 2. root-cause fix | **done**: every engine is forked by one process-lifetime thread (§2) |
| 3. pinning test, real engine | `tests/engine_session_end.rs`: **fails before, passes after** (§3) |
| 4. `cargo test -p acos-webview -p acos-art` (`ACOS_WEBVIEW_TEST=1`) | **PASS, 30 targets / 289 passed / 0 failed** (`evidence/cargo-test.log`) |
| 5. `webview-probe` ×5, `webview-fair` ×5 on ART | **PASS 5/5 + 5/5, broker log: 0 engine restarts, 1 broker for each batch** (§4) |

## 1. Evidence

### 1.1 Reproduced, and it is not ACOS teardown

`evidence/before/`: `acos verify scenarios/webview-probe.yaml --engine art` against a fresh private
broker, 3 runs, every one logs
`engine pid=… generation=1 died (watcher restart): exit status signal: 9 (SIGKILL)` then
`engine restarted … dropped 2 view(s)`. `before/ps-sampler-run1.log` (200 ms `ps` sampler): the engine's
`bwrap` outlives the launcher by one sample and is gone next, the **broker stays up and respawns an engine
(new bwrap)**. The killed process is the broker's *direct child* (`bwrap`), not the broker.

Then the same thing **without ART at all**: `tests/engine_session_end.rs` (two gates on one real broker +
real engine, session A's guest closed and its gate dropped) fails the same way
(`the shared engine (pid …) died after session A ended`) in 0.6 s. So the sender is inside
`acos-webview`, not in `acos-cli`/`acos-art` teardown.

### 1.2 Every suspect from the brief, eliminated with a measurement

No `strace`/`bpftrace`/root audit on this host, so `tools/wvkill/killtrace.c` is an `LD_PRELOAD` shim that
logs every `kill`/`killpg`/`tgkill`/`pidfd_send_signal`/`syscall(kill…)` of every dynamically linked
process in the tree (`evidence/before/killtrace-*.log`):

| suspect | result |
|---|---|
| session teardown `kill(-pgid)` / `cgroup.kill` of ART | the only kills are the launcher's own guest bwrap (and `guarded.sh`'s final group TERM/KILL, 300 ms **after** the engine died); **none targets the broker or its engine** |
| ownership-token reap (`ACOS_ART_SESSION_TOKEN` is inherited by the broker and engine, so `reap_session_leftovers` *could* match them) | not the sender: instrumented `reap_session_leftovers` logged **no** kill in the `verify` path (the broker, which carries the same token, also survived) |
| cgroup scope | WVSCOPE already showed independence (`ACOS_WEBVIEW_SCOPE=0`); the engine dies in the test above with no scope involved |
| broker's own `shutdown()` / `kill()` | `report_dead_engine` sees `signal 9` *before* any broker kill (`restart_now` logs first, then `shutdown`); and the test above has no idle stop |
| **kernel-originated SIGKILL** | the engine test, run **under the shim**, shows the SIGKILL with **no `kill()` from any process** — only a parent-death signal produces that |

### 1.3 The mechanism, and the caveat

`bwrap --die-with-parent` (in `sandbox::bwrap_argv`) is `prctl(PR_SET_PDEATHSIG, SIGKILL)` in bwrap. Per
prctl(2) the "parent" is **the thread that created the process**, not the process. `Broker::browser()` /
`Browser::spawn` forked bwrap on whichever thread first needed the engine — the connection thread of the
first session that used a WebView (`server.rs`, one thread per connection). When that session ended the
thread ended, the kernel SIGKILLed bwrap, and the engine died with it (it is inside bwrap's pid namespace).
That is exactly: only sessions that *used* a WebView, the broker survives, the watcher restarts the engine,
every other session loses its views, and it is independent of cgroups and `guarded.sh`.

*Caveat, stated plainly:* I did **not** capture the kernel's delivery (no tracing tool for it here) and a
stand-in (`bwrap --die-with-parent … sleep` forked from a short-lived thread) did **not** die on this host,
so I could not reproduce it at the bare-bwrap level. What is proven: (a) it is kernel-originated (no
`kill()` anywhere in the tree), (b) it requires no ART/launcher, (c) it is cured by — and only by — moving
the fork off the session thread (§3 fails before / passes after). If a different parent-death path is
involved, the pinned test is the oracle, not this paragraph.

## 2. The fix

`cdp.rs`: `CdpClient::spawn` now runs its fork on **one thread that lives as long as the process**
(`spawner`, a `OnceLock` channel + a `wv-engine-spawner` thread). The parent-death signal still fires when
the broker *process* dies — `--die-with-parent`'s purpose (a dead broker must not leave a Chromium behind) —
and never when a session does. No change to the sandbox argv, to the watcher, or to the wire.
Additive: `engineStatus.engine.pid` (WIRE.md §4 table), which is also what the test asserts on.

## 3. Tests

* `tests/engine_session_end.rs` (`ACOS_WEBVIEW_TEST=1`, real engine): A and B each render a view on one
  broker; A's guest closes and its gate drops; for 3 s the engine pid must stay alive, then B renders, with
  **same engine pid, same generation, `restarts == 0`, `openViews == 1`**, and a new session C joins the
  same engine. **Before the fix: FAILED** (`the shared engine (pid N) died after session A ended`, run
  twice, once under the kill-trace shim). **After: ok.**
* `tests/engine_thread_independence.rs`: the other half of `--die-with-parent` survives the fix — a broker
  *process* that exits takes its bwrap down. (A first version of this file also asserted
  "outlives the spawning thread" with a `sleep` stand-in; it passed before the fix too, so it pinned
  nothing and was removed.)

## 4. Acceptance on ART (`run-runs.sh`, one private broker per batch)

```
probe5 run1..5  rc=0 status=pass          probe5: engine restarts in the broker log = 0, shared brokers started = 1
fair5  run1..5  rc=0 status=pass          fair5:  engine restarts in the broker log = 0, shared brokers started = 1
        tap -> click: 155 498 506 353 584 ms
```
(`evidence/probe5`, `evidence/fair5`: verdicts, logcat, launcher log, per-run broker slices.)

## 5. Not done / notes

* `tools/wvkill/killtrace.c` is a diagnostic (build: `gcc -shared -fPIC -o killtrace.so killtrace.c -ldl`,
  run with `LD_PRELOAD` + `WVKILL_LOG`); it is not wired into anything.
* The pre-existing `acos-webview --die-with-parent` semantics for the *gate-spawned broker* (a gate thread
  spawning `systemd-run … acos-webview`) is a plain process, no PDEATHSIG is set on it, so it was never
  affected.
