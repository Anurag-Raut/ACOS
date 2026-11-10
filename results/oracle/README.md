# ACOS ART oracle (art-reference)

Authoritative validation of the ACOS fixture corpus against **real ART** on
Android 14 (API 34, `google_apis` x86_64), captured from a headless KVM
emulator. This supersedes the weak `jvm-reference` expectations for any
Android-semantics claim (DECISIONS.md ADR-0005).

## Pipeline

1. `tools/extract-bootclasspath.sh` — extracts the API-34 boot classpath jars
   (+ system fonts, `framework-res.apk`, `build.prop`) from the installed SDK
   system image into `.cache/boot/android-34/` with no sudo/network/mounts.
2. `tools/oracle/emulator.sh start|stop|status` — creates/boots/stops the
   dedicated `acos-oracle-34` AVD under `.cache/`.
3. `tools/oracle/run-dexprogs-art.sh` — runs all 20 console DEX fixtures on
   real ART (`dalvikvm64 -cp classes.dex Main`), 3× each, capturing exact
   stdout/stderr/exit into `fixtures/expected-art/dexprogs/`.
4. `tools/oracle/compare-jvm-art.sh` — diffs JVM vs ART → `jvm-vs-art.md`.
5. `tools/oracle/art-perf.sh` — on-device wall time reference → `art-perf.json`.
6. `tools/oracle/capture-app-reference.sh` — real-APK evidence →
   `fixtures/reference/`.

## Results

| file | content |
|---|---|
| `jvm-vs-art.md` | per-program JVM↔ART verdict and exact differing lines |
| `dexprogs-runs.json` | ART exit codes + determinism across 3 runs |
| `dexprogs-nondeterminism.txt` | programs whose 3 runs differed (none) |
| `art-perf.json` | `01_hello` wall-time p50/p95 on device; benchmark status |
| `free-before.txt`, `adb-before.txt`, `avd-before.txt`, `container-status-before.txt` | host state recorded before the emulator started |
| `emulator-start.log` | boot wall time + host RSS |

Summary: 19/20 console programs produce **byte-identical** stdout on JVM and
ART; `20_uncaught` differs only because the JVM expectation recorded the first
stderr line while ART emits the full stack trace
(`\tat Main.main(Main.java:14)`). No program was non-deterministic.
