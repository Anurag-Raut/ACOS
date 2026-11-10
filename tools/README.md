# ACOS fixture tooling

Build scripts for the executable fixture corpus. They need the Android SDK
(`ANDROID_HOME`, default `/home/Qui/Android/Sdk`) and OpenJDK 17. Both are
idempotent and write only under `fixtures/build/` (gitignored) plus the
intentionally checked-in `fixtures/expected/`.

| Script | What it does |
|---|---|
| `guarded.sh` | Runs any command under a hard host memory + wall-clock bound (systemd `MemoryMax` / `ulimit -v`, `timeout`), kills the whole process group on exit/timeout/error, exports `ACOS_HOST_RSS_MAX`/`ACOS_WALL_MAX_SECS`, and removes its scratch. **Every** `cargo test`, `acos run-dex`, benchmark or build must go through it. |
| `ramguard.sh` | Host RAM watchdog loop. Kills `target/*/acos*` processes when `MemAvailable` drops below ~9 GiB (~85% used) or a single ACOS process exceeds 8 GiB RSS, logging to `results/tmp/ramguard.log`. |
| `build-fixtures.sh` | Compiles every `fixtures/dexprogs/<name>/` with `javac --release 11`, runs it on the host JVM to capture `fixtures/expected/dexprogs/<name>.{stdout,exit[,stderr]}`, then converts it to DEX with `d8` into `fixtures/build/dexprogs/<name>/classes.dex`. |
| `build-apps.sh` | Builds `fixtures/apps/hello-views` and `fixtures/apps/hello-jni` into signed, zipaligned APKs under `fixtures/build/apps/` using `aapt2 + javac + d8 + zipalign + apksigner` (and NDK clang for the JNI libs). Verifies each with `apksigner verify` and `aapt2 dump badging`. |
| `apk_append.py` | Helper used by `build-apps.sh` to add `classes.dex` / `lib/**` to an `aapt2`-linked APK without a synthetic `META-INF/MANIFEST.MF`. |

Expectations under `fixtures/expected/` are labelled **jvm-reference** (host
OpenJDK 17), not Android/ART-verified. See `fixtures/README.md`.

Device install smoke-testing only happens if `adb devices` already lists a
device; no emulator is ever started by these scripts.

## Host-safety rule (ADR-0018, mandatory)

Host RAM must never exceed 90% (internal target 85%; the host has 62 GiB and no
swap, so exceeding it can hard-crash the machine). Every command that can
execute or build guest code runs under a hard memory and wall-clock bound, and
frees its resources as soon as it is done:

```bash
tools/guarded.sh --mem 4G --time 300 -- cargo test -p acos-vm
tools/guarded.sh --mem 2G --time 60  -- ./target/release/acos run-dex x.dex --main Main
tools/guarded.sh --mem 6G --time 3600 -- tools/bench-vm.sh
```

* `guarded.sh` prefers a systemd user scope (`MemoryMax`, swap off) and falls
  back to `ulimit -v`; it always wraps the command in `timeout`, kills the
  process group on every exit path, and exports `ACOS_HOST_RSS_MAX` (default
  `2G`) and `ACOS_WALL_MAX_SECS` so the VM aborts with the structured errors
  `HOST_MEMORY_LIMIT` / `TIMEOUT` even without the wrapper.
* `tools/build-fixtures.sh` and `tools/build-apps.sh` re-exec themselves under
  `guarded.sh` automatically.
* Run `tools/ramguard.sh &` during a parallel wave. It acts *before* 90% by
  killing ACOS processes once `MemAvailable < 9 GiB` (≈85% used); pass `--once`
  for a single check, `--dry-run` to log without killing.
* Scratch artifacts live under `target/tmp/` (gitignored) and are removed by
  the runner on exit. Do not leave processes, fds, temp dirs or `.cache`
  scratch behind; `ramguard.log` and `guarded.log` are the only retained
  evidence, under `results/tmp/`.

## Running tests

```bash
tools/guarded.sh --mem 4G --time 900 -- cargo test -p acos-vm
tools/guarded.sh --mem 4G --time 900 -- cargo clippy --workspace -- -D warnings
tools/guarded.sh --mem 2G --time 300 -- tools/diff-dexprogs.sh --corelib real
```

`tools/diff-dexprogs.sh`, `tools/bench-vm.sh` and the build scripts invoke
`guarded.sh` themselves; the explicit wrapper above is the rule for ad-hoc
`cargo`/`acos` commands.
