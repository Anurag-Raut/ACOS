# ACOS app reference evidence

Authoritative **`art-reference`** evidence for the ACOS fixture APKs, captured
by running the real apps on **real ART** — not a JVM, not a mock.

- **Fidelity label: `art-reference (emulator, API 34, google_apis x86_64)`.**
- **Device:** AVD `acos-oracle-34`, Android 14 (API 34), `google_apis` x86_64
  system image, qemu `qemu-system-x86_64-headless` under KVM, 2048 MB RAM,
  4 cores, `-gpu swiftshader_indirect`, headless.
- Display `1080x2400`, density `420 dpi`, locale `en-US`, font scale default
  (1.0), light mode.
- Captured with `tools/oracle/capture-app-reference.sh` while the emulator runs
  (`tools/oracle/emulator.sh start`). Every artifact is listed with a SHA-256 in
  the per-app `manifest.json`.

These are the **golden assertions** for ACOS's Activity/render milestones: a
correct ACOS runtime must reproduce the texts, bounds and lifecycle order below,
not the host-JVM behaviour.

## Layout

```
fixtures/reference/<app>/art-api34-x86_64/
  manifest.json            artifact list (size+sha256), device config, commands
  device-config.json       resolution, density, locale, font scale, dark/light
  am_start_initial.txt     `am start -W` cold-start timings
  screenshot_*.png         `adb exec-out screencap -p`
  ui_*.xml                 `uiautomator dump` raw hierarchy
  ui_*.json                parsed nodes (class/id/text/bounds/clickable/focused)
  observed-texts-*.txt     human-readable key nodes per state
  logcat_*.txt             `AcosFixture` / `AcosJniFixture` log lines
  lifecycle-summary.txt    (hello-views) lifecycle order
  meminfo_initial.txt      `dumpsys meminfo` PSS breakdown
  display.txt              `dumpsys window displays`
  commands.txt             exact commands used
```

## hello-views — `dev.acos.fixtures.helloviews`

Initial state (portrait):

| id | class | text (initial) | bounds |
|---|---|---|---|
| `title` | TextView | `Hello ACOS` | `[42,317][231,368]` |
| `input` | EditText | hint `Enter your name` | `[42,368][1038,486]` |
| `btn` | Button | `Greet` | `[42,486][273,612]` |
| `result` | TextView | `clicks: 0` | `[42,612][175,663]` |

Interaction (type `World` into `input`, tap `btn`):

- after click 1: `result` = **`Hello, World! clicks: 1`** `[42,612][392,663]`
- after click 2: `result` = **`Hello, World! clicks: 2`** `[42,612][392,663]`
- after rotate to landscape and back (activity recreated):
  `result` = **`clicks: 2`** (state restored from `onSaveInstanceState`).

Lifecycle log order (tag `AcosFixture`) — cold start:
`onCreate, onStart, onResume`; clicks log `greet name=World clicks=1/2`;
recreation emits `onPause, onStop, onSaveInstanceState clicks=2, onDestroy,
onCreate, onStart, onResume`; Back emits `onPause, onStop, onSaveInstanceState,
onDestroy`.

## hello-jni — `dev.acos.fixtures.hellojni`

The single `TextView` shows exactly (newline-separated, x86_64 `libhellojni.so`):

```
stringFromJNI=Hello from native C
add(20,22)=42
sumArray([1,2,3,4,5])=15
javaCallbackResult=42
voidCallback=from-native
registeredEcho=echo!
throwFromJNI caught=thrown from native
```

Log lines (tag `AcosJniFixture`): `onCreate`, `javaCallback x=21`,
`voidCallback s=from-native`.

## Fidelity rules

- A host-JVM (`jvm-reference`) result is **never** Android-verified; see
  `results/oracle/jvm-vs-art.md` for the measured JVM↔ART differences.
- A `jvm-reference` pass can never satisfy an `art-reference` requirement
  (DECISIONS.md ADR-0005).
- Cold-start times in `am_start_initial.txt` are informational (KVM emulator),
  not device-grade performance numbers. ART interpreter wall-time reference is
  in `results/oracle/art-perf.json`.
