# REL3I3 (v0.3.0 / v0.4.0 seam) — the delivery wake, and a dialog Back that can be acked

Branch `rel3i3` off `rel3i2` (`1b8cd743`, which contains the `rel-030` merge
`491b937c` / `7168439c`). Worktree `.t3/worktrees/acos/t3-2f0977d6-8a0d91192c03988c`.
Every ART run through `tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`;
builds in `$WT/.cache/rel3i3-target` (`CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`); private
`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`; `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`.

## 1. Status: DONE

Both orchestrator decisions are implemented at the root cause, and the
end-to-end claim holds on real ART:

* **the delivery wake** (decision a, option 1): the binder peer blocks in
  `poll(2)` on its socket plus a session wake fd instead of a 1 s
  `SO_RCVTIMEO`; the WMS signals that wake when an IME show/hide queues
  `IWindow` insets callbacks. `scenarios/back-layers-ime.yaml` now **passes**
  (22/22) and its IME steps are folded into `scenarios/back-layers.yaml`
  (22/22 pass).
* **the dialog Back ack** (decision b): the delivered window's own removal is
  the consumer's proof. `back-layers.yaml`'s dialog step is a plain
  `back: {}` and reports `handled: true`,
  `handled_evidence.source: "host_window_removed"`,
  `settle.bound: "host-window-removal"` — with no guest ack (there can be none).

## 2. What changed

### 2.1 The delivery wake (root cause of REL3I2 §3)

The measured failure: `wms set_ime_visible(false)` queued the app's insets
callbacks, but the peer was blocked in `recvmsg` on a 1 s receive timeout whose
only drain was that timeout (`Peer::flush_deliveries` on the idle tick). A
delivery produced **outside** the peer's turn therefore reached the app up to a
second late — longer than any step budget — so the app kept `ImeUp`.

* `acos_services::DeliveryWake` (`crates/acos-services/src/host.rs`): the
  session's self-pipe (a non-blocking `UnixStream` pair, `wake()`/`drain()`/
  `read_fd()`). A `ServiceHost` carries it (`set_delivery_wake`), and
  `registry::install_with_insets` attaches it to the `WmsState`, so the queue
  and the wake can never disagree about the session.
* `WmsState::set_ime_visible` (`crates/acos-services/src/wms.rs`) signals the
  wake when **this call** queued `IWindow` callbacks (before/after queue length),
  so a no-op show/hide wakes nobody.
* `acos_binder_peer`: `IdleWake { Frame, Deadline, Wake }` and
  `wait_for_idle(fd, wake_fd, deadline)` — copied in name, shape and policy from
  `modules:crates/acos-binder-peer/src/peer.rs` (MODLOC2 `fae90ebc`) so the
  rel-030→modules merge is a no-op here; plus `Peer::next_timed_deadline` (this
  branch has no timed module, so its only deadline is a delivery the peer already
  holds — the `modules` branch extends the second half).
* `acos-art` `serve()` (`crates/acos-art/src/launch.rs`): the idle turn waits in
  `wait_for_idle`; a `Wake` drains the pipe and flushes the deliveries, a
  `Deadline` flushes, a `Frame` is read as before. `stop` now **wakes** the
  sleeping peer (the receive timeout used to be how it noticed). The 1 s socket
  timeout stays only as a mid-frame stall bound (`read_frame` after `poll`).
  A host-template child (`PrebuiltHost`) re-attaches the wake to its own WMS
  state, because the template installed its graph in another process.

Why not a shorter idle tick: `wakeups` is the point. An idle session now has
**zero** wakeups (`an_idle_peer_with_nothing_queued_has_no_wakeups`), where the
1 s timeout woke it once a second and still delivered a queued callback late.

### 2.2 The dialog Back ack (decision b)

Dismissing a dialog destroys the window the Back was delivered to; its
`InputEventReceiver` is disposed before it can call `finishInputEvent(handled)`
(the guest logs `Attempted to finish an input event but the input event receiver
has already been disposed.`). So no ack can exist, and the strict settle could
not bind the draw either (`no-draw-after-action`: the app never traverses; the
composite changes because a layer went away).

* `InputDispatch` records **window removals** as `(counter, token)` — on
  `IWindowSession.remove` and on a `PeerClosed` drain. `removal_mark()` /
  `was_removed_since(token, mark)` are token-scoped: a **different** window's
  removal is never evidence about this input, and an earlier removal of the same
  id is not either (mark taken at publish).
* `input_dispatch::InputHost` exposes both; the ART key handler
  (`serve` in `launch.rs`) takes the mark before publishing and reports
  `gesture.window_removed` for the **delivered** token.
* `acos-cli`'s `delivery()` maps it to `InputDelivery::window_removed` (one
  constructor for both the one-shot and live paths), and
  `acos_protocol::ui::handled_answer` turns "no acks + the delivered window was
  removed" into `handled: true`.
* `acos_protocol::ui::handled_source` names the source:
  `guest` (an ack, or an explicit `handled:false`) → `host_window_removed` (no
  ack, delivered window removed) → `none` (nothing delivered / nothing acked).
* **The removal draw is attributed to the step**: `wait_for_settled_observation`
  / `wait_for_event_settled` take `(token, mark)`; when that token's removal is
  recorded, a sample drawn *before* the publish is still the post-action state
  and the settle binds `bound: "host-window-removal"` (the pixels are measured
  as usual: `pixels_changed: true`).
* Protocol: `StepResult.input` is now typed — `StepInputResult`,
  `HandledEvidence`, `HandledSource` (`enum source: guest |
  host_window_removed | none`) — and the schemas were regenerated with
  `UPDATE_SCHEMA=1 cargo test -p acos-protocol --test schemas`.
  The serialised bytes of the block are unchanged apart from the `source`
  spelling (`finishInputEvent` → `guest`), which the CLI/MCP replies and the
  verdict share from the same helper.

## 3. Acceptance

### 3.1 `cargo test` (evidence/cargo-test.log)

```
tools/art-runtime/run-exclusive.sh --task REL3I3 -- tools/guarded.sh --mem 12G --time 2400 -- \
  cargo test --offline -p acos-services -p acos-wm -p acos-binder-peer -p acos-art \
              -p acos-protocol -p acos-verify -- --test-threads=1
```
rc=0; **86 test binaries, 942 passed, 0 failed** (`grep -c "^test result: ok"`
= 86; the awk sum of the result lines is 942/0). New tests:

* `crates/acos-binder-peer/tests/peer.rs::delivery_wake::a_queued_delivery_is_flushed_within_five_ms_with_no_incoming_transaction`
  — a service object parks a delivery, the producer wakes the pipe, and the
  guest's socket receives the `F_TXN` frame in **< 5 ms** with nothing written
  to the socket by anyone else.
* `…::an_idle_peer_with_nothing_queued_has_no_wakeups` — 2 s of an idle serve
  loop: **0** wake turns (the 1 s receive timeout it replaces would have woken
  twice).
* `crates/acos-services/src/input_dispatch.rs::window_removal_tests::*` (3) —
  only the **delivered** token's removal counts; an earlier removal is not
  evidence; no removal means no evidence.
* `crates/acos-verify/src/executor.rs::region_tests::a_removed_delivered_window_is_host_handled_and_not_a_guest_ack`
  — `handled: true` + `source: host_window_removed` for the removed delivered
  window, `handled: null` + `source: none` when only a different window went
  away.

`cargo test -p acos-protocol --test schemas`: 2 passed with `UPDATE_SCHEMA=1`
(`crates/acos-protocol/schema/verdict.json` regenerated: +117 lines,
`HandledSource`/`HandledEvidence`/`StepInputResult`).

The two `acos-cli` test binaries whose source assertions I updated
(`rel3e_handled`, `rel3s_swipe`) build and run green
(evidence/cargo-test-cli.log); their ART bodies are `#[ignore]` and need APKs
(`tap-targets`, `scroll-list`) this worktree does not build — the live ART runs
below cover the same `source: guest` fact.

### 3.2 ART scenarios (evidence/*-verdict.json + *-art/verdict.json)

| scenario | result | key evidence |
|---|---|---|
| `scenarios/back-layers.yaml` | **pass**, 22/22 steps | dialog Back step 5: `handled: true`, `source: "host_window_removed"`, `acked: 0`, `settle.bound: "host-window-removal"`, `pixels_changed: true`; IME step 14 (`key back`): IME consumed it, step 15 `assert_log` `insets_ime visible=false`, step 16 `ImeDown`, activity still shows `Show dialog`; step 18 finishes the task with the ordered `onBackPressed → onPause → onDestroy`. |
| `scenarios/back-layers-ime.yaml` | **pass**, 22/22 steps | (was `expectation: fail`) hide reaches the app: `logcat` `insets_ime visible=true` (146) then `insets_ime visible=false` (147) in the same key step; no `dispatchKeyBack` for the IME press; final Back `bound: "window-gone"`. |
| `scenarios/form-validation.yaml` (typing) | pass, 28/28 | `type` steps `handled: false` (the Activity's own `finishInputEvent`), taps `source: guest`. |
| `scenarios/w22-type-clear.yaml` (typing) | pass, 7/7 | two `type` steps + a clear tap. |
| `scenarios/gaten-backstack.yaml` (REL3K Back stack) | pass, 25/25 | includes a `back` step `handled: true` `source: guest`. |

The guest log line that proves the wake (back-layers evidence):
`AcosBack: insets_ime visible=true` at 1794816281575 and
`AcosBack: insets_ime visible=false` at 1794816282101 — the hide lands inside
the key step, not on a later tick.

### 3.3 Latency before/after (indicative, SHARED, 5 reps)

`tools/bench/acos_bench.py --phase latency --reps 5 --fast-reps 5` through
`run-exclusive.sh` **without** `--exclusive` (shared slots), one arm from
`rel3i2`'s release build, one from this branch
(`evidence/bench-latency-before.json`, `…-after.json`). Judge the deltas against
the noise: 5 reps, host `loadavg1` 6.6 during the after arm, and the before arm
had one flaky `m4-strict` launch (`GUEST_EXITED`, 143 ms), so **treat this as
indicative, not a benchmark result**.

| M8 row | before p50 | after p50 | delta |
|---|---|---|---|
| counter tap, strict | 949.8 ms | 946.2 ms | −3.6 ms |
| counter tap, fast | 54.5 ms | 47.4 ms | −7.1 ms |
| form tap, strict | 925.6 ms | 931.7 ms | +6.2 ms |
| form tap, fast | 33.0 ms | 32.6 ms | −0.4 ms |

The poll loop adds no measurable latency to a tap: every delta is inside the
5-rep spread, and the strict rows (the 0.9 s settle-dominated ones) move in
opposite directions. The mechanism is also structurally latency-free for the
transaction path: a delivery queued during a peer turn still rides that turn
(`Peer::handle` drains at the end), and the wake is only the edge for
out-of-turn producers.

## 4. What remains

* `modules` merge: `wait_for_idle`/`IdleWake`/`Peer::next_timed_deadline` keep
  the MODLOC2 names and signatures; the `modules` branch adds the timed-module
  half of `next_timed_deadline` and its `ServiceControl` wake, which slots into
  the same `IdleWake::Wake` arm.
* The IME Back step in both scenarios still uses `settle: content`. With the
  hide now landing in the key step the strict rule may bind it too; that is a
  scenario tightening, not a product gap, and it was not measured as part of
  this task.
* Nothing else: no architectural wall was hit, and no benchmark arm of my own
  beyond the requested shared, indicative A/B.

## 5. Resources

Builds: release `acos-cli`+`acos-art` 1m39s (this branch), 1m24s (the `rel3i2`
A/B arm in a throwaway worktree `.cache/rel3i3-base`, removed at the end); one
test-profile build. Guarded arms `--mem 10..12G`; host stayed ≥ 40 GiB
available. Disk `/`: 74 % at start → 76 % at the busiest point
(`.cache/rel3i3-target` + `.cache/rel3i3-base-target`); both targets and the
base worktree are deleted at the end, and the committed evidence is 1.5 MiB.
No `pkill -f`/`pgrep -f`, no `~/.local`, no other worktree touched; every
process I started was killed by pid.
