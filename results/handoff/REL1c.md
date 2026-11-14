# REL1c handoff — installed ACOS works for an agent in another repo

**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/h4`
**Branch:** `rel1c-release` (from `7ccfad8` = REL1a install layout + REL1b MCP guide
merged). Not pushed, not merged.
**Track:** REL1c (release v0.2.0).

---

## 1. What changed

### 1.1 Every `acos-mcp` runtime path follows the one `ACOS_HOME` rule

`crates/acos-mcp/src/paths.rs` (**new**) is the crate's copy of the rule
`acos_art::repo_root` / `acos_cli::root` already implement:

```
ACOS_HOME set  -> $ACOS_HOME/<same relative path>   (never the checkout)
ACOS_HOME unset -> the pre-REL1c developer lookup, unchanged
```

| file | change |
| --- | --- |
| `crates/acos-mcp/src/paths.rs` | **new**: `acos_home[_with]`, `dev_root`, `runtime_path[_with]`, `installed_boot_dir_with`; 4 unit tests |
| `crates/acos-mcp/src/lib.rs` | `pub mod paths;` |
| `crates/acos-mcp/src/rn_backend.rs` | `repo_relative` → `paths::runtime_path` (driver DEX); boot dir: `ACOS_BOOT_DIR`, then exactly `$ACOS_HOME/.cache/boot/android-34`, then the legacy cwd/manifest walk. `repo_root_hint()` (the `CARGO_MANIFEST_DIR` hint) deleted |
| `crates/acos-mcp/src/flutter_backend.rs` | same for `.cache/flutter-1/lib-x86_64` and the Flutter driver DEX; `repo_root_hint()` deleted |
| `crates/acos-mcp/src/session_backend.rs` | `native_boot_root()` (the Landlock grant for the confined child): `ACOS_HOME` before the cwd walk and the `~/.cache/acos/boot/android-34` guess |
| `crates/acos-mcp/src/backend.rs` | `acos_verify` accepts `engine` and passes `--engine <v>`; without it MCP could only run the frozen Rust VM and an ART-only scenario (`animations: off`) was unreachable from MCP |
| `crates/acos-vm/src/vm.rs` | `default_boot_dir[_with_home]` + `acos_home()`: `$ACOS_HOME/.cache/boot/android-34` arm **before** the cwd walk. This is the boot classpath of the in-process MCP session (and of every `acos-vm` user); REL1a did not cover `acos-vm`, so an installed MCP launched from another directory had no `/system`. 3 tests |
| `crates/acos-vm/src/vfs.rs` | `VfsConfig::from_env` fallback boot root is `$ACOS_HOME/...` instead of cwd-relative |

`across-mcp` no longer contains any `CARGO_MANIFEST_DIR` path use outside
`tests/` (test-only) — `grep -rn CARGO_MANIFEST_DIR crates/acos-mcp/src` is empty.

### 1.2 `tools/release/register-mcp.sh`

* `--prefix DIR` (default `$ACOS_PREFIX`, else `~/.local`).
* **Installed wrappers win**: when `<prefix>/bin/acos` and `<prefix>/bin/acos-mcp`
  both exist they are registered (they export `ACOS_HOME` pinned to the
  versioned root); `target/release/*` is only the fallback when there is no
  install, then `PATH`. It prints which source it used.
* Idempotent: `claude mcp remove --scope <scope> acos` before `claude mcp add`,
  so a re-run repairs stale paths instead of duplicating the entry.
* `--dry-run` unchanged in spirit: prints the exact command + the `mcpServers`
  JSON, registers nothing.

### 1.3 `tools/release/install.sh`

* `--register-mcp` (default off) calls `register-mcp.sh --prefix <prefix>` after
  writing the wrappers.
* The report now ends with explicit **Next steps**: `acos doctor`, an
  `acos inspect`, an `acos launch --engine art --json`, and the exact
  `register-mcp.sh --prefix <prefix>` command for MCP.

### 1.4 Path allow-list documented (docs/MCP.md, docs/AGENT_GUIDE.md, docs/INSTALL.md)

`docs/MCP.md` §1: what `ACOS_ALLOWED_ROOTS` is (default set by
`register-mcp.sh` = `$HOME`), that paths are canonicalised so **a symlink that
escapes the roots is rejected** even when the link is inside one, that `..` is
rejected, and the three concrete fixes for `UNSAFE_PATH`. §5 repeats the rule.
`docs/AGENT_GUIDE.md` (embedded in `acos_help`) gets the same contract under
Limits → **Paths**, and the error table row for `UNSAFE_PATH` now says what to
do. `docs/INSTALL.md` documents `--register-mcp`, the MCP next steps, and that
the server follows `ACOS_HOME` like the CLI.

### 1.5 `tools/release/mcp_smoke.py` (new)

Stdlib-only MCP stdio client (`--mcp-bin`, `--apk`, optional `--scenario`,
`--scenario-apk`, `--artifacts`): initialize → tools/list → availability →
`acos_help` → `acos_launch` → `acos_observe_tree` ("0" + "Increment") →
`acos_tap Increment` → `acos_observe_tree` ("1") → `acos_destroy` →
`acos_verify` (+ `verdict.json` = pass). Prints every frame as one JSON line and
one `{"check": ...}` line per assertion; exit 0 only if all checks pass.

---

## 2. Acceptance

Every ART/guest run went through
`/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task REL1c -- tools/guarded.sh --mem 10G --time 900 --`.

### A. Unit tests for the path rule — **PASS**

```
cargo test --release --offline -p acos-mcp -- --test-threads=1
  unittests src/lib.rs       45 passed; 0 failed
  tests/docs.rs               1 passed
  tests/e2e.rs                1 passed
  tests/flutter_session.rs    1 passed
  tests/guide.rs              6 passed
  tests/native_session.rs     1 passed
  tests/protocol.rs          20 passed
  tests/rn_session.rs         1 passed
  tests/session.rs            3 passed
```

New tests: `paths::tests::{acos_home_wins_over_the_checkout,
unset_keeps_the_legacy_developer_lookup, empty_acos_home_is_unset,
installed_boot_dir_is_the_home_copy}` and
`vm::boot_dir_tests::{installed_home_wins_over_the_checkout,
installed_home_without_a_boot_dir_does_not_fall_through,
unset_keeps_the_developer_walk}`.

Honest note (pre-existing, not caused here): `process::tests::
output_limit_is_enforced` is racy under parallel test threads — a `/bin/echo`
child can be reaped before the reader observes the byte cap, so it returned
`Ok(Captured{stdout:[97]})` instead of `Err(OutputLimit)` in one parallel run.
It passed 3/3 in isolation and the suite is green with `--test-threads=1`.
Also: this worktree's `fixtures/build` was a **symlink into the main checkout**,
which makes the MCP allow-list correctly reject fixture paths
(`UNSAFE_PATH`); it was replaced by a hardlinked real directory (same as REL1b)
so the e2e test can run.

### B. Installed MCP driven from outside the repo — **PASS**

```
tools/release/install.sh --from-target $PWD/.cache/h4-target \
  --prefix $PWD/.cache/rel1c/prefix --version 0.2.0
→ installed ACOS 0.2.0, 1.5G, 3134 files; wrappers + current -> 0.2.0
```

Run from `/home/Qui/Projects/acos-rel1c-outside` (empty, outside any checkout)
with `env -i` (HOME, USER, LANG, PATH, XDG_RUNTIME_DIR, DBUS_SESSION_BUS_ADDRESS
plus `ACOS_BIN=<P>/bin/acos`, `ACOS_ALLOWED_ROOTS=/home/Qui`,
`ACOS_ARTIFACTS_DIR=<outside>/artifacts`, `ACOS_SESSION_DAEMON=1`,
`ACOS_SESSION_SOCKET=<outside>/session.sock`; **no** `ACOS_HOME` in the
environment — only the wrapper sets it), a daemon `<P>/bin/acos serve
--engine art` and `python3 tools/release/mcp_smoke.py`.

Transcript: `results/release/mcp-installed-roundtrip.jsonl` (33 frames + checks).

| check | result |
| --- | --- |
| `initialize` | `acos-mcp 0.1.0`, `2025-03-26` |
| `tools/list` | 23 tools, launch/observe_tree/tap/verify/help/doctor present |
| `tools/available` | `acos_launch`, `acos_observe_tree`, `acos_verify` `available=true` |
| `acos_help` | 18434 chars (guide embedded) |
| `acos_launch counter.apk` | `status=pass`, `engine=art`, session `s_mcp` |
| `acos_observe_tree` | `["0","Increment","Reset","Counter", …]` |
| `acos_tap Increment` | `accepted=true`, `state_changed=true` |
| `acos_observe_tree` | `["1","Increment","Reset","Counter", …]` |
| `acos_destroy` | `destroyed=true`, teardown verified |
| `acos_verify gaten-home --engine art` | `status=pass`, `verdict=pass`, 14 steps, 0 unsupported, fidelity `art-reference`, `session_exit.outcome=ok`, 6.0 s, peak RSS 251 MB, cgroup peak 280 MiB |
| final line | `{"smoke": "ok"}` |

The gaten APK was copied from `/home/Qui/Projects/acos/.cache/gaten/
gaten-dev-f344272-x86_64-release-unsigned.apk` into the outside directory and
the scenario's `app:` line was rewritten to that copy (by the smoke client);
artifacts/`verdict.json` (15121 B) has `verdict: pass`, `engine: art`.

### C. Nothing written into the checkout during B — **PASS, one explained hit**

Stamp before B (`WT/.cache/rel1c/stamp`), then in the main checkout:

```
find .cache/art/data .cache/art-sock .cache/sessions .cache/acos .cache/boot \
     fixtures/build out ~/.cache/acos -newer <stamp>
```

| path | hits |
| --- | --- |
| `.cache/art/data`, `.cache/art-sock`, `.cache/sessions`, `.cache/acos`, `.cache/boot`, `fixtures/build`, `out` | **0** |
| `~/.cache/acos` | 3 `.img` files (19:38, 19:43, 19:56) — **not mine** |

The three `~/.cache/acos/*.img` are the *developer* (ACOS_HOME-unset) shared boot
image: `pgrep -af 'acos-art launch'` during the window shows
`/home/Qui/.t3/worktrees/acos/.../acos-art launch --apk …/.cache/gaten/…` (a
concurrent agent) and the `wt-int` agent's runs. My B runs pin `ACOS_HOME` to
`<P>/share/acos/0.2.0`, and that tree holds its own boot image
(`.cache/acos/art/boot-images/fixed-d24b54eaf334c4f5`, `current.json`) plus
`.cache/art/build-cache` entries — i.e. the installed state grew, the checkout
did not. The outside directory stayed empty except the files copied into it and
`artifacts/`.

### D. `register-mcp.sh --dry-run --prefix <P>` — **PASS** (nothing registered)

`results/release/register-mcp-dryrun-installed.txt`:

```
binaries  : installed (/home/Qui/.paseo/worktrees/3oxa6xao/h4/.cache/rel1c/prefix)
acos      : <P>/bin/acos
acos-mcp  : <P>/bin/acos-mcp
roots     : /home/Qui
artifacts : /home/Qui/.acos-mcp-artifacts
scope     : user

dry run: claude mcp add --scope user acos \
  --env ACOS_BIN=<P>/bin/acos \
  --env ACOS_ALLOWED_ROOTS=/home/Qui \
  --env ACOS_ARTIFACTS_DIR=/home/Qui/.acos-mcp-artifacts \
  -- <P>/bin/acos-mcp
```

plus the equivalent `mcpServers` JSON. No `claude mcp add` ran.

### E. Dev path unchanged (`ACOS_HOME` unset, from the worktree) — **PASS**

`$WT/.cache/h4-target/release/acos verify … --engine art`, `ACOS_HOME` unset:

| scenario | verdict | engine / fidelity | steps |
| --- | --- | --- | --- |
| `scenarios/v2-art-counter.yaml` | **pass** | art / art-reference | 11, 0 unsupported |
| `scenarios/gaten-home.yaml` | **pass** (second attempt) | art / art-reference | 14, 0 unsupported |

The first gaten attempt failed with `GUEST_EXITED`
(`UNSUPPORTED_ANDROID_API … activityStopped` while a *concurrent* agent ran the
same APK in another worktree); the immediate retry passed with the same code.
Raw envelopes: `.cache/rel1c/evidence/{counter,gaten}.json` (gitignored); the
gaten pass was also proven installed in B.

---

## 3. Decisions

* The path rule lives in `acos-mcp::paths` rather than calling `acos_art`: the
  MCP crate does not link `acos-art`/`acos-cli`, and the rule is eight lines.
  `acos-vm` got its own copy for the same reason (it is the lowest layer).
* With `ACOS_HOME` set the MCP **never** falls back to the checkout, not even
  when the file is missing under the install root: a mixed tree (boot dir from
  the checkout, state under the install) is exactly the bug this track removes.
  The missing path then fails with the runtime's own error, which names the
  installed path.
* `ACOS_BOOT_DIR` still wins over `ACOS_HOME` (explicit override), unchanged.
* `acos_verify` gained an `engine` argument: without it the installed MCP could
  not run any ART-only scenario, which made "one verify-scenario call" on the
  installed server impossible rather than merely slow. It maps to the CLI's
  `--engine`; unknown values are refused by the CLI.
* The smoke client rewrites only the scenario's `app:` line, so a pass still
  means what the authored scenario means.

## 4. Remains

* `acos-cli/src/parity.rs`, `bench*.rs` still use `CARGO_MANIFEST_DIR` — dev-only
  tools, untouched (REL1a decision).
* `rn_backend`/`flutter_backend` now resolve their assets under `ACOS_HOME`, but
  the installer does not copy `crates/acos-jni/tests/data/*.dex` or
  `.cache/flutter-1`; RN/Flutter sessions in an install therefore still need
  those paths supplied explicitly (`ACOS_RN_DRIVER_DEX`, `ACOS_FLUTTER_LIB_DIR`,
  `ACOS_FLUTTER_DRIVER_DEX`). Documented behaviour is "deferred" already
  (`docs/AGENT_GUIDE.md` Limits); installing the RN/Flutter engines is a
  separate decision.
* `acos serve` re-execs `current_exe()`, so the daemon inherits the wrapper's
  `ACOS_HOME`; verified live here (the daemon built its boot image under the
  installed root, from an outside cwd).
* Not registered: no `claude mcp add` was executed (the orchestrator does the
  real `~/.local` install and registration).

## 5. Cleanup

The scratch prefix (`.cache/rel1c/prefix`, 1.5 GB + 0.5 GB created state) and
`/home/Qui/Projects/acos-rel1c-outside` were deleted after the evidence above was
written; `target/release` (test-only symlink to `.cache/h4-target/release`) was
removed. No daemon or guest was left running (`pgrep -af 'acos serve'` empty
after each run). Disk stayed at 83 % (`/`, 743 G of 946 G used) and host RAM
never exceeded ~58 % (60 GiB total, MemAvailable ≥ 30 GiB during the runs).

## 6. Reproduce

```bash
cd /home/Qui/.paseo/worktrees/3oxa6xao/h4
export CARGO_TARGET_DIR=.cache/h4-target
/home/Qui/Projects/acos/tools/guarded.sh --mem 12G --time 1200 -- \
  cargo test --release --offline -p acos-mcp -- --test-threads=1
PATH=$CARGO_TARGET_DIR/release:$PATH  # the e2e test looks for target/release/acos
# (or: ln -s ../.cache/h4-target/release target/release)

./tools/release/install.sh --from-target $PWD/.cache/h4-target \
  --prefix $PWD/.cache/rel1c/prefix --version 0.2.0

mkdir -p /home/Qui/Projects/acos-rel1c-outside && cd /home/Qui/Projects/acos-rel1c-outside
cp <checkout>/fixtures/build/apps/counter.apk .
<P>/bin/acos serve --socket $PWD/session.sock --json &
ACOS_BIN=<P>/bin/acos ACOS_ALLOWED_ROOTS=$HOME ACOS_SESSION_DAEMON=1 \
ACOS_SESSION_SOCKET=$PWD/session.sock \
  python3 <WT>/tools/release/mcp_smoke.py --mcp-bin <P>/bin/acos-mcp \
    --apk $PWD/counter.apk --scenario <scenario> --scenario-apk <apk>
```
