# 1 — Philosophy, product definition and task-to-capability mapping

## Why ACOS exists

Android is traditionally a *device first* platform. Ordinary emulators boot a device, initialize its framework and graphics stacks, then ask apps to run in that environment. ACOS reverses the relationship: an agent submits an execution or verification task; ACOS constructs the lowest-cost environment that can **correctly** execute it.

A test task should **not** silently get its own Android user, container, default display, heavyweight desktop UI, or unrelated system services. A simple UI assertion should not force a virtual camera or full notification UI. A service test should not allocate a display. In contrast, a real Android permission test needs the actual permission semantics; faking them to save memory would invalidate the result.

## Core principles

1. **Task, not device.** The execution unit is an agent job with specified evidence and state semantics.
2. **Pay only for what is used.** Defer capabilities, buffers, processes and assets until necessary. Make additions measurable.
3. **One shared core, many workers.** Share immutable code, compiled artifacts, API metadata and render infrastructure. Workers remain independently addressable.
4. **Isolation is a dial, not a binary default.** Explicit `shared`, `process`, `sandbox`, `core` modes. Sharing across task workers is consented to, never assumed for secret/account data.
5. **Display optional.** Zero surfaces for non-visual work, offscreen surfaces for UI verification, no host desktop window by default.
6. **Correctness has priority over speed.** Test and compare to Android reference behavior; refuse, escalate or label simulations that are not faithful.
7. **Agent-native instrumentation.** Stable, structured, typed APIs for UI, input, events, screenshots, state, performance, crash, ANR and diff; no scraping logcat prose as the primary interface.
8. **Determinism.** Control time/network/location/sensors when available, record random seeds, virtual time and scenario inputs, and make reruns reproduce failures.
9. **Zero silent compatibility holes.** Don't return success on unsupported Android behavior; expose missing API/feature in structured output.
10. **No arbitrary N=5 ceiling.** Display/session count is determined empirically by available resources and supported modes.
11. **Performance is end-to-end.** Judge code-change-to-verified-evidence latency and throughput, not isolated boot times alone.
12. **Security by construction.** Untrusted APKs cannot run with host-root-equivalent privileges. All isolation boundaries must be explicit and independently reviewed.

## Execution levels and what 'supported' means

| Level | Backend | Examples | What its success proves |
|---|---|---|---|
| H0 | Host tooling | compile, lint, manifest parsing, unit tests | only those checks, not Android runtime behavior |
| H1 | Source-aware host renderer/simulation | UI snapshots, isolated Compose/View fixtures | source-component rendering, NOT app runtime integration |
| H2 | ACOS native Rust runtime, shared app/task state | quick visual and flow checks for cooperative/debuggable apps | actual app instructions and native runtime behavior under supported APIs; not independent data |
| H3 | ACOS native Rust runtime, separate sandbox/process/data | repeated independent users, same APK version in many sessions | stronger state separation (subject to threat-model audit) |
| H4 | AOSP/ACVM oracle or dedicated device | semantics not yet implemented, OEM APIs, full device tests | the actual selected reference runtime or physical device behavior |

The **native engine is the strategic product**. H4 is a transparent correctness oracle/fallback while H2/H3 are built, not an excuse to never implement the native engine. Output must include `backend`, `fidelity`, `capabilities`, `fallback_reason` when applicable. Native success statistics exclude fallback successes.

## Required agent tasks and Android capabilities

| Agent activity | Minimum execution | Capabilities required | Expected result |
|---|---|---|---|
| Compile Kotlin/Java | H0 | JDK/Gradle/SDK compiler | build output + errors |
| Inspect APK, manifest, signature, resources, DEX | H0 | ZIP/APK parser, AXML, resources.arsc, DEX verifier | dependency graph + compatibility report |
| Unit-test business logic | H0 | JVM/unit test dependencies | test assertions |
| Render one Compose/View component | H1, then H2 oracle check as needed | host renderer, fonts, layouts, resource resolution | PNG + diff and fidelity label |
| Check theme/dark mode, font scale, density | H1/H2 | resources, configuration, layout/renderer | screenshots with per-config results |
| Open an actual APK / activity | H2 | DEX VM, framework APIs, lifecycle, resources | first live frame and event trace |
| Navigate, tap, type, scroll, back | H2 | input routing, event loop, activity/task manager | observed state transitions |
| UI semantics/accessibility verification | H2 | text, bounds, roles, actions, stable IDs | structured UI tree with text |
| Screenshot and compare after change | H2 | offscreen renderer, frame sequence, image diff | fresh PNG + visual difference report |
| Launch same APK on multiple displays | H2 | activities/tasks, independent input focus | parallel tasks, shared state labeled |
| Test separate logins/databases | H3 | per-session app data, process/UID isolation | independent accounts, no leakage |
| Test two conflicting APK builds | H3 | versioned package/resolution context | side-by-side independently installed builds |
| Test SharedPreferences/SQLite/file I/O | H2/H3 | filesystem APIs, storage namespaces | persistence/reset behavior |
| Test HTTP, websockets, TLS | H2/H3 | network stack, DNS, TLS, optional fake server | deterministic network evidence |
| Test WebView/javascript | H2 + web engine | HTML engine, renderer, network, JS interface | page/bridge behavior |
| Test JNI/C/C++ and ARM-only native libs | H2 + native ABI backend | JNI ABI, ELF, Bionic compatibility, ABI translation if required | native method results / crash fidelity |
| Test Flutter/RN/Unity | H2 + framework/native support | engine libs, JNI, graphics, event loops | full app UI/functionality |
| Test permission requests | H2/H3 if implemented; otherwise H4 | package/permission policy, UI, lifecycle | correct allow/deny/persist semantics |
| Test notification, alarm, jobs, background | H2 + capability services | corresponding schedulers, background restrictions | events and observable effects |
| Test virtual camera, GPS, motion, audio | H2 + selected modules | virtual HAL, stream injection, timestamps | deterministic synthetic device events |
| Test Bluetooth/NFC/SIM/SMS | selected modules or H4 | faithful Android APIs and device state | correct outcomes or explicit unsupported |
| Test process death, ANR, recreation | H2/H3 | lifecycle, task restore, watchdog, process manager | recorded deterministic failure & recovery |
| Profile heap, allocations, startup, frame time | H2/H3 | runtime profiler, code maps, renderer counters | per-task profile + trace |
| Test hardware-specific DRM/integrity/OEM | H4, sometimes real device | genuine keys/hardware/proprietary service | accurately scoped device-level result |
| Run CI against many APKs | H2/H3/H4 | scheduling, queueing, evidence artifact store | parallel verdicts and resource/cost accounting |

## Target app compatibility corpus

Build a versioned, redistributable **legal test corpus**, first with self-owned debug fixtures and open-source APKs; later compare a broad selection of real apps with user-provided APKs when authorized. Include:
- Java Views and Kotlin Activities; background worker; app lifecycle and process death.
- Compose navigation, Material components, custom fonts, animations.
- Flutter and React Native.
- WebView app; multi-process app with content providers and BroadcastReceivers.
- Native NDK JNI apps; ARM-only and x86_64-native libraries.
- Multimedia, SurfaceView/OpenGL/Vulkan, camera/mic permission flows.
- Apps with multiple launch modes, task affinities, document mode and `noHistory`.
- Split APKs and multiple DEX files.
- Apps relying on GMS/DRM/integrity separately labeled for licensed/device-dependent compatibility.

Supported API level and ABI must always be explicit, alongside `compatible / limited / unsupported / escalated`, observed crashes and missing functions.

## 'All Android APKs' — project commitment and boundary

**Commitment:** maximum Android APK compatibility is an explicit long-term objective, not an optional feature to drop when hard. However, not all APKs have the same requirements. Some depend on licensed services, proprietary native libraries, particular chipsets, device keystores, OEM APIs or integrity-attested physical devices. Do not promise those without the prerequisites. For normal Android SDK/NDK apps targeting an ACOS-supported API/ABI, strive for complete behavior compatibility and track coverage per feature and app class. If native support is incomplete, mark it honestly and optionally run an explicit reference-backend test, preserving the native coverage score.

## Success means closing the verification loop

A complete job is: change source -> build an artifact -> select cheap correct engine -> launch it -> execute interactions -> assert behavior -> produce reproducible evidence -> reset/cleanup. If no app launch is required, skip it. If a true APK must execute, make the first frame fast and evidence direct. A task that returns a false pass is worse than a slower task that produces a correct refusal or failure.
