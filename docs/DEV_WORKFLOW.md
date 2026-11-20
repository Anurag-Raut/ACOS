# ACOS Development Workflow

How contributors and AI coding agents build, run, test, and verify changes in ACOS. For the architecture behind these commands, read [`internals/overview.md`](internals/overview.md) and [`../AGENTS.md`](../AGENTS.md). For the full JSON/MCP contract, see [`AGENT_API.md`](AGENT_API.md), [`AGENT_GUIDE.md`](AGENT_GUIDE.md), and [`MCP.md`](MCP.md).

---

## 1. Environment setup

Before building or running tests in any worktree, source the shared environment script (see §14 for details):

```bash
source tools/agent-env.sh
export CARGO_TARGET_DIR="$PWD/.cache/target"
```

Verify that the Host has the extracted API-34 boot image, `bwrap`, `pasta`, and optional WebView binaries:

```bash
cargo run -p acos-cli -- doctor --json
```

If `.cache/boot/android-34` is missing, extract it from the installed Android SDK system image (see [`INSTALL.md`](INSTALL.md)):

```bash
tools/extract-bootclasspath.sh
```

---

## 2. Host safety (ADR-0018)

Host RAM must never exceed 90% (internal target ≤ 85%). Wrap builds, test suites, benchmarks, and guest runs with `tools/guarded.sh`:

```bash
tools/guarded.sh --mem 4G --time 900 -- cargo test -p acos-services
tools/guarded.sh --mem 4G --time 900 -- cargo clippy --workspace -- -D warnings
```

- Never run `sudo` or privileged containers.
- Clean up temporary `.cache/target` directories in worktrees once verification completes so the Host stays within disk budget.
- When running the ART reference emulator (`tools/oracle/emulator.sh`), allow at most one headless instance (≤ 2 GiB RAM) and shut it down immediately after capture (`pgrep -af qemu` must be empty).

---

## 3. Building fixtures and running scenarios

Build test APKs (already self-wraps with `tools/guarded.sh`):

```bash
tools/build-apps.sh
```

Run an end-to-end verification scenario on real ART (the default engine):

```bash
tools/guarded.sh --mem 4G --time 300 -- "$CARGO_TARGET_DIR/debug/acos" verify \
  --apk fixtures/build/apps/counter.apk \
  --scenario scenarios/counter.yaml \
  --json
```

Interactive agent sessions (launch → observe → tap/type → verify → close) keep one persistent Session alive over MCP via `acos-mcp`. See [`AGENT_QUICKSTART.md`](AGENT_QUICKSTART.md) and [`AGENT_GUIDE.md`](AGENT_GUIDE.md).

To watch live Sessions in a browser (zero overhead when no viewer is attached; ADR-0027):

```bash
"$CARGO_TARGET_DIR/debug/acos" view
```

To compare UI-tree parity against a physical phone or recorded captures:

```bash
tools/parity/run-parity.sh scenarios/counter.yaml
```

---

## 4. Regenerating & checking docs

Never hand-edit generated docs; regenerate from code and verify with tests:

```bash
# Regenerate docs/MODULES.md from ModuleRegistry::standard()
cargo run -p acos-services --example gen-modules-docs > docs/MODULES.md

# Regenerate docs/MCP.md tool table
cargo run -p acos-mcp --example gen-tools-docs

# Regenerate docs/WINDOW_LAYERS.md from AOSP reference
python3 tools/gen-window-layer-table.py

# Verify all doc, guide, schema, and module-table invariants
tools/guarded.sh --mem 4G --time 600 -- cargo test -p acos-mcp --test docs --test guide --test documentation_args
tools/guarded.sh --mem 4G --time 600 -- cargo test -p acos-services --test modules_doc
```

`crates/acos-mcp/tests/documentation_args.rs` scans top-level `docs/*.md` for `acos_*` tool call examples and asserts that every documented argument is accepted by the published closed MCP schema.

---

## 5. Checking AOSP sources before guessing

Before reverse-engineering Android framework or Binder wire formats, inspect the local read-only AOSP `android-14.0.0_r75` checkout (`.cache/aosp/`, recreated with `tools/fetch-aosp-reference.sh`; see [`AOSP_REFERENCE.md`](AOSP_REFERENCE.md)) and [`IMAGE_ABI_NOTEBOOK.md`](IMAGE_ABI_NOTEBOOK.md).

---

## 14. Shared compiler cache (sccache) — source `tools/agent-env.sh` first

Every agent worktree on this host shares one compiler cache, so a worktree does
not recompile the crates another worktree already compiled. There is exactly one
place that sets it up:

```bash
source tools/agent-env.sh          # in the worktree, before any build
```

`BRIEF_COMMON` asks agents to create `.cache/env.sh` (a stable path, because
worktree names are not); make it a one-liner so there is nothing to keep in sync:

```bash
mkdir -p .cache && echo 'source tools/agent-env.sh' > .cache/env.sh
```

What it exports:

| variable | value | why |
|---|---|---|
| `RUSTC_WRAPPER` | `~/.cargo/bin/sccache` | the cache wrapper |
| `SCCACHE_DIR` | `/home/Qui/.cache/sccache` | one cache for the host (disk, LRU-evicted) |
| `SCCACHE_CACHE_SIZE` | `40G` | ~2x a full workspace build |
| `SCCACHE_BASEDIRS` | `/home/Qui/.t3/worktrees/acos:/home/Qui/Projects/acos/.cache:/home/Qui/Projects/acos` | these are the layouts worktrees are checked out in; they are interchangeable checkouts, so the same sources under any of them hash to one key |
| `CARGO_INCREMENTAL` | `0` | sccache cannot cache incremental compiles at all |
| `CARGO_PROFILE_{DEV,TEST,RELEASE}_DEBUG` | `0` | target dirs several times smaller (the brief's rule) |
| `ACOS_FONT_DIR`, `ACOS_VIEW_TEST_DIR`, `.cache/{boot/android-34,art,gaten}` symlinks | as `BRIEF_COMMON` | the ART/fixture test env |

It does **not** set `CARGO_TARGET_DIR`: keep one target directory per track — the
cache is what makes that cheap. It never sets `ACOS_BOOT_DIR` or
`XDG_RUNTIME_DIR`, and it never touches `ACOS_HOME` or `~/.local`.

Notes:

* The cache is shared by *all* agents on the host, so a crate compiled on one
  branch is reused on another as long as its sources and flags are identical.
  Cross-worktree hits are what the patch in `tools/sccache/` exists for; stock
  sccache only reuses artifacts inside one checkout.
* A crate that bakes an absolute path into its artifact (`env!("CARGO_MANIFEST_DIR")`
  — `acos-mcp` does) is never served another checkout's build: its key keeps the
  real path, so it is recompiled per worktree. That is intentional.
* `sccache --show-stats` reports hits/misses and the cache size;
  `sccache --zero-stats` resets the counters. If a build seems uncached, check
  that `$RUSTC_WRAPPER` is set in the shell that runs `cargo` and that
  `sccache --show-stats` shows the expected `Base directories`.
* Rebuild/reinstall the wrapper with `tools/sccache/build.sh` (see
  `tools/sccache/README.md` for the one patch it applies and why).
* Measurements (cold vs warm vs a second worktree, hit rates, cache size):
  `results/handoff/BUILDCACHE/README.md`.
