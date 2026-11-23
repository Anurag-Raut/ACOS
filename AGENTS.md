# AGENTS.md

Guidance for AI coding agents and contributors working in the ACOS repository.

## How ACOS works

ACOS is an agent-first Android application execution and verification runtime on Linux (API 34, x86_64). Instead of booting a full Android VM per test, ACOS runs real stock Google ART (`dalvikvm64`), `framework.jar`, `libandroid_runtime`, `libhwui` (CPU Skia), and app native `.so` libraries directly inside an unprivileged Linux `bwrap` sandbox, intercepts `/dev/binder` in userspace via `artshim.so` (`LD_PRELOAD`), and routes every Android system service call over a Unix socket to a lightweight per-session Rust **Session Controller** (`acos-services` + `acos-wm`). Heavy, cross-session infrastructure—read-only framework/DEX/font pages, real internet egress (`acos-netd` + `pasta`), and headless Chromium WebView (`acos-webview`)—runs **once per Host** and is shared safely across concurrent **Sessions**.

Read [`docs/internals/overview.md`](docs/internals/overview.md) and [`docs/internals/glossary.md`](docs/internals/glossary.md) before changing runtime, service, module, or sandbox code.

## Canonical terminology

Use these exact terms across code, docs, and commit messages (see [`docs/internals/glossary.md`](docs/internals/glossary.md)):

| Term | Meaning |
|---|---|
| **Host** | The single Linux machine running ACOS (**always 1 per machine**). Hosts the shared read-only page pool and shared Host daemons (`acos-netd`, `acos-webview`, optional Host Zygote). |
| **Session** | One isolated virtual Android device (private `/data`, `bwrap` namespaces, seccomp/Landlock, optional netns/`veth`, isolated Chromium `BrowserContext`). Many Sessions run in parallel on one Host. |
| **Session Controller** | The per-session Rust daemon (`acos-services` + `acos-wm` + `acos-art` launcher) that answers Binder transactions, composites windows, and speaks CLI/MCP. **Do not call this "the host" or "Rust host"**—reserve **Host** strictly for the Linux machine. |
| **Guest App Process** | One Android PID (`dalvikvm64` + `framework.jar` + app DEX/native `.so`) inside a Session's sandbox. A Session starts with its primary app process and lazily spawns system apps (`permissioncontroller`, `systemui`, `webview`) only when needed. |

## Where code lives

### Active ART, Session, and Host crates

| Crate | Responsibility |
|---|---|
| `crates/acos-art` | ART launcher, `bwrap` mini-Android sysroot, `artshim.c` userspace `/dev/binder` LD_PRELOAD shim, `LaunchMain.java` guest harness, production Zygote (`zygote_prod`), live view producer |
| `crates/acos-services` | Per-session Rust Android system services (`core` + 23 attachable modules, `ModuleRegistry::standard()`, AMS/ATMS/PMS/WMS/IME/Content/Settings/Permissions/Notifications/Sensors/Location/etc.) |
| `crates/acos-wm` | C1 software compositor, window z-order layers, insets (`report` default), frame capture |
| `crates/acos-binder-peer` | Userspace Binder protocol bridge (`BC_*`/`BR_*` peer) between guest `artshim.so` and Session Controller |
| `crates/acos-parcel` / `crates/acos-aidl` | Android Binder `Parcel` wire codec and AIDL definitions |
| `crates/acos-netd` / `crates/acos-netd-ctl` | Host-shared network relay daemon (1 `pasta` + relay netns per Host, per-session netns + `veth`, netfilter ACLs, `dnsproxyd`) |
| `crates/acos-webview` | Host-shared WebView broker (1 `chrome-headless-shell` per Host in its own systemd scope, per-session `BrowserContext`, fair `beginFrame` queue in `src/fair.rs`) |
| `crates/acos-gles` / `crates/acos-gles-provider` | Software GLES2/EGL substrate (`angle-swiftshader` read-only `.so` + Rust GLES path) |
| `crates/acos-sandbox` / `crates/acos-seccomp-guard` | Fail-closed confinement (user/mount/pid/net namespaces, seccomp, Landlock, rlimits); cannot be disabled via env vars |
| `crates/acos-cli` | `acos` CLI (`launch`, `verify`, `observe`, `tap`, `view`, `doctor`, `bench`, `serve`) |
| `crates/acos-mcp` | MCP server (`acos-mcp`), closed tool schemas, embedded `docs/AGENT_GUIDE.md`, session lifecycle |
| `crates/acos-protocol` | Typed V1 JSON/MCP request/response/verdict/error contracts and generated JSON Schemas |
| `crates/acos-verify` | Scenario parser and verifier |
| `crates/acos-apk` / `crates/acos-dex` | Static APK (ZIP, AXML, `resources.arsc`, signing) and DEX 035–039 parsers |
| `crates/acos-sfgeom` / `crates/acos-sqlite` / `crates/acos-test-support` | AOSP SurfaceFlinger geometry helpers, vendored SQLite boundary, shared test utilities |

### Frozen legacy crates (do not add features)

`crates/acos-vm`, `crates/acos-runtime`, `crates/acos-render`, `crates/acos-jni`, `crates/acos-native-loader`, `crates/acos-framework-natives`, `crates/acos-arm64` belong to the frozen pre-ART Rust-VM track. Keep them compiling, but do not build new product features on them.

## Documentation

- **`docs/` is the single source of truth** for current architecture, contracts, and maintainer guides. Start from [`docs/README.md`](docs/README.md).
- **`docs/internals/`** holds current architecture ([`overview.md`](docs/internals/overview.md)), canonical terminology ([`glossary.md`](docs/internals/glossary.md)), active decisions ([`decisions.md`](docs/internals/decisions.md)), and architecture diagrams. Put cross-component constraints, ownership boundaries, concurrency rules, and non-obvious traps here—not PR diaries or feature catalogs.
- **`legacy/docs/` is a frozen historical archive** (pre-ART Rust-VM designs, old ACVM inventory, dated planning snapshots). Never follow `legacy/docs/` for new implementation work.
- **Rewrite in place.** When architecture, module behavior, or workflows change, update the relevant doc in `docs/` directly. Do not create new dated snapshot files (`PLAN_YYYY-MM-DD.md`, `MORNING-*.md`, etc.) or duplicate architecture summaries across root files.
- **Never hand-edit generated docs.** Regenerate them from code:
  - `cargo run -p acos-services --example gen-modules-docs > docs/MODULES.md` (verified by `crates/acos-services/tests/modules_doc.rs`)
  - `cargo run -p acos-mcp --example gen-tools-docs` for `docs/MCP.md` (verified by `crates/acos-mcp/tests/docs.rs`)
  - `python3 tools/build/gen-window-layer-table.py` for `docs/WINDOW_LAYERS.md`
- **Every `docs/*.md` tool call example is schema-tested.** `crates/acos-mcp/tests/documentation_args.rs` scans top-level `docs/*.md` and rejects any documented `acos_*` argument not accepted by the closed MCP schema.

## Threat model (read before reviewing)

ACOS is a **temporary development tool that coding agents use to build and test their own Android apps**. It is not an operating system, a phone, or a multi-tenant sandbox for untrusted users. Review and design against this model:

- **The guest app is the agent's own app under development, not an attacker.** Do not treat guest-supplied values (stream sizes, parcels, metadata, intents) as hostile input. A buggy app should get an honest error or a dropped frame, not hardening.
- **Data built by our own host is trusted.** The Session Controller (Rust) builds the frame blobs, camera metadata and service replies that the guest shims and `LaunchMain` read. A malformed blob means an ACOS bug, so it gets a test, not a defensive bounds check in the reader.
- **Per-app security boundaries are out of scope.** Permissions are one shared store per session. Per-package enforcement, auth between services, and privilege separation inside a session are not goals.
- **The host machine must stay safe.** The cheap host safeguards stay in place: bwrap confinement, Landlock/seccomp, rlimits, memory watchdogs, 127.0.0.1-only listeners, and same-uid socket checks (see `docs/SECURITY.md`). Flag a problem that lets a session harm the host or other users' processes.
- **ACOS graphics use software sync.** Every acquire/release/dequeue fence is `-1` by design (`crates/acos-art/shim/gralloc_shim.c`). Do not request fence waits.
- **What matters most:** correctness of verdicts (never a false pass), never fabricating state (no fake success, no substitute pixels), regressions against the previous release, and the tracked performance metrics (idle CPU, memory, latency).

## What may be committed

Commit what the product needs to build, run and be understood. Nothing else.

- **Yes:** source code, build scripts, test code, scenarios, fixture app **sources**, docs, and **one `.md` report per task** under `results/` (e.g. `results/handoff/<TASK>/REPORT.md`, release notes, gate READMEs).
- **No binaries or images.** Generate them at build or test time instead (example: the camera test frames come from `acos_services::modules::camera::write_test_frames`, written to `fixtures/build/` by `tools/build/build-apps.sh`). APKs, jars, `.so`, zips and fonts are build output.
- **No run output.** `repro.sh`, `artifacts/` folders, `verdict.json`, logcat/log/JSON/TXT captures, frames, screenshots and generated benchmark tables stay local. Summarise what matters in the task's `.md` report, quote the key numbers, and archive raw evidence outside git (the lead keeps `~/acos-evidence/`).
- **No one-off scripts under `results/`.** If a script is worth keeping, it belongs in `tools/<topic>/` with a README line.
- **Exceptions** need the owner's approval and a line in `tools/verify/layout-allow.txt`.

`tools/verify/check-layout.sh` enforces this for every file a branch adds (rule 6), and the `repo layout` GitHub check runs it on every PR. Run it before you push.

## Working rules & verification

1. **AOSP-first reuse ladder ([`docs/ENGINEERING_POLICY.md`](docs/ENGINEERING_POLICY.md)):** Reuse stock AOSP binaries and libraries by default. Before guessing Android wire formats or behavior, read the local read-only AOSP reference checkout ([`docs/AOSP_REFERENCE.md`](docs/AOSP_REFERENCE.md)) and [`docs/IMAGE_ABI_NOTEBOOK.md`](docs/IMAGE_ABI_NOTEBOOK.md).
2. **Honesty invariants (zero false passes):**
   - Never fabricate a `pass` when a step timed out, an API is missing, or a frame did not settle.
   - Unattached services must answer with structured `UNSUPPORTED_ANDROID_API` (or Android's honest missing-service path), never silent fake success.
   - Keep verdicts, fidelity labels, and settle records exact (ADR-0032).
3. **Host safety (ADR-0018):** Never exceed 90% Host RAM (target ≤ 85%). Wrap heavy builds, tests, and guest runs with `tools/guard/guarded.sh --mem <limit> --time <secs> -- <cmd>`. Never use `sudo` or privileged containers.
4. **Shared compiler cache (`sccache`):** Before building in any worktree, run:
   ```bash
   source tools/dev/agent-env.sh
   export CARGO_TARGET_DIR="$PWD/.cache/target"
   ```
   Clean up temporary `.cache/target` directories when finished to preserve disk space. See [`docs/DEV_WORKFLOW.md`](docs/DEV_WORKFLOW.md#14-shared-compiler-cache-sccache--source-toolsagent-envsh-first).
5. **Doc & contract checks after doc or API edits:**
   ```bash
   cargo test -p acos-mcp --test docs --test guide --test documentation_args
   cargo test -p acos-services --test modules_doc
   ```
