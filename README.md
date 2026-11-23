# ACOS — Agent-Native Android Execution Runtime

ACOS runs real Android APKs (API 34, x86_64) on Linux for AI coding agents without booting a full Android VM per session. Stock Google ART (`dalvikvm64`), `framework.jar`, `libandroid_runtime`, CPU Skia (`libhwui`), and native `.so` libraries execute inside an unprivileged `bwrap` sandbox; userspace `/dev/binder` (`artshim.so`) routes system calls to a lightweight per-session Rust **Session Controller** (`acos-services` + `acos-wm`), while heavy shared resources—read-only framework/DEX/font pages, real internet egress (`acos-netd`), and headless Chromium WebView (`acos-webview`)—run **once per Host** and scale cleanly to 20+ parallel Sessions (~0.6–0.9 GiB total vs ~64.5 GiB for 20 stock emulators).

## Documentation

- **[`docs/README.md`](docs/README.md)** — Single source of truth for all current ACOS documentation.
- **[`docs/internals/overview.md`](docs/internals/overview.md)** — Current architecture: AOSP vs ACOS cut, Host vs Session resource split, Host-shared concurrency model, and measured scaling.
- **[`docs/internals/glossary.md`](docs/internals/glossary.md)** — Canonical vocabulary (**Host**, **Session**, **Session Controller**, **Guest App Process**, **Module**).
- **[`docs/internals/decisions.md`](docs/internals/decisions.md)** — Curated active architectural decisions (full ADR log in [`DECISIONS.md`](DECISIONS.md)).
- **[`AGENTS.md`](AGENTS.md)** — Entrypoint for coding agents and contributors (crate map, doc rules, verification commands).
- **[`docs/AGENT_GUIDE.md`](docs/AGENT_GUIDE.md)** & **[`docs/MCP.md`](docs/MCP.md)** — Agent guide (also served via `acos_help`) and closed-schema MCP contract.
- **[`docs/MODULES.md`](docs/MODULES.md)** — Generated registry of `core` and all 23 attachable Android service modules.
- **[`legacy/docs/`](legacy/docs/README.md)** — Frozen historical archive (pre-ART Rust VM and dated planning snapshots).

## Quick start

```bash
# 1. Enable the shared compiler cache and test env (see docs/DEV_WORKFLOW.md)
source tools/dev/agent-env.sh
export CARGO_TARGET_DIR="$PWD/.cache/target"

# 2. Build the CLI and MCP server
tools/guard/guarded.sh --mem 4G --time 900 -- cargo build --release -p acos-cli -p acos-mcp

# 3. Check Host readiness (boot image, bwrap, pasta, chrome-headless-shell)
"$CARGO_TARGET_DIR/release/acos" doctor --json

# 4. Run a scenario on real ART
tools/guard/guarded.sh --mem 4G --time 300 -- "$CARGO_TARGET_DIR/release/acos" verify \
  --apk fixtures/build/apps/counter.apk --scenario scenarios/counter.yaml --json
```

See [`docs/INSTALL.md`](docs/INSTALL.md) for installation and image extraction, and [`docs/AGENT_QUICKSTART.md`](docs/AGENT_QUICKSTART.md) for the interactive MCP workflow (`acos_launch` → `acos_observe_tree` → `acos_tap`/`acos_text` → `acos_verify`).

## Host safety (ADR-0018)

Host RAM must never exceed 90% (target ≤ 85%). Wrap heavy builds, tests, and guest runs with `tools/guard/guarded.sh`:

```bash
tools/guard/guarded.sh --mem 4G --time 900 -- cargo test -p acos-services
tools/guard/guarded.sh --mem 4G --time 900 -- cargo clippy --workspace -- -D warnings
```

During parallel waves, run `tools/guard/ramguard.sh &` to enforce the Host RAM ceiling. Details: [`docs/DEV_WORKFLOW.md`](docs/DEV_WORKFLOW.md), [`docs/SECURITY.md`](docs/SECURITY.md), and [`DECISIONS.md`](DECISIONS.md) (ADR-0018).
