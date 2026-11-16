# SETTLE5 — settle under concurrency: what `probe-floor` actually means

Branch `settle5` from `rel-030` `7168439c`. One commit (`07b01dd6`, plus this
report's evidence). Test env: private `ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`,
`ACOS_VIEW_TEST_DIR` (see §7), every ART run inside
`tools/art-runtime/run-exclusive.sh -- tools/guarded.sh`.

**Status: PARTIAL.** The 5×3 acceptance on this host is PASS (§2, 15/15 + 15/15),
the demand-probe channel's silent-failure class is fixed and now reports itself
(§4), and the recorded NET4 failure is characterised exactly (§3) — but the
original 6.3 s "guest published nothing" event **did not reproduce** in 100
sessions across seven N=5/N=10 arms (§2), so its specific trigger is not proven.
The honest next step is named in §6, and one candidate is an architectural wall
(single-threaded peer, §5).

| # | brief item | verdict |
|---|---|---|
| 1 | reproduce on rel-030 without any network module, 3 reps, name the settle condition that never becomes true | **PARTIAL** — no reproduction in 100 sessions; the condition *is* named from the NET4 artifact (§3): `probe-floor` (newest probe sample not newer than the request floor), because the guest published no sample at all for 6.278 s |
| 2 | root cause (a)/(b)/(c), fix (b)/(c) at the root, (a) only with a no-false-pass rule | **PARTIAL/PASS on the class** — (b) fixed: a dead demand channel now polls at 300 ms instead of the 5 s idle fallback and both the death and a starved probe are named by the guest; all three states are reported on a timeout (§4). No budget change (justified in §3.4) |
| 3 | 5 concurrent × 3 reps pass; single-session settle latency unchanged | **PASS** — `evidence/gates.txt`: 15/15 (counter) + 15/15 (net-check, module off); latency unchanged (§2.2) |
| A | `cargo test -p acos-art -p acos-verify -p acos-cli` green | **PASS except one pre-existing order-dependent test** (§7): both failures reproduce without this branch's code and pass standalone |

---

## 1. The instrumented claims (what the code now records)

`probe-floor` used to mean only *"the newest probe sample is not newer than the
request's floor"*. That is equally consistent with three different worlds, and
all three were silent in the code:

| world | what used to happen | what happens now |
|---|---|---|
| (a) the guest's main looper cannot run the posted probe (busy/blocked) | the host waits out the 1.5 s tree deadline and says `probe-floor` | the guest prints `ACOSLAUNCH MARK probe-stalled pending_ms=…` (rate-limited to 2/s); the host parses it and a timeout carries `settle.probe.guest_stall_pending_ms` |
| (b) the demand channel is dead (guest control thread ended, or the wake write fails) | the write error was discarded (`let _ = writeln!`) and the control thread's exit printed nothing ("no stdin is normal") | `wake_failures` is counted per wake and reported; the guest marks `probe-control-exit reason=…`; **and** a dead control channel switches the guest fallback from 5 s to 300 ms (`P10_PROBE_DEAD_CHANNEL_MS`) |
| (c) the host refuses samples it did receive (seq not newer: a probe-thread restart) | `if newer { … }` dropped them without a trace | `stale_samples` is counted and traced (`probe stale seq=… latest=…`) |

Every timed-out settle now carries `settle.probe = {wakes_sent, wake_failures,
stale_samples, guest_stall_pending_ms, newest_sample_age_ms, note}` (and an input
timeout repeats it at the reply's top level). Real example
(`evidence/honest-timeout-with-probe-evidence.json`, `scenarios/p11-progress-timeout.yaml`):

```json
"settle": { "mode": "strict", "settled": false, "reason": "traversal", "bound": "timeout",
  "probe": { "wakes_sent": 4, "wake_failures": 0, "stale_samples": 0,
             "guest_stall_pending_ms": null, "newest_sample_age_ms": 13 } }
```

That record proves case (a) *from the data* — the channel was healthy (4 wakes
written, none refused, no stale sample, a sample 13 ms old) and the screen still
had framework work (`traversal`): an honest TIMEOUT, not a mystery.

`ACOS_ART_INPUT_TRACE` now traces the whole settle conversation: `settle entry
floor=… gen=… deadline_ms=…`, every `settle wake … written=…`, every reason
change with the sample's `seq/age/draw_seq/floor`, and every `probe rx … cause=…
lag_ms=…`. A passing session shows a sample for each wake within ≤2 ms.

The peer (the in-process host that answers the guest's binder transactions) now
names any transaction that holds it — and therefore the guest's main thread — for
longer than `ACOS_PEER_SLOW_MS` (default 250 ms): `peer transact slow
elapsed_ms=… call=<descriptor>#<method>`.

## 2. Reproduction attempts (all on rel-030, no network module)

Driver: `results/handoff/SETTLE5/settle5-driver.sh OUTDIR reps n` — N concurrent
`acos verify --engine art` of one scenario, each session with its own host trace
and a tee of the guest's stdout (every `ACOSPROBE` line with the guest's own
timestamp) and stderr. Scenarios: `settle5-probe.yaml` (counter: launch,
`wait_for`, `assert_text`, 3 pixel captures) and `settle5-netcheck.yaml` (the
NET4 fixture shape: net-check APK, module **off**, launch/assert/3 captures).

| arm | sessions | result |
|---|---|---|
| 1 × 1 smoke (before the change) | 1 | 1/1 |
| 5 × 3 counter | 15 | **15/15** |
| 5 × 3 net-check APK, module off | 15 | **15/15** |
| 10 × 2 counter (double cohort) | 20 | **20/20** |
| 5 × 3 counter with `--mem 12G` | 10 | **10/10** |
| 5 × 3 counter, final code | 15 | **15/15** |
| 5 × 3 net-check, final code | 15 | **15/15** |
| 1 × 5 single session (latency) | 5 | 5/5 |

`evidence/gates.txt` and `evidence/probe-cadence.tsv`. In every one of those
sessions the guest produced a sample for every wake, the largest gap between two
consecutive samples was **61 ms**, and no capture took more than 58 ms — the
1.5 s tree deadline was never within two orders of magnitude of being tight.
`evidence/probe-cadence.tsv` also shows the sessions of one rep start within
milliseconds of each other and finish a 285–320 ms span of samples; the failure
is not a margin effect, it is a guest that stops publishing for seconds.

What I could *not* reproduce: the NET4 condition (relay arm), because `acos-netd`
is not in `rel-030` (`crates/acos-netd` exists only on `net4`), and their
committed `n5relay` artifact set contains only `relay-rep*` — the "module off"
control they cite is not in the evidence, and NET3's own module-off control
(their report §2) *passed* tree binding at N=5. The relay arm therefore remains
untested from this branch, and it is still the most likely trigger (see §5).

### 2.1 Which settle condition never becomes true (from the NET4 artifact)

`evidence/net4-failing-settle-record.json` (`net4:relay-rep2/3`,
`steps/01-wait_for.json`):

```
step_duration_ms 6243
reason           probe-floor
probe_seq        4
tree_age_ms      6278
```

`tree_age_ms > step_duration_ms` is the decisive fact: the newest sample was
**older than the whole step**, i.e. it was received ~35 ms *before* the wait
began, and **not one sample arrived during 6.243 s** while the host asked for one
every 50 ms (124 wakes). So the condition that never became true is the very
first one — `sample.seq > entry_floor` — and it is not about the quiet window,
the draw sequence, the confirmation window or a newer store: there was no new
sample at all. The same signature (probe frozen at the launch's last sample,
`probe_seq 4-5`, ages 1.5–6.3 s) is the one ZPROD3b root-caused for the zygote
path (a wake channel that went nowhere), so the *host* side of the demand probe
is the correct place to look.

### 2.2 Latency (single session and 5 concurrent)

`evidence/settle-latency.txt`: capture step medians are **53 ms before and after**
(5 concurrent, counter), **54 ms before and after** (net-check), and 53 ms at N=1
(5 reps). Indicative only — same host, shared slot, no exclusive A/B; the bench
owner runs the sanctioned one.

## 3. Root-cause classification

### 3.1 The host did not miss a sample (not c, in the recorded failure)

For a *tree* request the strict wait sets `entry_floor = latest_probe().seq` at
entry and refuses every sample at or below it; the drain thread publishes a
sample only when its `seq` is strictly newer (`newer`), and the settle loop reads
`latest_probe` every 10 ms. With `tree_age_ms = 6278 > duration`, the host both
(a) saw no new sample and (b) correctly refused to bind the stale one: a
`probe-floor` timeout in that run is the **honest** verdict, not a race. The host
sent the demand wake at entry and then every 50 ms (`settle wake … written=true`
in the new trace). Nothing in the host wait can turn a 6 s guest silence into a
pass, and nothing in it should.

### 3.2 The guest side is a real hole (b) — fixed

`request_guest_probe` used to discard write errors, and the guest's control
thread returned silently on EOF. Both are now counted/named, and — the only
behavioural change — a *dead control channel* makes the guest poll at 300 ms
(`P10_PROBE_DEAD_CHANNEL_MS`) instead of the 5 s idle fallback. Why that is a
fix and not a band-aid: the 5 s fallback was chosen for an *idle but healthy*
channel (P10's cost model); with the demand channel gone, the only sample source
is that fallback, and 5 s is 3.3× the 1.5 s tree deadline, so **every** later
strict settle on a fully idle screen would time out. The 300 ms poll produces a
real, complete probe sample under the unchanged settle rule (fresh sequence,
`settled`, draw/publish ordering, confirmation window), so it cannot create a
pass from a stale observation. It only keeps the screen observable.

### 3.3 Why the recorded 6.3 s event does not look like "guest busy" (a)

The screen was net-check *after* launch, i.e. a static tree; five guests on a
32-core host at load1 4–8 cannot starve one main looper for 6 s. In the same rep
two sibling sessions completed their captures in 2.1 s. A *blocked* main thread
fits better than a busy one, and the two blocking shapes are:

1. the guest main thread is inside a binder transaction waiting for the
   in-process peer (single thread, `serve()` in `crates/acos-art/src/launch.rs`
   handles one frame at a time) — architectural, see §5;
2. the process/looper is stalled in the kernel (reclaim/I-O) under a memory
   cgroup that all five sessions share.

The new `peer transact slow` line and `probe-stalled` mark distinguish these on
the next occurrence (1 → a named slow `peer transact`, 2 → nothing at all from
the guest, `wake_failures=0`, `stale_samples=0`), which is exactly the evidence
this run lacked (NET4's failing artifacts were pruned of the launcher's
`live-session.json`, so the guest's own stderr is gone).

### 3.4 No adaptive settle budget (deliberately)

The brief allows a load-aware budget only if it cannot produce a false pass. A
strict settle already refuses stale samples, so *any* longer deadline is
false-pass-safe — but it is still wrong here: the failing event is 4× the
deadline, and the timeout is *correct* (the guest really produced nothing). An
adaptive budget would only delay an honest TIMEOUT and hide the channel's state.
So no budget was changed; instead the timeout now states the channel facts, and
the one measurable false-timeout source (a dead demand channel on an idle screen)
is fixed at its cause.

## 4. What changed

| file | change |
|---|---|
| `crates/acos-art/guest/java/dev/acos/art/LaunchMain.java` | `probe-stalled pending_ms=…` mark when a sample has been posted to the main looper for ≥500 ms; `probe-control-exit reason=…` when the demand control thread ends; `sProbeControlDead` switches the idle fallback to `P10_PROBE_DEAD_CHANNEL_MS` (300 ms) |
| `crates/acos-art/src/launch.rs` | `ProbeState.{wakes_sent, wake_failures, stale_samples, stall_pending_ms}`; `request_guest_probe` returns/records whether the wake was written; `probe_health_json` attached to every timed-out settle (`settle.probe`, top-level `probe` on input timeouts); settle/entry/wake/reason/probe-receipt traces under `ACOS_ART_INPUT_TRACE`; `peer transact slow` watchdog (`ACOS_PEER_SLOW_MS`) |
| `crates/acos-art/src/probe_channel_tests.rs` | the three FIFO wakes are counted (`wakes_sent == 3`) |

Tests added (all in `probe_line_tests` / `probe_channel_tests`):
`a_starved_probe_and_a_stale_sample_are_counted_for_the_timeout_evidence`,
`a_refused_probe_wake_is_counted_and_a_written_one_is_counted_separately`,
`a_dead_demand_channel_switches_the_guest_fallback_to_a_fast_poll`.

No settle rule, assertion, gate or tolerance was weakened. Nothing can become a
pass: the settle conditions are byte-identical, the new fields are evidence on
timeout replies, and the guest-side change only raises the sample rate.

## 5. Architectural wall (report, do not work around)

The in-process peer is a **single thread**: `serve()` reads one frame, calls
`peer.handle(frame)` (the whole `ServiceHost`: WMS/ATMS/PMS/Surface/VSync…) and
writes the replies before reading the next frame. The guest's *main* thread is
blocked inside `transact` for the duration of the handler. Therefore **any**
slow handler — a provider query, a `relayout`, a buffer op, a DNS/`getaddrinfo`
in the relay arm's `dnsproxyd` — stops the same main looper that must run the
posted probe sample, while the settle wait is waiting for exactly that sample.
If the next occurrence reports `peer transact slow call=<…>` (or the relay arm
reproduces), the fix is an asynchronous/multi-threaded peer (or a probe that is
independent of the main looper), i.e. a process/sandbox-model change — an
architectural wall per the brief. Options for the owner:

1. multi-threaded peer: one dispatch thread per connection/transaction class,
   with per-service locking (largest change, removes the whole class);
2. a bounded, *labelled* non-main-looper heartbeat sample (diagnostic only; can
   never back a pass) so a blocked looper is instantly distinguishable from a
   dead process without a stack dump;
3. keep the single thread and rely on the new evidence + `peer transact slow`.

## 6. What remains / smallest next action

1. Re-run this driver on the **relay arm** (`net4`'s `n5relay`, 3 × 5) with the
   new trace and `ACOS_PEER_SLOW_MS=100`: one run decides (a) vs §5's slow peer
   transaction, because both now print their own line.
2. If `probe-stalled` appears: the guest looper was blocked → §5.
3. If nothing appears and `wake_failures == 0, stale_samples == 0,
   wakes_sent > 0`: the wake was written and read but the probe thread never
   emitted → add the same `probe-stalled` mark to the probe *thread* (2-line
   change) and re-run.
4. The pre-existing test-ordering flake in §7 needs an owner (not this branch).

## 7. Tests and environment notes

```
cargo test --release --offline -p acos-art -p acos-verify -p acos-cli
```
`acos-art` 159 lib + 3 probe-channel + integration targets, `acos-verify` 89+29,
`acos-cli` 191 bin + every integration target: **green** in two full runs, with
two environment-bound exceptions, neither caused by this branch (it touches no
`acos-cli` code — `git diff --stat 7168439c..HEAD` is 7 files, 628+/18-):

* `live_registry::scratch_tests::the_test_scratch_root_leaves_room_for_a_unix_socket`
  fails when `ACOS_VIEW_TEST_DIR` is `$WT/.cache/lv` (this worktree's 57-byte path
  → 114-byte socket path). The brief's value fails the test's own 108-byte
  assertion; a short `ACOS_VIEW_TEST_DIR=/tmp/settle5lv` makes it pass. (The
  test's `BUDGET` of 72 is itself too generous for the deepest name.)
* `rel3d_daemon_identity::rel3d_a_foreign_daemon_never_answers_the_cli` fails
  only when the whole `-p acos-cli` suite runs (order/parallel interference with
  its sibling); 6/6 standalone runs pass.

Disk/RAM: peak host RAM observed during a 5-session run **17 GiB used** (inside
`guarded.sh --mem 20G`), 10-session run stayed under 26 GiB; disk 75 % → 77 %
during the runs and the whole target/scratch is deleted at the end of this task.

## 8. Reproduce

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/s5rt ACOS_SESSION_SOCKET=$PWD/.cache/s5rt/session.sock
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
CARGO_TARGET_DIR=$PWD/.cache/settle5-target \
  tools/guarded.sh --mem 12G --time 900 -- cargo build --release --offline -p acos-art -p acos-cli

# 5 concurrent × 3 reps, counter
tools/art-runtime/run-exclusive.sh --task SETTLE5 -- tools/guarded.sh --mem 20G --time 1300 -- \
  bash results/handoff/SETTLE5/settle5-driver.sh $PWD/.cache/settle5/run/n5x3 3 5
# the same on the NET4 fixture shape (module off): needs the net-check APK built
APK=$PWD/.cache/s5build/fixtures/build/apps/net-check.apk \
SCENARIO=$PWD/results/handoff/SETTLE5/settle5-netcheck.yaml \
  bash results/handoff/SETTLE5/settle5-driver.sh $PWD/.cache/settle5/run/n5x3-net 3 5
# the honest timeout that now carries settle.probe
tools/art-runtime/run-exclusive.sh --task SETTLE5 -- tools/guarded.sh --mem 8G --time 400 -- \
  env -u ACOS_HOME .cache/settle5-target/release/acos verify --engine art \
    --scenario scenarios/p11-progress-timeout.yaml --artifacts $PWD/.cache/s5-timeout
```
