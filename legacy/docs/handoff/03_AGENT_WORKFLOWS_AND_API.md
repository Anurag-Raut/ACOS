# 3 — Extensive agent workflows and the verification-loop API

**All commands/YAML schemas below are DESIGN PROPOSALS. They are not currently working ACOS commands.** They define the experience our implementation must eventually provide. The orchestrator should refine names only when preserving the fundamental task-first behavior.

## Standard response envelope

Every action returns machine-readable JSON, with stable IDs, causal sequencing and honest fidelity:

```json
{
  "job_id": "job_...",
  "session_id": "s_...",
  "request_id": "req_27",
  "status": "pass",
  "backend": "acos-native",
  "fidelity": "real-apk-native-runtime",
  "isolation": "sandbox",
  "capabilities_used": ["lifecycle", "ui.offscreen", "input", "storage"],
  "frame": {"display_id": "d_1", "sequence": 19, "fresh_after_request": "req_27", "timestamp_ns": 0, "sha256": "..."},
  "evidence": {"screenshot": "artifacts/19.png", "tree": "artifacts/19.json", "events": "artifacts/events.jsonl"},
  "metrics": {"wall_ms": 0, "peak_rss_bytes": 0, "cpu_ms": 0}
}
```

Values shown are schema examples, not measured results. `status` is `pass | fail | unsupported | escalated | error`; never call unsupported a pass. `backend` records ACOS native vs ACVM reference vs physical device.

## Workflow A — AI changes a Compose screen and verifies it

**Agent intent:** “Make the sign-in button full width and ensure it stays visible above the keyboard on a small screen.”

Desired sequence:

```bash
# 1. The coding agent edits its app source using its usual tools.
./gradlew :app:assembleDebug

# 2. ACOS statically discovers capabilities and selects the lightest valid execution mode.
acos inspect app/build/outputs/apk/debug/app-debug.apk --json > artifacts/requirements.json

# 3. Run real APK UI. No simulated phone launcher, no unnecessary Android user.
acos run --apk app/build/outputs/apk/debug/app-debug.apk \
  --activity com.example/.LoginActivity \
  --display offscreen:360x800 --isolation process --profile dev

# 4. Observe, interact, inspect, assert.
acos observe s_login --tree --text --json > artifacts/login.tree.json
acos action s_login tap --selector 'id=com.example:id/email'
acos action s_login type --text 'test@example.com'
acos action s_login tap --selector 'id=com.example:id/password'
acos action s_login type --text 'secret'
acos assert s_login --visible 'id=com.example:id/signIn'
acos capture s_login --fresh --after-action --output artifacts/login-keyboard.png
acos inspect-layout s_login --no-clipping --touch-targets --report artifacts/layout.json

# 5. Return machine-readable pass/fail, evidence and a reproducer to the coding agent.
acos destroy s_login
```

Expected result: task runtime and fidelity are known; screenshot is fresh; keyboard did not hide the button; no clipping; action history and app crashes attached. If `Compose` or IME behavior is not yet supported in native mode, ACOS must say so, then optionally invoke the reference backend with a distinct `escalated` status.

## Workflow B — Screenshot regression across an AI-generated patch

**Agent intent:** “Refactor Settings. Verify the appearance didn't accidentally change.”

```bash
acos verify visual --baseline-apk builds/before.apk --candidate-apk builds/after.apk \
  --screen com.example/.SettingsActivity \
  --viewport 393x851 --density 2.75 \
  --theme light --font-scale 1.0 \
  --output artifacts/settings-diff/
```

ACOS launches each build in a reproducible context, applies the same inputs/state, captures fresh frames, measures changes (pixel diff and optionally perceptual regions), reports text/layout/contrast changes, and links before/after/diff images. The agent can reason about intentional changes, but must not automatically rewrite an approved golden screenshot unless instructed.

## Workflow C — One core, many different apps in parallel

**Agent intent:** “Check Login in app A, Playback in app B, and Preferences in app C simultaneously.”

```bash
acos group create --name pr-184 --core shared --parallel 3
acos group run pr-184 --apk builds/a.apk --activity .Login --display offscreen --isolation process
acos group run pr-184 --apk builds/b.apk --activity .Playback --display offscreen --isolation process
acos group run pr-184 --apk builds/c.apk --activity .Preferences --display none --isolation process
acos group verify pr-184 --scenario scenarios/pr184.yaml --artifacts artifacts/pr184
acos group destroy pr-184
```

App C can remain headless when its test exercises storage or logic only; the engine should not allocate a display just because the other apps need one. All jobs get independent event streams and resource counters.

## Workflow D — Same APK, many isolated test accounts

**Agent intent:** “Run five separate login/session tests without account/data leakage.”

```bash
acos run --apk builds/app-debug.apk --name user-a --isolation sandbox --data ephemeral --display offscreen
acos run --apk builds/app-debug.apk --name user-b --isolation sandbox --data ephemeral --display offscreen
# ...more users, subject to RAM guard...
acos action user-a type --selector '@email' --text 'a@example.test'
acos action user-b type --selector '@email' --text 'b@example.test'
acos assert user-a --text 'Account A'
acos assert user-b --text 'Account B'
acos isolation-check user-a user-b --report artifacts/isolation.json
```

These are separate app **data identities**, not automatically Android `UserHandle`s. This is only legitimate after sandboxed storage, service caller identity, permissions, providers, native file access and network/process boundaries are proved. If independence is not implemented for a given APK, ACOS must escalate rather than silently use shared state.

## Workflow E — Many activities of one app with shared state (cheap mode)

**Agent intent:** “Preview five routes from the same app with a seeded fixture; no isolation required.”

```bash
acos run --apk builds/app.apk --name preview-shared --isolation shared-task --display offscreen
acos task launch preview-shared --activity .HomeActivity --display offscreen:360x640
acos task launch preview-shared --activity .AnalyticsActivity --display offscreen:360x640
acos task launch preview-shared --activity .SettingsActivity --display offscreen:360x640
acos capture-all preview-shared --fresh --output artifacts/routes/
```

Expected extremely cheap additional views, but label `shared process / shared static state / shared app data`. Some apps use singleTask/singleInstance/noHistory/process-global navigation and cannot be split safely: detect and refuse or switch to isolated mode.

## Workflow F — Two APK versions of the same package

**Agent intent:** “Compare branch `main` and PR branch without reinstalling over the other.”

```bash
acos compare --base builds/app-main.apk --candidate builds/app-pr.apk \
  --scenario scenarios/payment-flow.yaml --parallel --isolation sandbox \
  --artifacts artifacts/compare-payment
```

Both versions must keep their code/resources/data separate. If native ACOS package identity support is incomplete, choose separate cores or the reference backend transparently, not silently replace the first APK.

## Workflow G — JNI and native libraries

**Agent intent:** “Check a JNI image-filter function and preview its output.”

```bash
acos inspect builds/image-app.apk --native-libs --abi-map --json
acos run --apk builds/image-app.apk --isolation sandbox --display offscreen
acos test --session s_image --native-method 'com.example.Filter.apply' \
  --fixture fixtures/picture.png --assert-image fixtures/expected-filter.png
acos events s_image --kind native-loader,jni,crash --json
```

This is a hard engineering gate for the Rust runtime. Native `.so` files still use the standard JNI C ABI; the Rust engine must correctly implement `JavaVM`/`JNIEnv`, heap references, ABI symbol lookup and native threads. If ARM-only binary translation is unsupported on the target host, report exact missing ABI and, where possible, run on an ARM64 machine or the reference backend.

## Workflow H — Flutter, React Native and WebView

**Agent intent:** “Verify a Flutter checkout screen and React Native onboarding state; test an embedded WebView login.”

```bash
acos test suites/flutter-checkout.yaml --apk builds/flutter.apk --artifacts artifacts/flutter
acos test suites/react-native-onboarding.yaml --apk builds/rn.apk --artifacts artifacts/rn
acos test suites/webview-login.yaml --apk builds/webview.apk --network fixture://auth-server \
  --artifacts artifacts/webview
```

Do not pass merely because an Activity started. Require visible meaningful frames, correct text, touch, navigation, network, errors and native modules. Real Flutter and RN engines include native graphics and JNI/platform integration; support is not just DEX coverage.

## Workflow I — Deterministic network and backend errors

**Agent intent:** “Verify a 429 retry, an offline state and a 500 error screen.”

```bash
acos run --apk builds/app.apk --network controlled --clock fixed:2026-11-10T10:00:00Z
acos network rule s_api 'GET /user' --respond 429 --json fixtures/rate-limit.json
acos assert s_api --text 'Try again'
acos network offline s_api --on
acos assert s_api --text 'You are offline'
acos network offline s_api --off
acos network rule s_api 'GET /user' --respond 500
acos assert s_api --event 'error.http.500'
```

Test-network features are opt-in, bounded and visibly reported. A fake response proves app handling of that case, not real production-network behavior.

## Workflow J — Permission, camera, GPS, notifications

**Agent intent:** “Test location-permission denial and verify map fallback without starting a real camera/SIM stack.”

```bash
acos run --apk builds/map.apk --capability location.virtual,permissions --display offscreen
acos permission s_map deny android.permission.ACCESS_FINE_LOCATION
acos assert s_map --text 'Enable location'
acos permission s_map grant android.permission.ACCESS_FINE_LOCATION
acos sensor s_map location --lat 19.1 --lon 72.9
acos assert s_map --text 'Nearby'
```

When the Android permission UI, background restrictions or location provider semantics are not faithful, mark this task `simulated` or `unsupported` and run the AOSP oracle for real verification. Do not claim full Android behavior just because a mock API returns success.

## Workflow K — App crash, ANR, process death and recovery

**Agent intent:** “Check app restores route and draft text after process death.”

```bash
acos run --apk builds/app.apk --display offscreen --isolation sandbox
acos action s_draft type --selector '@draft' --text 'unsent draft'
acos lifecycle s_draft kill-app-process
acos lifecycle s_draft restart
acos assert s_draft --text 'unsent draft'
acos events s_draft --kind crash,anr,process,activity --json
```

Provide structured stack traces, app lifecycle state transitions and whether behavior reflects actual Android semantics, not just raw Linux process killing.

## Workflow L — Headless data/back-end behavior, no screen

**Agent intent:** “Test SQLite migration / encryption / cache eviction in an app without visual interaction.”

```bash
acos run --apk builds/app.apk --display none --isolation sandbox \
  --capability storage,network
acos invoke s_db --test-endpoint 'migrateDatabase'
acos assert s_db --sql 'SELECT version FROM schema_metadata' --equals 6
acos capture s_db --fresh   # should return NO_DISPLAY instead of allocating one
acos destroy s_db
```

Do not invent magical invocation of arbitrary private methods in non-debug APKs. `invoke --test-endpoint` is available only for source-owned/debuggable builds with explicitly injected test hooks. For APK-only, use exported activities/receivers/providers or supported public interfaces.

## Workflow M — Agent's entire PR verification loop

**Agent intent:** “Make a requested code change, prove it worked, and attach a reproducible PR report.”

```bash
acos devloop --project . --build './gradlew :app:assembleDebug' \
  --apk app/build/outputs/apk/debug/app-debug.apk \
  --scenario scenarios/pr-regression.yaml \
  --output artifacts/pr-regression --json
```

ACOS records build time, install/setup, app start, interaction-to-frame, assertions, crash events, reset, cleanup and peak/cumulative RAM. It returns a single result with per-step timings, reference comparisons and artifact paths. The coding agent decides whether to amend the code and rerun.

## Workflow N — CI and massively parallel regression testing

```bash
acos test scenarios/release-smoke.yaml --apk artifacts/app-release.apk \
  --parallel 12 --max-memory-percent 80 \
  --output ci/android-verification/ --report junit,json,html
```

Scheduler honors actual resource budgets. Independent sessions never share secrets. App capability requirements determine whether each job runs in shared-view, isolated native worker, separate core or explicit reference backend. If 12 jobs don't fit, queue rather than OOM. Track useful verified completions per GiB-minute, not simply number of started jobs.

## Workflow O — APK-only third-party smoke test

**Agent intent:** “Here's an APK. Tell me whether it launches, draws and navigates without source code.”

```bash
acos inspect third-party.apk --json
acos run --apk third-party.apk --display offscreen --isolation sandbox --fidelity strict
acos observe s_third --tree --text --json
acos capture s_third --fresh --output output/first.png
acos report s_third --compatibility --missing-features --crashes --json
```

No recompilation, manifest editing or hidden injection unless APK owner explicitly consents. If unsupported, output exact failing service/API/native symbol and issue a reference-backend test only when opted in; maintain separate compatibility results.

## Suggested scenario YAML

```yaml
version: 1
name: login-and-navigation
app: app/build/outputs/apk/debug/app-debug.apk
engine: native-preferred
fallback: report-and-escalate
api_level: target-selected-by-project
isolation: sandbox
display:
  mode: offscreen
  width: 393
  height: 851
  density: 2.75
capabilities:
  - ui
  - input
  - storage
steps:
  - launch: {activity: "com.example/.LoginActivity"}
  - wait_for: {selector: "id=com.example:id/email", timeout_ms: 5000}
  - type: {selector: "id=com.example:id/email", text: "test@example.test"}
  - type: {selector: "id=com.example:id/password", text: "test-password"}
  - tap: {selector: "text=Sign in"}
  - assert_visible: {text: "Welcome"}
  - screenshot: {name: "welcome", fresh_after_last_action: true}
  - assert_no_crashes: true
  - reset: {scope: app}
  - assert_clean_state: true
output:
  traces: true
  ui_tree: true
  screenshots: true
  resource_metrics: true
```

## First-class failure semantics

- `PASS`: correct assertions under stated fidelity, fresh evidence, correct app execution.
- `FAIL`: task ran but assertions or correctness checks failed; explain repro steps.
- `UNSUPPORTED`: native engine lacks required API, ABI, native symbol, feature or behavior; **not** counted as pass.
- `ESCALATED`: fallback executed in reference backend; label which backend actually provided the verdict.
- `ERROR`: test harness/internal engine failure (timeout, image not produced, crash of ACOS itself), separate from an app crash.
- `INCONCLUSIVE`: cannot establish behavior (e.g., insufficient reference, nondeterministic animation, no stable screenshot); not pass.

**Rule:** An agent must be able to tell *what actually ran, which code and API level were used, which app state was isolated, how the screenshot was linked to its action, whether the test was native or fallback, and what it cost*. That is what makes the OS agent-first.
