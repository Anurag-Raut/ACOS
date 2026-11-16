# REL3I2 (v0.3.0) — IME Back (sender link) + dialog Back ack

Branch `rel3i2` off `rel3i` (`853f2be2`), then `git merge rel-030`
(`7168439c`, PERF3/REL3D/REL3K2/MERGEINTEG with `StepResult.attempts` and the
regenerated schemas) — merged clean, `HEAD` `491b937c`. Worktree
`.t3/worktrees/acos/t3-768008df-cf406afeda8d2316`. Every ART run through
`tools/art-runtime/run-exclusive.sh` + `tools/guarded.sh`; builds in
`$WT/.cache/rel3i2-target` (`CARGO_PROFILE_DEV_DEBUG=0`,
`CARGO_PROFILE_TEST_DEBUG=0`, `CARGO_INCREMENTAL=0`); private
`ACOS_RUNTIME_DIR`/`ACOS_SESSION_SOCKET`; `ACOS_VIEW_TEST_DIR=$PWD/.cache/lv`.

## 1. Status: PARTIAL

Item 1's **sender link is found and fixed at the root cause**, and the fix is
proven twice: the unit test pinning the token property, and the ART run where
the IME now consumes the Back (`back-down hid the soft input seq=1`,
`wms set_ime_visible(false)`, `FINISHED(handled=true)` reaching
`ViewRootImpl.ImeInputStage`). The scenario `scenarios/back-layers-ime.yaml`
still fails, but **one step later**: at the app-visible hide, not at the key.

Item 2 (dialog Back host ack) is **not done**. REL3K2's `settle: content` for
the dialog step is unchanged. Item 3 (typing scenarios + REL3K/REL3K2 Back
scenarios) stays green.

| brief item | state |
|---|---|
| 1. sender link root cause + `back-layers-ime.yaml` passing on ART | **PARTIAL** — root cause fixed (§2); the scenario fails at the *next* link, the app-visible hide (§3), which is a delivery-plumbing decision (§5). |
| 2. dialog Back: host-recorded ack `handled_evidence.source: "host_window_removed"`, `settle: content` → `back: {}` | **NOT DONE** |
| 3. typing + REL3K/REL3K2 Back scenarios green | **PASS** — see §4 |

## 2. Item 1: the sender link (root cause, fixed)

### 2.1 The measurement that named the cause

`ACOS_PEER_DEBUG=1` was not needed: the AOSP code path answers it. Two binds of
the same session were already visible in REL3I's trace
(`bind sequence=1 … channel="acos-ime-1 (client)"`, `bind sequence=5 …`), and
`InputMethodManager.H.handleMessage` is where a bind result's channel is
adopted:

```java
case MSG_START_INPUT_RESULT: if (res.id != null) updateInputChannelLocked(res.channel);
private void updateInputChannelLocked(InputChannel channel) {
    if (areSameInputChannel(mCurChannel, channel)) return;   // keeps mCurChannel
    ...
    mCurChannel = channel;
}
private static boolean areSameInputChannel(lhs, rhs) { ... return lhs.getToken() == rhs.getToken(); }
```

and `InputChannel::openInputChannelPair` mints `sp<IBinder> token = new
BBinder();` **per pair**. ACOS wrote the `IInputMethodSession` binder as the
token (REL3I §2.1), i.e. the same token for every bind of a session. So:

* bind 5 looked like "the same connection" to the framework → it kept bind 1's
  channel (`mCurChannel`, its fd), and its `ImeInputEventSender` wrote keys into
  **pair 1's client end**;
* `ImeState::start_input` had already replaced `self.channel` with **pair 5's**
  server end, so pair 1's server end was dropped;
* the app's `InputPublisher.publishKeyEvent` therefore wrote into a socket whose
  peer was gone. The framework then took the documented fall-through
  (`sendInputEventOnMainLooperLocked` returns `DISPATCH_NOT_HANDLED`) and the
  key reached the view tree — the Activity acked `handled=true` and finished,
  which is exactly REL3I's observed `acked: 2 … NO_FRESH_FRAME`.

Nothing was wrong with the channel *body*, the fd wiring, or
`NativeInputEventSender`'s looper registration (REL3I's candidates 2 and 3); the
token identity was.

### 2.2 The fix

* `acos-services::ime::ImeState::start_input` now allocates a **fresh token per
  channel pair**: `next_channel_token()` →
  `BinderRegistry::register_local(CHANNEL_TOKEN_DESCRIPTOR)`, exactly
  `openInputChannelPair`'s `new BBinder()`.
* `ServiceHost::registry` is an `Arc<BinderRegistry>` (with
  `binder_registry_arc()`), and `registry::install_with_handles` attaches it to
  the IME state (`ImeState::attach_binder_registry`). A service that must mint a
  handle while answering a transaction cannot reach `&mut ServiceHost`, and the
  token must be a real, allocated, releasable handle — not a fabricated id.
* `acos-parcel::BinderRegistry` gained a manual `Debug` (counts only) so
  `ImeState` keeps its derive.

### 2.3 Proof

Unit test (new, `crates/acos-services/tests/ime_wire.rs`):

* `each_bound_channel_carries_its_own_token` — two binds of one session produce
  two different channel tokens (fails before the change: both were the session
  binder).
* `the_captured_live_start_input_request_is_answered_with_a_bound_ime_and_channel`
  now asserts the token is **not** the session binder.

ART (`evidence/ime-trace-merged.log`, `evidence/ime-logcat-merged.txt`,
`evidence/back-layers-ime-merged-verdict.json`,
`evidence/ime-merged-key-step.json`) — `scenarios/back-layers-ime.yaml` on the
**merged** tree:

```
bind sequence=1 client=0x100000006 session=0x4c channel="acos-ime-1 (client)"
bind sequence=5 client=0x100000006 session=0x4d channel="acos-ime-5 (client)"
wms set_ime_visible(true)
showSoftInput client=0x100000006 shown=true
== step 15 (`key back`): pass, handled: true, acked 2/2 ==
wms set_ime_visible(false)          <-- REL3I had *no* line here
back-down hid the soft input seq=1  <-- the IME consumed the press
```

The key step went from `NO_FRESH_FRAME` (the task had finished because the
Activity consumed the Back) to `status: pass` with the IME as the consumer.

## 3. The linker the fix exposed: the app-visible hide (measured)

Step 16 (`assert_text status == "ImeDown"`) now fails:
`ASSERTION_FAILED: expected text exactly "ImeDown", got text "ImeUp"`.

On the host side the hide happened (`wms set_ime_visible(false)` in the trace,
the `HideInsets`/`InsetsControlChanged` pair queued for the focused window). In
the guest log the app **never** receives an insets dispatch that says the IME is
gone:

```
evidence/ime-logcat-merged.txt:
  83/84   insets_ime visible=false     (launch-time)
 142       InsetsController: show(ime(), fromIme=true)
 144/145   insets_ime visible=true      <- only `true` after the hide
```

An extra diagnostic run with one motion event after the key (a tap that makes
the guest transact again; `evidence/ime-trace-diag-refocus.log`,
`evidence/ime-logcat-diag-refocus.txt`) still shows only `visible=true` before
the refocus:
the hide's `InsetsState` never reaches the app.

Cause (measured, in the code): the IME **hide** is driven by a key on the input
channel, so its `IWindow` insets callbacks have no transaction to ride. The two
drains are `ImeWireService::drain_raw_deliveries` (only after a
`showSoftInput`/`hideSoftInput` transaction — the hide has none) and
`ServiceHost::drain_raw_deliveries_all`, which only runs from `Peer::
flush_deliveries` on the binder peer's "idle tick". That tick is a **1 s
`SO_RCVTIMEO`** whose comment says it exists only "to notice `stop` after a
guest death"; the peer blocks in `recvmsg` otherwise, so a channel-driven
delivery waits up to a second — longer than any step budget — and in this
scenario never lands. REL3I §2.2 claimed this path worked; it had never been
reached (the IME never hid before this fix).

## 4. Tests (merged tree)

```
tools/art-runtime/run-exclusive.sh --task REL3I2 -- tools/guarded.sh --mem 12G --time 2400 -- \
  cargo test --offline -p acos-services -p acos-wm -p acos-binder-peer -p acos-art -- --test-threads=1
# rc=0: 76 test binaries, 678 passed, 0 failed  (evidence/cargo-test-merged.log)
```

`ACOS_VIEW_TEST_DIR=$PWD/.cache/lv` is what the `acos-art` `live_view` tests
need, otherwise their unix socket path is too long.

ART on the merged tree:

* `scenarios/back-layers.yaml` — **pass**, 15 steps
  (`evidence/back-layers-merged-green-verdict.json`); the dialog layer still runs
  with REL3K2's documented `settle: content` (item 2 not done).
* `scenarios/back-layers-ime.yaml` — still `expectation: fail`, failing at step
  16 (`ImeDown` vs `ImeUp`), with step 15 (`key back`) **pass** and the IME as
  the consumer (`evidence/back-layers-ime-merged-verdict.json`).

Schema: `cargo test -p acos-protocol --test schemas` → 2 passed, 0 failed on the
merged tree; `UPDATE_SCHEMA` was not needed (no diff under
`crates/acos-protocol/schema/`). `StepResult.input` is carried as an untyped
object in `verdict.json`, so `handled_evidence.source` cannot drift from the
schema today; the values in this branch are unchanged (`finishInputEvent` /
`none`). If item 2 lands, its `guest` / `host_window_removed` values need a
schema change only when that block gets typed (and then
`UPDATE_SCHEMA=1 cargo test -p acos-protocol --test schemas`).

## 5. What the orchestrator must decide

The remaining link is a **delivery-wakeup** question in the binder peer, i.e.
the area the brief lists as a wall:

1. **Eventfd + `poll()` in the peer serve loop** (root cause): the IME writes one
   byte to a delivery-wake eventfd whenever it queues insets callbacks;
   `serve()` polls `[peer fd, wake fd]` and calls `flush_deliveries` on the wake.
   Correct for every "delivery with no incoming transaction"; touches
   `acos-art/src/launch.rs::serve` + one `Arc<OwnedFd>` in `ImeState`.
   Needs a decision because a process with several guests must per-session it.
2. **Short idle tick** (`SO_RCVTIMEO` 1 s → ~100 ms): one line, but H4's comment
   explicitly rejected a 10/s idle timer, and it costs idle wakeups in every
   benchmark.
3. **Longer key step** (wait out the 1 s tick in the key action): no code in the
   peer, but adds up to a second of latency to every IME-hide key and needs the
   step budget raised.

My recommendation is (1); (2) is a stopgap that would make this scenario pass
today.

## 6. Resources

Builds: release `acos-cli` 1m39s, `acos-art`/`acos-binder-peer` 1m08s, guard
`--mem 12G`; one release rebuild after the `rel-030` merge. Peak host: `free -g`
never below 40 GiB available (guarded arms `--mem 10..12G`). Disk `/`: 78 % at
start, 79 % after the release+debug build (`.cache/rel3i2-target` ≈ 2 GiB,
deleted at the end). No `pkill -f`/`pgrep -f`, no `~/.local`, no other worktree
touched.
