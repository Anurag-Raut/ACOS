# Sandbox certification for untrusted APKs (S7, 2026-11-11)

Scope: the **interactive ART path** (`acos verify --engine art`), i.e. the product
path that now runs a guest with the window session, the C1 compositor surface,
the input channel, the trusted-launcher probe channel, logd capture and the
gralloc/binder shims. This inherits the S1–S6 certification (bwrap namespaces,
seccomp, mandatory Landlock, cgroup `MemoryMax`, pids cap, RSS watchdog,
adversarial guest corpus) and asks the new question: **what can a hostile APK /
guest reach through the channels added after S6, and does any of them let it
allocate, read, kill or lie its way past the host?**

Evidence: `results/art-runtime/S7.md` (threat table, tests, teeth, ledger).
Tests: `crates/acos-services/tests/sandbox_c1.rs` (new), plus S5/S6.
Start/end stamps for the S7 work: see §6.

> This document is a **recommendation to the owner**. It does not enable
> untrusted APKs, and nothing in this repo does.

## 1. Overall verdict

**NOT CERTIFIED for untrusted APKs today.** One host-safety defect in the window
session is still open (a guest-declared `float[]` length is allocated unbounded,
§4.2), and the probe/verdict channel cannot be made unforgeable in-process
(§4.4). Both are small; §5 lists the exact conditions to move to
**CERTIFIED-WITH-LIMITS**.

| # | surface | verdict | basis |
|---|---|---|---|
| 1 | C1 compositor transaction (`android.ui.ISurfaceComposer` `SET_TRANSACTION_STATE`: `BufferData`, flattened `GraphicBuffer`, fds, ids, counts) | **CERTIFIED** (after S7) | `sandbox_c1` 9/9; 7/9 fail without the fix; `surface_client` 12/12 |
| 2 | Window session (`addToDisplay*`, `relayout`, insets/config replies) | **NOT CERTIFIED** | `window_session.rs` allocates `vec![0.0f32; client_len]` from the request (§4.2). Everything else in the file is bounded/typed and covered by `window_session_*` tests |
| 3 | Binder peer parcels → `ServiceHost` (malformed/oversized/fd-bomb to any service) | **CERTIFIED-WITH-LIMITS** | peer caps: `MAX_FRAME = 256 KiB`, `MAX_FDS = 16`; services decoders fail structured, never panic; not fuzzed at scale |
| 4 | Input channel (guest → host acks) | **CERTIFIED-WITH-LIMITS** | fixed-size `FINISHED` (0x18) / `TIMELINE` (0x20) with exact-length checks, `MAX_POINTERS`; guest acks are guest-asserted (verdict trust, not containment) |
| 5 | Probe / stdout / verdict channel (`ACOSPROBE`, `ACOSLAUNCH`, marker files) | **NOT CERTIFIED for verdict integrity** | the guest can print both prefixes and write its own marker files; containment is bounded (128 KiB line cap, tail caps) |
| 6 | logd capture | **CERTIFIED-WITH-LIMITS** | 50 000-entry drop-oldest cap, `u16` tag/message lengths, datagram from the guest socket; the evidence file is guest-writable (S6 residual) |
| 7 | gralloc/render/binder shims | **CERTIFIED-WITH-LIMITS** | S5/S6 guest corpus; the compositor now accepts only an anonymous regular file as a frame fd (§4.1) |

The distinction that matters: **containment** (can a hostile guest allocate,
read, kill or crash the host?) and **verdict integrity** (can it make ACOS
report something untrue?). Item 5 is the only surface where containment is fine
and integrity is not; item 2 is a containment defect.

## 2. Threat table (new surface)

| surface | hostile input | host handling after S7 | verdict |
|---|---|---|---|
| `setTransactionState` | `width*height*stride` huge, negative, overflowing (e.g. all `u32::MAX`) | `checked_buffer_bytes`: ≤ 8192 px dimension/stride, `stride*4*height` ≤ 64 MiB, checked arithmetic, `stride >= width`; rejected before any allocation | CERTIFIED |
| `setTransactionState` | `GraphicBuffer` header `len` = 16 MiB / `i32::MAX` | header must be 52..=4096 bytes and a multiple of 4; no `vec![0i32; len/4]` from a declared size | CERTIFIED |
| `setTransactionState` | `fd_count` = 1000 (fd bomb) | fd count 0..=8; fd numbers must be ≥ 0 | CERTIFIED |
| `setTransactionState` | fd that is **not** a memfd: a linked path (the app's APK, a framework file), a socket/pipe/dir/device, a closed number, fd reuse | `validate_guest_buffer_fd`: `/proc/self/fd/N` must be a regular file **and** anonymous (`/memfd:…` or `… (deleted)`); otherwise structured `BadRequest`. A short memfd is never a partial frame (`capture_latest` short-read → `None`) | CERTIFIED (residual §4.1) |
| `setTransactionState` | `CallbackId[]` count `i32::MAX` (pre-fix: 34 GiB `with_capacity`) | count ≤ 4096 before the allocation | CERTIFIED |
| `setTransactionState` | composer / display / uncache / listener counts huge | ≤ 256 / 16 / 1024 / 64, rejected before the loop | CERTIFIED |
| `setTransactionState` | duplicate/unknown buffer ids, cache eviction abuse | buffer cache is drop-oldest **16 entries**, layer handle pool **16**; an unknown cached id is a structured miss, never a blank frame | CERTIFIED |
| fd exhaustion while storing | `dup` fails (`EMFILE`) | no panic: the cache entry is skipped and the frame is not stored (`try_clone` → `Option`) | CERTIFIED (defensive) |
| window session `addToDisplay*` / `addToDisplayWithoutInputChannel` | `sizeCompatScale` length `i32::MAX` | **not bounded** (8 GiB `vec![0.0f32; len]`) | **NOT CERTIFIED** (P0, §4.2) |
| binder peer → any service | frame > 256 KiB, > 16 fds, malformed objects/offsets | peer rejects/caps; unused fd objects are dropped; decoders return structured errors | CERTIFIED-WITH-LIMITS |
| input channel → host | arbitrary bytes where a `FINISHED`/`TIMELINE` reply is expected | exact size (0x18/0x20) + kind check; extra bytes rejected; pointer count ≤ `MAX_POINTERS` on the host→guest direction | CERTIFIED-WITH-LIMITS |
| probe channel | app prints `ACOSPROBE {"seq":N,"tree":…}` to stdout; prints `ACOSLAUNCH {"outcome":"ok"…}`; writes `application-created.marker` / `activity-resumed.marker` | host accepts any probe line with the prefix (last `seq` wins) and reads the marker files from the guest-writable staging dir | **NOT CERTIFIED** for verdict integrity (§4.4) |
| logd | log spam, oversized tag/message, fake `OK guard=active` lines | 50 000-entry cap with drop counting; `u16` lengths; guard evidence file is guest-writable | CERTIFIED-WITH-LIMITS (§4.5) |
| shim (gralloc/render/binder) | arbitrary guest code in the shim's address space | shim runs in the guest; the host only sees memfds and binder parcels (surface 1/3) | CERTIFIED-WITH-LIMITS |

## 3. What S7 changed

`crates/acos-services/src/surface_client.rs` only (size/fd/id/type validation;
no protocol or compositor behaviour change):

* new public bounds `MAX_BUFFER_DIMENSION`, `MAX_BUFFER_STRIDE`,
  `MAX_BUFFER_BYTES`, `MAX_GRAPHIC_BUFFER_HEADER_BYTES`,
  `MAX_GRAPHIC_BUFFER_FDS`, `MAX_CALLBACK_IDS`, `MAX_LISTENERS`,
  `MAX_COMPOSERS`, `MAX_DISPLAYS`, `MAX_UNCACHE_BUFFERS`;
* `checked_buffer_bytes()` used by decode, `capture_latest()` and the W6 dump
  helper, so a declared geometry is never an allocation even if a future caller
  reaches the state directly;
* `validate_guest_buffer_fd()` (regular + anonymous only) and
  `bad_transaction()` (structured `BadRequest` for every bound);
* `try_clone`/`cache_buffer` no longer panic on `dup` failure.

New tests: `crates/acos-services/tests/sandbox_c1.rs` (9). All use the real
`ServiceHost` and the real legacy transaction decoder; no ART, so they run in
milliseconds and are safe on a shared host.

## 4. Residuals and the exact requests

### 4.1 Anonymous ≠ memfd (accepted residual)
The type check accepts an anonymous regular file (`/memfd:…` or `… (deleted)`),
not only a `memfd_create` object. Reason: the repo's own C1 fixtures are unlinked
temp files, and an unlinked file the guest already owns leaks nothing the guest
could not read anyway. A **linked** path (the disclosure case) is refused. If
strict memfd-only is required, change `validate_guest_buffer_fd` to require the
`/memfd:` prefix and update `tests/surface_client.rs::test_memfd` to use
`rustix::fs::memfd_create` (rustix `fs` is already in the workspace via
`acos-parcel`), then re-run both test files.

### 4.2 P0 — window-session `sizeCompatScale` length (W13's file, not edited)
`crates/acos-services/src/window_session.rs:373`:

```rust
fn write_size_compat_scale(reply: &mut Parcel, values: &[f32], client_len: i32) -> SResult<()> {
    let len = usize::try_from(client_len).unwrap_or(0);
    let mut out = vec![0.0f32; len];          // guest-declared, unbounded
```

`client_len` is read from the request (`addToDisplayAsUser` /
`addToDisplayWithoutInputChannel`, lines ~404, ~634). A hostile guest sends
`i32::MAX` and the host attempts an 8 GiB allocation. A real client sends the
length of its own `float[]`, which in the image is 0 or 1.

Requested patch (owner: W13 / next window-session change, since W13 owns this
file):

```rust
pub const MAX_SIZE_COMPAT_SCALE: i32 = 8; // the image's client array is 0..=1

fn write_size_compat_scale(...) -> SResult<()> {
    if !(0..=MAX_SIZE_COMPAT_SCALE).contains(&client_len) {
        return Err(ServiceError::BadRequest {
            interface: DESCRIPTOR.to_string(),
            method: "addToDisplayAsUser".into(),
            detail: format!("sizeCompatScale length {client_len} exceeds {MAX_SIZE_COMPAT_SCALE}"),
        });
    }
    ...
}
```

Test to add (services, no ART): build `addToDisplayAsUser` with
`sizeCompatibleScale` length `i32::MAX`, assert `BadRequest` and that the reply
is not written; then the real length (1) still decodes. Also audit the other
`read_int32`-then-allocate shapes in the same file (`InsetsState` counts,
`MergedConfiguration`, string arrays) — the same pattern, same cap.

### 4.3 Input acks are guest-asserted
`FINISHED`/`TIMELINE` are length-checked and bounded, but the HMAC field is
zeros and not verified: a hostile guest can claim `handled: true` for a tap it
never delivered. The engine already requires a **new C1 generation + changed
pixels + a settled probe** after a tap, so a forged ack alone does not pass a
step; a guest that draws the expected pixels can, by definition, pass. Do not
treat an ack as independent evidence in reports.

### 4.4 Probe/milestone spoofing (verdict integrity, no code fix here)
* `launch.rs` accepts any stdout line starting `ACOSPROBE ` as a tree sample
  (last `seq` wins) and any `ACOSLAUNCH {…}` line for the outcome; `launch`
  also reads the guest-written `application-created.marker` /
  `activity-resumed.marker` from the run's staging dir.
* The app shares the guest process with `LaunchMain`, so a per-run nonce passed
  by env/file/stdout is readable by the app: **there is no unforgeable
  in-process channel**. A nonce is still worth adding as an *accident
  tripwire* (an app that prints `ACOSPROBE` by chance is rejected), not as
  attestation.
* Consequences: for untrusted APKs, the tree and lifecycle in a verdict are
  **guest-reported**. Containment is unaffected (bounded caps), so this is a
  trust/labelling problem, not a host-safety one.
* Options, in order of cost: (a) label every `tree`/`mode` field as
  guest-reported and require host-observed corroboration (new C1 generation,
  changed pixels, tap→frame latency) for a `pass`; (b) run the probe in a
  separate trusted process that owns the window session and exposes an
  authenticated channel; (c) add the nonce tripwire
  (`ACOSLAUNCH_PROBE_NONCE` → `LaunchMain` prints `ACOSPROBE <nonce> {…}`,
  host drops lines without it) and document that it is not attestation.
  (c) needs a guest-Java edit (`crates/acos-art/guest/java/.../LaunchMain.java`)
  plus `launch.rs`; both are outside S7 ownership.

### 4.5 Guard/logd evidence is guest-writable
`<run>/out/seccomp.log` is written inside the guest (S6 residual 4). It proves
what the constructor installed and reported, not a tamper-proof audit trail. A
guest cannot *disable* the installed filter (it is installed before `main`), but
it can forge the evidence line. A host-side `SECCOMP_RET_USER_NOTIF` supervisor
remains the proper attribution fix (S6 §5 #3).

### 4.6 Not covered
* No live hostile-APK run was added: the fixtures toolchain
  (`tools/build-apps.sh`) has no hostile-probe fixture, and building one means
  editing `fixtures/` + the build script (outside S7 ownership). The unit tests
  exercise the exact host-side decoder the live path uses; the live milestone
  proves the honest path still works end to end.
* The binder-peer and parcel codecs were read and bounded, not fuzzed at scale.
  A `cargo-fuzz`/proptest campaign over `setTransactionState`, window-session
  methods and peer frames is the next evidence step.

## 5. Conditions to allow untrusted APKs (recommendation)

All of these, in this order:

1. **Fix §4.2** (window-session `float[]` length cap) and the same
   read-length-then-allocate pattern in `window_session.rs`; add the negative
   test. Re-run `tools/guarded.sh --mem 8G cargo test -p acos-services -p acos-wm
   -- --test-threads=1`.
2. **Decide §4.4**: either label guest-reported tree/lifecycle and require
   host-observed corroboration for `pass`, or ship the out-of-process probe.
   Do not claim device parity from guest-reported frames without a comparator.
3. **Keep the S1–S7 layers mandatory and fail-closed**: bwrap namespaces,
   seccomp, Landlock plan, cgroup `MemoryMax` (≥ 2 GiB), `pids.max` (512), RSS
   watchdog, C1 caps from §3. A missing layer must refuse the run, never degrade.
4. **Resource caps per session**: `ACOS_MAX_ACQUIRED_BUFFERS` ≤ 64 (default 2),
   16 cached buffers, 16 layer handles, ≤ 64 MiB per buffer; measure that a
   hostile session's cgroup peak stays under the scenario budget.
5. **Teardown on hostile exit**: kill the process group + cgroup and verify no
   `acos-art`/`LaunchMain`/`bwrap`/scope survivor (S5/S6 leak tests + the
   milestone's clean teardown). Confirmed after S7's milestone run: no ACOS
   survivor.
6. **Add one live hostile fixture** (our own dex) that prints fake
   `ACOSPROBE`/`ACOSLAUNCH`, writes fake markers, sends hostile
   `setTransactionState` geometry and a `sizeCompatScale` bomb; assert the host
   refuses/labels rather than trusts. This is the missing end-to-end proof; it
   needs a small fixtures build-script addition (outside S7).
7. Then, and only then, mark the product path **CERTIFIED-WITH-LIMITS** with the
   residuals of §4.1/4.3/4.5/4.6 stated in every untrusted-APK report.

## 6. Verification (commands actually run)

| check | command | result |
|---|---|---|
| new corpus | `tools/guarded.sh --mem 8G --time 600 -- cargo test -q -p acos-services --test sandbox_c1 -- --test-threads=1` | **9 passed** (2026-11-11T21:08:41Z) |
| teeth (pre-fix decoder restored from HEAD, same test) | same command | **7 failed / 2 passed**, each failure the expected acceptance of a hostile shape (21:08:56Z); fixed file restored |
| C1 regression | `… --test surface_client` | **12 passed** |
| acceptance suite | `tools/guarded.sh --mem 8G --time 1200 -- cargo test -q -p acos-services -p acos-wm -- --test-threads=1` | **rc=0, 31 binaries, 272 tests, 0 failed** (21:09:51Z) |
| S5/S6 corpus | `… cargo test -q -p acos-art --test sandbox_s5 --test sandbox_s6 -- --test-threads=1` | **5 + 5 passed** (21:10:28Z) |
| milestone (interactive path) | `tools/art-runtime/run-exclusive.sh --task S7 -- tools/guarded.sh --mem 8G --time 900 -- ./target/debug/acos verify --scenario scenarios/v2-art-counter.yaml --apk fixtures/build/apps/counter.apk --engine art --json` | **pass, 11/11 steps, engine=art, mode=live**, rc=0 (21:10:02–21:10:13Z) |
| teardown | `pgrep -a -f 'acos-art\|LaunchMain'` after the milestone | no ACOS survivor (21:10:56Z) |

## 7. Decision on verdict integrity (2026-11-11, codex advisor gpt-6.1-sol, orchestrator decision)

Chosen path: **A with stricter pass semantics** (B optional, C deferred, D as complement).
- Tree, lifecycle markers, resource ids, launch outcome lines, input `handled` acks and guest logs are **guest-asserted**; they are labelled so in results and may be used for navigation (finding tap targets) but never alone justify a pass on an untrusted-APK run.
- A pass for an untrusted APK needs host-controlled evidence that independently satisfies the assertion: C1 pixels (reference/region match, OCR with uncertainty), host-injected input, host-observed binder transactions. "Pixels changed" never substitutes for "expected text appeared". Tree-only assertions on untrusted runs return an insufficient-evidence result (fail closed).
- A per-run nonce is an accident tripwire only; it is not a security boundary (the app shares the probe's process).
- Stale text above: the sizeCompatScale cap landed in e55b3ec.
- Planned work (S8, after W15/W16 free `engine/art.rs`): provenance field on every evidence item (host-observed | guest-asserted), fail-closed rules for untrusted runs, a pixel/region assertion, live hostile fixture (forged probe/marker lines, hostile parcels), then re-certify. Untrusted APKs stay disabled until all section 5 gates pass. **Implemented in S8 — see §8.** The product path is still not certified; only the labelled test override (`ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1`) can run the hostile fixture.

## 8. S8 — verdict-integrity implementation (2026-11-11)

S8 implements the §7 decision. It does **not** certify untrusted APKs; it makes
the verdict channel fail closed and proves the failure with a live hostile APK.

### 8.1 What the verdict now says

Every ART-engine result carries:

* `trust: trusted | untrusted` on the verdict and `engine.json`;
* `evidence_provenance` (run level) and `steps[].provenance` (per step), copied
  into each `steps/*.json` artifact.

| evidence item | provenance | justified by |
|---|---|---|
| C1 frame / `frames/*.png` | **host-observed** | the host read the compositor's pixels |
| region / screenshot assertion | **host-observed** | evaluated on those pixels, never on the tree |
| host-injected tap/key/type | **host-observed** | the host wrote the input message |
| binder transactions | **host-observed** (label reserved) | host-owned service state |
| static APK manifest | **host-observed** | parsed by the host |
| View tree | **guest-asserted** | built by the in-guest probe |
| lifecycle / launch markers | **guest-asserted** | guest-written files and stdout |
| input `handled` acks | **guest-asserted** | guest `FINISHED`/`TIMELINE` replies |
| logs | **guest-asserted** | guest logd socket |

Per-step labels: `launch` → frame host-observed, markers/tree guest-asserted;
tree assertions → tree guest-asserted; input steps → host input + frame
host-observed, acks/tree guest-asserted; `capture`/`assert_screenshot`/
`assert_region` → frame host-observed; `assert_log` → logs guest-asserted.

### 8.2 Trust modes (exact)

| condition | result |
|---|---|
| default (no `--untrusted`, no `trust:` in the scenario) | `trust: trusted`; existing strict step rules unchanged |
| `--untrusted` **or** top-level scenario `trust: untrusted`, without `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1` | **refused before ART starts** with `UNTRUSTED_APK_NOT_CERTIFIED` (status `blocked`, no session, no frame) |
| untrusted + `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1`, tree-only step (`wait_for`, `assert_text`, `assert_visible`, `assert_not_visible`, `assert_state`) | **fails closed** with `INSUFFICIENT_HOST_EVIDENCE`; the tree is not used to justify a pass |
| untrusted + override, `tap` with a selector | allowed: the host injects the tap and resolves the point from the tree **for navigation only** |
| untrusted + override, `capture` / `assert_screenshot` / `assert_region` | allowed: host-observed pixels |
| untrusted + override, `assert_log` / markers / acks alone | cannot justify a pass (guest-asserted; on ART logs are unsupported anyway) |
| `assert_region.text_ocr` | `UNSUPPORTED_ASSERTION` (no OCR engine in this build), never a pass |

`assert_region` is a scenario step: `{x, y, w, h, equals_png?, hash?,
min_distinct_colors?, max_distinct_colors?, text_ocr?}`. It always captures a
**fresh** frame. `hash` is SHA-256 of the region's tightly packed RGBA8 bytes;
`equals_png` is an exact reference (region-sized, or frame-sized and cropped to
the same region); colour statistics count distinct RGBA values. "Pixels
changed" is never accepted as "the expected text appeared"; use OCR (absent) or
an exact pixel/region criterion.

### 8.3 Freshness with teeth

* A delivered input that changes no pixels (focus-only tap, a key the view
  consumes silently) is **not stale**. The guest session reports the injected
  gesture with no settled changed frame; the engine re-binds a fresh tree/frame
  and records `InputDelivery { delivered: true, visual_change: false }`. A tap
  step then passes as `input_delivery` and explicitly claims **no** state change.
* A missing frame without delivery evidence remains `NO_FRESH_FRAME`; a reply
  that reuses a frame remains `STALE_FRAME`. `capture`, `assert_screenshot` and
  `assert_region` still require a new completed frame.

### 8.4 Live hostile fixture (our own dex)

`fixtures/apps/hostile-probe` (built by `fixtures/build-hostile-probe.sh`) prints
forged `ACOSPROBE` lines with `seq=9999` claiming id `verdict` has text
"HOSTILE PASS", prints forged `ACOSLAUNCH` success lines, writes marker-shaped
files, and draws a **solid red** surface with no text.

| run | result |
|---|---|
| trusted control (`scenarios/s8-hostile-forged.yaml`) | **pass** — demonstrates the forgery works when the tree is trusted; the verdict labels tree/markers guest-asserted |
| untrusted (`scenarios/s8-hostile-untrusted.yaml`, override set) | **fail** `INSUFFICIENT_HOST_EVIDENCE` — the forged tree cannot pass |
| untrusted region (`scenarios/s8-hostile-region.yaml`, override set) | **fail** `ASSERTION_FAILED`: host pixels show **1** distinct colour in the region; the claim was text |
| untrusted without the override | **blocked** `UNTRUSTED_APK_NOT_CERTIFIED`; the ART session never starts |

Evidence: `results/art-runtime/evidence/S8-hostile-{trusted,untrusted,region,refused}/`,
`results/art-runtime/S8.md`.

### 8.5 Conditions that still remain before CERTIFIED-WITH-LIMITS

The verdict channel is now fail-closed, but the §5 gate is not complete:

1. **Hostile containment parcels are still unit-tested only.** The live hostile
   fixture forges the verdict channel and draws the wrong pixels; it does not
   send a hostile `setTransactionState` geometry/`sizeCompatScale` bomb. §5 #6
   asks for both in one live APK. The S7 `sandbox_c1` corpus covers the decoder.
2. **Input acks remain guest-asserted** (§4.3): the HMAC field is not verified.
   A guest that draws the expected pixels can still pass a host-pixel assertion
   by definition; a forged ack alone cannot pass a tree-only untrusted step.
3. **Logs and markers remain guest-writable** (§4.5), so their provenance stays
   `guest-asserted`; they are never sufficient on an untrusted run.
4. **OCR is not implemented.** `text_ocr` returns `UNSUPPORTED_ASSERTION`; a
   text claim on an untrusted run needs an implemented OCR path or an exact
   pixel reference.
5. **Binder transactions are labelled but not surfaced** as per-step evidence
   today. The host observes them; a future evidence item must carry the label.
6. **No untrusted APK is allowed in the product path.** The only override is
   `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1`, used by the hostile-fixture tests; arbitrary
   third-party APKs remain refused.

When items 1 and 4–5 are closed (and §4.2/§4.3/§4.5 residuals accepted or fixed),
re-run the §6 verification and this section before changing the verdict.
