# ACOS — Specification

**Product:** ACOS — a Rust-first, agent-native Android application execution and
verification OS.
**Status:** clean-sheet, nothing executes yet (see `STATUS.md`). All CLI/JSON
shapes below are **proposed contracts**, not implemented commands.
**Derived from:** `docs/handoff/00`–`06`, `docs/handoff/AGENT_KICKOFF.md`, and
the legacy inventory in `docs/reference/ACVM_INVENTORY.md`.

---

## 1. Product contract

An AI coding agent should be able to change Android app code, build an APK, run
**only the minimum environment that can correctly execute the task**, inspect the
live screen and state, interact, assert the intended behaviour, receive reliable
evidence, and release resources — without operating a whole simulated phone.

ACOS is a **task-first execution and verification system**. Its unit is a task
with a required oracle and evidence level, not a device session. It must:

1. construct the lowest-cost environment that can **correctly** satisfy the
   task's declared evidence level;
2. execute the real artifact (compiled DEX / real APK / real native `.so`), not
   a substitute instruction language or a mock that can pass while a phone would
   fail;
3. report exactly what ran (`backend`, `fidelity`, `capabilities`,
   `fallback_reason`) and never report a fallback as native success;
4. refuse, escalate or label anything it cannot faithfully execute —
   `UNSUPPORTED` is never `PASS`;
5. share immutable runtime/framework/caches across workers and scale past a
   fixed session count, with isolation as an explicit per-task dial;
6. be honest about compatibility: the long-term goal is ordinary third-party
   APKs (Java/Kotlin, Views/Compose, Flutter, RN, WebView, JNI/native,
   multi-process) across explicitly supported API levels/ABIs, but coverage is a
   live quantitative ledger, not a headline.

### 1.1 The Rust mandate

The managed runtime implementation itself is Rust: DEX decoding, verification,
interpreter, eventual JIT/AOT frontend, class loader, object model, GC,
threading, the standard JNI `JavaVM`/`JNIEnv` ABI, native bridge, and runtime
observability. Embedding Google's C++ `libart.so` and calling that the engine is
**not** ACOS. Guest native ELF/C/C++ libraries remain valid inputs and call the
Rust-owned ABI. The Linux kernel, host graphics drivers and third-party native
dependencies need not be rewritten in Rust; any non-Rust dependency needs an
explicit portability/performance/licence rationale.

### 1.2 North-star metric

> **verified Android development tasks / (GiB of RAM × minute)**, gated by
> correctness and compatibility.

It is always reported together with: task wall-clock time; peak and integrated
memory; CPU; cold/warm latency; number of independent concurrent workloads;
isolation level; and **false-pass rate (must be zero)**. A fast mock is never a
substitute for a correct real execution.

---

## 2. Execution and fidelity levels

Every result carries its level and backend. `native success statistics exclude
fallback successes.`

| Level | Backend | Example work | What success proves |
|---|---|---|---|
| **H0** | Host tooling | compile, lint, manifest/resource/APK parsing, JVM unit tests, DEX verification | only those checks — **not** Android runtime behaviour |
| **H1** | Source-aware host renderer/simulation | component/UI snapshots, isolated Compose/View fixtures | source-component rendering and layout — **not** app runtime integration; `preview-only` |
| **H2** | ACOS native Rust runtime, shared state | quick visual/flow checks for cooperative/debuggable apps | actual app instructions and native runtime behaviour under supported APIs; not independent data |
| **H3** | ACOS native Rust runtime, separate sandbox/process/data | repeated independent users; same APK version in many sessions | stronger state separation (subject to threat-model audit) |
| **H4** | AOSP/legacy ACVM oracle or dedicated device | semantics not yet implemented, OEM APIs, full device tests | the actual selected reference runtime or physical device |

Rules:

- **Every result carries `fidelity`** (`host-proof`, `preview-only`,
  `real-apk-native-runtime`, `reference-oracle`, `physical-device`, …). A
  JVM match is never reported as Android-verified; a preview never passes a
  runtime oracle.
- **H4 is a transparent correctness oracle/fallback while H2/H3 are built** — not
  an excuse to never implement the native engine. Native coverage is tracked
  separately.
- Output fields: `backend`, `fidelity`, `capabilities`, `fallback_reason` (when
  applicable).
- **Escalation is monotone and bounded**: a failed oracle stays failed; do not
  rerun on another level until a regression disappears. Each escalation records
  reason and full cost.
- No available H4 target means `unsupported`, never "skipped-and-counted-pass".

---

## 3. Isolation levels

Isolation is a per-task dial, declared explicitly, never inferred from a RAM
budget.

| Level | Meaning | Independence claimed | Cost |
|---|---|---|---|
| `shared-task` | another activity/task in a shared app process/data state | **none**; cheapest | lowest |
| `process` | separate process, potentially same package/storage identity | process-only; **not** independent data by default | low |
| `sandbox` | separate OS process/UID/credentials/mount namespace and app data | needs verified service identity, permissions, providers, JNI/native | medium |
| `core` | fully separate engine instance for ABI/API conflicts and stronger service-state independence | strongest short of `vm` | high |
| `vm` | sandboxed full Android/backend for untrusted or device-level fidelity | audited VM boundary | highest |

Rules:

- Use `shared-task` only when no cross-task independent state is claimed; label
  shared static state, shared app data and shared crash radius loudly.
- Do not call a Linux sandbox an Android-user equivalent until permission,
  storage, Binder, provider and process semantics are demonstrated.
- **Untrusted APKs never run with host-root-equivalent privilege.** The legacy
  privileged-redroid model is explicitly rejected for untrusted input.
- More than five concurrent workloads is a design requirement; there is no
  hard-coded five-session ceiling. Real capacity is bounded by CPU, RAM,
  rendering, app compatibility and isolation mode, and is measured, not assumed.

---

## 4. CLI contract (proposed)

One typed contract across CLI, JSON, MCP and SDKs. Every command emits exactly
one JSON object on stdout; logs go to stderr; failures exit non-zero.

```text
acos inspect app.apk --json
acos run --apk app.apk --activity com.example/.MainActivity \
          --isolation sandbox --display offscreen:393x851
acos observe <session> --tree --text --json
acos capture <session> --fresh --format png
acos action <session> tap --x 140 --y 320
acos action <session> type --text 'hello'
acos assert <session> --text 'Welcome'
acos events <session> --since-seq 10
acos reset <session> --scope app
acos profile <session> --memory --cpu --frames
acos destroy <session>
acos verify --apk app.apk --scenario scenarios/login.yaml --json --artifacts out/
acos capability report app.apk --json
acos devloop --project . --build './gradlew :app:assembleDebug' \
          --apk app/build/outputs/apk/debug/app-debug.apk \
          --scenario scenarios/pr-regression.yaml --output artifacts/ --json
```

Minimum response envelope (stable request/response IDs, session IDs and frame
sequence IDs):

```json
{
  "job_id": "job_...", "session_id": "s_...", "request_id": "req_27",
  "status": "pass | fail | unsupported | escalated | error | inconclusive",
  "backend": "acos-native | acvm-reference | physical-device",
  "fidelity": "real-apk-native-runtime",
  "isolation": "sandbox",
  "capabilities_used": ["lifecycle", "ui.offscreen", "input", "storage"],
  "frame": {"display_id": "d_1", "sequence": 19,
            "fresh_after_request": "req_27", "timestamp_ns": 0, "sha256": "..."},
  "evidence": {"screenshot": "artifacts/19.png", "tree": "artifacts/19.json",
               "events": "artifacts/events.jsonl"},
  "metrics": {"wall_ms": 0, "peak_rss_bytes": 0, "cpu_ms": 0}
}
```

Contract rules:

1. **Frame freshness**: a capture names the request it answered; a stale frame
   must be impossible to return (`fresh_after_request`, monotone `sequence`,
   content `sha256`, production timestamp).
2. **Explicit status**: `pass | fail | unsupported | escalated | error |
   inconclusive`; unsupported is never pass.
3. **Identity on every session-touching response**: session id, display id,
   package/activity, isolation, backend.
4. **Typed capability negotiation** for locating, input, text/semantics,
   persistence, network, permissions, graphics, native/JNI, scheduling.
5. **Scenario format** (proposed YAML): `version`, `name`, `app`, `engine`,
   `fallback`, `api_level`, `isolation`, `display{mode,width,height,density}`,
   `capabilities[]`, `steps[]`, `output{}`.

---

## 5. Error taxonomy

Structured, actionable, never a hang or a false pass. Minimum set (extendable,
additive):

| Code | Meaning |
|---|---|
| `UNSUPPORTED_ANDROID_API` | native engine lacks a required `android.*` API/behaviour |
| `UNSUPPORTED_NATIVE_SYMBOL` | a required ELF/Bionic symbol or ABI is unavailable |
| `WRONG_ABI` | APK/native library ABI is not supported by this engine |
| `REQUIRES_GMS` | app needs licensed Google Mobile Services |
| `NEEDS_REAL_DEVICE` | behaviour needs a real device/OEM/hardware target |
| `NO_FRESH_FRAME` | no fresh frame satisfying the request exists |
| `NO_DISPLAY` | capture/observe requested on a headless task |
| `NO_TEXT` | text/semantics requested but unavailable on the chosen route |
| `UNVERIFIED_FIDELITY` | evidence cannot be tied to the required fidelity level |
| `STALE_REF` | a UI node reference is no longer valid |
| `ISOLATION_UNSATISFIED` | requested isolation not actually implemented for this app |
| `RAM_GUARD` / `RAM_ABORT` | host memory admission/abort rule triggered |
| `UNSAFE_PATH` | artifact path rejected |
| `UNSUPPORTED_BACKEND` | requested backend does not exist |
| `INTERNAL` | ACOS engine/harness error, distinct from an app crash |

`PASS`/`FAIL`/`UNSUPPORTED`/`ESCALATED`/`ERROR`/`INCONCLUSIVE` follow handoff 03
§"First-class failure semantics". `INCONCLUSIVE` (nondeterministic animation,
insufficient reference, no stable screenshot) is **not** pass.

---

## 6. Non-goals and explicit boundaries

- **Not** a phone emulator with a new coat of paint; not a redroid/ACVM iteration.
- **Not** a wrapper around C++ `libart.so`; the managed runtime is Rust-owned.
- **No automatic allocation** of Android users, devices, displays, GUI windows,
  system servers, WebViews, HALs, cameras, Bluetooth, location, notification
  UIs, per-session VMs, or full Android boot.
- **No `--privileged` container** as an execution boundary for untrusted APKs.
- **No hard-coded five-session limit.**
- **No fake success for proprietary dependencies**: GMS, DRM, Play Integrity,
  device-specific hardware, privileged OEM APIs and some Android versions may be
  impossible without their real dependencies (or a licence) and are declared so.
- **No architecture diagrams or synthetic DEX demos counted as an Android app
  being supported.**
- **No public compatibility headline** ("all APKs run") — only the ledger.
- Full CTS certification is a separate question and is not implied by passing a
  limited subset.

### 6.1 Honest present capability

Nothing executes yet. `COMPATIBILITY.md` starts with every cell at `none`. No
capability below H0 exists and no fixture has been executed by ACOS. Any future
claim must link to a reproducible evidence artifact in `results/`.
