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

> This document started as a **recommendation to the owner**. The owner's
> 2026-11-12 decision (recorded in §8.8) adopts the CERTIFIED-WITH-LIMITS
> verdict as product policy: untrusted APKs are **opt-in** (`--untrusted` or a
> scenario `trust: untrusted`) and trusted stays the default. The limits in §8.7
> are the contract of that opt-in.

## 1. Overall verdict

**CERTIFIED-WITH-LIMITS (re-audit 2026-11-12, §8.6).** The S7 window-session
`sizeCompatScale` defect is fixed, the gate-1 audit was repeated across
`acos-services` / `acos-parcel` / `acos-binder-peer` and the remaining
guest-sized allocations were bounded (`bebe650`; `gate1_bounds` 3/3 green), and
the live hostile fixture exercises forged probes, a forged forest, probe spam,
hostile layer/geometry counts and the `sizeCompatScale` bomb. Every §5 gate is
met with evidence. The limits are explicit in §8.7: guest-asserted tree /
markers / acks / logs can never justify an untrusted pass (host-observed pixels
or input must corroborate), a frame fd may be an anonymous unlinked file rather
than strictly `memfd` (§4.1), OCR is absent, and the 16-layer composite
worst-case was not constructed (caps are enforced; hostile sessions measured
67–80 MiB). Since 2026-11-12 product policy is **opt-in untrusted** (§8.8):
untrusted runs are allowed when the caller asks for one explicitly, and trusted
stays the default.

| # | surface | verdict | basis |
|---|---|---|---|
| 1 | C1 compositor transaction (`android.ui.ISurfaceComposer` `SET_TRANSACTION_STATE`: `BufferData`, flattened `GraphicBuffer`, fds, ids, counts) | **CERTIFIED** (after S7) | `sandbox_c1` 9/9; 7/9 fail without the fix; `surface_client` 12/12 |
| 2 | Window session (`addToDisplay*`, `relayout`, insets/config replies) | **CERTIFIED** (after S7/W44) | `sizeCompatScale` is capped at 8 (`window_session.rs:373-381`); the live `i32::MAX` parcel is refused; the rest of the file is bounded/typed and covered by `window_session_*` tests |
| 3 | Binder peer parcels → `ServiceHost` (malformed/oversized/fd-bomb to any service) | **CERTIFIED-WITH-LIMITS** | peer caps: `MAX_FRAME = 256 KiB`, `MAX_FDS = 16`; services decoders fail structured, never panic; not fuzzed at scale |
| 4 | Input channel (guest → host acks) | **CERTIFIED-WITH-LIMITS** | fixed-size `FINISHED` (0x18) / `TIMELINE` (0x20) with exact-length checks, `MAX_POINTERS`; guest acks are guest-asserted (verdict trust, not containment) |
| 5 | Probe / stdout / verdict channel (`ACOSPROBE`, `ACOSLAUNCH`, marker files) | **CERTIFIED-WITH-LIMITS for verdict integrity** | the guest can print both prefixes and write its own marker files, so they are labelled `guest-asserted`; untrusted tree/log/marker-only assertions fail closed (`INSUFFICIENT_HOST_EVIDENCE`); containment is bounded (8 MiB line cap, oversized lines drained/counted, tail caps) |
| 6 | logd capture | **CERTIFIED-WITH-LIMITS** | 50 000-entry drop-oldest cap, `u16` tag/message lengths, datagram from the guest socket; the evidence file is guest-writable (S6 residual) |
| 7 | gralloc/render/binder shims | **CERTIFIED-WITH-LIMITS** | S5/S6 guest corpus; the compositor now accepts only an anonymous regular file as a frame fd (§4.1) |

The distinction that matters: **containment** (can a hostile guest allocate,
read, kill or crash the host?) and **verdict integrity** (can it make ACOS
report something untrue?). Item 5 is containment-fine but integrity-limited by
design (guest-asserted evidence is labelled and never sufficient alone); item 2
was the last containment defect and is fixed.

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
| window session `addToDisplay*` / `addToDisplayWithoutInputChannel` | `sizeCompatScale` length `i32::MAX` | capped at `MAX_SIZE_COMPAT_SCALE=8` before the allocation; structured refusal; live W44 parcel bomb refused | **CERTIFIED** (fixed after S7; §8.6) |
| binder peer → any service | frame > 256 KiB, > 16 fds, malformed objects/offsets | peer rejects/caps; unused fd objects are dropped; decoders return structured errors | CERTIFIED-WITH-LIMITS |
| input channel → host | arbitrary bytes where a `FINISHED`/`TIMELINE` reply is expected | exact size (0x18/0x20) + kind check; extra bytes rejected; pointer count ≤ `MAX_POINTERS` on the host→guest direction | CERTIFIED-WITH-LIMITS |
| probe channel | app prints `ACOSPROBE {"seq":N,"tree":…}` to stdout; prints `ACOSLAUNCH {"outcome":"ok"…}`; writes `application-created.marker` / `activity-resumed.marker` | host still reads any probe line with the prefix (last `seq` wins) and the guest-written markers, but labels them `guest-asserted`; untrusted tree/log/marker-only assertions fail closed; 8 MiB line cap with oversized-line counting | **CERTIFIED-WITH-LIMITS for verdict integrity** (§8.6) |
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
- Planned work (S8, after W15/W16 free `engine/art.rs`): provenance field on every evidence item (host-observed | guest-asserted), fail-closed rules for untrusted runs, a pixel/region assertion, live hostile fixture (forged probe/marker lines, hostile parcels), then re-certify. Untrusted APKs stay disabled until all section 5 gates pass. **Implemented in S8 — see §8.** At S8 time the product path was not certified and only the labelled test override (`ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1`) could run the hostile fixture; **superseded by §8.8**, which allows an explicit opt-in without any env var.

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
min_distinct_colors?, max_distinct_colors?, text_ocr?}`. `x`/`y`/`w`/`h` are
**display** pixels (tree bounds and taps use the same space); the executor maps
them onto the frame with the frame's `display_origin` and fails when the frame
does not cover the region (`docs/AGENT_API.md` §9.3). It always captures a
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
re-run the §6 verification and this section before changing the verdict. **W44
closed item 1 on 2026-11-12** (live hostile parcels now in one APK); items 2–6
are carried as the declared limits in §8.7.

### 8.6 Re-audit 2026-11-12 (W44, final)

Audit basis: current tree at `6a12285` (W44 report) + `bebe650` (gate-1 bounds)
and `22f82ed` (W42 layer teardown), not the S7/S8 snapshots. First W44 pass
started `date -u` **2026-11-12T03:31:44Z** and found two unbounded allocations;
the follow-up pass started **2026-11-12T04:04:18Z**, rebuilt
`.cache/w44-target/release/{acos,acos-art}` and re-ran the gate audit, §6 and
the live hostile scenarios. Final evidence timestamps: gate1_bounds
**04:10:37–04:10:38Z**; §6 tests **04:06:31–04:09:07Z**; cleanup
**04:10:22–04:10:33Z**; hostile parcel/forest/spam **04:09:16–04:09:30Z**;
`counter` 11/11 **04:09:39–04:09:45Z**. Every hostile run used our own fixtures
and `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1`, under
`tools/art-runtime/run-exclusive.sh --task W44*` +
`tools/guarded.sh --mem 8G --time 600|900`. No third-party APK was run.

**Re-audit verdict: CERTIFIED-WITH-LIMITS.** Every §5 gate is met with evidence
from this run; the limits are named in §8.7. `CERTIFIED` (without limits) is not
claimed because the guest-asserted evidence channel, the anonymous-unlinked
frame-fd rule, OCR absence and the unmeasured 16-layer composite worst case are
all real limits, even though containment is bounded.

| §5 gate | status on the current code | file:line and evidence |
|---|---|---|
| 1. no guest length drives an allocation | **MET** | `gate1_bounds` **3/3 passed** (04:10:37Z). `parcelable.rs:2571` caps `VAL_MAP` at `min(size, remaining/8)`; `cursor.rs:359` refuses `size > remaining`; `cursor.rs:530` caps projection at `min(count, remaining/4)`. Full audit: every `with_capacity` / `vec![..; n]` / `reserve` / `resize` / `repeat` in `acos-services`, `acos-parcel` and `acos-binder-peer` is either constant/host-sized or bounded by `check(total)` / `remaining` (see `results/art-runtime/W44.md` §2). `window_session.rs:373-381` caps `sizeCompatScale` at 8 and the live `i32::MAX` parcel is refused. `surface_client.rs` geometry/header/callback counts are bounded; `input_channel` pointers are capped at 16; `acos-binder-peer` frames are capped at `MAX_FRAME`/`MAX_FDS`. |
| 2. verdict integrity decision (§4.4/§7) | **MET WITH LIMITS** | `trust`/`evidence_provenance`/per-step labels in `crates/acos-cli/src/{trust,provenance}.rs`; untrusted tree/logs fail closed (`trust.rs:133-180`). Live: forged two-window forest with `truncated:false` → `INSUFFICIENT_HOST_EVIDENCE` untrusted, `ELEMENT_NOT_FOUND` trusted (`W44-rerun-forest-{untrusted,trusted}/verdict.json`); forged text fails the host-pixel region assertion; spam untrusted fails closed, trusted control passes. Tree/markers/acks/logs remain guest-asserted (§4.3/§4.5). |
| 3. S1–S7 layers mandatory / fail closed | **MET** | `crates/acos-art/src/cgroup.rs` refuses on a missing cgroup (`CgroupUnavailable`), reads back `memory.max`, `cgroup.kill`; seccomp/Landlock/bwrap unchanged. 04:06:31–04:09:07Z: `cargo test -p acos-art --test sandbox_s5 --test sandbox_s6` → **5 + 5 passed**. |
| 4. per-session caps incl. composite and the 8 MiB probe line | **MET (with a measurement limit)** | composite caps `MAX_COMPOSITE_LAYERS=16`, canvas ≤ 64 MiB, total input ≤ 256 MiB (`surface_client.rs:151-154`, enforced `:840-915`); probe line read whole up to 8 MiB and oversized lines drained/counted (`acos-art/src/launch.rs:1104`, `:1421-1484`). Fresh hostile-session cgroup peaks: parcel **67 MiB**, spam **80 MiB** (`W44-rerun-*/art-frames/session-exit.json`, `memory.max=2048MiB`, `oom_kills=0`). The 16-layer × 64 MiB composite worst case was not constructed; the caps are enforced but that memory peak is a named limit. |
| 5. hostile exit teardown | **MET** | `acos-art/src/launch.rs:1617-1626` (`kill_guest`: process group + `cgroup.kill`) and teardown path `:783-830`; `cleanup::guest_crash_still_removes_the_run_root` passed 04:10:22–04:10:33Z. Fresh hostile sessions: `guest_exit: 0`; no W44 `acos-art`/`LaunchMain`/`bwrap` survivor. |
| 6. live hostile fixture (our dex) | **MET** | Four signed variants: `.MainActivity` (forged probe/launch + two-window `truncated:false` forest + fake markers), `.ForestActivity` (missing node), `.SpamActivity` (>8 MiB line + 64 forged lines), `.ParcelActivity` (257 composer states, `0xffffffff` GraphicBuffer geometry, `i32::MAX` sizeCompatScale). Fresh host refusals in `W44-rerun-parcel/art-frames/unsupported.jsonl`: `composer count 257 exceeds 256`, `GraphicBuffer geometry 4294967295x4294967295 stride 4294967295 ... outside`, `sizeCompatScale length 2147483647 exceeds 8`. Spam trusted control parsed the last valid forged sample (`truncated:false`, id `probe-spam`); untrusted forest/spam fail closed. |
| 7. then mark CERTIFIED-WITH-LIMITS | **MET** | this section + §8.7; the product path still refuses untrusted APKs by default (no code change in this task). |

### 8.7 Limits of this certification (2026-11-12)

These are declared limits, not open gates; every one is labelled in evidence.

1. **Guest-asserted evidence** — tree, lifecycle/launch markers, input `handled`
   acks and logs are guest-reported. On an untrusted run they are labelled
   `guest-asserted` and never justify a pass; only host-observed pixels
   (`capture`/`assert_screenshot`/`assert_region`), host-injected input and the
   static manifest can. OCR is absent, so a text claim needs an exact
   pixel/region reference (`text_ocr` → `UNSUPPORTED_ASSERTION`).
2. **Frame fd** — the compositor accepts an anonymous regular file
   (`/memfd:…` or unlinked `… (deleted)`), not strictly a `memfd_create`
   object (§4.1). A linked path is refused; an unlinked file the guest already
   owns leaks nothing extra.
3. **Logd / marker evidence** — guest-writable; provenance is `guest-asserted`
   (§4.5). A host-side seccomp supervisor remains the attribution fix.
4. **Composite worst case** — per-layer and aggregate caps are enforced
   (16 layers, 64 MiB canvas, 256 MiB input), but the 16-layer × 64 MiB
   construction was not run; measured hostile sessions peaked at 67–80 MiB.
5. **Binder transactions** — labelled `host-observed` at run level, not yet
   surfaced as a per-step artifact.
6. **Product path** — at the time of this re-audit untrusted APKs were still
   refused by default with `UNTRUSTED_APK_NOT_CERTIFIED` unless the test-only
   `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1` override was set. **Changed on
   2026-11-12 by the owner's opt-in policy (§8.8):** an explicit `--untrusted`
   is allowed with no env var, and the refusal is only reachable through a
   certification downgrade.

Residuals already accepted in earlier sections remain as described in §4.1,
§4.3, §4.5 and §8.5.

### 8.8 Product policy: opt-in untrusted (2026-11-12, owner decision)

**Decision.** The owner chose on 2026-11-12 that untrusted APKs are **opt-in
only**. They are *allowed* in the product — the sandbox verdict is
CERTIFIED-WITH-LIMITS (§8.6) — but never reached automatically.

Rules, as implemented in `crates/acos-cli/src/trust.rs` and
`crates/acos-cli/src/verify.rs`:

1. **Explicit opt-in only.** `acos verify --untrusted` or a scenario
   `trust: untrusted` marks a run untrusted, and it now runs **with no
   environment variable**. There is no auto-detection of APK origin.
2. **Trusted is the default.** A run without the flag and without the scenario
   key is trusted (our own fixtures), exactly as before.
3. **Verdict integrity is unchanged.** In untrusted mode only host-observed
   evidence may justify a pass. Tree-only, log-only and marker-only assertions
   fail closed with `INSUFFICIENT_HOST_EVIDENCE` (§8.7 item 1);
   `assert_region` / `assert_screenshot` / host-injected input still run. The
   guard is `TrustGuard` in `trust.rs` and was not weakened by this policy.
4. **Every verdict and envelope is labelled** with `trust`,
   `certification: "certified-with-limits"`, a `certification_limits` pointer
   (`docs/SANDBOX_CERTIFICATION.md` §8.7) and a `certification_policy` pointer
   (this section). Step artifacts carry `trust`/`certification` too.
5. **The test override is a redundant alias.** `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1`
   is still accepted and recorded, so existing hostile tests and scripts keep
   working unchanged; it gates nothing.
6. **`UNTRUSTED_APK_NOT_CERTIFIED` stays registered** (`acos-protocol`) and is
   produced by exactly one function, `trust::untrusted_gate`, only when the
   const flag `trust::CERTIFICATION` is a downgrade
   (`Certification::NotCertified`). Today it is
   `Certification::CertifiedWithLimits`, so the code is unreachable; the unit
   test `certification_downgrade_restores_the_refusal` in `trust.rs` proves the
   flag flips the behaviour.

Consequences for readers of evidence: a pass on an untrusted run is a
**host-pixel / host-input** pass, never a tree or log claim; an
`INSUFFICIENT_HOST_EVIDENCE` failure means the run was allowed but the evidence
class was not, not that ACOS refused to run the APK. Tests:
`crates/acos-cli/tests/w51_untrusted_policy.rs` (allowed without env var,
tree-only fail-closed, trusted default unchanged, alias redundant),
`crates/acos-cli/tests/sandbox_s8.rs` and `w44_trust.rs`. Live evidence:
`results/art-runtime/W51.md`.
