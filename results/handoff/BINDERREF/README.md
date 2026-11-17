# BINDERREF — binder driver + host references (status: DONE for the acceptance table; two findings the orchestrator should read first)

Branch `binderref` from `bindercb2` (`aebe0009`). Worktree env per
BRIEF_COMMON_0316: private `ACOS_RUNTIME_DIR=/run/user/1000/binderref`, private
`ACOS_SESSION_SOCKET`, every ART run under
`tools/art-runtime/run-exclusive.sh --task BINDERREF` + `tools/guarded.sh`.
Target dir `.cache/binderref-target`.

## TL;DR — read this before anything else

1. **The brief's premise about the 25-of-26 loss was wrong, and so was
   BINDERCB2 §4's.** The driver already ordered the node root exactly as the
   orchestrator asked (BR_INCREFS + BR_ACQUIRE pended on the *sending* thread
   before BR_TRANSACTION_COMPLETE), libbinder acknowledged both
   (`BC_INCREFS_DONE`, `BC_ACQUIRE_DONE`), and every host→guest transaction in
   the failing runs was dispatched with `NO_ERROR` (`F_REP` flags 0, 0-byte body
   — the oneway-stub success shape, never `TF_STATUS_CODE`/`-32`). The measured
   sequence is §2 below; `results/handoff/BINDERREF/r25.json` /
   `r3c.json` carry it.
2. The loss had **two other causes**, both measured from inside the app:
   * the fixture appended its log lines to a `TextView` from **two binder
     looper threads at once**, which silently loses lines (that alone made a
     *working* 2-of-2 delivery look like 1-of-2);
   * a registered `PhoneStateListener` that the app itself no longer references
     has its **Java object collected**, and the callback then never runs even
     though the driver delivered it and libbinder dispatched it. With the app
     holding its listeners (the normal app pattern — `r25k.json`,
     `v2-repeat*.json`), every listener gets its callback.
3. Also fixed: the 60-of-101 wall was real and is the **transaction buffer**:
   the heap never reclaimed, so `BC_FREE_BUFFER` returned nothing to the 1 MB map
   and a session died after ~3000 transactions. The heap that reclaims is
   `native/txn_heap.c` — FLAKEWIN's heap (the one heap for the driver and the
   host tests) plus this branch's free list (§3a); 101 of 101 registrations and
   101 of 101 callbacks follow from it.

## 1. The acceptance table (final build, `results/handoff/BINDERREF/m-*.json`)

| run | executed | dispatched (logd, app counter) | verdict | file |
|---|---|---|---|---|
| 26 registered listeners, one injected ring | 26 | **26** | pass | `m-repeat25.json` |
| 46 registered listeners | 46 | **46** | pass | `m-repeat45.json` |
| 101 registered listeners | 101 | **101** | pass | `m-repeat100.json` |
| long-lived listener, 65 s idle | 1 | **1** | pass | `m-longlived.json` |
| unregister (`LISTEN_NONE`), 65 s, inject | 0 | **0** | pass | `m-unregister.json` |
| unregister with `E3G_DEBUG=1`: the node is released | 0 | 0 | pass | `m-unregister.json` (trace) |

`m-repeat100.json` reports `app_ringing=101`, `dispatched_ringing=101`,
`app_state_lines=202` (101 initial-state + 101 ring) and
`ui_ringing_lines=52`: a single `TextView` holding 202 lines is **truncated by
the UI-tree dumper**, so the per-line UI count is a lower bound at that size.
The authoritative UI-visible evidence is the `CALLTOTAL` line the app prints
from its own counter (`CALLTOTAL state_lines=202 ringing=101 listeners=101
held=101`), plus the logd count, and it carries **no**
`ART_RESOURCE_LIMIT` warning: nothing was dropped for lack of a buffer. `drive.py`
asserts both and fails on any over-count (a duplicate delivery).

## 2. The measured sequence (why the previous diagnosis was wrong)

`--mode repeat --registers 3`, `E3G_DEBUG=1` (driver trace to logd, tag
`acos-binder`, read back with `acos_logs`), `r3c.json`:

```
tid=2  node: local binder=7f0a1d642ba0 cookie=7f0b5d60cef0 -> wire=9      # export #1 (send thread)
tid=2  node refs: BR_INCREFS+BR_ACQUIRE weakref=7f0a1d642ba0 cookie=7f0b5d60cef0
tid=2  node INCREFS_DONE weakref=7f0a1d642ba0 cookie=7f0b5d60cef0        # libbinder incWeak
tid=2  node ACQUIRE_DONE weakref=7f0a1d642ba0 cookie=7f0b5d60cef0        # libbinder incStrong
... same for wire=22, 23, 24 ...
tid=13 deliver seq=170 wire=9  code=6 weakref=7f0a1d642ba0 cookie=7f0b5d60cef0
tid=19 deliver seq=171 wire=22 code=6 ...
tid=19 REPLY seq=171 flags=0x0 size=0/0     # onTransact ran (a failed dispatch is flags=0x8)
tid=13 REPLY seq=170 flags=0x0 size=0/0
tid=19 deliver seq=172 wire=23 code=6 ...
tid=13 deliver seq=173 wire=24 code=6 ...
tid=19 REPLY seq=172 flags=0x0 size=0/0
tid=13 REPLY seq=173 flags=0x0 size=0/0
       CALL state=RINGING ... listener=4      # only the newest listener logged
       CALLTOTAL state_lines=5 ringing=1 listeners=4
```

* The root commands are emitted **before** `BR_TRANSACTION_COMPLETE` (the
  kernel queues node work on `proc->todo` and the sender consumes it inside
  `waitForResponse`, before the transaction's reply returns; this driver pends
  them on the sending thread in the same batch, which is the same ordering
  guarantee).
* libbinder **acknowledges** both roots, so the `JavaBBinder` is `incStrong`ed
  while the sending `Parcel` still holds its own reference
  (`Parcel::writeObject` → `acquire_object`).
* All four deliveries are dispatched (`flags=0x0`, empty body — `IPhoneStateListener`
  is a oneway interface, so a successful `onTransact` writes nothing; a dead
  target or an unknown code produces `TF_STATUS_CODE` + a status word instead).
* The app's own counter, printed from its UI thread, says **ringing=1 of 4**.

So the transaction reached the right node, was dispatched, and the Java callback
for three of the four listeners still did not run. Two independent causes, both
measured:

### 2a. A registered listener whose Java object nobody holds is collected

`r10w.json` (11 listeners, the app keeping them only in a
`WeakReference` list, `Report callbacks` doing `System.gc()`):

```
CALLTOTAL state_lines=12 ringing=1 listeners=11 alive=1 alive_after_gc=1
```

Only the listener the app still referenced (`callListener`, the newest) survived
— and it is the only one that ran. With the app holding all of them
(`r25k.json`): `ringing=26`, `alive=11/11`-style behaviour, 26 of 26 printed.

Mechanism: the driver keeps the *`JavaBBinder`* alive (`BR_ACQUIRE`), and on
stock Android that object's `mObject` global reference (`android_util_Binder.cpp:
JavaBBinder(env, object) : mObject(env->NewGlobalRef(object))`) is what keeps the
*Java* `Binder` (here the `IPhoneStateListener.Stub` inside the
`PhoneStateListener`) alive. In this guest that chain does not hold: the app's
Java object is collected while the node is rooted and dispatched to successfully.
**This is an open root-cause question for the orchestrator** (see §6): it is not
a ref-count problem on the driver side (the ack proves the strong ref), so it is
either the image's `JavaBBinder` holding a weak reference, or the guest ART layer
losing the Java object. It is *not* what the acceptance table needs (an app that
registers a callback normally holds it), but it is a real difference from Android.

### 2b. The fixture raced on its own log view

`r1.json` (2 listeners, before the fixture fix): both deliveries answered
`flags=0x0`, the app's UI text showed **1** line. The two callbacks ran on two
different binder looper threads (tid 13 and 19) and both appended to the same
`TextView` (`Editable`, not thread-safe) — one append was lost. The fixture now
marshals the view append to the UI thread and also writes every line to logd, and
`Report callbacks` prints the counters from the UI thread. `r1b.json` = 2 of 2
after the fix.

## 3. What this branch implements

### Driver (`crates/acos-binder-peer/native/binder_driver.c`)

* **Transaction-buffer allocator** (`mm_alloc`/`mm_free`): first-fit over freed
  blocks, split on use, coalesce on free, high-water mark as last resort, with
  the block table kept sorted (drivers 13/14 in `driver_test.c`). `BC_FREE_BUFFER`
  is the receiver's release (`IPCThreadState::freeBuffer` from the received
  `Parcel`'s destructor), so reuse from that point is `binder_alloc_free_buf`
  semantics, not premature reuse.
* No silent wedges: a reply or delivery with no buffer answers `BR_FAILED_REPLY`
  / `F_REP(-ENOMEM)` instead of leaving a caller waiting forever.
* **Per-node reference model**: `strong` (host references reported over the
  wire) and `rooted`; a new node starts at one (the transaction transfers a
  reference), `F_REF` acquire/release adjust it, and the last release queues
  `BR_RELEASE` + `BR_DECREFS` for that node's `(weakref, cookie)`, drops the node
  from the table (so a later transaction to that wire answers `DEAD_OBJECT`), and
  wakes a reader so the unroot happens even when no transaction is in flight
  (`binder_dec_node` → `BINDER_WORK_NODE` → `binder_wakeup_proc_ilocked`).
  Node work is per process ("whichever thread reads delivers it"), like
  `proc->todo`.
* **HELLO** on connect (`F_HELLO`, `seq` = wire protocol version 2) and the
  `F_REF` frame; the release trace (`node release …`, `node unref: BR_RELEASE +
  BR_DECREFS …`) is in logd under `E3G_DEBUG`.
* `WARN`'s logd mirror now uses `__android_log_vprint`. It used
  `__android_log_print(prio, tag, fmt, va_list)`, which consumes the `va_list`
  pointer as the first `%` argument: **every warning with arguments was garbage
  in logd** (`transaction buffer exhausted (139818998707856/139831030787232
  used, -198541192 blocks)`), which is why BINDERCB/BINDERCB2 evidence could not
  tell what those numbers were.

### The one heap (`native/txn_heap.{c,h}`) — merged from FLAKEWIN, reuse added here

`flakewin` (HEAD `5eb6c44c`) fixed the *same* heap from the other end: no lock,
so two binder threads got the same offset and one parcel overwrote the other
(`Adding window failed`). Its heap is a separate translation unit
(`native/txn_heap.c`), host-testable without bionic/JNI, and the driver, both
build paths (`build.rs`, `acos-art`'s `build_driver`, cache key
`binder-driver/v3`) and the tests hash it. That is the one heap now:

* this branch's `mm_alloc`/`mm_free` and `g_blk` table are **gone** from
  `binder_driver.c`, which now calls `txn_alloc`/`txn_free` and
  `txn_heap_init(g.map + 4096, g.map_size - 4096)`;
* `txn_heap.c` keeps FLAKEWIN's leaf mutex (that is the race fix) and gains this
  branch's **free list**: first fit over freed blocks, split on use, coalesce on
  free, high-water as fallback (`binder_alloc_new_buf` over
  `alloc->free_buffers`), the block table grown on demand instead of the fixed
  512 entries that silently stopped tracking;
* `txn_heap_used()` stays the high-water mark (what FLAKEWIN's tests print) and
  `txn_heap_in_use()` is new (live bytes), so the tests can show both "the map
  came back" and "nothing leaked";
* bindercb2's growing node/handle tables stay where they were (driver, untouched
  by this merge).

Lock order is documented in both files: the heap mutex is a leaf, `g.mu` is
never held across a `txn_alloc`/`txn_free`. Reuse is safe for the reason the
driver already had: `BC_FREE_BUFFER` is honoured only for a buffer the
*receiving* thread owns (`T.txn_buf`, the kernel's `binder_get_txn_from_and_acq_inner`),
so a stale or replayed free cannot hand a live buffer to a second owner.

**Merge scope, stated plainly:** a full `git merge flakewin` was attempted and
conflicts in **13 files that are not this track's** (`crates/acos-verify/*`,
`crates/acos-mcp/tests/guide.rs`, `crates/acos-cli/src/{engine,launch,verify}.rs`,
`crates/acos-art/src/launch.rs`, `crates/acos-protocol/schema/*.json`,
`docs/AGENT_GUIDE.md`, `tools/build-apps.sh`, `DECISIONS.md` — bindercb2's base
vs rel-030, 83 commits and 1853 files). Resolving those would mean integrating
BINDERCB into rel-030, which is the integration track's job, so this branch
took FLAKEWIN's heap commit (`a8bf0f42`) instead: `txn_heap.{c,h}`,
`build.rs`, `acos-art`'s `build_driver`/cache key, `tests/txn_heap.rs` and the
FLAKEWIN evidence are all in as-is, and the driver resolution keeps this
branch's node model. Also of note: `tools/build-apps.sh` cannot build
`keystore-probe` on this branch at all (`dev.acos.art.SoftwareKeyStore` comes
from rel-030), which is the same integration gap from the other side.

### Wire (`protocol.rs`, `peer.rs`)

* `F_HELLO` (`PROTOCOL_VERSION = 2`) is the first frame of a connection; the peer
  records the driver's version and a newer driver is a structured
  `UNSUPPORTED_ANDROID_API` record, never a guess.
* `F_REF` (peer → driver, `target` = driver wire handle, `code` =
  `REF_ACQUIRE`/`REF_RELEASE`). A driver that did not announce v2 never receives
  one (`ref_frames_suppressed` counts what was withheld) — the frozen-contract
  rule for an old pair.
* `Peer::retain_binder`/`release_binder`/`held_refs_of`, and every handled frame
  flushes the host's dropped references into `F_REF` release frames.

### Host (`acos-services`)

* `ServiceHost::retain_binder`/`release_binder`/`held_refs`/
  `take_released_binders`: a registration *is* a strong reference. Refused for
  an unregistered handle (a reference to nothing).
* `ServiceObject::drain_raw_retained`/`drain_raw_released`: the seam a raw
  handler (one whose transactions are not in the decoded AIDL tables) uses to
  report "I stored this callback" / "I dropped it"; the dispatcher applies them,
  including on the dynamic-node path.
* `telephony.rs` (ITelephonyRegistry): `listenWithEventList` with a non-empty
  event list retains the callback; `LISTEN_NONE`, a re-registration of the same
  binder, and `removeOnSubscriptionsChangedListener` release it.

### Fixture (`fixtures/apps/tail-probe`)

* Every line goes to logd (`android.util.Log`, tag `tailprobe`) as well as the
  view; the view append is marshalled to the UI thread; a `Report callbacks`
  action prints `CALLTOTAL state_lines=… ringing=… listeners=… held=…` from the
  app's own counters on the UI thread, and each listener tags its line with
  `listener=<id>`.
* The app keeps its listeners in a list (the normal app pattern). The
  drop-the-listener variant was the §2a experiment and is described here, not
  left in the app.

## 4. Tests (driver first)

`cargo test -p acos-binder-peer -p acos-services` green (driver: 18 checks;
peer: 17 tests; services: 96 unit + the rest; no failures).

New checks, each failing before the change it covers:

| # | check | fails without |
|---|---|---|
| 13 | a freed transaction buffer is reused by the next transaction | the free list (`again != b`) |
| 14 | 4000 registration-shaped rounds reuse the 1 MB map | the free list (`transaction buffer exhausted`) |
| 15 | the host's last reference sends `BR_RELEASE` + `BR_DECREFS` for that node | `F_REF`/node model (no commands) |
| 16 | a transaction to a released node answers `DEAD_OBJECT` | node release |
| 17 | a released object exported again gets a new node and is rooted | node release + re-root |
| 18 | the re-exported node receives callbacks | node release + re-root |

`driver-test-heap-before.txt` / `driver-test-heap-after.txt` isolate the heap:
the *same* driver, linked against FLAKEWIN's monotonic `txn_heap.c` and against
this branch's:

```
not ok 13 … again=0x7f7f70d838d8 (a fresh block each time)  -> ok 13 (again == b)
not ok 14 … failed=1 high-water 534744 -> 1036248 of 1036288 -> ok 14 (… 3816 of 1036288)
not ok 18 … delivered=0                                     -> ok 18
RESULT failures=3                                           -> RESULT failures=0
```

`tests/txn_heap.rs` runs the same comparison under load (4 threads, 200k
allocations, 64 MB of traffic) and gets `high-water=3816, in_use=0, reused=1`
against `high-water=64564696, reused=0` for the monotonic heap.

`driver-test-before.txt` / `driver-test-after.txt` are the *same* test binary
against the driver from `aebe0009` and this branch:

```
not ok 13 … again=0x7f220a3838d8                       (no reuse)
not ok 14 … failed=1 high-water 538840 -> 1040344 of 1040384   (map exhausted)
not ok 15 … BR_RELEASE=0 BR_DECREFS=0                  (no release path)
not ok 16 … dead_object replies +0                     (node not released)
not ok 17 … wire=106 -> 106, re-rooted=0               (no new node)
not ok 18 … delivered=0
RESULT failures=6        ->        RESULT failures=0
```

Peer tests: `a_dropped_registration_releases_the_guest_node_over_the_wire`
(hello → retain ×2 → release ×2 → one `F_REF` with the driver's wire handle) and
`a_driver_without_the_handshake_never_gets_a_reference_frame`.

## 5. Live proof of the release path (`v-unregister-ref.json`, `E3G_DEBUG=1`)

```
tid=13 hello: wire protocol v2
tid=2  node: local binder=7f3c94a61d90 cookie=7f3c84a19790 -> wire=9
tid=2  node refs: BR_INCREFS+BR_ACQUIRE weakref=7f3c94a61d90 cookie=7f3c84a19790
...
tid=14 node release wire=9 weakref=7f3c94a61d90 cookie=7f3c84a19790   # F_REF release from the peer
tid=13 node unref: BR_RELEASE+BR_DECREFS weakref=7f3c94a61d90 cookie=7f3c84a19790
```
App: `CALL unregistered`, `CALLTOTAL state_lines=1 ringing=0 listeners=1 held=1`,
and the injected ring after the unregister produces no callback (verdict pass).

## 6. Open items for the orchestrator

1. **§2a is a real gap between ACOS and Android** and it is *not* the driver's
   refcount: a rooted node dispatches, yet the Java object behind it is
   collected. Options: (i) verify the image's `JavaBBinder` constructor (strong
   vs weak `NewGlobalRef`) by disassembling `libandroid_runtime.so` (the copy in
   `.cache/parcel-evidence` is stripped; the constructor calls through the JNIEnv
   table at `NewGlobalRef` = index 20 / `NewWeakGlobalRef` = 226); (ii) if it is
   strong, the guest ART layer is losing a JNI global ref and that is a separate
   track; (iii) accept it and document that a callback whose Java object the app
   drops is not reachable (Android apps must hold their listeners).
2. Reference accounting is wired for the telephony registry only. The other
   registrations the brief names (`hw.*` sensor/event connections, location,
   `IIntentReceiver` receivers, `IAccessibilityManagerClient`, notification /
   PendingIntent senders) still hold their callbacks without a host reference:
   they need the same two-line `retain`/`release` in their modules. The mechanism
   (host counters → `F_REF` → `BR_RELEASE`/`BR_DECREFS`) is in place and tested.
3. Module detach (`ServiceHost::end_scope`) and process death do not release the
   references a module's callbacks held; `end_scope` drops services and their
   objects (which drops the module state), so the release has to be reported
   before that (a `ServiceObject::release_all` hook or a `Drop` that reports
   through the same seam). The session-teardown half is moot for the driver (it
   dies with the guest) but matters for a long-lived peer.

## 7. Files

* `crates/acos-binder-peer/native/binder_driver.c` — node ref model,
  `F_HELLO`/`F_REF`, node-work wakeup, honest logd format, ref trace, and the
  heap call sites (`txn_alloc`/`txn_free`, `ART_RESOURCE_LIMIT`).
* `crates/acos-binder-peer/native/txn_heap.{c,h}` — FLAKEWIN's locked heap (from
  `flakewin`/`a8bf0f42`) with this branch's free list, split/coalesce and
  `txn_heap_in_use`; `tests/txn_heap.rs` races it and checks reuse.
* `crates/acos-binder-peer/native/driver_test.c`, `tests/driver.rs` — checks
  13–18.
* `crates/acos-binder-peer/src/protocol.rs`, `src/peer.rs` — wire v2, hello,
  `F_REF` emission, retain/release API.
* `crates/acos-services/src/host.rs` — host reference counters + outbox, trait
  seam; `src/modules/telephony.rs` — retain on register, release on unregister.
* `fixtures/apps/tail-probe/src/.../MainActivity.java` — logd copy, UI-thread
  append, `Report callbacks`, listener ids, held list.
* `results/handoff/BINDERREF/drive.py` — the MCP driver: logd (`tailprobe`) and
  driver-trace capture, `Report callbacks`, the assertions above.
* Evidence: `m-repeat25.json`, `m-repeat45.json`, `m-repeat100.json`,
  `m-longlived.json`, `m-unregister.json` (final build, merged heap),
  `v2-*.json` (the build before the merge), `driver-test-heap-before/after.txt`,
  `r1.json`, `r1b.json`, `r3c.json`, `r10w.json`, `r25.json`, `r25k.json`,
  `r5-debug.json` (the calibration sequence in §2).

## 8. Commands

```
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
CARGO_TARGET_DIR=$PWD/.cache/binderref-target cargo test --offline -p acos-binder-peer -p acos-services
ACOS_APP_ONLY=tail-probe tools/build-apps.sh

ACOS_BIN=$PWD/.cache/binderref-target/release/acos \
ACOS_ART_BIN=$PWD/.cache/binderref-target/release/acos-art \
ACOS_ALLOWED_ROOTS=$PWD \
tools/art-runtime/run-exclusive.sh --task BINDERREF -- tools/guarded.sh --mem 14G --time 900 -- \
  env -u ACOS_HOME ACOS_RUNTIME_DIR=/run/user/1000/binderref \
  ACOS_SESSION_SOCKET=/run/user/1000/binderref/session.sock E3G_DEBUG=1 \
  python3 results/handoff/BINDERREF/drive.py \
  --binary $PWD/.cache/binderref-target/release/acos-mcp \
  --apk fixtures/build/apps/tail-probe.apk --mode repeat --registers 25 --idle-s 1 \
  --min-priority I --artifacts $PWD/.cache/binderref-run/x \
  --out results/handoff/BINDERREF/x.json
```
