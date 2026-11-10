# ACOS scenarios

YAML scenarios are the agent's reproducible verification unit. The format is
defined and validated by `acos-protocol` (`cargo test -p acos-protocol` parses
every file here). The runtime contract is in
[`docs/AGENT_API.md`](../docs/AGENT_API.md).

Run the two fixtures (build the APKs first with `tools/build-apps.sh`):

```bash
acos verify --apk fixtures/build/apps/hello-views.apk \
  --scenario scenarios/hello-views.yaml \
  --json --artifacts out/hello-views

acos verify --apk fixtures/build/apps/hello-jni.apk \
  --scenario scenarios/hello-jni.yaml \
  --json --artifacts out/hello-jni
```

Both runs must produce `verdict.json` with `verdict: pass` **and** a complete
artifacts directory (`steps/`, `frames/`, `tree/`, `events.jsonl`,
`resources.json`, `repro.*`). `acos-protocol`'s `validate_artifacts_dir` rejects a
claimed pass whose evidence is missing.

## Golden values

Every expected value below is from the ART reference captures in
`fixtures/reference/*/art-api34-x86_64/`, not from a JVM or a guess.

* Display: **1080x2400 @ 420 dpi**, locale `en-US`, light theme, font scale 1.0.
* APK SHA-256:
  * `hello-views.apk` — `05896b5cd534b0857c13ca5baec8b131c905b3a908bbbaad4ee05db9f21b007d`
  * `hello-jni.apk` — `c37a48e0620659fd8b68cb51f1b63d05a149d21fa3e14d9f3487bec102841018`

### hello-views

| id | initial text | initial bounds |
|---|---|---|
| `title` | `Hello ACOS` | `[42,317][231,368]` |
| `input` | `Enter your name` (hint) | `[42,368][1038,486]` |
| `btn` | `Greet` | `[42,486][273,612]` |
| `result` | `clicks: 0` | `[42,612][175,663]` |

After typing `World` and tapping `Greet`:

* click 1 — `result` = `Hello, World! clicks: 1`, bounds `[42,612][392,663]`
* click 2 — `result` = `Hello, World! clicks: 2`, same bounds
* rotate to landscape (activity recreated) — `result` = `clicks: 2`, bounds
  `[170,526][303,577]`; `input` still `World`, bounds `[170,282][2358,400]`
* rotate back to portrait — `result` = `clicks: 2`, bounds `[42,612][175,663]`

Lifecycle (`AcosFixture`): `onCreate → onStart → onResume` on launch;
`onPause → onStop → onSaveInstanceState clicks=2 → onDestroy → onCreate →
onStart → onResume` per recreation; Back produces
`onPause → onStop → onSaveInstanceState clicks=2 → onDestroy`.

### hello-jni

The report TextView (no resource id; selected by class + bounds
`[0,275][1080,2337]`) contains exactly:

```text
stringFromJNI=Hello from native C
add(20,22)=42
sumArray([1,2,3,4,5])=15
javaCallbackResult=42
voidCallback=from-native
registeredEcho=echo!
throwFromJNI caught=thrown from native
```

Log (`AcosJniFixture`): `onCreate`, `javaCallback x=21`,
`voidCallback s=from-native`.

## Negative scenarios

Files under `negative/` are valid scenarios that **must not pass**. They exist to
prove false passes are impossible:

| file | expected failure |
|---|---|
| `wrong-text.yaml` | `ASSERTION_FAILED` — wrong expected string |
| `missing-element.yaml` | `ELEMENT_NOT_FOUND` / `ASSERTION_FAILED` — selector matches nothing |
| `wrong-bounds.yaml` | `ASSERTION_FAILED` — correct text, wrong bounds |
| `wrong-lifecycle-order.yaml` | `ASSERTION_FAILED` — impossible ordered log sequence |
| `missing-jni-line.yaml` | `ASSERTION_FAILED` — one of seven JNI lines is wrong |

They set `expectation: fail`. A runner must compare this with the actual verdict:

* `expectation: pass` — verdict must be `pass`.
* `expectation: fail` — verdict must be exactly `fail` (the app ran and a check
  did not hold). A `timeout`, `error`, `crash` or `unsupported` verdict does **not**
  satisfy a negative test; it means the test did not run correctly.
* `expectation: non_pass` — any non-pass verdict is acceptable.

If a `negative/*.yaml` scenario passes, the runner itself is broken and the
result must be reported as a harness failure, never as a passing test.

## Step semantics that matter

* `wait_for` and every `assert_*` step are **event-driven**. The runtime waits for
  a new frame/tree/log event. There is no sleep in the format and polling with
  fixed delays is not conformant.
* `assert_log.since_previous_step: true` scopes the regex sequence to lines
  produced by the previous step. Without it, an ordered sequence is matched over
  the whole session log.
* `capture.fresh: true` (default) requires a frame produced after the request.
  A stale frame can never satisfy it.
* Assertions on text/bounds verify the exact ART-reference values; if the
  runtime's layout differs, the scenario fails with a structured diff rather
  than silently accepting a near match.
