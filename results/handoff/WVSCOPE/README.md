# WVSCOPE — a broker that outlives its session, and one that cannot lie about its build

Branch `wvscope`, from `wvtail` (`75bfb6d4`). Both items are the two decisions
WVTAIL §4.4 and §3 left to the orchestrator; the scope of each change is
`crates/acos-webview` (plus the one shared build fingerprint both daemons use).
Private `ACOS_RUNTIME_DIR` + `ACOS_SESSION_SOCKET` + `ACOS_WEBVIEW_SOCKET_DIR`,
every ART run through `tools/art-runtime/run-exclusive.sh --task WVSCOPE` +
`tools/guarded.sh`, builds in `.cache/wvscope-target` (`*_DEBUG=0`, incremental
off), fixtures built in the worktree (`tools/build-apps.sh`,
`tools/build-dogfood-apps.sh`).

## Status

| item | state |
|---|---|
| 1. The shared broker must not die with the session that spawned it | **done** (§1): spawned in its own transient scope, fallback documented + tested, idle stop kept |
| 1a. Host test: session scope SIGKILLed while a second session uses a WebView | **PASS**, both arms with the real engine (§1.3) |
| 1b. Unit test of the spawn command | **done** (§1.4) |
| 2. A stale broker must not serve old code | **done** (§2): build id in the attach handshake, typed refusal, build-private endpoint, ADR note |
| 2a. Byte-level wire test | **done** (`gate::tests::the_attach_line_is_exactly_the_documented_bytes`) |
| 3. `cargo test -p acos-webview -p acos-art -p acos-services` | **PASS, 107 targets / 951 passed / 0 failed** (§3) |
| 4. `webview-probe` ×5 and `webview-fair` ×5 on ART | **PASS 5/5 + 5/5** (§4) |

## 1. The broker no longer shares its spawner's cgroup

WVTAIL §4.3 measured the mechanism: `setsid` changes the session but **not the
cgroup**, so a run harness that tears its session scope down (a scope-wide
SIGKILL, which `guarded.sh` and every run script do) killed the host's engine for
*every* session — the MERGE040A `NO_VIEW: no view <id>` plus one `render` that
never answered, in another agent's run.

`gate.rs::spawn_broker` now starts the daemon inside **its own transient scope**:

```
systemd-run --user --scope --quiet --collect --unit=acos-webview-<endpoint>-<pid> \
    -- <acos-webview> --shared --socket <endpoint> [--cache …] [--idle-shutdown-secs N]
```

* the unit name is derived from the endpoint (12 hex of its sha256) plus the
  spawning pid, so two gates racing for one endpoint cannot collide on a unit
  name (the broker's own bind race still decides who serves), and the unit is
  `--collect`ed when the daemon exits;
* the daemon keeps its `--idle-shutdown-secs` lifetime rule — it stops on its own
  timer, not on its parent's death, and now not on its session's cgroup either;
* measured on this host: the daemon ends up in
  `…/app.slice/acos-webview-498642f67618-1048783.scope`, *not* in the session's
  `run-…scope`, and `ps` shows it as a direct child of the gate (still reapable);
* **fallback**: where `systemd-run --user --scope` is not usable (no user
  manager, a container without systemd) the gate logs one warning naming the
  consequence — "the shared broker is spawned into this session's cgroup
  (setsid): a session teardown that kills its scope will kill the host's engine
  for every other session" — and keeps today's behaviour. `ACOS_WEBVIEW_SCOPE=0`
  selects that arm explicitly.
* availability is probed once per process with a throwaway unit
  (`… --unit=acos-webview-probe-<pid> -- /bin/true`, 5 s cap).

### 1.3 The host test (real engine): `cargo test -p acos-webview --test broker_scope`

`crates/acos-webview/tests/broker_scope.rs`, opt-in with `ACOS_WEBVIEW_TEST=1`.
It runs **both arms** of the same mechanism, each in a child process of its own
inside a transient scope, and kills that scope's cgroup with
`systemctl --user kill --signal=SIGKILL` — what a run harness does:

| arm | measured |
|---|---|
| `scope` (the fix) | child cgroup `…/acos-wvscope-<pid>-scope.scope`; broker cgroup `…/acos-webview-<tag>-<pid>.scope` (**different**); after the SIGKILL the child is dead (`signal 9`) and the broker **is alive**; a second session attaches to that broker (`broker_spawns == 0`), creates a view, loads a page, and gets a real PNG frame, with **no** `engine.restarted` event |
| `fallback` (`ACOS_WEBVIEW_SCOPE=0`) | broker cgroup *is* the session's scope; after the SIGKILL the broker **is dead** (the bug, reproduced); the second session has to start a new engine (`broker_spawns == 1`) |

```
$ ACOS_WEBVIEW_TEST=1 cargo test -p acos-webview --test broker_scope -- --nocapture --test-threads=1
test a_broker_in_its_own_scope_outlives_the_session_scope_that_spawned_it ... ok
```

That the fallback arm reproduces the death is what makes the test a proof rather
than a hope: if the scope is ever dropped from the spawn command, the first arm
fails.

### 1.4 The spawn command, as data

`gate::spawn_plan` returns `BrokerSpawn {program, args, scope}` — what
`spawn_broker` runs — so the argv is pinned without running anything:

* `the_spawned_daemon_runs_in_its_own_transient_scope` — `systemd-run` with
  `--user --scope --quiet --collect --unit=acos-webview-…`, `--`, then the daemon
  and its own arguments in order; and the fallback is exactly the binary itself
  (argv[0], never an argument — a bug the first version of this change had, and
  the integration test caught it);
* `the_scope_unit_is_per_endpoint` — 12 hex of the endpoint's sha256, equal for
  one endpoint, different for another.

## 2. A broker from another build is refused

WVTAIL §3 measured it: the host-wide endpoint was held by a leftover daemon built
from an earlier branch, so a run of the new code silently measured the old code.

* **Build id** (`acos-webview::build_id()`): `"<version>+g<git sha>.<source
  digest>[-dirty]"`. The digest is one implementation
  (`build-support/fingerprint.rs`) `include!`d by both build scripts —
  `crates/acos-webview` (the broker, and the gate that links it) and
  `crates/acos-mcp` (REL3D's `acos serve` identity), so every binary of one build
  agrees and two copies of the digest cannot drift. It is *not* a hash of the
  executable: the gate and the broker are two files of the same build, and it is
  the shared code that must match.
* **Handshake**: the gate ⇄ shared-broker attach line is `attach:2` and carries
  the client's build id; the daemon must answer with its own. Normative copy:
  `results/handoff/WVB/WIRE.md` §0.1 (with the exact bytes); the executable copy
  is `gate::encode_attach` + `parse_attach_answer` + `classify_attach`, and the
  byte-level test is `the_attach_line_is_exactly_the_documented_bytes`.
* **Refusal and recovery**: `BUILD_MISMATCH` names both builds, the daemon's live
  session count and whether it is exiting.
  * live sessions → the daemon is left alone (its engine belongs to somebody) and
    the gate uses **its own endpoint**, `<dir>/broker-<source8>.sock`;
  * no session → the daemon stops itself and the gate takes `broker.sock` over,
    so no daemon is wasted (bounded 3 s wait for the endpoint to free up);
  * `ok` **without** a build id is refused like a mismatch (a pre-v2 daemon
    cannot prove what it is); `attach:1` is still *served* by a v2 daemon, with a
    log line, because it is the client that must not trust a stranger.
* **On the guest wire** the same fact is additive: `hello.broker.{build,pid}`,
  `engineStatus.build` (WIRE.md §4.1).
* ADR-0031 amendment in `DECISIONS.md` (both items, with the measurements).

Tests (no engine):

| test | pins |
|---|---|
| `another_builds_daemon_with_sessions_is_refused_and_this_build_uses_its_own` | the guest's frames come from *this* build's daemon (`{"daemon":"this-build"}`), the stale one saw the attach and **no** frame, `build_mismatches == 1`, `endpoint() == build_private_endpoint(canonical)`, `broker_spawns == 0` |
| `another_builds_idle_daemon_exits_and_this_build_takes_the_endpoint` | the stale daemon (sessions:0, exiting:true) removes its socket; the gate spawns **this build's real `acos-webview`** on the same endpoint (`broker_spawns == 1`, `stale_exits == 1`, spawned pid alive) and the guest gets the broker's own `PROTO_VERSION` refusal through the gate — a full guest→gate→broker round trip without an engine |
| `only_this_build_is_accepted`, `the_daemons_mismatch_answer_is_read_whole` | the classification, including "no build id", a pre-v2 daemon's `attach version unsupported`, and garbage |
| `a_build_private_endpoint_is_next_to_the_shared_one_and_names_the_build` | the endpoint is `broker-<source8>.sock` in the same directory (short enough for `sun_path`) |

## 3. `cargo test -p acos-webview -p acos-art -p acos-services`

`evidence/cargo-test-3crates.log` (`ACOS_WEBVIEW_TEST=1`, i.e. the engine and
scope tests included; run under `run-exclusive.sh --task WVSCOPE` +
`guarded.sh`): **107 test targets, 951 passed, 0 failed**.

```
$ ACOS_WEBVIEW_TEST=1 cargo test -p acos-webview -p acos-art -p acos-services
test result: ok. … (107 targets)
```

No test was weakened, ignored or removed.

### 3.1 One real finding on the way: the test guest paired blobs by arrival, not by id

The first full run failed in `fair_pulls::frame_pulls_are_fair_between_two_views_one_of_them_slow`
("the coalesced burst" arm):

```
assertion `left == right` failed: the blob must belong to the JSON before it
  left: "g1214"  right: "g1209"
```

The same file on the unchanged `wvtail` sources passed
(`evidence/fair-pulls-ab-wvtail-sources.log`), so this branch's timing exposed
it — and the fault was in the **test guest**, not the broker: eight threads call
`guest.render(view)` on one connection, and the helper handed each caller
"the next blob on the channel". WIRE.md §1/§2 allow `render` responses out of
order, so only the blob's **id** says which request it belongs to; the old helper
was right only by luck of scheduling.

`tests/common/mod.rs` now does it the way the wire specifies: the reader thread
delivers each blob to the request it names, and checks the ordering invariant
once, where the stream is seen — a BLOB's id must equal the id of the JSON
immediately before it. That is a *stronger* assertion than before (it holds for
one render at a time and for eight), and the arm is now deterministic:

```
$ ACOS_WEBVIEW_TEST=1 cargo test -p acos-webview --test fair_pulls -- --test-threads=1
test result: ok. 2 passed; 0 failed     (run twice)
```

## 4. Acceptance runs on ART (one private broker, so the *surviving* daemon is the thing measured)

`run-runs.sh <scenario> <count> <tag>`: one ART acquisition per run against **one
broker** of this branch (`ACOS_WEBVIEW_SOCKET_DIR` under `$ACOS_RUNTIME_DIR`), so
every run after the first proves the point — it attaches to the daemon the
previous session left behind. `grep -c "shared broker on"` over the batch = **1**.

### 4.1 `webview-probe` ×5 — 5/5 pass

`evidence/probe5/run1..5` (verdict, steps, logcat, launcher stderr, per-run
broker log slice):

```
probe5 run1..5  rc=0  status=pass   run wall 1030–1376 ms
one broker for the batch: "shared broker on" x1
```

### 4.2 `webview-fair` ×5 — 5/5 pass, taps under the 1 s bound

`evidence/fair5/run1..5`:

```
status: pass pass pass pass pass
tap -> page click: 153 365 2 513 154 ms   (worst 513 ms; WVTAIL's 20-run worst 529 ms)
silent slow ops: per-run broker slices in evidence/fair5/run*.broker.log
```

## 5. What is **not** fixed (reported, not hidden)

* **The engine still dies at the end of a session**, and the broker restarts it
  (`engine pid=… died (watcher restart): exit status signal: 9 (SIGKILL); stderr
  tail: startup noise only`). Measured as **pre-existing and independent of this
  change**: it happens identically with `ACOS_WEBVIEW_SCOPE=0`
  (`evidence/fallback1/broker.log`) and *without* `guarded.sh` at all, i.e. it is
  not the session scope's cgroup. The engine is in the broker's own scope during
  the run (`ps … cgroup` in §1), so what kills it is inside the process tree, not
  a cgroup teardown — WVTAIL §4.3's attribution was therefore incomplete. It does
  not affect this track's acceptance (the broker survives, and the next session
  works against it), but it is the remaining half of MERGE040A's symptom and it
  deserves its own item: a second session that is *live* when the first one ends
  loses its views.

## 6. Files

* `crates/acos-webview/src/gate.rs` — `ATTACH_PROTO: 2`, `encode_attach`,
  `AttachAnswer`, `parse_attach_answer`, `classify_attach`,
  `build_private_endpoint`, `use_build_private_endpoint`, `wait_for_stale_exit`,
  `SpawnScope`, `BrokerSpawn`, `spawn_plan`, `systemd_scope_available`,
  `spawn_scope`, `warn_once`, `spawn_broker`; `GateStats::{build_mismatches,
  stale_exits}`, `Gate::endpoint()`, `mod tests`.
* `crates/acos-webview/src/server.rs` — the attach handshake (`session_for`:
  v2 build check, `BUILD_MISMATCH` with `sessions`/`exiting`, the idle stale
  daemon stopping itself, v1 served with a warning), `hello.broker`, `engineStatus.build`.
* `crates/acos-webview/src/lib.rs` — `VERSION`, `GIT_SHA`, `SOURCE_HASH`,
  `BUILD_DIRTY`, `build_id()`, `build_tag()`.
* `crates/acos-webview/src/wire.rs` — `code::BUILD_MISMATCH`.
* `crates/acos-webview/build.rs`, `build-support/fingerprint.rs`,
  `crates/acos-mcp/build.rs` — one build fingerprint for both daemons.
* `crates/acos-webview/tests/{broker_build.rs,broker_scope.rs}` (+ `common`,
  `gate_lazy` for the attach v2 stub).
* `results/handoff/WVB/WIRE.md` §0.1 (+ §4.1, §7), `DECISIONS.md` ADR-0031
  amendment, `results/handoff/WVSCOPE/`.

## 7. Cleanup

`.cache/wvscope-target`, `.cache/lv`, `.cache/rt-wvscope`, `.cache/rt-fallback`
and the `fixtures/build` copy are deleted at the end; the brokers this track
started are stopped (by their own socket path) and their transient units collected.
