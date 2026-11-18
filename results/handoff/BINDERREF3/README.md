# BINDERREF3 — the last three callback sites, the §2a verdict, caller pid/uid

Branch `binderref3` from `modules` (`45a1feab`). Worktree env per
BRIEF_COMMON_0316 (`ACOS_FONT_DIR`, `.cache/boot/android-34`, `.cache/art`,
`.cache/gaten`, `fixtures/build` hardlinked). Target dir `.cache/br3-target`.

**Status: PARTIAL.** Item 2 (item 2 of the brief: wire the remaining module
callbacks + the leak test) is done, tested and committed. Items 1 (§2a probe)
and 3 (BINDERPID) are **not** done: the one-hour budget was spent on item 2's
code, its four new tests, and the host-side correction it needed (§2). §4/§5
below are the measured design inputs for both, so the next agent does not
re-derive them.

## 1. What is committed

| site | registration | release | ledger |
|---|---|---|---|
| `hw.biometric` `IAuthService` | `authenticate` stores the `IBiometricServiceReceiver` | scripted `auth` control op, `cancelAuthentication` (one shot), `reset`, module detach | `BiometricState::refs` |
| `system.autofill` `IAutoFillManager` | `addClient` stores the `IAutoFillManagerClient` | `removeClient`, session teardown | `AutofillState::refs` |
| `inputmethod` `IInputMethodManager` | `addClient` stores the `IInputMethodClient` **and** its `IRemoteInputConnection` | process teardown (`release_all_refs`); the interface has no `removeClient` (checked against the image's AIDL table) | `ImeState::refs` |

Both `IInputMethodManager` implementations are wired: the shipped node is
`ime_wire::ImeWireService` (the raw `startInput` path), which now forwards
`drain_retained`/`drain_released`/`release_all_refs` to its generic
`ImeService` — a hook on `ImeService` alone would never run, because the host
drains **the object it dispatched to**.

## 2. The host-side correction this exposed (real bug, fixed)

A module can answer a pending request **outside a binder transaction**: the
biometric scripted result is the control op `auth`. The host applied
`drain_retained`/`drain_released` only after `call`/`raw_transact`, so that
release sat in the ledger until the guest's *next* transaction — the guest's
node stayed rooted (and the app's callback object alive) in between. Fixed with
`ServiceHost::drain_refs()`, called by `ModuleRegistry::control` after a
successful control op (`crates/acos-services/src/module.rs`). Test:
`a_pending_biometric_authenticate_references_its_receiver_until_answered`.

## 3. Proof

```
CARGO_TARGET_DIR=$PWD/.cache/br3-target cargo test --offline \
  -p acos-binder-peer -p acos-services
```
`acos-binder-peer`: 5+2+12+10+23+2 green. `acos-services`: 167 lib + every
integration binary green **except** `tests/moda11y6_gate.rs` (5 failures), which
asserts `fixtures/build/apps/a11y-probe.apk is built` — that APK does not exist
in the main checkout either (`ls fixtures/build/apps | grep a11y` is empty), so
it is an unmade fixture, not a regression; build the fixtures with
`tools/build-apps.sh` before reading that binary's result.

New tests (`crates/acos-services/tests/binder_refs.rs`, 4 + 1 leak):

| test | what it fails on |
|---|---|
| `a_pending_biometric_authenticate_references_its_receiver_until_answered` | a stored receiver that is not referenced, or a control-op answer that does not release it |
| `cancelling_a_biometric_authentication_releases_its_receiver` | the one-shot cancel path leaking the reference |
| `an_autofill_client_is_referenced_until_remove_client` | `addClient`/`removeClient` not being a hold/release pair, a repeat registration double-counting |
| `an_ime_client_and_its_remote_connection_are_referenced_until_teardown` | either binder of the IME client not held, or teardown leaving them rooted |
| `a_hundred_attach_detach_cycles_return_the_ledger_to_baseline` | **the leak test**: 100 × (attach `hw.biometric`, `authenticate`, detach) must return `held_ref_ids()` to its baseline and report one release per round |

## 4. Item 1 (§2a) — design verified, probe not run

Read `results/handoff/BINDERREF2/README.md` §4 first; the finding stands and
the orchestrator's hypothesis matches AOSP: `PhoneStateListener`'s
`IPhoneStateListenerStub` keeps a `WeakReference<PhoneStateListener>`, so a
dropped listener is not called on a real phone either (API 34
`PhoneStateListener(Executor)`: `mListenerStub = new IPhoneStateListenerStub(this)`).
`m-weak.json`/`m-weak-err.json` (`alive=0`, `ringing=0`, no callback body ran)
are therefore **AOSP-consistent**, i.e. not an ACOS bug, for the
framework-wrapped-stub case.

Still to measure (the probes the brief asks for, neither run yet):

1. **Strong stub.** A probe that owns its callback binder itself (an AIDL
   `Stub` subclass in a **static** field, so the Java object is app-reachable
   independently of the registration), `GC.gc()` ×2, idle, then a module
   delivery. Expectation: `GuestRefs` keeps the entry (`acos_services
   {"op":"status"}` / the peer's `node release` absence) and the callback runs.
   This is the case the tree cannot currently pass *if* the app keeps **only**
   the binder id and not the object — see 2.
2. **Weak stub.** The same probe with the stub only weakly held. Expected
   AOSP-faithful sequence: object collected → the node the host rooted must be
   released (`BR_RELEASE`+`BR_DECREFS`) → the host's ledger entry dropped →
   no crash, no stale delivery. ACOS has no `JavaBBinder` global-reference edge
   (BINDERREF2 §4), so today the object is collected *while the node stays
   rooted*: measure it, then either accept (documented) or take the ownership
   decision (per-node owner released on `BR_RELEASE`/`BR_DECREFS`) — that is
   the "architectural wall" the brief says to stop and report.

Instrumentation to build first (BINDERREF2 §4 last paragraph): mirror
`__android_log_print` lines from `BinderTransport::binder_id_of` (tag
`acos-binder`) so the guest's export/root path is visible in `acos_logs`
alongside the driver's `node: local binder=…` lines, and one line per
`native_roots` insert. That is what turns "the object vanished" into "the object
was never rooted on this path".

## 5. Item 3 (BINDERPID) — the design, ready to implement

Two directions, both already have their seam:

**guest → host (what `Binder.getCallingUid/Pid` must report inside a host
service).** The frame header's last 8 bytes are `reserved`
(`crates/acos-binder-peer/src/protocol.rs::HEADER_SIZE`, `struct fhdr`), exactly
`u32 sender_pid` + `u32 sender_euid`. Steps:
1. Bump `PROTOCOL_VERSION` 3 → 4 (and `ACOS_WIRE_VERSION` in
   `native/binder_driver.c`); the driver already refuses frames a peer did not
   advertise.
2. `serve.rs` reads `SO_PEERCRED` on the accepted fd (`getsockopt(SOL_SOCKET,
   SO_PEERCRED)`) → the guest process's **host-visible** pid (the kernel
   translates it into the peer's pid namespace, so this is the pid `ps` and
   `ProcessRecord.pid` show), keyed by tag.
3. The uid is per **process**, from the session/process table, not the socket
   (`bwrap --unshare-user` maps the inner app uid onto host 1000, so
   `SO_PEERCRED`'s uid is 1000 = `SYSTEM_UID`): read it from the ATMS
   `ProcessRecord` for the tag (`AtmsService::process(tag).uid`, the uid PMS
   allocated for that package — this is what differs per app in a multi-process
   session). `AtmsService::process_started` currently hardcodes `pid: 4242`
   (`atms.rs:676`/`835`); give it the real pid from step 2 so the two agree.
4. The peer pushes `acos_parcel::registry::push_calling_identity(uid, pid)`
   around `handle_request`/`service_manager` dispatch and pops it after (the
   in-process transport already does exactly this in
   `acos-runtime/src/transport.rs:174`), so a module answers with
   `acos_parcel::calling_uid()/calling_pid()` — the host-side
   `Binder.getCallingUid()`.

**host → guest (what a guest callback sees).** `deliver_incoming` in
`native/binder_driver.c` sets `tr.sender_pid = 0; tr.sender_euid = 0;`; it must
carry the values from the `F_TXN` frame (`struct inq` gains
`sender_pid/sender_euid`), and the peer fills them with its own pid
(`getpid()`) and `1000` (`SYSTEM_UID`) — on a device a callback into an app
comes from `system_server`. Same for `build_reply` (`BR_REPLY`'s sender).
`Binder.getCallingPid()/getCallingUid()` in the guest then stop answering 0 on
a delivered callback.

**The probe the acceptance wants** ("`checkCallingPermission` result differs per
app uid"): a fixture app calling `Context.checkCallingPermission(...)` (or a
module answering `getCallingUid()`) from inside a host service, with two
processes (`multi-package-probe` / `proc-a` + `proc-b`, tags 0/1, uids 10192 and
the second package's PMS uid): the answer must be `PERMISSION_GRANTED` for the
uid the permission is granted to and `PERMISSION_DENIED` for the other, and
`Binder.getCallingPid()` must equal that process's real pid (not 4242, not 0).

## 6. Files

* `crates/acos-services/src/modules/biometric.rs` — `BiometricState::refs`,
  hold/release on `authenticate`/`cancelAuthentication`, release on the
  scripted `auth`/`reset`, the three drain hooks.
* `crates/acos-services/src/misc.rs` — `AutofillState::refs`,
  `addClient`/`removeClient` + hooks on the autofill `ServiceObject`.
* `crates/acos-services/src/ime.rs` — `ImeState::refs`, `add_client` holds the
  client and its remote connection (and swaps the remote connection on a
  re-registration), `ImeService` hooks.
* `crates/acos-services/src/ime_wire.rs` — `ImeWireService` forwards the three
  hooks to its generic service (it is the node the host dispatches to).
* `crates/acos-services/src/host.rs` — `ServiceHost::drain_refs()`.
* `crates/acos-services/src/module.rs` — `ModuleRegistry::control` drains after
  a control op.
* `crates/acos-services/tests/binder_refs.rs` — the five new tests.
