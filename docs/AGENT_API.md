# ACOS agent API — V1 contract

**Status:** contract v1, defined in `crates/acos-protocol` (typed Rust + JSON
Schema in `crates/acos-protocol/schema/`). Runtime crates implement this
contract; the CLI/MCP server adapts it. There is exactly one consumer: an AI
coding agent (`SPEC.md`, owner directive D1). No human-facing UX is implied.

The contract is normative in these places:

| artifact | purpose |
|---|---|
| `crates/acos-protocol/src/*.rs` | typed requests/responses, errors, scenario, verdict |
| `crates/acos-protocol/schema/*.json` | checked-in JSON Schema (`UPDATE_SCHEMA=1` refreshes) |
| `scenarios/*.yaml` | executable examples with ART-reference values |
| this document | workflow + interpretation rules |

Every command writes exactly **one JSON object** to stdout and the same object is
returned as MCP tool content. With `--json`, **stdout is only that JSON
document** — no summary, no progress lines. Human diagnostics (and, on a
non-pass run, the one-line `verify: CODE (class): message` trailer) go to
**stderr**, so `acos verify … --json > out.json 2>&1` produces invalid JSON.
Redirect stderr separately (`2>err.txt`) when you capture the envelope.

---

## 1. The agent loop

```text
inspect  ->  (capability-report)  ->  verify  ->  read verdict.json  ->  fix  ->  rerun
```

```bash
# 1. What is this APK? (static only; does not execute)
acos inspect --apk fixtures/build/apps/hello-views.apk --json > out/requirements.json

# 2. Optional: can this engine do what the scenario needs?
acos capability-report --apk fixtures/build/apps/hello-views.apk \
  --scenario scenarios/hello-views.yaml --json

# 3. Run the scenario and write evidence
acos verify --apk fixtures/build/apps/hello-views.apk \
  --scenario scenarios/hello-views.yaml --json --artifacts out/hello-views

# 4. If it is not a pass, read the structured failure (never guess)
cat out/hello-views/verdict.json
cat out/hello-views/steps/03-assert_text.json
cat out/hello-views/events.jsonl

# 5. Fix the app or the scenario, then rerun into a fresh directory
#    (a run owns its artifacts dir: reusing another run's dir is INVALID_ARGUMENT;
#     pass --overwrite only when you mean to discard that evidence)
```

Rules for the loop:

* `unsupported` means the engine cannot do the task. Do **not** "fix" the app to
  silence it; use another backend/target or report it as unsupported.
* `fail` means the app ran and a check did not hold. Read the assertion diff.
* `crash`, `timeout`, `blocked`, `error` are not passes either.
* Keep the artifacts directory from a failing run; it is the evidence for the
  diagnosis. Reuse the same scenario so the comparison is meaningful. A passing
  run is pruned to the evidence it declares (§10.2); use
  `--keep-artifacts all` to reproduce a bug against the raw captures, and
  `acos artifacts gc` to bound the collection of old runs.
* A pass is valid only when its evidence exists (see §10).

---

## 2. The response envelope

Every response is this object (the existing `inspect` fields are a subset):

```json
{
  "request_id": "req_19",
  "ok": true,
  "status": "pass",
  "backend": "acos-native",
  "fidelity": "native-runtime",
  "validated_against": "art-reference",
  "session_id": "s_login",
  "isolation": "sandbox",
  "frame": {
    "frame_seq": 19,
    "produced_at": "2026-11-10T13:51:14Z",
    "cause_id": "req_19",
    "width": 1080,
    "height": 2400,
    "density": 420,
    "content_hash": "3f2a…",
    "meaningful": true
  },
  "capabilities_used": ["lifecycle", "ui.offscreen", "input"],
  "result": {"clicks": 1},
  "metrics": {"wall_ms": 42, "peak_rss_bytes": 67108864},
  "evidence": {"screenshot": "frames/19.png", "tree": "tree/19.json", "events": "events.jsonl"}
}
```

Optional fields (`validated_against`, `session_id`, `isolation`, `frame`,
`error`, `result`, `evidence`, `job_id`) are omitted when absent. Seven keys are
always present: `request_id`, `ok`, `status`, `backend`, `fidelity`,
`capabilities_used` and `metrics`.

> **Where the frame lives.** Commands that attach a frame to the envelope
> (`capture`, `action`) put its metadata at the top-level `frame`. **`observe`
> does not**: it nests the frame inside `result` (`result.frame`, alongside
> `result.tree` for `--tree`, as the CLI and MCP do today). Read
> `result.frame.frame_seq` from `observe`, and the top-level `frame` from the
> frame-producing commands.

### 2.1 Invariants (structural, not advisory)

* `ok: true` **iff** `status: "pass"`.
* A non-pass status **must** carry an `error` object.
* A pass status **must not** carry an `error`.
* The error code determines the status:
  * `ASSERTION_FAILED`, `ELEMENT_NOT_FOUND` → `fail`
  * `APP_CRASHED` → `crash`
  * `APP_NOT_RESPONDING`, `TIMEOUT` → `timeout`
  * `SESSION_BUSY`, `RESOURCE_LIMIT`, `RAM_GUARD`, `RAM_ABORT`,
    `ISOLATION_UNAVAILABLE`, `ISOLATION_UNSATISFIED` → `blocked`
  * `UNSUPPORTED_*`, `NEEDS_REAL_DEVICE`, `REQUIRES_GMS`, `WRONG_ABI`,
    `NO_DISPLAY`, `NO_TEXT`, `UNSUPPORTED_BACKEND` → `unsupported`
  * everything else → `error`

`Envelope` has private fields; only `Envelope::pass`, `Envelope::failure` and a
validating `Deserialize` can construct it. Deserializing `ok:true` with
`status:fail`, or `UNSUPPORTED_*` as `error`, fails. This makes a false pass
unrepresentable without editing the protocol crate.

### 2.2 Status meanings

| status | meaning | is it a pass? |
|---|---|---|
| `pass` | requested checks held under the stated fidelity | **yes** |
| `fail` | the app ran; an assertion/correctness check did not hold | no |
| `error` | harness/input/engine failure (not an app verdict) | no |
| `unsupported` | the engine cannot perform the requested behaviour | **no** |
| `crash` | the app crashed | no |
| `timeout` | wall/CPU budget exceeded, or ANR | no |
| `blocked` | cannot start until an external condition is resolved | no |

### 2.3 Error object

```json
{
  "code": "UNSUPPORTED_NATIVE_SYMBOL",
  "message": "libhellojni.so imports __android_log_print (not implemented)",
  "hint": "Read details.symbol/details.library; implement/import the symbol, rebuild the .so for the supported ABI, or report the exact missing symbol. Do not fake a return value.",
  "retryable": false,
  "details": {"symbol": "__android_log_print", "library": "libhellojni.so"}
}
```

`code` is stable; `message` is not. Branch on `code`, use `hint` as the next
action, use `details` for the exact API/symbol/opcode/path. `retryable: true`
means retrying the same request after addressing the hint may succeed; it never
means the failure is benign. The full table is in §12.

---

## 3. Fidelity — what a pass actually proves

| fidelity | what ran | what a pass proves |
|---|---|---|
| `static-analysis` | host APK/DEX/manifest parsing | only that parsing/analysis succeeded — **no runtime behaviour** |
| `jvm-reference` | host OpenJDK | pure-Java semantics only; **never** Android/ART/framework/JNI/rendering |
| `native-runtime` | ACOS's own Rust VM/loader/renderer | real execution by ACOS, but **not validated against ART** unless `validated_against` says so |
| `art-reference` | real ART on a reference Android image | authoritative Android framework behaviour at the stated API/ABI |
| `physical-device` | a real device/OEM target | device behaviour, including OEM/hardware paths |
| `simulated` | mocks/stubs | the mock path only; **not** an Android success |

Rules:

* A `jvm-reference` pass can never satisfy an `art-reference` requirement
  (ADR-0005). Nor can `native-runtime` without ART validation.
* `validated_against` records a stronger oracle that checked a weaker result
  (for example `native-runtime` + `validated_against: art-reference`).
* `fidelity` is the achieved level, never the requested one. If the caller asks
  for `art-reference` and only `native-runtime` is available, the correct result
  is `unsupported`/`UNVERIFIED_FIDELITY`, not a pass with an optimistic label.
* `verify` records both `fidelity` and optional `required_fidelity` in
  `verdict.json`.

`Fidelity::satisfies(required)` is implemented in the protocol crate; agents
should not hand-roll these comparisons.

---

## 4. `unsupported` is not `fail` and never a pass

| situation | correct result | what the agent does |
|---|---|---|
| app's text is wrong | `fail` + `ASSERTION_FAILED` + diff | fix the app/expectation |
| selector matches nothing | `fail` + `ELEMENT_NOT_FOUND` | re-observe; fix selector or expectation |
| engine lacks a Java API | `unsupported` + `UNSUPPORTED_JAVA_API` | do not rewrite the app to hide it; escalate or report |
| ARM-only `.so` on x86_64 | `unsupported` + `UNSUPPORTED_ABI` (APK-level; `WRONG_ABI` only for a per-ELF `e_machine` mismatch at load) | supply the ABI or change target |
| app needs GMS | `unsupported` + `REQUIRES_GMS` | use a GMS target or report |
| no display on a headless session | `unsupported` + `NO_DISPLAY` | create a display or drop UI steps |
| app crash | `crash` + `APP_CRASHED` | read stack; fix app/platform call |
| main thread hung | `timeout` + `APP_NOT_RESPONDING` | inspect ANR stack; fix |
| host out of memory | `blocked` + `RESOURCE_LIMIT`/`RAM_GUARD` | reduce concurrency/budget; retry |

If a runtime cannot distinguish "the app is wrong" from "the engine is
incomplete", the honest answer is `unsupported`/`error` — never `pass`.

---

## 5. Frame freshness

A screenshot or UI tree is evidence only for the request/action that produced
it.

* Every frame has a monotonic per-session `frame_seq`, a `cause_id` (usually the
  `request_id`), `produced_at`, `content_hash` and `meaningful`.
* `capture` defaults to `fresh: true`. A capture with `after_seq: N` may only
  return a frame with `frame_seq > N`. A capture with `after_cause: req_X` may
  only return a frame caused by `req_X`.
* A stale frame must produce `NO_FRESH_FRAME` (retryable), never an older image.
* `meaningful: false` frames (blank/placeholder) must never satisfy a UI
  assertion or screenshot assertion.
* UI assertions record the `frame_seq` they inspected; the verdict links it to
  `frames/<seq>.png` and `tree/<seq>.json`.
* The legacy `seq`/`frame`/`sha256` idea is preserved: only the frame answering
  *this* request is acceptable.

---

## 6. Sessions, isolation and reset

`run` creates a session. Every other session command takes `base.session_id`.

| isolation | independence claimed | notes |
|---|---|---|
| `shared-task` | none | another task in a shared process/data store; a crash kills all tasks |
| `process` | process only | separate process; app data may still be shared |
| `sandbox` | separate process/UID/credentials/data | required before claiming independent data |
| `core` | fully separate engine instance | for ABI/API conflicts, stronger service-state independence |
| `vm` | full VM boundary | for untrusted/device-level fidelity |

`reset` scopes:

| scope | effect | honest reporting |
|---|---|---|
| `task` | recreate the task only | `state_reset:false` on shared tasks; data stays |
| `app` | clear app data and relaunch | `data_cleared:true` |
| `data` | clear app data/caches | `data_cleared:true` |
| `all` | everything the session owns | destroy/recreate semantics |

`destroy` is idempotent: destroying an absent session returns
`destroyed:false, already_absent:true` (or `SESSION_NOT_FOUND` if the engine
cannot distinguish). After destroy, the session id is invalid everywhere.

Isolation is a *claim*, not a RAM setting. If the engine cannot provide the
requested isolation, return `ISOLATION_UNAVAILABLE`/`ISOLATION_UNSATISFIED`
rather than silently downgrading.

---

## 7. Resource budgets

Requests and scenarios may declare:

```json
{"memory_mib": 512, "wall_ms": 60000, "cpu_ms": 30000}
```

* `memory_mib` is the maximum attributable memory for the request/session.
* `wall_ms` and `cpu_ms` are hard limits. Exceeding them is `TIMEOUT`; exhausting
  memory is `RESOURCE_LIMIT` or `RAM_GUARD`/`RAM_ABORT`.
* A budget is never a suggestion: the engine must not silently exceed it.
* `profile` returns a `ResourceReport` with explicitly named metrics
  (`wall_ms`, `cpu_ms`, `peak_rss_bytes`, `memory_mib`, frames/events/steps).
  Do not compare memory numbers with different definitions.

---

## 8. Commands

All requests have a `base` object with identity fields:

```json
{
  "request_id": "req_19",
  "session_id": "s_login",
  "frame_seq": 18,
  "cause_id": "req_18",
  "isolation": "sandbox",
  "display": {"mode": "offscreen", "width": 1080, "height": 2400, "density_dpi": 420},
  "budget": {"memory_mib": 512, "wall_ms": 60000, "cpu_ms": 30000},
  "capabilities": ["ui.text", "input"]
}
```

Only `request_id` is required; the rest are request-specific. The examples below
show the whole request body (the CLI may expose the same fields as flags).

### 8.1 `inspect`

Static APK analysis. Use it first; it never executes code.

```json
{"base": {"request_id": "req_1"}, "apk": "fixtures/build/apps/hello-views.apk", "native_libs": true, "abi_map": true}
```

`result` (abbreviated; the full type is `InspectResult`):

```json
{
  "apk_path": "fixtures/build/apps/hello-views.apk",
  "apk_size_bytes": 12756,
  "zip_entries": 12,
  "package": "dev.acos.fixtures.helloviews",
  "version_code": 1,
  "version_name": "1.0",
  "min_sdk": 26,
  "target_sdk": 34,
  "label": "Hello Views",
  "launcher_activity": "dev.acos.fixtures.helloviews.MainActivity",
  "permissions": [{"name": "android.permission.INTERNET", "max_sdk": null}],
  "components": {"activities": [{"kind": "activity", "name": "dev.acos.fixtures.helloviews.MainActivity", "exported": true}], "services": [], "receivers": [], "providers": []},
  "dex_files": [{"name": "classes.dex", "size": 8000}],
  "native_abis": ["x86_64"],
  "signing": {"v1": false, "v2": true, "v3": false, "v3_1": false, "source_stamp": false, "note": "signature schemes detected only; not cryptographically verified"},
  "resources": {"present": true, "packages": 1, "entries": 9, "type_specs": 4, "global_strings": 12, "types": [{"name": "string", "entries": 4}]}
}
```

Common errors: `APK_NOT_ZIP`, `APK_NO_MANIFEST`, `APK_IO`, `AXML_MALFORMED`,
`ARSC_MALFORMED`, `APK_SIGNING_MALFORMED`.

### 8.2 `run-dex`

Execute a DEX `main` on the ACOS VM (console tier, not an Android app).

```json
{"base": {"request_id": "req_2", "budget": {"wall_ms": 10000}}, "path": "fixtures/build/dexprogs/01_hello/classes.dex", "main": "Main", "args": [], "heap_max_bytes": 268435456, "gc_stress": false}
```

```json
{"exit_code": 0, "stdout": "hello\n", "stderr": "", "uncaught_exception": null}
```

Common errors: `DEX_BAD_*`, `DEX_VERIFY_FAILED`, `DEX_LOAD_ERROR`,
`UNSUPPORTED_BYTECODE`, `UNSUPPORTED_JAVA_API`, `VM_INTERNAL`.

### 8.3 `run`

Launch an app and create a session.

```json
{
  "base": {
    "request_id": "req_3",
    "isolation": "sandbox",
    "display": {"mode": "offscreen", "width": 1080, "height": 2400, "density_dpi": 420},
    "budget": {"memory_mib": 512, "wall_ms": 30000},
    "capabilities": ["lifecycle", "ui.text", "input"]
  },
  "apk": "fixtures/build/apps/hello-views.apk",
  "activity": "dev.acos.fixtures.helloviews/.MainActivity",
  "data_policy": "ephemeral",
  "name": "views"
}
```

```json
{
  "session_id": "s_views",
  "package": "dev.acos.fixtures.helloviews",
  "activity": "dev.acos.fixtures.helloviews/.MainActivity",
  "isolation": "sandbox",
  "display": {"mode": "offscreen", "width": 1080, "height": 2400, "density_dpi": 420},
  "fidelity": "native-runtime",
  "frame": {"frame_seq": 1, "produced_at": "2026-11-10T13:51:14Z", "cause_id": "req_3", "width": 1080, "height": 2400, "density": 420, "content_hash": "…", "meaningful": true}
}
```

Common errors: `WRONG_ABI`, `ISOLATION_UNAVAILABLE`, `ISOLATION_UNSATISFIED`,
`NEEDS_REAL_DEVICE`, `REQUIRES_GMS`, `APK_*`, `APP_CRASHED`, `TIMEOUT`.

### 8.4 `observe`

Read the UI tree/text and frame metadata. In the CLI/MCP JSON the frame is at
`result.frame` (not the top-level `frame`); `result.tree` carries the tree when
`--tree` is set.

```json
{"base": {"request_id": "req_4", "session_id": "s_views"}, "tree": true, "text": true, "include_frame": true, "max_nodes": 200}
```

```json
{
  "session_id": "s_views",
  "package": "dev.acos.fixtures.helloviews",
  "frame": {"frame_seq": 4, "produced_at": "2026-11-10T13:51:15Z", "cause_id": "req_4", "width": 1080, "height": 2400, "density": 420, "content_hash": "…", "meaningful": true},
  "tree": {
    "frame": {"frame_seq": 4, "produced_at": "2026-11-10T13:51:15Z", "cause_id": "req_4", "width": 1080, "height": 2400, "density": 420, "content_hash": "…", "meaningful": true},
    "package": "dev.acos.fixtures.helloviews",
    "truncated": false,
    "text_available": true,
    "nodes": [{"id": "n1", "class": "android.widget.TextView", "resource_id": "dev.acos.fixtures.helloviews:id/result", "text": "clicks: 0", "content_desc": "", "bounds": [42, 612, 175, 663], "visible_bounds": [42, 612, 175, 663], "visible": true, "enabled": true, "focused": false, "clickable": false, "scrollable": false, "checked": false, "children": [], "package": "dev.acos.fixtures.helloviews"}]
  },
  "text": "android.widget.TextView | dev.acos.fixtures.helloviews:id/result | clicks: 0 | [42,612][175,663]"
}
```

Node identity: `id` is tree-local (`n1`), `resource_id` is the Android id. Node
ids are valid only for this tree snapshot.

Common errors: `SESSION_NOT_FOUND`, `NO_DISPLAY`, `NO_TEXT`, `NO_FRESH_FRAME`,
`TIMEOUT`.

### 8.5 `capture`

Capture a fresh frame.

```json
{"base": {"request_id": "req_5", "session_id": "s_views", "frame_seq": 4}, "format": "png", "fresh": true, "after_seq": 4, "after_cause": "req_4", "output": "frames/5.png"}
```

```json
{"frame": {"frame_seq": 5, "produced_at": "2026-11-10T13:51:16Z", "cause_id": "req_5", "width": 1080, "height": 2400, "density": 420, "content_hash": "…", "meaningful": true}, "format": "png", "path": "frames/5.png", "bytes": 42914, "width": 1080, "height": 2400}
```

Common errors: `NO_FRESH_FRAME` (retry/wait), `NO_DISPLAY`, `SESSION_NOT_FOUND`.

### 8.6 `action`

One input action: `tap`, `long_press`, `swipe`, `type`, `key`, `back`, `rotate`,
`wait`.

```json
{"base": {"request_id": "req_6", "session_id": "s_views"}, "action": {"kind": "tap", "selector": {"id": "btn"}}}
```

```json
{"session_id": "s_views", "action": "tap", "accepted": true, "state_changed": true, "input_ack_seq": 9, "frame": {"frame_seq": 9, "produced_at": "2026-11-10T13:51:17Z", "cause_id": "req_6", "width": 1080, "height": 2400, "density": 420, "content_hash": "…", "meaningful": true}}
```

Other action shapes:

```json
{"kind": "type", "selector": {"id": "input"}, "text": "World", "clear": false}
{"kind": "key", "key": "enter"}
{"kind": "swipe", "from": {"x": 540, "y": 1800}, "to": {"x": 540, "y": 600}, "duration_ms": 300}
{"kind": "rotate", "orientation": "landscape"}
{"kind": "wait", "selector": {"id": "result"}, "timeout_ms": 5000}
```

Common errors: `ELEMENT_NOT_FOUND`, `SESSION_NOT_FOUND`, `TIMEOUT`,
`APP_NOT_RESPONDING`, `NO_DISPLAY`, `STALE_REF`.

### 8.6.1 Settle modes: `fast` navigation vs `strict` verification (P1)

`acos_tap`, `acos_text`, `acos_key` and an input `acos_action` accept an
optional `settle` argument (`strict` default, `fast` navigation,
`content` content-stable — see §8.6.4a):

```json
{"base": {"request_id": "req_6", "session_id": "s_views"},
 "id": "inc", "settle": "fast"}
```

* **`strict` (default, event-only)** — the P11 event rule is now the only
  strict rule (P12, owner decision 2026-11-12; the old 150 ms quiet window is
  deleted, and `ACOS_STRICT_SETTLE` is ignored with a one-line warning). The
  guest reports on the UI thread that there is no pending
  traversal/layout/dirty draw on any attached `ViewRootImpl`, no pending
  Choreographer `INPUT`/`ANIMATION`/`TRAVERSAL`/insets callback, no running
  per-view `Animation`/`LayoutTransition`/`ViewPropertyAnimator`, no
  main-Looper message due inside `ACOS_STRICT_SETTLE_HORIZON_MS` (default
  **256 ms**, so the 40 ms and 200 ms `postDelayed` family is covered), and
  stable window/focus/IME state. The sample must have been drawn at/after the
  gesture publish and must stay on the same draw for
  `ACOS_STRICT_SETTLE_CONFIRM_MS` (default **24 ms**) so C1 commits that draw
  before the reply. Replies label `settle.bound:"strict-event"` and add
  `strict_rule:"event"`. Every strict input reply also states
  `settle.pixels_changed:true|false` against the pre-action capture (a
  no-visual-change tap is reported, not failed; `acos verify` records it in the
  step's `settle` evidence). Since the owner decision of 2026-11-12 the
  pixel-unchanged guard is **off by default**: the four conditions above already
  prove the draw is after the action, and requiring different pixels only
  produced false timeouts. `ACOS_STRICT_SETTLE_PIXEL_GUARD=on` restores the P11
  experiment. A tap with **no draw after the action** still times out
  (`reason:"no-draw-after-action"`). On timeout the reply is not settled and
  names the guest's reason (`reason`, plus `animating:true` for frame/animator
  reasons) under `bound:"timeout"` — but it still carries the latest tree and
  frame identity, so the agent can use what is on screen (next bullet and
  §8.6.4). `acos_assert`, `acos verify` assertions and the fast-rule rejection
  are unchanged: a timeout is never a pass. `acos wait`/`wait_for` remain the
  contract for updates scheduled beyond the horizon (for example a 1000 ms
  `postDelayed`): a settle reply is only the state rendered at that moment,
  never a promise about a later message.
* **`fast`** — for navigation and for any look at an animating screen. The
  engine snapshots the tree *before* the input is published, then binds the
  first probe tree that differs from that baseline and the first C1 store
  after that probe sample. A framework animation (for example a Button
  ripple) that keeps storing frames no longer gates the reply, so the returned
  frame **may be mid-animation**. If no tree change is observed within the
  bounded window (`ACOSLAUNCH_FAST_WINDOW_MS`, default 100 ms = two 50 ms
  probe periods), the fast rule's own `quiet-fallback` runs and the reply
  reports `state_changed: false`; fast never claims a change it did not
  observe in the tree. `fast` is immediate and is never assertion evidence.

Every input reply carries a `settle` object (also copied into
`timing.input`):

```json
"settle": {
  "mode": "fast",
  "settled": true,
  "ack_ms": 19,
  "first_change_ms": 24,
  "frame_ms": 39,
  "state_changed": true,
  "bound": "state-commit"
}
```

| field | meaning |
|---|---|
| `mode` | `fast`, `strict` or `content`; the mode this observation was taken with |
| `settled` | `true` only when a settled observation was returned; `false` on a settle timeout (the tree/frame are still present for inspection; `acos verify` fails the step as `TIMEOUT`; see §8.6.4) or the documented no-fresh-frame failure |
| `ack_ms` | publish → this gesture's own `FINISHED` acknowledgment. Focus/touch-mode acks queued at window attach are drained and discarded, never counted |
| `first_change_ms` | publish → the first tree-probe sample that differs from the pre-action baseline (`null` when no change was seen / no baseline) |
| `frame_ms` | publish → the C1 store the reply's frame is bound to (`null` when none) |
| `state_changed` | in `fast` mode, the guest's tree-change verdict (`false` after a fallback); `null` in `strict`, where the host's pixel+tree comparison in `state_changed` on the input result is authoritative |
| `bound` | `state-commit` / `quiet-fallback` (fast), `strict-event` (the only strict rule since P12), or `timeout` |

An input verb can also return the tree in the same round trip:

```json
{"base": {"request_id": "req_6", "session_id": "s_views"},
 "id": "inc", "settle": "fast", "observe": {"tree": true, "png": false}}
```

The reply then carries `tree` (plus `tree_max_nodes`, `tree_node_count`,
`tree_truncated`) from the same settled acos-art reply that produced the ack,
and the usual `frame` reference (`frame_seq`, `md5`, `captured_at`). No extra
traversal is issued. `observe.png:true` is rejected on an input verb: use
`acos_observe` for the PNG.

**Assertions are always strict.** `acos_assert` rejects `settle:"fast"` with
`INVALID_ARGUMENT`; `acos verify` assertions, `assert_screenshot` and
`assert_region` never pass a settle option and always use the strict rule. A
scenario cannot request fast for an assertion step. Acknowledgement is not
success: a fast reply with `ack_ms` is still only a delivered input; the
state change is whatever `settle.state_changed` (fast) or the host pixel+tree
comparison (strict) actually measured.

### 8.6.2 Tree-only observations and `settle` on the tree verb (P3b)

`acos_observe_tree` and `acos_observe` with `tree:true` or `text:true` are
**tree-only**: they do not clone the C1 buffer, encode a PNG, write a PNG, or
compute an md5, and the reply carries no `png`/`md5`. The tree stays bound to a
frame identity through cheap C1 metadata (`buffer_generation`, `frame_number`,
`buffer_id`, `layer_id`, `width`, `height`) plus the usual `frame` reference;
a later screenshot can therefore be checked against the frame the tree was
observed at. `acos_observe {png:true}` is still a real capture and is unchanged.

Both tree verbs accept P1's `settle` argument:

```json
{"base": {"request_id": "req_9", "session_id": "s_views"},
 "settle": "fast"}
```

* **`strict` (default)** — the event rule from §8.6.1 (P12: the 150 ms quiet
  window is deleted). Use it whenever the tree is evidence.
* **`fast`** — for inspection and navigation. It returns the newest probe sample
  whose C1 frame generation is at least the generation current when the request
  began, so a sample from before the request's last frame store is never served.
  If the newest sample predates that store, the host asks the guest for an
  immediate probe (out of its 50 ms cadence) and waits a bounded 750 ms. A fast
  tree may be mid-animation and is never assertion evidence; if no fresh sample
  arrives the reply is `NO_FRESH_TREE`, never the old tree.

A strict tree request uses the event rule from §8.6.1: it waits for a fresh
settled sample after request entry (`bound:"strict-event"`,
`strict_rule:"event"`). With an explicit `settle_timeout_ms` (§8.6.4) the reply
may instead be `settled:false`, `bound:"timeout"` and still carry the latest
tree and frame identity; `acos_assert` refuses such a tree.

Both modes add a `settle` block to the result:

```json
"settle": {"mode": "fast", "settled": true, "probe_seq": 68,
           "tree_age_ms": 2, "bound": "probe-fresh"}
```

| field | meaning |
|---|---|
| `mode` | `fast` or `strict` |
| `settled` | `true` when the returned tree satisfied that mode's rule |
| `probe_seq` | the guest probe sequence of the returned sample |
| `tree_age_ms` | host receipt of the probe → reply |
| `bound` | `probe-fresh` (fast), `strict-event` (strict), or `timeout` (no settled sample; the tree is the latest observed state) |

`acos_assert` still rejects `settle:"fast"` with `INVALID_ARGUMENT`;
`acos verify` assertions remain strict by construction. A fast tree can also be
requested as `acos_observe`'s `settle`; the same labelling applies. The default
for every tree verb remains `strict`, so existing callers see no change.

### 8.6.3 `settle` on `launch` and `reset` (P9)

`acos_launch` and `acos_reset` accept the same `settle` vocabulary, plus a
`pixels` switch:

```json
{"base": {"request_id": "req_1", "session_id": "s_views"},
 "apk": "fixtures/build/apps/counter.apk", "settle": "fast"}
```

* **`strict` (default)** — the launch/reset reply binds its tree with the P12
  event rule (§8.6.1): C1 advanced, a fresh probe sample was drawn at/after the
  request and the framework reported no pending work, and the same draw stayed
  newest for the confirmation window. This is the mode every assertion and
  pixel check runs on. `settle_timeout_ms` (§8.6.4) bounds it; a timeout still
  returns the latest tree with `settled:false`.
* **`content`** — SE2 (§8.6.4a): the reply binds the first traversal, then proves
  the tree content stable for the window. It is tree-only (a content-settled
  frame is never pixel evidence), so `pixels:true` with `settle:"content"` is
  refused with `INVALID_ARGUMENT`.
* **`fast`** — the reply binds the app's **first traversal** from a probe sample
  whose C1 generation is at least the generation current when the request began
  (P3b's freshness rule). No quiet window, no second probe period. A fast launch
  reply may therefore be mid-animation and is **never** assertion evidence: use
  `acos_observe_tree`/`acos_assert` (strict by default) before concluding anything
  from it.

Fast readiness is: the Activity is resumed, the first traversal was drawn, C1
has stored a buffer for it, **and** the probe sample the reply binds is bound to
that store generation. Nothing is inferred from a timer.

`pixels` selects the reply's frame evidence:

| `pixels` | reply `frame` | cost |
|---|---|---|
| **`false` (default)** | `frame_seq`, `buffer_generation`, `frame_number`, `pixels:false`, `identity:"C1 buffer metadata (no pixel copy)"`; `md5` is empty | no clone, no PNG encode/write/md5 |
| `true` | the same identity **plus** `md5` of the captured PNG (P7's launch reply) | ~25 ms |

`settle:"fast"` together with `pixels:true` is refused with `INVALID_ARGUMENT`:
a mid-animation frame can never back a pixel claim. A later
`acos_observe {png:true}` is still a real capture and can be checked against the
`buffer_generation`/`frame_number` the launch reply named.

Both verbs return the same `settle` object the tree verbs return, at the top
level of the result:

```json
"settle": {"mode": "fast", "settled": true, "probe_seq": 1,
           "tree_age_ms": 0, "bound": "probe-fresh"}
```

| field | meaning |
|---|---|
| `mode` | `fast` or `strict` |
| `settled` | `true` when the returned tree satisfied that mode's rule |
| `probe_seq` | the guest probe sequence of the returned sample |
| `tree_age_ms` | host receipt of the probe → reply |
| `bound` | `probe-fresh` (fast), `strict-event` (strict), or `timeout` |

`reset` also returns `tree` (plus `tree_node_count`, `tree_max_nodes`,
`tree_truncated`), the relaunched app's bound tree, so "reset → first correct
screen" is one round trip; a `pixels:false` reset carries the C1 metadata
identity instead of a PNG in exactly the same way.

The default for both verbs is `strict` **and** `pixels:false`, so a launch reply
no longer copies a PNG nobody asked for; pass `pixels:true` to get P7's
pixel-identical launch reply back. Assertions, `acos verify`, scenario
assertions and `assert_screenshot` never pass a settle option: no check is
weakened by any of this.

### 8.6.4 Screenshot any time, animating screens, and `settle_timeout_ms` (P12)

An agent must never be trapped by a screen that animates forever. Three
independent ways out, cheapest first:

1. **Frame-only `acos_observe` / `acos observe --screenshot` is an immediate
   capture.** It issues one `screenshot` command on the held guest and returns
   the newest completed C1 buffer (with `frame_seq`, `md5`,
   `buffer_generation`); it does **not** run a settle rule and does not wait for
   the app to stop drawing. Use it whenever you only need to see the screen.
2. **`settle:"fast"` on a tree verb is immediate.** It binds the newest probe
   sample (and, for launch/reset, the first differing traversal) without a
   settle wait. The frame may be mid-animation, so it is for inspection and
   navigation, never for assertions.
3. **`strict` + `settle_timeout_ms`** when you want the strongest observation
   but a bounded wait. `settle_timeout_ms` is accepted by `acos_launch`,
   `acos_reset`, `acos_observe_tree`, `acos_observe`, `acos_tap`, `acos_text`,
   `acos_key` and `acos_action` (and `split` equivalents), by scenario steps
   (`tap`/`type`/`key`/`launch`/`reset`), and by the CLI as
   `--settle-timeout-ms`. Default: the engine's long-standing deadline, so
   existing callers are unchanged. On expiry the reply is still a real
   observation: `ok:true`, the latest `tree`, the latest frame identity,
   `settle.settled:false`, `settle.bound:"timeout"`, `settle.reason` (the
   guest's own reason, e.g. `anim`, `frame`, `queue`) and
   `settle.animating:true` for animation/frame reasons, plus a `warning` that
   says it is not evidence. **The verifier is stricter than the agent API:** a
   scenario step (`tap`/`type`/`key`/`launch`/`reset`) whose strict settle times
   out fails as `TIMEOUT` (the latest tree/frame and `settle.reason` stay in
   the step evidence), and `verify` assertions never run on an unsettled tree.
   `settle_timeout_ms` moves the deadline; it never converts a transient,
   still-changing screen into a pass. `acos_assert` also refuses unsettled
   trees, while `acos_observe`/`acos_observe_tree` and the frame-only observe
   still return the latest tree/frame for inspection.

```json
{"base": {"request_id": "req_7", "session_id": "s_progress"},
 "settle": "strict", "settle_timeout_ms": 500}
```

On an indeterminate `ProgressBar` (or any forever-animating screen) use
`settle:"fast"` or a frame-only `acos_observe` to see it, and
`settle_timeout_ms` when you need the tree that is on screen without paying the
full default deadline. If the screen is animating for real, `animations:"off"`
(see §8.6.5) is the functional-check answer; do not turn it on for
animation/transition tests.

### 8.6.4a Content-stable settle: the forever-animating screen (SE2, ADR-0026 #3)

Some screens animate forever in a way that does not change what the user can
read: an endless Reanimated/SVG loop behind a form, a spinner beside a heading.
Gaten's welcome/onboarding pages are the reference (GR3 §4.3, backlog G3e: a
10 s looping timeline plus a 200 ms auto-advance retry). On such a screen the
strict event rule never reaches a verdict — it is an honest `TIMEOUT` — and
`fast` is navigation only, so no text could be *proven*.

`settle:"content"` is the opt-in third rule (scenario step or serve verb,
**strict stays the default everywhere**). It is settled when the UI tree
**content signature** is unchanged across fresh traversals spanning at least
**300 ms**, with at least **2 traversals**, inside the step's timeout:

* the signature is a hash over, in visible/tree order, each node's
  `(class, resource_id, text, content_desc, hint, checked, selected, enabled,
  focused, clickable, scrollable, visible, window, package)` plus the child
  structure and the tree's `package`/`truncated`/`text_available`. Nothing else
  is hashed, and no field is silently dropped from that list. A text value that
  an animation keeps updating (a countdown, a progress percentage) therefore
  changes the signature: the screen is *not* content-stable and the step is a
  `TIMEOUT`, which is the correct answer;
* **geometry (`bounds`, `visible_bounds`) is deliberately not hashed.** On the
  screen this rule exists for (ADR-0026 decision 3), an animating view's
  reported rect drifts ~1 px from one fresh traversal to the next while every
  content field is identical, so hashing it would make the window impossible to
  span. The consequence is the other way round: geometry is **never evidence**
  on a content-settled frame. An assertion that names a rectangle fails closed
  with `ASSERTION_FAILED`, `details.reason:"content_settle_geometry"` — in
  *either* position, the assertion's own `bounds` **or** `selector.bounds`
  (which is compared exactly), for `assert_text`, `assert_visible`,
  `assert_not_visible` and `assert_state`. Assert without a rectangle, or settle
  strictly;
* the window is configurable upward with `ACOS_CONTENT_SETTLE_WINDOW_MS`
  (never below 300 ms); the step's `settle_timeout_ms` (default 10 s) bounds
  the wait;
* pixels are **never** consulted. Content stability says nothing about the
  pixels, which may still be animating.

Where it is accepted:

| surface | `settle: content` |
|---|---|
| scenario `tap` / `type` / `key` | yes — the action is delivered, then its tree content is proven stable |
| scenario `launch` | yes — the first tree is bound by the content rule, and its frame is **not** pixel evidence |
| serve/MCP `acos_observe`, `acos_observe_tree`, `acos_launch`, `acos_reset` | yes (tree-only; `pixels:true` with `content` is refused like `fast`) |
| serve/MCP `acos_tap`, `acos_text`, `acos_key`, `acos_action` | yes |
| `acos_assert` | no argument needed: an assertion after a content step evaluates the **content-settled bound tree** (no strict re-traversal, which would only time out) |
| scenario `assert_screenshot` / `assert_region` (pixel/frame claims) | **never** — a pixel assertion bound only by content settle fails closed, see below |
| any assertion naming `bounds` (the assertion's or the selector's) | **never** — geometry is not hashed, so a rectangle claim on a content-settled frame is `ASSERTION_FAILED` / `content_settle_geometry`; assert without a rectangle or settle strictly |
| `wait_for` / `tap` / `type` with `selector.bounds` | allowed: these resolve their selector on a traversal they take themselves (a content input's resolution traversal is proven fresh), not on the bound frame |

Provenance: the step result and every observation it backs records the rule
that bound it. A content settle reply is

```json
"settle": {"mode": "content", "settled": true, "bound": "content-stable",
           "window_ms": 300, "traversals": 3,
           "signature": "0f0f0f0f0f0f0f0f", "frame_seq": 41,
           "elapsed_ms": 812, "pixel_evidence": false}
```

`window_ms`, `traversals` and `signature` are the measured facts; the same
object is written into `steps/<i>-<kind>.json` (`settle`). A timeout carries
`settled:false`, `bound:"content-timeout"`, the last signature and
`reason:"content-not-stable"`, and the step fails `TIMEOUT` like any other
settle timeout.

**Pixel assertions stay honest.** A screenshot comparison, a region pixel check
or any other frame/pixel claim cannot be justified by content stability. In
`acos verify`, `assert_screenshot`/`assert_region` bound to a content-settled
frame fail closed with `ASSERTION_FAILED`, `details.reason:
"content_settle_pixel"`, `details.pixel_evidence: false` and the content
`settle` record; in serve, `pixels:true` together with `settle:"content"` is
`INVALID_ARGUMENT`. A fresh strict `capture` (or any strict step) rebinds the
frame and pixel assertions work again — pixels are settled by pixels.

**Bounds claims stay honest too.** Content stability is text/nodes/state, not
geometry. `assert_text`/`assert_visible`/`assert_not_visible`/`assert_state`
with a `bounds` — inside the assertion *or* inside its `selector` — bound to a
content-settled frame fail closed with `ASSERTION_FAILED`, `details.reason:
"content_settle_geometry"`, `details.geometry_evidence: false` and
`details.bounds_claim.{assertion,selector}`. This is the false-pass class it
closes: a drifting node reports `0,101,…` instead of `0,100,…`, so
`assert_not_visible {selector: {text: "Error", bounds: [0,100,200,140]}}` would
otherwise report "node absent" and pass while the banner is on screen.

`settle:"fast"` is unchanged: it is immediate navigation and never backs a
pass. `expect_change: true` (a pixel claim) with `settle: content` is
`INVALID_SCENARIO`, exactly like `fast`. If a backend cannot supply a content
record for a step that asked for `settle: content`, the step fails closed with
`UNSUPPORTED_ASSERTION`, `details.reason:"content_settle_unhonored"` — the rule
is never silently downgraded to strict/fast.

### 8.6.5 `animations:"on"|"off"` (P12)

Every session has an animation setting, default **`on`** (real Android
behaviour). `off` sets Android's three animation scales
(`animator_duration_scale`, `transition_animation_scale`,
`window_animation_scale`) to `0` — the standard CI setting — before the app's
first traversal:

* Surfaces: `acos launch --animations off|on` (ART session, `--keep`), `acos
  verify --animations off|on` (ART engine), the MCP/serve `acos_launch` field
  `animations:"off"|"on"`, and a scenario's top-level `animations: off`.
* It is **launch-time only** (changing it mid-session would be a different
  device state; destroy and relaunch the session instead).
* Every reply for that session states `animations:"off"|"on"`, and verify's
  `engine` block carries it too, so a functional pass cannot be mistaken for
  animation evidence.
* Mechanism (the real framework's own path): the WMS host answers
  `IWindowManager.getCurrentAnimatorScale()` with the session scale and the
  guest installs it with `ValueAnimator.setDurationScale` — the same two points
  a device's `Settings.Global` observer updates. That covers
  `ValueAnimator`/`ObjectAnimator`/`StateListAnimator` (including the
  framework's ~0.9 s Button press/elevation animator) and the native
  `RenderNodeAnimator` (ripple). ACOS's WMS runs no transition animations of its
  own, so there is no server-side window/Activity transition duration to scale;
  app-side transition animators use the same `ValueAnimator` scale.
* When to use: functional checks (tap → state, forms, assertions) where the
  framework's press/ripple animation only adds latency. When **not** to use:
  anything testing animation, transition, ripple or timing behaviour; those
  need `animations:"on"`.

### 8.6.6 `insets:"carve"|"report"` (W7, default `report` since W7b)

The app-window inset model is a launch-time choice, default **`report`**
(AOSP's phone-like edge-to-edge model; ADR-0026 decision 6). It is selected by
`ACOS_WM_INSETS=carve|report`, by `acos launch --insets carve|report`, by
`acos verify --insets carve|report`, by a scenario's top-level
`insets: carve|report`, or by `acos-art launch --insets carve|report` for
direct runs. A scenario's value is part of its evidence: the verdict states
`insets` so a pass cannot be attributed to the wrong model.

* `carve` — the pre-W7 shim: the WMS gives the app the display's application
  area (`[0,128][1080,2337]` on the API-34 profile) and the reported
  `InsetsState` therefore calculates to zero. Frozen baselines captured against
  the carved frame stay reproducible by pinning `insets: carve`; nothing is
  carved by ambient default any more.
* `report` — AOSP's window model: the app window is the full display
  (`[0,0][1080,2400]`) and the WMS reports the status-bar, navigation-bar and
  system-gesture sources. `PhoneWindow.generateLayout` sends
  `fitInsetsTypes=0` for non-floating windows, so `WindowLayout` does not carve
  the frame; `DecorView` consumes the reported insets for a non-edge-to-edge
  app, and an edge-to-edge app (RN `SafeAreaView`, `setDecorFitsSystemWindows
  (false)`) applies them itself. Floating dialogs keep the default
  `systemBars()` fit and stay inside the application area.
* Geometry comes from the same one source as the rest of the launch:
  `ACOS_ART_*` / `acos-art launch --width/--panel-height/--density/
  --status-inset/--nav-inset`, or `ACOS_DISPLAY_OVERRIDE`. The defaults are the
  API-34 profile (1080x2400@420, status 128, navigation 63).
* Frames under `report` are display-sized: `display_origin` is `[0,0]` (or
  absent, meaning identity) and tree bounds, PNG pixels and tap coordinates all
  share the display space with no status-bar offset.
* Every ART reply states the model the guest was launched with, and a `reset`
  relaunch reuses it. `verify`/`serve` read `ACOS_WM_INSETS`; the MCP
  `acos_launch` field is `"insets": "carve"|"report"`.
* The W7 regressions pin their model: `scenarios/w7-insets-report.yaml`
  (`insets: report`, must pass) and `scenarios/w7-insets-carve-negative.yaml`
  (`insets: carve`, must fail at `status=128` after proving the carved frame).
  This matters because the build default is now `report`: a negative that
  relied on the default would become a false pass.

### 8.7 `assert`

Evaluate one assertion.

```json
{
  "base": {"request_id": "req_7", "session_id": "s_views"},
  "assertion": {"kind": "text", "selector": {"id": "result"}, "text": {"exact": "Hello, World! clicks: 1"}, "bounds": [42, 612, 392, 663]},
  "fresh_frame": true
}
```

```json
{
  "passed": true,
  "result": {"kind": "text", "passed": true, "matched_node_ids": ["n4"], "message": null},
  "frame": {"frame_seq": 12, "produced_at": "2026-11-10T13:51:19Z", "cause_id": "req_7", "width": 1080, "height": 2400, "density": 420, "content_hash": "…", "meaningful": true}
}
```

Failed assertion (note `passed:false` but the command itself ran):

```json
{
  "passed": false,
  "result": {
    "kind": "text",
    "passed": false,
    "diff": {"expected": "clicks: 9", "actual": "clicks: 0", "path": "nodes[3].text", "detail": "exact match failed"},
    "matched_node_ids": ["n4"]
  }
}
```

Assertion kinds: `text`, `visible`, `not_visible`, `state`, `log`, `no_crash`,
`screenshot`. Common errors: `ASSERTION_FAILED`, `ELEMENT_NOT_FOUND`, `NO_TEXT`,
`NO_FRESH_FRAME`.

### 8.8 `events`

Read the append-only event stream.

```json
{"base": {"request_id": "req_8", "session_id": "s_views"}, "since_seq": 3, "kinds": ["lifecycle", "crash", "log"], "limit": 100, "timeout_ms": 1000}
```

```json
{
  "session_id": "s_views",
  "events": [
    {"seq": 4, "timestamp_ms": 1700000000004, "session_id": "s_views", "kind": "log", "tag": "AcosFixture", "level": "info", "message": "onResume"},
    {"seq": 5, "timestamp_ms": 1700000000005, "session_id": "s_views", "kind": "frame", "frame_seq": 4, "cause_id": "req_8", "meaningful": true, "content_hash": "…"}
  ],
  "next_seq": 5,
  "truncated": false
}
```

Pass `next_seq` as `since_seq` on the next call. Common errors:
`SESSION_NOT_FOUND`, `TIMEOUT` (no matching event in the window).

### 8.9 `reset`

```json
{"base": {"request_id": "req_9", "session_id": "s_views"}, "scope": "app"}
```

```json
{"session_id": "s_views", "scope": "app", "state_reset": true, "data_cleared": true, "isolation": "sandbox"}
```

`settle` (`strict` default / `fast`) and `pixels` (default `false`) work as on
`acos_launch` — see §8.6.3. The reply also carries the relaunched app's `tree`
and the `settle` object.

Common errors: `SESSION_NOT_FOUND`, `ISOLATION_UNSATISFIED` (asked to clear data
on a shared task).

### 8.10 `profile`

```json
{"base": {"request_id": "req_10", "session_id": "s_views"}, "memory": true, "cpu": true, "frames": true, "duration_ms": 250}
```

```json
{"session_id": "s_views", "report": {"wall_ms": 250, "cpu_ms": 210, "peak_rss_bytes": 67108864, "memory_mib": 64, "frames_captured": 3, "events": 12, "sessions": 1, "steps": 0}}
```

Common errors: `SESSION_NOT_FOUND`, `RESOURCE_LIMIT`, `TIMEOUT`.

### 8.11 `destroy`

```json
{"base": {"request_id": "req_11", "session_id": "s_views"}}
```

```json
{"session_id": "s_views", "destroyed": true, "already_absent": false}
```

Common errors: `SESSION_NOT_FOUND`.

### 8.12 `verify`

Run an APK + YAML scenario and write the artifacts directory.

```json
{
  "base": {"request_id": "req_12", "budget": {"memory_mib": 512, "wall_ms": 60000}},
  "apk": "fixtures/build/apps/hello-views.apk",
  "scenario": "scenarios/hello-views.yaml",
  "artifacts": "out/hello-views",
  "reference": "fixtures/reference/hello-views/art-api34-x86_64",
  "required_fidelity": "native-runtime",
  "flags": ["--isolation", "process"]
}
```

`result` contains the full `Verdict` (see §10), its path, and the artifact
validation:

```json
{
  "verdict": {"schema_version": 1, "verdict": "pass", "fidelity": "native-runtime", "backend": "acos-native", "isolation": "process", "request_id": "req_12", "started_at": "2026-11-10T13:00:00Z", "finished_at": "2026-11-10T13:00:01Z", "app": {"path": "fixtures/build/apps/hello-views.apk", "package": "dev.acos.fixtures.helloviews", "sha256": "05896b5c…"}, "scenario": {"path": "scenarios/hello-views.yaml", "name": "hello-views", "sha256": "…"}, "steps": [], "resources": {"wall_ms": 1000, "frames_captured": 4, "events": 44, "sessions": 1, "steps": 0}, "reproducer": {"acos_version": "0.1.0", "apk_path": "fixtures/build/apps/hello-views.apk", "apk_sha256": "05896b5c…", "scenario_path": "scenarios/hello-views.yaml", "scenario_sha256": "…", "flags": ["--isolation", "process"], "isolation": "process", "display": {"mode": "offscreen", "width": 1080, "height": 2400, "density_dpi": 420}, "fidelity": "native-runtime", "backend": "acos-native"}, "notes": []},
  "verdict_path": "out/hello-views/verdict.json",
  "artifacts_dir": "out/hello-views",
  "artifact_validation": {"ok": true, "issues": []}
}
```

Common errors: `INVALID_SCENARIO`, `ARTIFACTS_INVALID`, `ASSERTION_FAILED`,
`APP_CRASHED`, `UNSUPPORTED_*`, `UNVERIFIED_FIDELITY`.

### 8.13 `capability-report`

Ask before spending time on a scenario that the engine cannot run.

```json
{"base": {"request_id": "req_13"}, "apk": "fixtures/build/apps/hello-jni.apk", "scenario": "scenarios/hello-jni.yaml", "native_libs": true}
```

```json
{
  "apk": "fixtures/build/apps/hello-jni.apk",
  "package": "dev.acos.fixtures.hellojni",
  "abis": ["arm64-v8a", "x86_64"],
  "min_sdk": 26,
  "target_sdk": 34,
  "checks": [
    {"capability": "jni", "status": "supported", "code": null, "detail": "JNIEnv + RegisterNatives implemented for x86_64", "hint": null},
    {"capability": "ui.text", "status": "supported", "code": null, "detail": "accessibility tree", "hint": null}
  ],
  "all_required_supported": true,
  "fidelity": "native-runtime",
  "status": "pass"
}
```

`status: pass` here means the capability report succeeded and everything needed
is supported. If any required capability is missing, the status is
`unsupported` and the checks carry the exact code/hint.

---

## 9. Scenario format

Top level:

```yaml
version: 1                    # required; only 1 is accepted
name: hello-views             # required, non-empty
description: ...              # optional
app: fixtures/build/apps/hello-views.apk
engine: native-preferred      # native-preferred | native-only | reference-only
fallback: report-and-escalate # report-and-escalate | report-only | forbid
api_level: 34
isolation: process            # shared-task | process | sandbox | core | vm
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
capabilities: [lifecycle, ui.text, input]
budgets: {memory_mib: 512, wall_ms: 60000, cpu_ms: 30000}
expectation: pass             # pass | fail | non_pass
output: {traces: true, ui_tree: true, screenshots: true, resource_metrics: true, logs: true}
steps: [...]
```

A step is a **one-key map**. Valid keys:

| step key | payload |
|---|---|
| `launch` | `activity` / `intent_action` / `data` / `extras` / `clear_state` |
| `wait_for` | `selector`, `timeout_ms`, `require_visible` |
| `tap` | `selector` or `x`+`y`, optional `timeout_ms`, `settle`, `settle_timeout_ms`, `expect_change` |
| `type` | optional `selector`, `text`, `clear`, `settle`, `settle_timeout_ms`, `expect_change` |
| `key` | `key` (`back`, `home`, `enter`, `tab`, `delete`, `escape`, `volume_up`, `volume_down`, `power`, `menu`, `dpad_*`), `settle`, `settle_timeout_ms`, `expect_change` |
| `back` | `{}` or `{expect_change: true}` |
| `rotate` | `orientation: portrait\|landscape` |
| `assert_text` | `selector`, `text` matcher, optional `bounds`, `timeout_ms` |
| `assert_visible` / `assert_not_visible` | `selector`, optional `bounds`, `timeout_ms` |
| `assert_state` | `selector` + at least one of `visible/enabled/focused/clickable/scrollable/checked` |
| `assert_log` | `tag`, `patterns` (regex list), `ordered`, `since_previous_step`, `timeout_ms`, `min_matches` |
| `assert_no_crash` | optional `since_seq` |
| `assert_screenshot` | `baseline`, `tolerance` (0..1), `region_masks` (display pixels) |
| `assert_region` | `x`, `y`, `w`, `h` (**display pixels**), plus `equals_png` / `hash` / `min_distinct_colors` / `max_distinct_colors` |
| `capture` | `name`, `format: png\|raw`, `fresh` |
| `reset` | `scope: task\|app\|data\|all` |
| `set_network` | `offline`, `rules[]` (`method`,`url`,`status`,`headers`,`body`,`delay_ms`,`times`) |
| `set_permission` | `permission`, `state: grant\|deny\|revoke\|reset` |
| `set_locale` | `locale` (`en-US`, `pt_BR`) |
| `set_density` | `density_dpi` (72..1000) |
| `set_theme` | `theme: light\|dark\|system` |
| `set_time` | `timestamp` (RFC 3339 UTC), optional `timezone` |
| `call_activity_method` | optional — only debuggable/source-owned builds with an injected hook |

Explicitly **not** in the format: `sleep`. `wait_for` and assertions are
event-driven. The runtime waits on a matching frame/tree/log event; polling with
fixed delays is non-conformant.

### 9.1 Selectors

`selector` fields (all specified fields are ANDed):

```yaml
selector:
  id: dev.acos.fixtures.helloviews:id/result   # exact; bare `result` matches suffix after `/`
  text: {exact: "clicks: 0"}                   # or {contains: "clicks"} or {regex: "^clicks: [0-9]+$"}
  content_desc: {contains: "Submit"}
  class: android.widget.TextView
  bounds: [42, 612, 175, 663]
  index: 0                                     # 0-based occurrence in document order
```

Matching is case-sensitive. Regexes are unanchored Rust `Regex::is_match`
(anchor explicitly with `^`/`$`). Document order is pre-order depth-first. An
empty selector is invalid. `bounds` are `[left, top, right, bottom]` pixels.

### 9.2 Validation

`validate_scenario` returns **all** errors with JSON paths:

```json
{
  "errors": [
    {"code": "INVALID_SCENARIO", "path": "steps[3].assert_log.patterns[1]", "message": "invalid regex: unclosed group", "hint": "Fix the scenario field at `path`; ..."}
  ],
  "warnings": []
}
```

Parse failures are also `INVALID_SCENARIO` at `$`. Warnings (for example an
optional `call_activity_method`, or a headless display with UI steps) do not
invalidate the scenario.

### 9.3 Coordinate systems (W58)

ACOS has **two** pixel spaces and every coordinate in the product belongs to
exactly one of them. Both are absolute; there is no window-local space in the
agent API.

| space | origin | used by |
|---|---|---|
| **display** | the logical display's top-left, `[0,0]`; `display: {width, height}` in the scenario is its size (1080x2400 on the API-34 reference) | UI-tree bounds (`assert_text.bounds`, selectors with `bounds`), `tap` `x`/`y`, `assert_region` `x`/`y`/`w`/`h`, `assert_screenshot.region_masks` |
| **frame** | the captured frame's pixel `(0,0)`; the PNG is `width` x `height` | the PNG bytes themselves, region `hash`es, `equals_png` baselines, `screenshot` comparison |

Display space is Android's `View.getLocationOnScreen` / `uiautomator dump` /
`adb shell input tap` space: an agent that reads a tree bound can tap it and
assert it without any arithmetic.

A frame is **not necessarily the whole display**. The ART runtime composites
the display's application area (`[0,128][1080,2337]` on the reference), so the
PNG excludes the status bar and the navigation bar. Every frame's metadata says
where it sits:

```json
"frame": {"frame_seq": 34, "width": 1080, "height": 2209,
          "display_origin": [0, 128], "display_size": [1080, 2400]}
```

`display_origin` is the display coordinate of the PNG's pixel `(0,0)`, so
`png_y = display_y - display_origin[1]` (128 px here). A frame that covers the
whole display omits `display_origin`, which means the identity mapping (the
AOSP `screencap` case: the reference replay backend serves full-display frames).
`display_size` is the space tree bounds are in.

Rules the runner enforces:

* `assert_region` takes **display** pixels and converts internally with
  `display_origin`. Writing a PNG pixel there is off by the status-bar inset and
  the assertion **fails** (loudly, with both rectangles in the diff), so the
  mistake can never become a false pass.
* A region the frame does not cover — the status bar band above it or the
  navigation bar band below it — fails with the frame geometry in the detail.
  A frame cannot prove pixels it does not contain.
* `region_masks` are display pixels too; a mask outside the frame excludes
  nothing (it is dropped) and one that straddles the frame edge is clipped.
* A passing `verify` run repeats this mapping in `verdict.notes`, naming the
  concrete `display_origin` for the run.

`acos serve` replies carry the same fields on the `frame` object of
`launch`/`observe`/`observe_tree`/`capture` (the patch is recorded in
`results/art-runtime/W58.md` §7; the daemon is W55's file).

### 9.4 Expectations and false-pass prevention

`expectation` is how a negative test proves the runner is honest:

* `pass`: the verdict must be `pass`.
* `fail`: the verdict must be exactly `fail`. `timeout`, `crash`, `error` and
  `unsupported` do **not** satisfy a `fail` expectation.
* `non_pass`: any non-pass verdict is acceptable.

If a `scenarios/negative/*.yaml` file passes, the runner is broken; report a
harness failure, never a successful test.

### 9.5 `expect_change`: prove that an action changed the screen (SE1)

Strict settle does **not** require the pixels to change (ADR-0026 decision 2,
the P12 rule): an action that draws but leaves the screen pixel-identical still
settles, passes, and reports the fact as `settle.pixels_changed: false` in the
step's `settle` evidence. That is deliberate - a tap that only moves focus is
not a failure.

The trap it leaves open is a **vacuous flow**: a tap on a control that is
already in the state the scenario asserts writes nothing, and every following
assertion then holds on state the tap never produced (GR4 Backlog `G4c`: a
Gaten `gaten-kanji` draft was 3/3 while its tap was a no-op).

`expect_change: true` is the per-step opt-in that closes it. It is accepted on
the input steps `tap`, `type`, `key` and `back`, and the rule is:

```yaml
steps:
  - tap: {selector: {id: inc}, expect_change: true}   # must change the screen
```

* after the strict settle, `settle.pixels_changed: true` passes as usual;
* `settle.pixels_changed: false` fails the step with `ASSERTION_FAILED`,
  `details.reason: "expect_change"`, `details.pixels_changed: false`,
  `details.step_kind`, and `details.before` / `details.after` frame refs
  (`frame_seq`, plus `frame: frames/<seq>.png` when that frame was captured);
* a reply with **no** measured `pixels_changed` (a backend without the evidence,
  or a fast frame) is `UNSUPPORTED_ASSERTION` - a pixel claim that cannot be
  measured never passes;
* `expect_change: true` + `settle: fast` is rejected at parse time
  (`INVALID_SCENARIO` at `steps[i].<kind>.expect_change`): a fast frame may be
  mid-animation and is never verification evidence;
* the flag is a boolean; any other YAML type is a parse error.

Absent (the default `false`), nothing changes: the P12 rule, every existing
scenario and every settle reply are untouched.

A step that provokes no **draw** at all does not settle, so it is a `TIMEOUT`
(`reason: "no-draw-after-action"`), not an `expect_change` failure; both are
non-passes. The same flag is available per action on the serve/MCP input tools
(`acos_tap`, `acos_key`, `acos_text`) as `expect_change: true`.

---

## 10. Artifacts and `verdict.json`

`verify --artifacts out/` writes:

```text
out/
  run.json                  # who owns this directory (request_id + started_at)
  verdict.json
  steps/00-launch.json
  steps/01-wait_for.json
  steps/02-assert_text.json
  frames/<frame_seq>.png
  frames/<frame_seq>-<capture-name>.png   # named copy for every `capture` step
  tree/<frame_seq>.json
  logs/logcat.txt
  events.jsonl              # one JSON Event per line, increasing seq
  resources.json            # ResourceReport
  repro.sh
  repro.json                # Reproducer
  retention.json            # what retention kept/pruned (§10.2)
  art-frames/               # engine side: live-frames/, live-session.json, session-exit.json
```

`steps/NN-<kind>.json` uses a two-digit index and the step kind
(`03-assert_text.json`). Artifact paths in the verdict are **relative** and must
not contain `..`.

`run.json` is written **before** the first step and names the run that owns the
directory (`request_id`). See §10.4 for the ownership rule.

`verdict.json`:

```json
{
  "schema_version": 1,
  "verdict": "pass",
  "status": "pass",
  "fidelity": "native-runtime",
  "required_fidelity": "native-runtime",
  "backend": "acos-native",
  "isolation": "process",
  "request_id": "req_verify",
  "session_id": "s_views",
  "started_at": "2026-11-10T13:00:00Z",
  "finished_at": "2026-11-10T13:00:01.250Z",
  "app": {
    "path": "fixtures/build/apps/hello-views.apk",
    "package": "dev.acos.fixtures.helloviews",
    "sha256": "05896b5cd534b0857c13ca5baec8b131c905b3a908bbbaad4ee05db9f21b007d"
  },
  "scenario": {
    "path": "scenarios/hello-views.yaml",
    "name": "hello-views",
    "sha256": "6d1f…"
  },
  "steps": [
    {
      "index": 0,
      "step": "launch",
      "status": "pass",
      "duration_ms": 212,
      "frame_seq": 1,
      "assertions": [],
      "evidence": [
        {"kind": "frame", "path": "frames/1.png"},
        {"kind": "tree", "path": "tree/1.json"}
      ]
    },
    {
      "index": 2,
      "step": "assert_text",
      "status": "pass",
      "duration_ms": 4,
      "frame_seq": 2,
      "assertions": [
        {
          "kind": "text",
          "passed": true,
          "matched_node_ids": ["n4"],
          "expected": {"selector": {"id": "result"}, "text": {"exact": "Hello, World! clicks: 0"}, "bounds": [42, 612, 392, 663]},
          "actual": {"node": "n4", "text": "Hello, World! clicks: 0", "hint": "", "bounds": [42, 612, 392, 663], "visible": true}
        }
      ],
      "evidence": [
        {"kind": "frame", "path": "frames/2.png"},
        {"kind": "tree", "path": "tree/2.json"}
      ]
    },
    {
      "index": 3,
      "step": "capture",
      "status": "pass",
      "duration_ms": 9,
      "frame_seq": 3,
      "name": "meanings-collapsed",
      "assertions": [],
      "evidence": [
        {"kind": "frame", "path": "frames/3.png"},
        {"kind": "capture", "path": "frames/3-meanings-collapsed.png"},
        {"kind": "tree", "path": "tree/3.json"}
      ]
    }
  ],
  "captures": {"meanings-collapsed": "frames/3-meanings-collapsed.png"},
  "failure": null,
  "resources": {"wall_ms": 1250, "cpu_ms": 980, "peak_rss_bytes": 75497472, "memory_mib": 72, "frames_captured": 4, "events": 44, "sessions": 1, "steps": 24},
  "reproducer": {
    "acos_version": "0.1.0",
    "apk_path": "fixtures/build/apps/hello-views.apk",
    "apk_sha256": "05896b5cd534b0857c13ca5baec8b131c905b3a908bbbaad4ee05db9f21b007d",
    "scenario_path": "scenarios/hello-views.yaml",
    "scenario_sha256": "6d1f…",
    "flags": ["--isolation", "process"],
    "isolation": "process",
    "display": {"mode": "offscreen", "width": 1080, "height": 2400, "density_dpi": 420},
    "fidelity": "native-runtime",
    "backend": "acos-native"
  },
  "notes": []
}
```

`status` and `verdict` carry the same answer in the envelope's vocabulary
(`pass`, `fail`, `unsupported`, `blocked`, `error`, `timeout`, `crash`).
`verdict` is the historical field; `status` is the one every other ACOS reply
uses, so a `jq .status` check works on `verdict.json` too. They are validated to
agree (a mismatch is `ARTIFACTS_INVALID`).

Every assertion result echoes **`expected` and `actual` on pass as well as on
failure**: the selector, matcher, bounds and flags that were asserted, and the
node text/hint/bounds/flags that were seen. A reviewer can therefore prove *what*
was checked from the artifacts alone, not only that it held. On failure the same
data is also in `diff` (with `path` and a human `detail`).

A `capture` step's own `name` is persisted in three places: the step JSON
(`"name"`), the PNG file name (`frames/<frame_seq>-<name>.png`, sanitised to
`[A-Za-z0-9._-]`, max 64 chars, with the frame sequence kept for uniqueness) and
`verdict.json`'s `captures` map (`{name: path}`). The canonical
`frames/<frame_seq>.png` is still written (it is what `frame_seq` names); the
named copy is the same bytes under a second name.

A non-pass verdict has `failure` populated:

```json
{
  "failure": {
    "code": "ASSERTION_FAILED",
    "step_index": 12,
    "summary": "result text mismatch",
    "error": {"code": "ASSERTION_FAILED", "message": "expected `Hello, World! clicks: 1`, got `Hello, World! clicks: 2`", "hint": "Compare expected vs actual in the assertion diff, ...", "retryable": false}
  }
}
```

### 10.1 Validity rule

`validate_artifacts_dir(path)` (in `acos-protocol`) enforces:

* `verdict.json` exists, parses, and has `schema_version: 1`; `status` and
  `verdict` agree.
* Reproducer hashes are non-empty and match `app.sha256` / `scenario.sha256`;
  flags are sorted and de-duplicated.
* For a claimed **pass**:
  * there is at least one step and every step is `pass` with no `error`;
  * every passing step declares at least one evidence artifact;
  * every declared evidence path exists, is relative, does not escape the
    artifacts directory, and is not a symlink escape;
  * every `frame_seq` has `frames/<seq>.png` and `tree/<seq>.json`;
  * `steps/NN-<kind>.json` exists and parses for every step;
  * `events.jsonl`, `resources.json`, `repro.json`, `repro.sh` exist;
    `events.jsonl` lines and `resources.json`/`repro.json` parse;
  * every `captures` path exists, and every step that carries a `name` has a
    `captures` entry (a name without a file is a dangling declaration).
* A missing artifact for a claimed pass makes the verdict **invalid**
  (`ARTIFACTS_INVALID`), even if the JSON says `pass`.

Non-pass verdicts may omit heavy evidence, but any declared path must still be
safe and existing.

`repro.json` + `repro.sh` are the deterministic reproduction block: APK SHA-256,
scenario SHA-256, ACOS version, flags, isolation, display, fidelity and backend.
Regenerate rather than hand-edit.

### 10.2 Retention: bounded artifacts (`--keep-artifacts`, `ACOS_KEEP_ARTIFACTS`)

A real ART run captures ~10 MiB per frame, so a fifteen-step scenario used to
write 0.5–2 GB: the mirrored raw captures under `art-frames/live-frames/` plus
`frames/` PNGs no step ever declared. Retention bounds a run without losing the
evidence a verdict depends on. The rule is:

| run status | `all` | `referenced` (default) | `none-on-pass` |
|---|---|---|---|
| `pass` | everything | declared evidence + first and last frame | reports, trees, logs — no frames |
| `fail`, `timeout`, `error`, `unsupported`, `crash`, `blocked` | everything | **everything** | **everything** |

* "Declared evidence" is what `verdict.json` and `steps/NN-*.json` name: every
  step's `frame_seq` (`frames/<seq>.png`, `tree/<seq>.json`), every file in
  `evidence[]`, `logs/logcat.txt`, the step documents, the reports.
* Nothing a retained JSON names is ever deleted: before removing a candidate the
  runner scans every retained JSON document (and the serialized verdict) for the
  path; a hit keeps the file. `retention.json` then lists it under
  `kept_because_referenced`.
* `art-frames/live-session.json` and `art-frames/session-exit.json` are written at
  session shutdown, after the pruning pass. A `png` field whose capture was
  pruned becomes `"png_pruned": true` and the document gains a `retention`
  block, so no session record points at a missing file.
* Retained PNGs are re-encoded **losslessly** (`png` crate, real deflate, Sub
  filtering). The ART capture path writes zlib *stored* blocks — an unfiltered
  raw buffer in a PNG wrapper — so the same pixels take ~10× less space as
  evidence. The frames a gate reads are unchanged in content; only the
  compression is.
* `art-frames/live-frames/` keeps only its first and last capture (session entry
  and exit state) on a pass; the intermediate duplicates of `frames/` are gone.
* `retention.json` records `mode`, `decision`, `kept_files`/`kept_bytes`,
  `pruned_files`/`pruned_bytes`, `frames_reencoded`/`reencode_saved_bytes`, the
  pruned list (a deletion receipt, capped at 200 entries) and any
  `kept_because_referenced`. The verdict's `notes` also summarise it.

Measured on this host (API 34, 1080×2400): `v2-art-counter` 109.0 → 0.3 MiB
(−99.7 %), `gaten-home` 195.3 → 8.8 MiB (−95.5 %), a failing Gaten negative
131.8 → 131.8 MiB (unchanged, by design). `tools/check-artifact-references.py`
proves no report dangles after retention.

### 10.3 Pruning the collection: `acos artifacts gc`

Retention bounds one run; `gc` bounds the collection of runs under an artifacts
root:

```bash
acos artifacts gc [--dir <out>] [--older-than <dur>] [--keep-last <n>] [--dry-run] [--json]
```

* `--dir` defaults to `$ACOS_ARTIFACTS_DIR`, else `out`.
* A run directory is any directory containing `verdict.json` (nested scenario
  paths such as `out/suite/T03-render-good/` included).
* `--keep-last` (default 1) never prunes the newest N run directories **per
  scenario** (scenario = `verdict.scenario.name`).
* A **non-pass** run younger than `--older-than` (default `24h`) is never
  touched, and neither is a run whose verdict cannot be read.
* `--dry-run` prints the same freed-byte figure and removes nothing. The text
  output ends with `N run(s) pruned, M kept, X freed`; `--json` returns the same
  as the per-run list.

### 10.4 One run owns its artifacts directory (`--overwrite`)

Before this rule, a second `acos verify --artifacts <same dir>` appended to the
first run's `steps/` and `frames/`; a frame number from the first run could be
mistaken for the second run's evidence. That is the stale-evidence failure mode
an agent must never ship, so:

* A run **claims** its directory before writing anything (`run.json` names the
  owning `request_id`).
* A non-empty directory written by another run is refused with
  **`INVALID_ARGUMENT`**. The message and `details` name the previous run id
  (`details.previous_run_id`, `details.artifacts`, `details.run_id`), and the
  `hint` says to use a fresh directory or `--overwrite`.
* `--overwrite` (MCP `acos_verify {"overwrite": true}`) is the explicit consent:
  the existing contents are removed first, then the new run writes its own. The
  new `run.json` records `overwrote_run_id`, and the verdict carries
  `artifacts_overwrote_run` plus a note.
* The **default** directory (no `--artifacts`: `out/<scenario stem>`) belongs to
  the scenario, so a rerun replaces the previous run there exactly as
  `--overwrite` would (recorded in `run.json` / the verdict). Only a directory
  you name explicitly is refused.
* An **empty** directory (for example one a runner just `mkdir`-ed) is claimed
  without complaint. A partially written directory from a crashed run is not
  fresh either: it is refused as "an incomplete earlier run" (no run id to name).
* `repro.sh` therefore takes the replay's artifacts directory as its required
  first argument (`sh repro.sh /path/to/new-dir`) and never writes back into the
  directory it came from.

The check is in the CLI entry point, so every front door shares it: the CLI,
`acos serve`'s delegated `verify`, and MCP `acos_verify` (which runs
`acos verify`). A refused run writes nothing and leaves the previous evidence
byte-identical.

---

## 11. MCP tools

`acos_protocol::mcp_tools()` returns the MCP `tools/list` payload as data; the
server is not in the protocol crate. One tool per command:

| tool | command |
|---|---|
| `acos_inspect` | `inspect` |
| `acos_run_dex` | `run-dex` |
| `acos_run` | `run` |
| `acos_observe` | `observe` |
| `acos_capture` | `capture` |
| `acos_action` | `action` |
| `acos_assert` | `assert` |
| `acos_events` | `events` |
| `acos_reset` | `reset` |
| `acos_profile` | `profile` |
| `acos_destroy` | `destroy` |
| `acos_verify` | `verify` |
| `acos_capability_report` | `capability-report` |

Each descriptor has `name`, an agent-facing `description` (when to use, what it
returns, common errors) and `inputSchema` generated from the request type.

The default `acos-mcp` server adds six server-local session tools
(`acos_launch`, `acos_tap`, `acos_key`, `acos_swipe`, `acos_logs`, `acos_status`)
served in-process by `SessionBackend`, plus the `acos_capabilities` and
`acos_help` meta tools. See §18.6 for their contract.

---

## 12. Error code table

`retryable` means retrying the same request after addressing the hint may
succeed. It never means the result is a pass.

| code | retryable | hint (what to do next) |
|---|---|---|
| `UNSUPPORTED_ANDROID_API` | `no` | Inspect the exact API in details.api/details.at; avoid it, guard it with a version/feature check, or mark the task unsupported. Never count it as a pass. |
| `UNSUPPORTED_JAVA_API` | `no` | Read details.api/details.at; replace it with an implemented API, or declare the task unsupported and (if allowed) escalate to the reference backend. |
| `UNSUPPORTED_BYTECODE` | `no` | Read details.opcode/details.at; the VM cannot execute this bytecode yet. Reduce the code feature or report the exact opcode as a coverage gap. |
| `UNSUPPORTED_NATIVE_SYMBOL` | `no` | Read details.symbol/details.library; implement/import the symbol, rebuild the .so for the supported ABI, or report the exact missing symbol. Do not fake a return value. |
| `NEEDS_REAL_DEVICE` | `no` | This behaviour requires a real/OEM device or hardware. Report it as unsupported; do not claim a native or simulated pass. |
| `REQUIRES_GMS` | `no` | The app depends on licensed Google Mobile Services. Use a GMS-enabled reference target or report unsupported. |
| `WRONG_ABI` | `no` | The loader cannot map this individual ELF (`e_machine`/class mismatch). Use a library built for the engine ABI. |
| `UNSUPPORTED_ABI` | `no` | The APK ships native code but no `lib/<host-abi>/`. Supply an ABI split that includes x86_64, or do not request native loading. |
| `NO_DISPLAY` | `no` | Create the session with a display (offscreen:WxH) before capture/observe, or drop the step for a headless logic-only scenario. |
| `NO_TEXT` | `no` | The chosen route has no text. Enable the text/semantics capability or use structural selectors (id/class/bounds) instead. |
| `UNSUPPORTED_RELOCATION` | `no` | Report the relocation type and library; use a build without it or extend the loader. Do not skip the load. |
| `RENDER_UNSUPPORTED_OP` | `no` | Report the draw operation; simplify the view, or accept a lower fidelity only if the verdict says so explicitly. |
| `UNSUPPORTED_BACKEND` | `no` | Choose a backend listed by capability-report; do not silently fall back. If escalation is allowed, record the backend actually used. |
| `NO_FRESH_FRAME` | `yes` | Do not reuse an older capture. Wait for a new frame after the action, or request capture with the correct after_seq/cause_id, then retry. |
| `UNVERIFIED_FIDELITY` | `no` | Evidence is not tied to the required fidelity (for example a JVM result for an ART requirement). Re-run on a qualifying backend or report unsupported. |
| `APK_NOT_ZIP` | `no` | Verify the path is a readable APK/zip; rebuild the APK. This is not an app-behaviour failure. |
| `APK_NO_MANIFEST` | `no` | The APK has no AndroidManifest.xml. Rebuild it with aapt2/gradle; do not attempt to run it. |
| `APK_IO` | `yes` | Check the APK path and permissions; this is a host I/O failure, not an app failure. |
| `APK_ENTRY` | `no` | A zip entry is corrupt; rebuild the APK and retry inspect. |
| `APK_SIGNING_MALFORMED` | `no` | The signing block is malformed. Re-sign the APK; if this is a third-party APK, report it instead of installing it. |
| `AXML_MALFORMED` | `no` | The binary manifest is malformed. Rebuild with aapt2; if third-party, record the parse gap. |
| `ARSC_MALFORMED` | `no` | The resource table is malformed. Rebuild resources; use --no-resources only if the scenario does not need them. |
| `DEX_BAD_MAGIC` | `no` | Verify the file starts with `dex\n`; pass a .dex/.apk, not a jar/class file. |
| `DEX_UNSUPPORTED_VERSION` | `no` | Recompile for a supported DEX version (035-039) or extend the parser. Do not treat it as executable. |
| `DEX_BAD_CHECKSUM` | `no` | Rebuild or re-sign the DEX; decide explicitly whether checksum enforcement is enabled (ADR-0017). |
| `DEX_BAD_SIGNATURE` | `no` | Rebuild the DEX; SHA-1 enforcement is on by default for Android semantics. |
| `DEX_TRUNCATED` | `no` | The DEX is incomplete; rebuild it and retry. |
| `DEX_BAD_ENDIAN` | `no` | Recompile the DEX for little-endian reverse-endian layout. |
| `DEX_INDEX_OUT_OF_RANGE` | `no` | The DEX is corrupt; rebuild it. Report the offset if it came from a third party. |
| `DEX_BAD_INSTRUCTION` | `no` | The DEX instruction stream is malformed; rebuild and re-run verification. |
| `DEX_UNUSED_OPCODE` | `no` | The DEX uses an unused opcode; rebuild with a standard d8 version. |
| `DEX_BAD_LEB128` | `no` | The DEX encoding is corrupt; rebuild it. |
| `DEX_BAD_MUTF8` | `no` | A DEX string is corrupt; rebuild it. |
| `DEX_BAD_ENCODED_VALUE` | `no` | A DEX encoded value is corrupt; rebuild it. |
| `DEX_BAD_CLASS_DATA` | `no` | The DEX class data is corrupt; rebuild it. |
| `DEX_BAD_CODE_ITEM` | `no` | The DEX code item is corrupt; rebuild it. |
| `DEX_BAD_DEBUG_INFO` | `no` | Debug info is corrupt; rebuild with or without debug info and retry. |
| `DEX_BAD_ANNOTATION` | `no` | An annotation is corrupt; rebuild the DEX. |
| `DEX_BAD_MAP` | `no` | The DEX map is corrupt; rebuild the DEX. |
| `DEX_BAD_HEADER` | `no` | The DEX header is corrupt; rebuild the DEX. |
| `DEX_VERIFY_FAILED` | `no` | Read the verifier reason; fix the source or bytecode, or record the exact verification gap. Do not execute unverified code as a pass. |
| `DEX_OVERFLOW` | `no` | The DEX sizes overflow; rebuild it or report the exact structure. |
| `DEX_LOAD_ERROR` | `no` | Read details; ensure the DEX is on the classpath and the main class exists. This is a load failure, not an assertion failure. |
| `ELF_MALFORMED` | `no` | The .so is malformed; rebuild it with a supported NDK/sysroot. |
| `NATIVE_DEP_NOT_FOUND` | `no` | Add the missing dependency to the APK's lib/<abi>/ set or report it. |
| `NATIVE_INIT_FAILED` | `no` | Read the initializer failure; fix the JNI_OnLoad/constructor, or report unsupported. |
| `NATIVE_IO_ERROR` | `yes` | Check file permissions and paths for the native library. |
| `NATIVE_SYMBOL_NOT_FOUND` | `no` | Check the symbol name/version; rebuild the library or fix the JNI declaration. |
| `NATIVE_INVALID_ARGUMENT` | `no` | Fix the native-loader request arguments. |
| `VM_INTERNAL` | `no` | This is an ACOS bug, not an app bug. Capture the request, scenario, events and stack; do not retry blindly; report it. |
| `JAVA_EXCEPTION` | `no` | An exception escaped. Inspect exception/stack, fix the app (or the missing platform behaviour), and rerun. |
| `APP_CRASHED` | `no` | Read crash.exception/stack and the last events. Fix the app or report the unsupported platform call; a crash is never a pass. |
| `APP_NOT_RESPONDING` | `yes` | The main thread is blocked. Inspect the ANR stack and recent events; fix the deadlock/long task, then rerun. |
| `ASSERTION_FAILED` | `no` | Compare expected vs actual in the assertion diff, inspect the linked frame/tree, then fix the app or the expectation. Rerun the scenario. |
| `ELEMENT_NOT_FOUND` | `no` | Re-observe the tree and check the selector (id/text/class/bounds). If the node is legitimately absent, fix the scenario expectation. |
| `STALE_REF` | `no` | Re-run observe/assert to obtain fresh node ids; never reuse node refs across a mutation. |
| `SESSION_NOT_FOUND` | `no` | Create a session with run first; list/keep the session id. Destroying a session invalidates it. |
| `SESSION_BUSY` | `yes` | Wait for the in-flight request to finish or use a separate session. Do not issue concurrent mutations on one session. |
| `ISOLATION_UNAVAILABLE` | `no` | Choose an isolation level this app/engine supports (shared-task < process < sandbox < core < vm), or report the missing guarantee. |
| `ISOLATION_UNSATISFIED` | `no` | The requested isolation is not actually guaranteed. Do not claim data independence; escalate or report unsupported. |
| `RAM_GUARD` | `yes` | Free memory, lower --parallel, or raise the budget within policy; retry after the host has headroom. |
| `RAM_ABORT` | `yes` | Execution was aborted by the RAM guard. Reduce concurrency/memory or run on a larger host; retry is safe. |
| `TIMEOUT` | `yes` | Increase the step budget if the work is legitimate, or fix the hang. Retry after checking events; do not treat timeout as pass. |
| `RESOURCE_LIMIT` | `yes` | A declared budget (memory/CPU/wall) was exhausted. Raise it explicitly or reduce the workload; report the measured value. |
| `INVALID_SCENARIO` | `no` | Read every error path in the validation result and fix the YAML. Do not edit the runtime to accept an invalid scenario. |
| `INVALID_ARGUMENT` | `no` | Fix the named request argument. Read error.details for the field and accepted values. |
| `UNSAFE_PATH` | `no` | Use a path inside the workspace/artifacts directory. Absolute paths, `..` and symlink escapes are rejected. |
| `ARTIFACTS_INVALID` | `no` | The artifacts directory contradicts its verdict. Re-run verify; never accept a pass without frame/tree/step evidence. |
| `RENDER_BAD_HANDLE` | `no` | Do not reuse released render handles; re-observe and retry. |
| `RENDER_FONT_NOT_FOUND` | `no` | Install/point at a font that supports the required script; rerun rendering. |
| `RENDER_INVALID_ARGUMENT` | `no` | Fix the renderer argument; read details. |
| `RENDER_SIZE` | `no` | Use a valid display size (non-zero, within the budget) or fix the surface request. |
| `RENDER_DECODE` | `no` | The image/font decode failed; replace the asset or use a supported format. |
| `RENDER_IO` | `yes` | Check the render output path and permissions. |
| `RENDER_STRICT` | `no` | Strict rendering rejected a fidelity loss. Fix the unsupported draw operation or relax strict mode explicitly. |
| `INTERNAL` | `no` | ACOS harness failure, not app behaviour. Capture request/scenario/artifacts and report it; do not count it as a pass. |

---

## 13. What a coding agent should do on each error code

Use the table above per code. The playbooks:

**Engine cannot do it (`unsupported`)** — `UNSUPPORTED_ANDROID_API`,
`UNSUPPORTED_JAVA_API`, `UNSUPPORTED_BYTECODE`, `UNSUPPORTED_NATIVE_SYMBOL`,
`UNSUPPORTED_RELOCATION`, `NEEDS_REAL_DEVICE`, `REQUIRES_GMS`, `WRONG_ABI`,
`NO_DISPLAY`, `NO_TEXT`, `UNSUPPORTED_BACKEND`, `RENDER_UNSUPPORTED_OP`.
Do not rewrite the app to hide the gap. Read `details`, decide whether the task
can be done at a different fidelity/target, and report unsupported if not.
**Never record this as pass or as an app bug.**

**The app is wrong (`fail`)** — `ASSERTION_FAILED`, `ELEMENT_NOT_FOUND`.
Read the assertion diff (`expected`, `actual`, `path`), inspect the linked
`frames/<seq>.png` and `tree/<seq>.json`, then change the app or the scenario
expectation. Rerun. `ELEMENT_NOT_FOUND` often means the selector is stale; call
`observe` again before editing code.

**The app crashed or hung** — `APP_CRASHED`, `APP_NOT_RESPONDING`, `JAVA_EXCEPTION`.
Read the crash/ANR event (`exception`, `message`, `stack`), look at the last
actions and frames, fix the app or the unsupported platform call. A crash and a
timeout are **not** assertion failures; do not "fix" the scenario to skip them.

**Bad build artifact** — `APK_NOT_ZIP`, `APK_NO_MANIFEST`, `APK_IO`,
`APK_ENTRY`, `APK_SIGNING_MALFORMED`, `AXML_MALFORMED`, `ARSC_MALFORMED`,
`DEX_BAD_*`, `DEX_TRUNCATED`, `DEX_VERIFY_FAILED`, `DEX_LOAD_ERROR`,
`ELF_MALFORMED`, `NATIVE_DEP_NOT_FOUND`, `NATIVE_INIT_FAILED`,
`NATIVE_IO_ERROR`, `NATIVE_SYMBOL_NOT_FOUND`, `NATIVE_INVALID_ARGUMENT`.
Rebuild/re-sign/re-package and retry `inspect` first. These are host/artifact
problems, not app-behaviour verdicts.

**Session / resource problems** — `SESSION_NOT_FOUND`, `SESSION_BUSY`,
`STALE_REF`, `TIMEOUT`, `RESOURCE_LIMIT`, `RAM_GUARD`, `RAM_ABORT`.
Create the session first, serialize mutations on one session, re-observe after
mutations, and reduce concurrency or raise an explicit budget. `TIMEOUT` is not
a pass.

**Evidence / fidelity problems** — `NO_FRESH_FRAME`, `UNVERIFIED_FIDELITY`,
`ARTIFACTS_INVALID`, `UNSAFE_PATH`. Wait for a new frame after the action, use
`after_seq`/`after_cause`, re-run on a qualifying fidelity, and write artifacts
inside the artifacts directory. If the verdict says pass but the evidence is
missing, the pass is invalid: rerun, do not patch `verdict.json`.

**Invalid input** — `INVALID_SCENARIO`, `INVALID_ARGUMENT`, `ISOLATION_UNAVAILABLE`,
`ISOLATION_UNSATISFIED`. Fix every path reported by scenario validation; fix the
named request argument; choose an isolation level the app/engine supports or
report the missing guarantee.

**Harness failure** — `VM_INTERNAL`, `INTERNAL`, `RENDER_*`, `RENDER_STRICT`.
This is an ACOS bug or a strict-rendering rejection, not an app verdict. Capture
the request, scenario, events and stack; do not retry blindly or claim pass.

---

## 14. Contract decisions runtime crates must implement exactly

1. **Envelope construction.** Use `Envelope::pass`/`Envelope::failure` or the
   validating `Deserialize`. Never emit `ok:true` with a non-pass status, and
   never map an `UNSUPPORTED_*` code to `error` or `fail`.
2. **One error taxonomy.** Use `ErrorCode`; every code emitted by `acos-apk`,
   `acos-dex`, `acos-vm`, `acos-native-loader` and `acos-render` is already in
   the enum (see `EXISTING_CRATE_CODES`). Do not invent a second string set.
   Add `details` for the exact API/symbol/opcode/path.
3. **Statuses are exhaustive.** `pass | fail | error | unsupported | crash |
   timeout | blocked`. `blocked` is for preconditions (busy session, resource
   guard, unavailable isolation); `unsupported` is for "the engine cannot do it".
4. **Fidelity is honest.** `native-runtime` means ACOS executed it and it is not
   ART-validated. Use `validated_against` when an oracle checked it. A JVM pass
   never satisfies an ART requirement.
5. **Frame fields.** `frame_seq`, `produced_at`, `cause_id`, `width`, `height`,
   `density`, `content_hash`, `meaningful`. A capture with `after_seq` must only
   return `frame_seq > after_seq`. Stale = `NO_FRESH_FRAME`.
6. **UI model.** Nodes use `id` (tree-local), `resource_id` (Android id),
   `class`, `text`, `content_desc`, `bounds` `[l,t,r,b]`, flags and children.
   Selectors follow §9.1; document order is pre-order DFS.
7. **Scenario steps are one-key maps** (`launch`, `wait_for`, ...). Exactly one
   key per step. `type` is the key name (Rust uses `r#type`). No sleeps:
   `wait_for`/assertions are event-driven.
8. **`since_previous_step`** on `assert_log` scopes the log regex sequence to the
   previous step; use it for lifecycle assertions.
9. **Artifacts.** `verdict.json`, `steps/NN-<kind>.json`, `frames/<seq>.png`,
   `tree/<seq>.json`, `logs/`, `events.jsonl`, `resources.json`, `repro.sh`,
   `repro.json`. Paths in the verdict are relative and safe. A pass without
   evidence is invalid (`validate_artifacts_dir`).
10. **Reproducer determinism.** `flags` sorted and de-duplicated; APK/scenario
    SHA-256 and ACOS version present; no absolute temp paths.
11. **MCP names.** `acos_<command>` with inputSchema from the request type.
12. **Paths.** Request paths may be workspace-relative or absolute; artifact
    paths in verdicts/evidence are always relative. Reject `..` escapes.
13. **Additive evolution.** Existing field names/meanings never change. New
    fields are optional. Codes are never re-used for a different meaning.

---

## 15. Recommended changes to the existing `acos inspect` envelope

The existing `acos-cli` `inspect` emits:

```json
{"request_id":"req_…","ok":true,"status":"pass","backend":"acos-native","fidelity":"static-analysis","capabilities_used":["apk.inspect"],"error":null,"result":{…},"metrics":{"wall_ms":3}}
```

It is already compatible (subset). Recommended improvements, in priority order:

1. **Error object:** emit `hint`, `retryable` and optional `details` (use
   `ProtocolError`). The current `{code,message}` is not actionable enough for an
   agent.
2. **Status from the code:** the current `run-dex` path uses
   `code.starts_with("UNSUPPORTED")`; use `ErrorCode::default_status()` instead so
   a new unsupported code cannot be mislabelled. Keep `run-dex`'s `UNSUPPORTED_*`
   → `unsupported` behaviour.
3. **Metric type:** `wall_ms` is `u128` in the CLI; JSON numbers above
   `u64::MAX` are not portable. Use `u64` (`Metrics.wall_ms`).
4. **Add `command`/`protocol_version`:** optional `"command": "inspect"` and
   `"protocol_version": 1` would make envelopes self-describing when logged or
   piped through MCP. Additive, no breaking change.
5. **Add `schema_version` to `result`?** Not necessary; keep the result shape and
   the typed `InspectResult`. If result fields are added, keep them additive.
6. **`fidelity` is right (`static-analysis`)**, but document loudly that a
   `status:pass` here is a *parsing* pass, not app verification. The typed
   contract sets `validated_against: null`; never set `native-runtime` for
   inspect.
7. **Error details for APK/DEX failures:** include the path and byte offset
   (`details.offset`) so an agent can point at the malformed structure.
8. **Emit `capabilities_used` as a stable list** (`["apk.inspect"]`) — already
   done — and add `"apk.native-libs"`/`"apk.abi-map"` when those flags are used.
9. **Do not make `inspect` execute code or allocate a display.** If a future
   inspection needs execution, emit a distinct command/envelope with
   `fidelity: native-runtime`; never silently upgrade the inspect fidelity.
10. **Keep `error: null` on pass** (current behaviour) for compatibility; the
    protocol's `Envelope` omits it when absent, which also deserializes fine.

---

## 16. Host resource ceilings (`run-dex` / `acos-vm`)

`acos-vm` enforces two hard ceilings on the **host process**, independent of the
guest heap (`ACOS_HEAP_MAX`, which only bounds managed bytes and produces a Java
`OutOfMemoryError`). They exist because a VM bug or an unpruned host-side table
can otherwise drive the host to tens of gigabytes (host-safety incident
2026-11-10). See ADR-0018 and `docs/FINDINGS.md`.

| Env | Default | Effect on breach |
|---|---|---|
| `ACOS_HOST_RSS_MAX` | `2G` (2 GiB) | Aborts with structured code `HOST_MEMORY_LIMIT`. Accepts `K`/`M`/`G` suffixes; `0` disables. |
| `ACOS_WALL_MAX_SECS` | `600` (10 min) | Aborts with structured code `TIMEOUT`. `0` disables. |

* The RSS ceiling is sampled from `/proc/self/status:VmRSS` at most every
  100 ms, on every allocation/GC threshold and once per scheduler turn, so the
  hot paths stay cheap.
* The wall watchdog is checked on the same points and also bounds the blocking
  `Looper` pump, so a permanently parked thread cannot hang a run past the
  deadline.
* On breach, `Vm::run` returns a `RunOutcome` with `exit_code: 70`, a
  `stderr` line `acos-vm error: HOST_MEMORY_LIMIT: ...` (or `TIMEOUT`) and the
  typed `vm_error`. In the `--json` envelope the same code/message is reported
  in `error`.
* Every wrapped run (`tools/guarded.sh`) exports both variables so the wrapper's
  bound and the VM's bound agree. `tools/ramguard.sh` is the out-of-process
  backstop: it kills `target/*/acos*` processes when `MemAvailable < 9 GiB`
  (≈85% used on the 62 GiB host) or a single process exceeds 8 GiB RSS.

---

## 16.1 Boot-image source: `ACOS_ART_BOOT_IMAGE` (H13 / H13b)

`acos-art`, and therefore every ART session started by `acos serve` or
`acos verify --engine art`, reads the boot image from one of three sources. The
knob is an **environment seam**, not a CLI flag: `acos serve` inherits it and
every launch child sees it. **Unset means `shared-fixed`** (ADR-0026 §4).

| Value | Image | Address policy | Cache |
|---|---|---|---|
| unset, `shared-fixed` (**default**) | uncompressed image at the recorded base `0x70000000` (B2/B3; `ACOS_SHARED_BOOT_IMAGE` still names an explicit directory) | `-Xnorelocate`: **no image ASLR** (owner accepted) | content-addressed on disk, built once per machine and reused by every serve and session |
| `lz4` | the sysroot's stock LZ4 boot image | ART relocates per process (stock image ASLR) | none; each session decompresses ~26.3 MiB privately |
| `shared-aslr` | uncompressed image relocated to a random page-aligned base once per **serve cohort** | `-Xnorelocate`: fixed for the cohort, a new base per serve | per-cohort directory under `<ACOS_ART_DATA>/boot-image/` |

Rules and behaviour:

* A set-but-unknown value fails the launch (`Error::Layout`) before any guest
  starts. `shared-aslr` refuses a simultaneous `ACOS_SHARED_BOOT_IMAGE`.
* **The `shared-fixed` cache (H13b).** The entry directory is
  `fixed-<input-hash>/` under `ACOS_BOOT_IMAGE_CACHE`, else `ACOS_CACHE_DIR`,
  else `$XDG_CACHE_HOME/acos`, else `~/.cache/acos`, then `art/boot-images`
  (private `0700`, files `0444`, never `/tmp`). The hash covers the ACOS
  image-format version, the `dex2oat64` flags (`--image-format=uncompressed`,
  `--base`, `--instruction-set`), the boot-classpath environ text and the
  identity (path, size, mtime) of every sysroot boot jar
  (`system/framework/*.jar`, `system/apex/*/jars/*.jar`) and of `dex2oat64`,
  `dalvikvm64`, `libart.so`, `libartbase.so`, `libnativehelper.so`. A changed
  image, jar or runtime rebuilds once; everything else hits.
* **Atomic and concurrent-safe.** The build runs in a unique temp directory on
  the same filesystem, every file is `fsync`ed, `manifest.json` is written,
  and the directory is `rename`d into place; the build holds an exclusive
  `flock` on `<cache>/.lock`, so two serves starting together build once and
  the second hits.
* **Corruption is detected.** `manifest.json` records the input hash, the
  image-format version and, per file, its size and a sampled checksum (first
  and last 4 KiB). A truncated or partially written entry fails validation and
  is rebuilt; a cache miss, an input change and damage are all the same
  rebuild path.
* **Failure policy.** An explicit `ACOS_ART_BOOT_IMAGE=shared-fixed` fails the
  launch closed. The *default* (knob unset) falls back to the sysroot LZ4 image
  and reports it: the default must not make a machine that cannot build the
  image unable to run any app.
* **Reply field.** Every launch reply, `acos verify` verdict
  (`engine.timing.boot_image` in the artifact `engine.json`) and `acos serve`
  session list/launch reply carries
  `boot_image: {mode, cache, reason?, dir?, input_hash?, base?, prep_ms?}` with
  `cache` ∈ `hit` (validated existing entry), `built` (this launch built it,
  `prep_ms` is the cost), `fallback` (the default build failed; `reason` says
  why, the LZ4 image is in use), `external` (`ACOS_SHARED_BOOT_IMAGE`) or
  `none` (LZ4).
* **Measured (H13b, `results/bench/H13b.md`).** Cold cache: first launch of the
  machine builds in **3.58 s** (`session_start_ms` 3.82 s). Warm cache: every
  later serve/session reports `cache: hit` and boots in **153–161 ms**, the
  same as LZ4 (158–177 ms) — the +3.6 s per-serve cost H13 measured is gone.
  Memory and gates are unchanged (the image is the same file H13 measured:
  ~−10–12 MiB/session at N=5/10).

---

## 17. The `verify` runner and the `Backend` seam

The scenario runner lives in `crates/acos-verify` and is exposed as
`acos verify`. It is the entry point an agent calls in the verification loop:

```bash
acos verify <scenario.yaml> [--apk <apk>] [--artifacts <dir>] [--overwrite] \
  [--keep-artifacts all|referenced|none-on-pass] \
  [--reference <art-ref-dir>] [--required-fidelity <f>] [--flag <f>] [--json]
```

* `--apk` overrides the scenario's `app`; otherwise the scenario path is used.
* `--artifacts` defaults to `out/<scenario stem>` and receives the full evidence
  directory (§10). The run owns that directory. A rerun into the default
  directory replaces the previous run; a non-empty directory you name explicitly
  is refused (`INVALID_ARGUMENT`, naming the previous run id) unless
  `--overwrite` is passed (§10.4).
* `--reference` selects the **reference-replay** backend (below). Without it the
  *native* backend runs.
* `--keep-artifacts` selects artifact retention (§10.2). Default `referenced`
  (`ACOS_KEEP_ARTIFACTS` is the environment equivalent). A non-pass run always
  keeps every artifact.
* `--required-fidelity` makes a pass that does not reach that fidelity an
  `unsupported` (`UNSUPPORTED_BACKEND` with
  `required_fidelity`/`achieved_fidelity` in `details`), never an optimistic
  pass.
* Exactly one JSON envelope is written to stdout and the same object is returned
  by MCP `acos_verify`. A non-pass verdict exits non-zero.
* With `--json`, **stdout is only the envelope**. Progress, warnings and the
  on-failure summary line go to stderr; `2>&1` therefore corrupts the JSON —
  capture stderr in its own file (`2>err.txt`). In text mode the summary is on
  stdout.

### 17.1 The `Backend` trait

The runner never talks to the VM, loader, renderer or input path directly. It
asks a `Backend` (`crates/acos-verify/src/backend.rs`) for one operation at a
time and checks that the matching **capability** is switched on first:

| capability | step(s) it enables | backend method |
|---|---|---|
| `static-manifest` | static `launch` target resolution | `manifest` |
| `lifecycle` | `launch` | `launch` |
| `ui.tree` | `wait_for`, `assert_text`, `assert_visible`, `assert_not_visible`, `assert_state` | `observe` |
| `logs` | `assert_log` | `logs_since` |
| `crash-reports` | `assert_no_crash` | crash state in `observe`/`launch` |
| `screenshot` | `capture`, `assert_screenshot` | `capture` |
| `input` | `tap`, `type`, `key`, `back`, `rotate` | `input` |
| `reset` | `reset` | `reset` |
| `environment` | `set_network`, `set_permission`, `set_locale`, `set_density`, `set_theme`, `set_time` | `configure` |

A method that is not implemented returns `BackendError::Unsupported`; the
executor turns that into `status: unsupported`, code `UNSUPPORTED_BACKEND`, with
the exact capability in `error.details.capability` and the backend/step in
`details.backend`/`details.context`. `call_activity_method` is always
unsupported until an injected debug hook exists.

Every backend returns a `fidelity()` and a `protocol_backend()` label recorded
in the verdict. The executor only claims `pass` when **every** step was
executed and passed at that fidelity.

### 17.2 Backends today

| backend | fidelity | supports now | how selected |
|---|---|---|---|
| `NativeBackend` (`acos-native`) | `native-runtime` | static manifest (`acos-apk`) and `launch` to the M2 `attachApplication` point (`acos-runtime`) | default |
| `ReferenceBackend` (`acvm-reference`) | `art-reference` | static manifest, UI tree and log stream replayed from the checked-in `fixtures/reference/<app>/art-api34-x86_64/` capture | `--reference` |

The native backend deliberately does **not** fake a UI tree, screenshot, log
stream or input: those are M2/M3 capabilities. It reports them as `unsupported`,
so a positive scenario stops at the first such step and yields a truthful
**partial** verdict. For example, `hello-views.yaml` passes `launch` and then
reports `wait_for` as `unsupported` (`ui.tree`).

The reference replay backend exists so the assertion logic and false-pass
prevention can be exercised before M2/M3 land. It replays real ART evidence and
labels the result `art-reference`; it cannot react to input, so both positive
scenarios remain partial. It is **not** an app execution backend.

### 17.3 Honesty rules enforced by the executor

1. A step whose capability is missing is `unsupported`; it is never skipped and
   never counted as a pass.
2. Execution stops at the first non-pass step; later assertions are not
   evaluated against stale state (`verdict.notes` records the stop).
3. A scenario that declares `expectation: fail`/`non_pass` can never produce
   `status: pass`. If it would, the runner emits `status: error`,
   code `INTERNAL`, and a note — a harness failure, never a successful test.
4. A `required_fidelity` stronger than the achieved fidelity turns a would-be
   pass into `unsupported`.
5. Artifacts are validated against the verdict (`validate_artifacts_dir`); a
   claimed pass without its frame/tree/step evidence is invalid.
6. **Frame binding (the advisor rule).** A screenshot and the UI tree it is
   compared against are one `SessionObservation`, so they always share a
   `frame_seq`; a backend whose `tree.frame` does not match the observation
   frame is a defect (`INTERNAL`), never a pass. A non-meaningful
   (blank/placeholder) frame never satisfies a UI or screenshot assertion. A
   `capture` with `fresh: true` must advance the frame, and a tap is only
   accepted when the same completed frame shows the change in **both** the UI
   tree and the screenshot. §17.5 has the detail.

### 17.4 Plugging in M2/M3 without a schema change

To add a capability later (for example a real UI tree in M3):

1. Implement the backend method (for example `observe`) against the runtime.
2. Switch the corresponding field in `Capabilities` to `true`.
3. Return the protocol types already used by the scenario format (`UiTree`,
   `FrameMeta`, `LogRecord`, `CrashRecord`).

No scenario key changes and no executor rewrite is needed. The reference replay
backend is the worked example of a second implementation.

### 17.5 The frame-binding rule (screenshot + UI tree + tap)

The one vertical slice that proves input works is
`launch -> screenshot + UI tree -> tap -> changed screenshot/tree`. The runner
enforces it so a backend cannot pass by showing the change in only one channel:

* **One observation, one frame.** `observe`, `capture` and `input` return a
  `SessionObservation` carrying the frame metadata, the UI tree and (when
  available) the encoded screenshot. `SessionObservation::binding_violation`
  rejects a tree whose `frame_seq`/`content_hash` differs from the observation
  frame (`INTERNAL`). There is no way to pair a screenshot with a tree from a
  different traversal.
* **Completed frame.** A `launch`, `wait_for`, `tap`, `capture` or
  `assert_screenshot` establishes the frame that later assertions must describe.
  A pure `assert_text`/`assert_visible`/`assert_not_visible`/`assert_state` that
  observes a *different* frame without one of those steps is reported as a
  binding failure (`ASSERTION_FAILED`), not silently accepted.
* **Fresh capture.** `capture: {fresh: true}` (the default) must return
  `frame_seq` greater than the previous completed frame, else `NO_FRESH_FRAME`.
  An action (`tap`/`type`/`key`/`back`/`rotate`) must also complete a new frame;
  a stale frame would let an assertion bind to the pre-action traversal.
* **Tap changes both.** A `tap` is accepted only when `changed_from` reports the
  UI-tree content hash *and* the screenshot bytes changed relative to the
  previous observation (each only when the backend advertises that capability).
  The step records a `state_change` assertion result; when a channel is missing
  the result says which one, and never claims the other.
* **Screenshot comparison.** `assert_screenshot` compares the captured frame to
  the baseline with `crates/acos-verify/src/screenshot.rs`, honouring
  `tolerance` and `region_masks`. A frame whose size differs from the baseline
  is an error, never an optimistic pass.

---

## 18. Agent session commands (launch / observe / input / logs / session)

The M2 milestones are landed one rung at a time. These commands expose the
rungs that exist **today** with the same envelopes the finished commands will
use, so an agent can build its loop now and keep the same calls as the engine
fills in. Nothing here fabricates a frame, a tree or a pass: a stage that did
not complete is reported as `blocked`/`not_reached` with the structured
blocker, and `ok:true` only appears when every rung ran.

The commands are implemented in `acos-cli/src/launch.rs` and
`acos-cli/src/session.rs`; the in-process session type is
`acos_runtime::session::AgentSession` (a worker thread owns the `Vm` behind a
channel command loop, the same pattern as `SessionPool`). There is **no
`acos serve` daemon yet**: a CLI invocation owns its session for the duration
of one command. The MCP server (`acos-mcp`, default `SessionBackend`) holds the
same `AgentSession` across tool calls inside the server process.

### 18.1 `acos launch <apk> [--timeout SECS] [--json]`

Boots the APK on the real framework bytecode and prints the stage ladder:

```text
apk_parsed → process_booted → bind_application → application_on_create
           → client_transaction → activity_on_create → on_resume → first_frame
```

Every rung is `ok` only when the real framework completed it. The first rung
that did not is `blocked` and carries the root blocker; later rungs are
`not_reached` and carry the same blocker (so an agent can read the reason from
any non-ok rung). `reached_stage` is the last `ok` rung. The envelope is a
`pass` only when `first_frame` ran; otherwise the envelope is a failure whose
`error.code` is the blocker's stable code (`JAVA_EXCEPTION`,
`UNSUPPORTED_BACKEND`, `VM_TIMEOUT`, ...).

Real blocked-stage envelope captured on 2026-11-10 (counter fixture; trimmed
stack tail):

```json
{
  "request_id": "req_f4b9f_18e6be0e8b2f25d5",
  "ok": false,
  "status": "error",
  "backend": "acos-native",
  "fidelity": "native-runtime",
  "session_id": "s_default",
  "capabilities_used": ["apk.inspect", "lifecycle"],
  "error": {
    "code": "JAVA_EXCEPTION",
    "message": "java exception: java.lang.IllegalArgumentException: unknown service name \"dev.acos.fixtures.counter\" ...",
    "hint": "An exception escaped. Inspect exception/stack, fix the app (or the missing platform behaviour), and rerun.",
    "retryable": false,
    "details": {
      "stage": "application_on_create",
      "reached_stage": "bind_application",
      "classes": "1580",
      "classes_loaded": 1580,
      "hint": "handleBindApplication threw before Application.onCreate"
    }
  },
  "result": {
    "package": "dev.acos.fixtures.counter",
    "activity": "dev.acos.fixtures.counter.MainActivity",
    "reached_stage": "bind_application",
    "classes_loaded": 1580,
    "wall_ms": 4999,
    "peak_rss_bytes": 506757120,
    "events": [],
    "stages": [
      {"stage": "apk_parsed",            "ok": true,  "status": "ok",          "blocker": null},
      {"stage": "process_booted",        "ok": true,  "status": "ok",          "blocker": null},
      {"stage": "bind_application",      "ok": true,  "status": "ok",          "blocker": null},
      {"stage": "application_on_create", "ok": false, "status": "blocked",     "blocker": {"code": "JAVA_EXCEPTION", "message": "java exception: ... unknown service name ...", "details": {"stage": "application_on_create", "classes": "1580"}}},
      {"stage": "client_transaction",    "ok": false, "status": "not_reached", "blocker": {"code": "JAVA_EXCEPTION", "message": "..."}},
      {"stage": "activity_on_create",    "ok": false, "status": "not_reached", "blocker": {"code": "JAVA_EXCEPTION", "message": "..."}},
      {"stage": "on_resume",             "ok": false, "status": "not_reached", "blocker": {"code": "JAVA_EXCEPTION", "message": "..."}},
      {"stage": "first_frame",           "ok": false, "status": "not_reached", "blocker": {"code": "JAVA_EXCEPTION", "message": "..."}}
    ]
  },
  "metrics": {"wall_ms": 4999, "peak_rss_bytes": 506757120, "classes_loaded": 1580}
}
```

Read it as: *the APK parsed, the process booted and `bindApplication` was
delivered; `handleBindApplication` threw before `Application.onCreate`, so no
later rung ran.* The blocker's `details.stage` names the exact rung. This is a
real `error`, not a `fail` and not a `pass`.

If the boot classpath is absent on the host, `launch` returns
`BOOT_CLASSPATH_MISSING` (or a `VM_INTERNAL` naming the missing path) with no
ladder; the CLI integration test skips loudly in that case rather than pretend.

### 18.2 `acos observe [--screenshot out.png] [--tree] [--text] [--apk APK] [--session ID]`

Binds to `frame_handoff::FrameStore::latest()`. A completed frame is the only
thing that makes `observe` a pass:

* **No completed frame** → `status: unsupported`, `error.code:
  UNSUPPORTED_BACKEND`, `error.details.reason: "not_ready"`, plus the
  `stage_reached` and `published_frames` count. There is no blank image and no
  empty tree: `result` is absent. This is the honest answer before the first
  frame and is what the CLI/MCP tests assert.
* **A completed frame** → `pass` with `result.frame`
  (`frame_seq`, `width`, `height`, `content_hash`, `cause_id`,
  `meaningful`), `result.published_frames` and, with `--screenshot`, the
  written PNG path and byte count. The pixels come from the frame the guest
  actually produced; the CLI does not paint a placeholder.
* `--tree`/`--text` walk the live framework `View` hierarchy of the session VM
  (`WindowManagerGlobal` → `ViewRootImpl`/`mViews` → `ViewGroup.getChildAt`) and
  return `result.tree` with `nodes` (`id`, `class`, `resource_id`, `text`,
  `content_desc`, `bounds`, `visible_bounds`, `visible`, `enabled`, `focused`,
  `clickable`, `scrollable`, `checked`, `package`, `children`), `result.windows`
  and `result.truncated`; `--text` adds the flat `result.text` rendering. No
  screenshot is required or taken. `bounds` are the framework's own
  `getLocationOnScreen` result, i.e. the same coordinates `uiautomator` reports.
  `visible` means **visible to the user** (Android `View.isVisibleToUser()`:
  shown — it and all ancestors `VISIBLE` and attached — and its global visible
  rect, clipped by every ancestor scroll/pager and by the window/display, is
  non-empty); `visible_bounds` is that clipped rect in display pixels (absent
  when nothing is visible). A warm-mounted but off-screen node (a pager's next
  page) therefore stays in the tree with `visible: false` and no
  `visible_bounds`. Selector steps (`assert_visible`, `assert_text`,
  `assert_not_visible`, `state`, `wait_for`, `tap`) match only user-visible nodes
  by default (`selector.include_invisible: true` opts into invisible ones for
  diagnostics); a selector that matches only an invisible node fails with
  `ELEMENT_OFFSCREEN` naming its bounds, and a tap lands on the centre of the
  node's `visible_bounds`, never outside it.
  Before the app adds a root window this is `status: unsupported`,
  `error.code: UNSUPPORTED_BACKEND`, `error.details.reason: "not_ready"`
  (`no_view_hierarchy_api` when the boot classpath lacks the root list), so an
  empty tree is never a pass. MCP: `acos_observe_tree { session_id, max_nodes }`.

`--apk APK` seeds the session with a real `launch` first (the same ladder as
18.1) so `observe` describes the state the launch reached; without `--apk` the
session is a fresh un-launched VM.

### 18.3 `acos tap X Y` / `acos tap --text LABEL` / `acos key CODE` / `acos swipe X1 Y1 X2 Y2`

Input goes through `acos_services::input`'s dispatcher via
`input_java::inject_tap`/`inject_swipe`/`inject_key` and the `wms_java` window
registry. The envelope is a `pass` when the event was accepted and delivered to
a window; the structured errors are the dispatcher's own:

| condition | `error.code` | `error.details` |
|---|---|---|
| no window contains the point | `ELEMENT_NOT_FOUND` | `{"kind":"NoWindowAtPoint","x":"…","y":"…"}` |
| no focused window for a key | `ELEMENT_NOT_FOUND` | `{"kind":"NoFocusedWindow"}` |
| `--text` with no extracted tree | `ELEMENT_NOT_FOUND` | `{"kind":"NoUiTree","label":"…"}` |
| one coordinate missing | `INVALID_ARGUMENT` | `{"field":"x/y"}` |

`key` accepts a numeric code (`4` = BACK, `66` = ENTER) or a name (`back`,
`home`, `enter`, `tab`, `escape`, `delete`, `volume_up`, `volume_down`,
`power`). `swipe` takes `--duration-ms` and `--steps`. A successful input
envelope includes `accepted`, `state_changed: false` and the last
`input_ack_seq`; it does **not** claim the screen changed — only a subsequent
`observe` frame can prove that.

### 18.4 `acos logs [--apk APK] [--session ID] [--json]`

Reads the guest log buffer from the `log` native (drained by
`boot_and_run_lifecycle` and appended as `I AcosFixture: …` records so the
lifecycle order is visible). An empty buffer is a valid `pass` with
`result.count: 0`; the command never fabricates lifecycle lines.

### 18.5 `acos session status|reset|close [--json]`

* `status` → `pass`, `result.{launched,package,apk,stage_reached,published_frames,live}`.
* `reset` → `pass`, recreates the task state (`state_reset: true`). The
  in-process guest VFS and log sink are process-global, so `data_cleared` is
  honestly `false` until the per-session VFS lands.
* `close` → `pass`, idempotent (`destroyed: true`, `already_absent: false`).
  The CLI's session is process-local; closing it ends the command.

### 18.6 MCP tools (in-process sessions)

The default `acos-mcp` binary installs `SessionBackend`, which wraps
`CliBackend` and owns a `Mutex<HashMap<session_id, AgentSession>>`. The session
tools hold their state across `tools/call` requests:

| tool | wire command | behaviour |
|---|---|---|
| `acos_run` / `acos_launch` | `run` | launch the APK, return the ladder |
| `acos_observe` | `observe` | latest frame or structured `not_ready` |
| `acos_observe_tree` | `observe --tree` | live `View` hierarchy or `not_ready` |
| `acos_action` | `action` | `{kind: tap|swipe|key|back}` |
| `acos_tap` | — | `{x,y}` or `{text}` |
| `acos_key` | — | `{code}` |
| `acos_swipe` | — | `{from,to,duration_ms}` |
| `acos_logs` | — | guest log buffer |
| `acos_status` | — | launched/stage/frames |
| `acos_reset` | `reset` | recreate task state |
| `acos_destroy` | `destroy` | drop the session |

Sessions are created lazily: an `observe`/`status`/`logs`/`action` on an unknown
`base.session_id` creates a fresh un-launched session (so `observe` returns
`not_ready` rather than `SESSION_NOT_FOUND`), and `destroy` removes it. A
server wired to the bare `CliBackend` answers these six server-local tools with
`UNSUPPORTED_BACKEND` rather than a false pass. `acos_capabilities` and
`acos_help` remain server tools; `tools/list` now returns 22 descriptors
(13 protocol commands + 2 meta + 7 session tools).

---

## 19. `acos parity` — device/ACOS UI-tree parity

`acos parity` compares a normalized UI tree with ACOS's tree. There is no
silent fallback and no empty pass: a missing device or an unavailable view
hierarchy is a structured non-pass.

### 19.1 Live: `acos parity --device <adb serial> <apk> [--output DIR] [--keep]`

1. Refuses any APK whose package does not start with `dev.acos.fixtures.`
   before installing it. Only `adb install`, `am start`, `uiautomator dump`,
   `screencap` and `adb uninstall` of that package are issued; no device
   setting is read or written.
2. Captures the reference as `uiautomator dump` XML plus a screenshot.
3. Boots ACOS in-process, launches the APK, walks the live `View` hierarchy and
   captures the ACOS tree.
4. Emits one JSON object: `{ok, mode: "physical-device", fidelity:
   "physical-device", matches, differences[], device_nodes, acos_nodes,
   identical, options, device, package, output}`.

If the phone is missing the result is `ok: false`, `code: NEEDS_REAL_DEVICE`,
exit 1. If ACOS cannot reach a root view the result is `ok: false` with the
launch booleans and `reason: "not_ready"` — a blocker on the engine.

### 19.2 Recorded: `acos parity --recorded <app> [--bounds-scale R] [--bounds-tolerance PX] [--acos-tree FILE]`

Runs the same comparator with no phone: the device side is
`fixtures/reference-device/<app>/snapshot-*.json`, the ACOS side is either a
stored `observe --tree` JSON (`--acos-tree`) or the checked-in API-34 ART
reference `ui_initial.xml`. `tools/parity/recorded.sh` runs every fixture app.

### 19.3 Comparator

Nodes match by `resource_id`, else `text|class`, else `class`. Matched nodes
compare `text` (trimmed), `class`, and `bounds` after `--bounds-scale` with a
per-edge `--bounds-tolerance`; `--strict-state` adds
`enabled`/`focused`/`clickable`, `--ignore-bounds` disables bounds. Difference
kinds are `missing_in_acos`, `extra_in_acos`, `text_differs`, `class_differs`,
`bounds_differ` (labelled `bounds_differ_density` when scaling was applied) and
`state_differs`.

See `tools/parity/README.md` and `fixtures/reference-device/COMPARISON.md`.
