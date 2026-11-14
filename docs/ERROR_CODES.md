# ACOS error codes (one table)

Canonical taxonomy: `crates/acos-protocol/src/error.rs` (`ErrorCode`); the JSON
Schemas under `crates/acos-protocol/schema/` are generated from it. Codes are
additive: never renamed or reused for a different meaning. Every code means
"this did not work"; only a real pass returns `ok: true` / exit 0.

**Exit status** is the process status when the code is the top-level failure:

- `1` — `acos launch/observe/tap/verify/...` prints an `ok:false` envelope (or a
  text error) and fails; a non-pass verdict is never 0.
- `70` — `acos run-dex` / `acos-vm` error envelope (`UNSUPPORTED_*` or
  `VM_INTERNAL`). The same code reached through `launch`/`verify` is `1`.
- `2` — `acos-art` harness error (`acos-art: <CODE>: <detail>`), and CLI usage
  errors. MCP has no process exit: the JSON-RPC result carries `ok:false` and
  the protocol status (`unsupported`, `blocked`, `fail`, `timeout`, `error`).

**ABI rule (ADR-0018 W59-A / E1).** An APK that ships native code but no
`lib/<host-abi>/` is `UNSUPPORTED_ABI` on every route (CLI gate, daemon RN
backend, Flutter session). `WRONG_ABI` is only the loader-level condition for a
single ELF whose `e_machine` the in-process loader cannot map.

**The ART path gates it before the guest (ABIGATE).** `acos-art` decides the
package ABI from the APK index and refuses *before* the image, props, logd or any
guest process exists, so the envelope is `UNSUPPORTED_ABI` with
`status: unsupported` — never `GUEST_EXITED` and never `INTERNAL`. The same gate
covers `acos launch/verify --engine art`, `acos serve` sessions and
`acos_launch`/`acos_verify` over MCP. `error.details` carries
`apk_abis: ["arm64-v8a", ...]`, `host_abi: "x86_64"` and `libs_sample:
["lib/arm64-v8a/libfoo.so", ...]` (a bounded, sorted sample of what the APK does
ship), so a caller can act without re-reading the APK.

The rule is AOSP's, not ours: `PackageManager` decides **per package**.
`NativeLibraryHelper.findSupportedAbi` keeps the ABI directory whose index in the
device's supported list is lowest, returns `NO_NATIVE_LIBRARIES` for an APK with
no `lib/`, and `INSTALL_FAILED_NO_MATCHING_ABIS` for `lib/` with no match;
`iterateOverNativeFiles` then extracts only that ABI
(`frameworks/base/core/jni/com_android_internal_content_NativeLibraryHelper.cpp`).
Consequences ACOS reproduces deliberately:

* a pure-Java APK (no `lib/`) always launches — there is no ABI question;
* an APK with both `lib/x86_64/` and `lib/arm64-v8a/` launches as an x86_64
  package and only `lib/x86_64/*` is installed;
* an APK with `lib/x86_64/liba.so` **and** an arm64-only `libb.so` also launches:
  the package has an x86_64 directory, `liba.so` loads, and the first
  `dlopen("libb.so")` fails with the app's own `UnsatisfiedLinkError`. That is an
  app-packaging bug the platform reproduces, not an install gate, so it is never
  reported as `UNSUPPORTED_ABI`.

| Code | Meaning | Component | Exit |
|---|---|---|---|
| `UNSUPPORTED_ANDROID_API` | The native engine lacks a required `android.*` API or behaviour. | acos-vm | 70 / 1 |
| `UNSUPPORTED_JAVA_API` | The native engine lacks a required Java/library API. | acos-vm | 70 / 1 |
| `UNSUPPORTED_BYTECODE` | A Dalvik opcode or bytecode feature is not implemented. | acos-vm | 70 / 1 |
| `UNSUPPORTED_NATIVE_SYMBOL` | A required native (ELF/Bionic/JNI) symbol is not implemented. | acos-native-loader / acos-vm | 70 / 1 |
| `NEEDS_REAL_DEVICE` | The behaviour needs a real device, OEM service or hardware target. | engine / CLI | 1 |
| `REQUIRES_GMS` | The app requires licensed Google Mobile Services. | engine / CLI | 1 |
| `WRONG_ABI` | One ELF's `e_machine`/class is not what the loader can map (per-library; not an APK ABI inventory miss). | acos-native-loader | 70 / 1 |
| `UNSUPPORTED_ABI` | The APK ships native code, but none for the host ABI. Names the available ABIs; on the ART path it is decided **before** any guest starts and `details` carries `apk_abis`, `host_abi`, `libs_sample`. | acos-apk / acos-services / acos-art / acos-cli / acos-runtime / acos-jni / acos-mcp | 1 |
| `NO_DISPLAY` | Capture/observe was requested on a headless session. | acos-runtime / MCP | 1 |
| `NO_TEXT` | Text/semantics were requested but the chosen route cannot provide them. | acos-runtime / MCP | 1 |
| `UNSUPPORTED_RELOCATION` | A native relocation kind is not implemented by the loader. | acos-native-loader | 70 / 1 |
| `RENDER_UNSUPPORTED_OP` | The renderer hit an operation it cannot draw faithfully. | acos-render | 1 |
| `UNSUPPORTED_BACKEND` | The requested backend does not exist in this build. | acos-cli / MCP | 1 |
| `UNSUPPORTED_RN` | The APK is not structurally a React Native APK (no RN/Hermes runtime or no JS bundle). | acos-runtime / acos-mcp | 1 |
| `UNSUPPORTED_RN_SETUP` | A React Native session failed before the bridge mounted. | acos-runtime / acos-mcp | 1 |
| `UNSUPPORTED_RN_DRIVER` | The React Native Java driver failed to mount or drive the bridge. | acos-runtime / acos-mcp | 1 |
| `UNSUPPORTED_RN_NO_RENDERER` | React Native bridge has no native view managers, so it cannot produce a pixel frame (`observe --frame`). | acos-runtime / acos-mcp | 1 |
| `UNSUPPORTED_RN_KEY` | Key input is not wired through the React Native bridge. | acos-runtime | 1 |
| `UNSUPPORTED_RN_TEXT` | Text input is not wired through the React Native bridge. | acos-runtime | 1 |
| `UNSUPPORTED_RN_NO_TARGET` | A `tap --text`/`--testID` selector matched no node in the RN tree. | acos-runtime | 1 |
| `NO_FRESH_FRAME` | No frame satisfying the request (freshness / sequencing) exists. | acos-runtime / acos-render | 1 |
| `UNVERIFIED_FIDELITY` | Evidence cannot be tied to the required fidelity level. | acos-verify | 1 |
| `APK_NOT_ZIP` | The input is not a readable zip/APK container. | acos-apk | 1 |
| `APK_NO_MANIFEST` | The container is a zip but has no `AndroidManifest.xml`. | acos-apk | 1 |
| `APK_IO` | I/O failure while reading the APK from disk. | acos-apk | 1 |
| `APK_ENTRY` | A zip entry could not be read or decompressed. | acos-apk | 1 |
| `APK_SIGNING_MALFORMED` | The APK Signing Block is present but malformed. | acos-apk | 1 |
| `AXML_MALFORMED` | The binary `AndroidManifest.xml` (AXML) is malformed. | acos-apk | 1 |
| `ARSC_MALFORMED` | The `resources.arsc` resource table is malformed. | acos-apk | 1 |
| `DEX_BAD_MAGIC` | DEX magic is not `dex\n`. | acos-dex | 70 / 1 |
| `DEX_UNSUPPORTED_VERSION` | DEX version is not supported. | acos-dex | 70 / 1 |
| `DEX_BAD_CHECKSUM` | DEX Adler-32 checksum does not match. | acos-dex | 70 / 1 |
| `DEX_BAD_SIGNATURE` | DEX SHA-1 signature does not match. | acos-dex | 70 / 1 |
| `DEX_TRUNCATED` | DEX is truncated relative to its header/map. | acos-dex | 70 / 1 |
| `DEX_BAD_ENDIAN` | DEX endian tag is invalid. | acos-dex | 70 / 1 |
| `DEX_INDEX_OUT_OF_RANGE` | A DEX index is out of range. | acos-dex | 70 / 1 |
| `DEX_BAD_INSTRUCTION` | A DEX instruction is malformed. | acos-dex | 70 / 1 |
| `DEX_UNUSED_OPCODE` | A DEX opcode is defined as unused. | acos-dex | 70 / 1 |
| `DEX_BAD_LEB128` | A DEX LEB128 value is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_MUTF8` | A DEX MUTF-8 string is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_ENCODED_VALUE` | A DEX encoded value is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_CLASS_DATA` | A DEX class data item is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_CODE_ITEM` | A DEX code item is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_DEBUG_INFO` | DEX debug info is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_ANNOTATION` | A DEX annotation is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_MAP` | The DEX map is malformed. | acos-dex | 70 / 1 |
| `DEX_BAD_HEADER` | The DEX header is malformed. | acos-dex | 70 / 1 |
| `DEX_VERIFY_FAILED` | DEX structural or bytecode verification failed. | acos-dex | 70 / 1 |
| `DEX_OVERFLOW` | Integer overflow while parsing a DEX structure. | acos-dex | 70 / 1 |
| `DEX_LOAD_ERROR` | A DEX file could not be loaded into the VM. | acos-vm | 70 / 1 |
| `ELF_MALFORMED` | An ELF native library is malformed. | acos-native-loader | 70 / 1 |
| `NATIVE_DEP_NOT_FOUND` | A native dependency is missing. | acos-native-loader | 70 / 1 |
| `NATIVE_INIT_FAILED` | A native library initializer failed. | acos-native-loader | 70 / 1 |
| `NATIVE_IO_ERROR` | Native I/O failed. | acos-native-loader | 70 / 1 |
| `NATIVE_SYMBOL_NOT_FOUND` | A required native symbol was not found. | acos-native-loader | 70 / 1 |
| `NATIVE_INVALID_ARGUMENT` | A native-loader argument is invalid. | acos-native-loader | 70 / 1 |
| `VM_INTERNAL` | A VM invariant or implementation bug (never user input). | acos-vm | 70 / 1 |
| `JAVA_EXCEPTION` | A live Java exception escaped the entry point. | acos-vm | 70 / 1 |
| `APP_CRASHED` | The app process crashed (uncaught exception / signal / abort). | acos-vm / acos-runtime | 1 |
| `APP_NOT_RESPONDING` | The app is not responding (ANR / main-thread stall). | acos-runtime | 1 |
| `ASSERTION_FAILED` (SE2 `content_settle_pixel`) | A pixel/frame assertion (`assert_screenshot`, `assert_region`) was bound to a frame settled only by `settle: content`. The step fails closed: details carry `reason: "content_settle_pixel"`, `pixel_evidence: false`, `step_kind` and the content `settle` record (`mode`, `window_ms`, `traversals`, `signature`). Content stability says nothing about pixels; a strict capture rebinds the frame. |
| `ASSERTION_FAILED` (SE2b `content_settle_geometry`) | An assertion that names a rectangle — its own `bounds` or `selector.bounds`, for `assert_text`/`assert_visible`/`assert_not_visible`/`assert_state` — was bound to a frame settled only by `settle: content`. The content signature deliberately does not hash geometry (an animating view's rect drifts ~1 px per traversal), so a bounds claim fails closed: `details.reason: "content_settle_geometry"`, `geometry_evidence: false`, `bounds_claim.{assertion,selector}`, `step_kind`, the content `settle` record and `signature_geometry_fields`. Assert without a rectangle, or settle strictly. (Steps that *resolve* a target — `wait_for`/`tap`/`type` — are not refused: they match on a traversal they take themselves.) |
| `ASSERTION_FAILED` | An assertion ran and did not hold. Also used by SE1's opt-in `expect_change` on an input step: the action was delivered and the strict settle completed, but the settled frame is pixel-identical to the pre-action frame, so the step proved no visual change. The error carries `details.reason = "expect_change"`, `details.pixels_changed = false`, `details.step_kind`, and the `details.before`/`details.after` frame refs (`frame_seq` + `frames/<seq>.png` when captured). | acos-verify / acos-cli | 1 |
| `ELEMENT_NOT_FOUND` | A selector matched no node (the tree was complete). | acos-runtime / acos-verify | 1 |
| `ELEMENT_OFFSCREEN` | A selector matched only nodes that are **not visible to the user**: present in the tree (attached, `VISIBLE`) but off screen or clipped away by an ancestor scroll/pager/hidden window, so its clipped visible rect is empty. `wait_for`/`assert_text`/`assert_visible`/`tap` match user-visible nodes by default, so this is a failure, never a pass; the error details carry the node's `bounds` and (empty) `visible_bounds`. Scroll it into view first (or use `selector.include_invisible` for a deliberate diagnostic). | acos-verify / acos-cli | 1 |
| `TREE_TRUNCATED` | A selector matched nothing in a tree the probe reported as truncated (`UiTree.truncated`, any window). Non-pass, non-retryable; the hint names the node budget. Never `ELEMENT_NOT_FOUND`. | acos-verify / acos-cli | 1 |
| `STALE_REF` | A UI node reference is no longer valid. | acos-runtime | 1 |
| `INSUFFICIENT_HOST_EVIDENCE` | A guest-asserted channel was asked to justify a pass on an untrusted APK run: the UI tree (`UNTRUSTED_TREE_ASSERTION`), the guest log (`UNTRUSTED_LOG_ASSERTION`, an `assert_log` step), or crash reports. Fail closed and require host-observed evidence. The run itself is allowed (untrusted is an explicit opt-in); only the evidence class is refused. | acos-cli / acos-verify | 1 |
| `UNSUPPORTED_ASSERTION` (SE2 `content_settle_unhonored`) | A step asked for `settle: content` but the backend returned no content settle record, so the requested rule was never applied. Fails closed (never downgraded to strict/fast): `details.reason: "content_settle_unhonored"`, `details.step_kind`, and the record actually returned in the diff's `actual`. |
| `UNSUPPORTED_ASSERTION` | The requested assertion kind has no implementation in this build (for example `assert_region.text_ocr` with no OCR engine). Also SE1's `expect_change` on a backend/settle rule that reports no measured `settle.pixels_changed`: the pixel claim cannot be tied to evidence, so the step is `unsupported`, never a pass. | acos-verify | 1 |
| `SESSION_NOT_FOUND` | The requested session does not exist (or was already destroyed). | acos-runtime / MCP | 1 |
| `SESSION_BUSY` | The session is busy with another request. | acos-runtime / MCP | 1 |
| `ISOLATION_UNAVAILABLE` | The requested isolation level is not implemented for this app. | acos-runtime / acos-sandbox | 1 |
| `CONFINEMENT_UNAVAILABLE` | The process could not enter its confinement sandbox; native code is refused (fail closed). | acos-sandbox / acos-cli / acos-native-loader | 1 |
| `LANDLOCK_UNAVAILABLE` | The Landlock filesystem layer could not be assembled or proven on a kernel that supports it (fail closed). Distinct from `SECCOMP_UNAVAILABLE`. | acos-art / acos-sandbox | 2 |
| `ISOLATION_UNSATISFIED` | The requested isolation level is advertised but its guarantees are unmet. | acos-runtime / acos-sandbox | 1 |
| `RAM_GUARD` | A memory admission/guard rule was triggered. | acos-runtime / acos-art | 1 / 2 |
| `RAM_ABORT` | Execution was aborted by the RAM guard. | acos-runtime / acos-art | 1 / 2 |
| `TIMEOUT` (settle `content`) | `settle: content` could not see the tree content signature unchanged for the window (>= 300 ms across >= 2 traversals) inside the step's timeout. Details carry `reason: "content-not-stable"`, the last `settle` record (`window_ms`, `traversals`, `signature`, `bound:"content-timeout"`). The screen's text/nodes/bounds/state kept changing — an animating value (a countdown, a progress percentage) is never content-stable, and a timeout is never a pass. |
| `TIMEOUT` | The operation exceeded its wall/CPU budget. `wait_for` also uses it when its live-tree condition is never satisfied by `timeout_ms`; the failure details carry `timeout_ms`, `elapsed_ms`, `polls`, and `last_frame_seq`. | acos-runtime / acos-art / acos-verify | 1 / 2 |
| `RESOURCE_LIMIT` | A declared resource budget was exhausted. | acos-runtime / acos-art | 1 / 2 |
| `UNTRUSTED_APK_NOT_CERTIFIED` | Reserved for a **certification downgrade**. An explicit `--untrusted` / `trust: untrusted` is allowed today (sandbox verdict CERTIFIED-WITH-LIMITS, product policy `docs/SANDBOX_CERTIFICATION.md` §8.8), so this code is unreachable in a normal build. It is produced by `trust::untrusted_gate` only when the const flag `trust::CERTIFICATION` is `Certification::NotCertified`; `ACOS_ALLOW_UNTRUSTED_FOR_TESTS=1` is an accepted but redundant alias and no longer bypasses anything. | acos-cli | 1 |
| `INVALID_SCENARIO` | The scenario is structurally or semantically invalid. Also the launch gate: a `launch.activity` the APK does not declare is a scenario error (status `error`), never `ASSERTION_FAILED` — the app never ran, so it is not an app verdict. The component is expanded exactly as Android does (`pkg/.Cls` -> `pkg.Cls` per `ComponentName.unflattenFromString`; `pkg/Cls`, `.Cls` and a bare `Cls` per `PackageParser.buildClassName`), and `error.details` carries `requested`, `resolved`, `declared_activities` and `launcher_activity`. | acos-verify | 1 |
| `INVALID_ARGUMENT` | A request argument is invalid. | acos-cli / MCP | 1 |
| `UNSAFE_PATH` | An artifact path was rejected (escape, absolute path, symlink, ...). | acos-cli / acos-protocol | 1 |
| `ZIP_SLIP` | A native-library entry name attempts to escape its target directory. | acos-apk | 1 |
| `ARTIFACTS_INVALID` | The artifacts directory is incomplete or contradicts its verdict. | acos-verify | 1 |
| `RENDER_BAD_HANDLE` | A render handle is invalid or already released. | acos-render | 1 |
| `RENDER_FONT_NOT_FOUND` | A required font could not be found. | acos-render | 1 |
| `RENDER_INVALID_ARGUMENT` | A renderer argument is invalid. | acos-render | 1 |
| `RENDER_SIZE` | A render surface has invalid dimensions. | acos-render | 1 |
| `RENDER_DECODE` | An image could not be decoded. | acos-render | 1 |
| `RENDER_IO` | Renderer I/O failed. | acos-render | 1 |
| `RENDER_STRICT` | Strict rendering mode escalated an otherwise-recordable fidelity loss. | acos-render | 1 |
| `HOST_DISK_FULL` | The host scratch filesystem refused a write (ENOSPC/EDQUOT) or has less free space than `ACOS_ART_MIN_DISK_MB`. Class `host_guard`, **retryable**: free space under `.cache/` (old frames/run data) or move `--artifacts`/`ACOS_ART_DATA`, then retry. | acos-art / acos-cli / acos-verify | 1 / 2 |
| `HOST_PANIC` | An ACOS harness process panicked. Class `harness`, not retryable; `error.details.session_exit` carries the stage and panic message. The CLI catches the panic, emits this envelope on stderr and exits 1 (never 101 with empty stderr). | acos-art / acos-cli | 1 / 2 |
| `HOST_MEMORY_LIMIT` | A host memory ceiling was hit (launcher watchdog, cgroup limit, memcg OOM). Class `host_guard`, **retryable** after reducing N/concurrency or freeing memory. | acos-art / acos-cli | 1 / 2 |
| `HOST_MEMORY_LOW` | No ART session was started: admitting it would leave `MemAvailable` below 10% of `MemTotal` (`MemAvailable` − the session's memory budget < 10% of `MemTotal`). Class `host_guard`, **retryable**; `error.details` carries `mem_available_mib`, `mem_total_mib`, `requested_mib`, `min_free_mib`, `min_free_percent`, `budget_env`. Nothing is queued and no guest, cgroup or artifact directory is created — free memory (destroy a session, clear scratch) and call again. `ACOS_MEMORY_GUARD=off` disables the admission check on a lab host; it never disables the per-session cgroup ceiling (`ACOS_ART_MEM_MAX`, default 2 GiB). Distinct from `HOST_MEMORY_LIMIT`, which is a ceiling hit *during* a run. | acos-cli / acos-mcp | 1 / blocked |
| `SECCOMP_UNAVAILABLE` | The guest seccomp filter could not be built, installed or proven (`OK guard=active` absent). Class `unsupported`, not retryable; guest code is refused (fail closed). Distinct from `LANDLOCK_UNAVAILABLE`. | acos-art / acos-cli | 1 / 2 |
| `GUEST_EXITED` | The guest process ended before the requested lifecycle stage. Class `harness` (needs triage), not retryable. `error.details.session_exit` carries `guest_exit`, `guest_signal`, `stage` and the guest's own `ACOSLAUNCH` crash line; a SIGSYS/exit-126 with a guard `ERR` re-classifies to `SECCOMP_UNAVAILABLE`, and a SIGSEGV/ABRT re-classifies to `APP_CRASHED`. | acos-art / acos-cli | 1 / 2 |
| `INTERNAL` | ACOS itself failed (distinct from an app crash). Class `harness`. Only genuine harness defects and unregistered producer codes (the hint names the code). | acos-cli / harness | 1 |

## Codes outside the protocol enum

`acos-art` (real-ART harness) and `acos-arm64` have their own error enums and
emit these strings in `acos-art: <CODE>: ...` form. They are stable, but not
part of `acos-protocol::ErrorCode` yet; a consumer that branches on them must
match the string. `acos-art` exits 2 for all of them.

| Code | Meaning | Component | Exit |
|---|---|---|---|
| `MEMORY_CGROUP_UNAVAILABLE` | The fail-closed memory/pids cgroup could not be created. | acos-art | 2 |
| `UNSUPPORTED_SYSCALL` | The guest guard observed a denied syscall during a run (structured run result; the protocol code is `SECCOMP_UNAVAILABLE`). | acos-art | 1 |
| `ARM64_UNAVAILABLE` | The arm64 qemu transport (qemu, bionic sysroot, or guard) could not be assembled. | acos-arm64 | 1 |

## Failure class and the ART code mapping (W46)

`ProtocolError` carries `class` next to `code`/`retryable`/`hint`. The class is
derived from the code (`ErrorCode::class()`), always serialized, and restored
from the code on deserialization, so a code can never serialize with the wrong
owner. Values:

| `class` | Owner | Examples |
|---|---|---|
| `assertion` | The check or the app's observed behaviour | `ASSERTION_FAILED`, `ELEMENT_NOT_FOUND`, `ELEMENT_OFFSCREEN`, `TREE_TRUNCATED` |
| `app_crash` | The app process | `APP_CRASHED`, `JAVA_EXCEPTION`, `APP_NOT_RESPONDING` |
| `unsupported` | A capability the engine/host cannot provide (never a pass) | every `UNSUPPORTED_*`, `SECCOMP_UNAVAILABLE`, `LANDLOCK_UNAVAILABLE` |
| `harness` | The ACOS harness itself, or a failure that still needs triage | `INTERNAL`, `HOST_PANIC`, `GUEST_EXITED` |
| `infra_timeout` | A wall/CPU/log deadline | `TIMEOUT`, `NO_FRESH_FRAME` |
| `host_guard` | A host resource guard | `HOST_DISK_FULL`, `HOST_MEMORY_LIMIT`, `HOST_MEMORY_LOW`, `RAM_GUARD`, `RESOURCE_LIMIT` |

`acos-cli` maps every launch/reply code from `acos-art` and every fact in
`<artifacts>/art-frames/session-exit.json` (`art_code`, `stage`, `guest_exit`,
`guest_signal`, `guest_crash`, `cgroup`, `disk_free_bytes`, `panic`) through one
table (`crates/acos-cli/src/failure.rs`). A lost session is classified from the
record before the generic `INTERNAL: session exited` is used; an art code that
is not in the table becomes `INTERNAL` **with a hint naming that code** (a named
harness gap). `session-exit.json` is surfaced in `verdict.json` and
`engine.json` (`session_exit`) and in failure `error.details.session_exit`.

### Engine capability notes (W22)

* `UNSUPPORTED_BACKEND` for the ART engine now names only the capabilities that
  are genuinely absent: `environment` (`set_*` steps) and `input.rotate`. Logs,
  reset, key/back, `type_text` (including `clear: true`) and crash reports are
  implemented; a missing one is still a named capability, never a fallback.
* Every `verify` verdict and envelope carries `trust` (`trusted` / `untrusted`,
  or `unresolved` when the scenario's `trust:` value is invalid),
  `certification: "certified-with-limits"`, `certification_limits`
  (`{"doc": "docs/SANDBOX_CERTIFICATION.md", "section": "§8.7"}`) and
  `certification_policy` (§8.8). Step artifacts carry `trust` and
  `certification` as well.
* `assert_log` on the ART engine reads the guest's own logd sink
  (`logd`/logcat). Its provenance is **guest-asserted**, recorded as
  `steps[].provenance.logs = "guest-asserted"`; an untrusted run refuses the
  channel with `INSUFFICIENT_HOST_EVIDENCE`. A record's `seq` is the entry's
  absolute logd ordinal (+ a session offset that survives `reset`).
* `assert_log.timeout_ms` waits for the line in real time (poll, backing off to
  100 ms) and is the `wait_log` semantic; on deadline the assertion fails, never
  passes.
* `reset` on the ART engine is host-observed: the engine stops the guest and
  relaunches the same APK in a fresh `acos-art` process with an empty app-data
  directory (fresh process, fresh Activity, cleared state). It never taps the
  app's own Reset control, and the engine's frame/log sequences stay monotonic
  across the relaunch so post-reset evidence is provably newer.

Adding a new code: add the `ErrorCode` variant + `as_str` + hint + `class()` +
`ALL_ERROR_CODES` (and `default_status`/`retryable` if not the defaults),
regenerate the schemas (`UPDATE_SCHEMA=1 cargo test -p acos-protocol
schemas_are_up_to_date`), add one row here, and (for an `acos-art` code) add the
string to the explicit table in `crates/acos-cli/src/failure.rs` and to
`EXISTING_CRATE_CODES` so the coverage test enforces it.
