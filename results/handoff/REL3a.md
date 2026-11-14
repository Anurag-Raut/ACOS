# REL3a handoff — MCP session tools run real ART by default

**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/h4`
**Branch:** `rel3-mcp-art` (from `main` = `c517a76`, the v0.2.0 candidate).
Not pushed, not merged. **Track:** REL3a (release blocker).

---

## 1. Problem this fixes

A fresh agent driving the installed `~/.local` ACOS through MCP only saw the
frozen in-process Rust VM / RN-bridge backend on every session tool: the Gaten
RN app answered `UNSUPPORTED_RN_SETUP` (missing `rn-bridge-probe.dex`), there
was no launch ladder, `acos_reset` reported `data_cleared: false`, and
`acos_status` after `acos_destroy` still said `live: true`. ART was reachable
only by pointing `ACOS_SESSION_DAEMON` at an externally started
`acos serve --engine art`, which the registered config did not do.

## 2. What changed

### 2.1 The MCP server owns a private ART daemon (`crates/acos-mcp/src/private_daemon.rs`, new)

* On the **first session tool call** (nothing is spawned at startup, so
  `acos_inspect`/`acos_verify` alone pay nothing) the server lazily spawns
  **its own** `acos serve --engine art` on a private socket under
  `$XDG_RUNTIME_DIR/acos/mcp-<pid>-<rand>/session.sock`, mode 0700, and refuses
  a path longer than `sun_path` instead of failing in the daemon.
* Every session tool (`acos_launch`, `acos_run`, `acos_observe`,
  `acos_observe_tree`, `acos_action`/`tap`/`key`/`text`/`swipe`, `acos_logs`,
  `acos_status`, `acos_reset`, `acos_destroy`) is forwarded to it through the
  existing `DaemonClient` (`crates/acos-mcp/src/daemon.rs`, unchanged wire
  format). Replies are the real engine envelopes: `engine: "art"`,
  `fidelity: "art-reference"`, the launch `stages` ladder to `first_frame`, a
  real `acos_reset` (`data_cleared: true`, `isolation:
  fresh-guest-process`), and `SESSION_NOT_FOUND` after `acos_destroy`.
* **The daemon dies with the server.** `PR_SET_PDEATHSIG(SIGTERM)` is set in
  `pre_exec`; the child is forked from a dedicated thread that parks for the
  life of the process (`spawn_on_lifetime_thread`). This mattered: the kernel
  delivers `PDEATHSIG` when the **forking thread** exits, and a tool call runs
  on a short-lived worker thread, so a naive `pre_exec` took the daemon down the
  moment the first call returned. Caught by the real-ART smoke (launch passed,
  the next observe said `SESSION_NOT_FOUND`).
* On MCP stdin EOF/exit the server lists the daemon's sessions, destroys each,
  sends `SIGTERM`, waits up to 5 s, reaps, and removes the socket directory
  (`SessionBackend::shutdown` / `ArtDaemonHandle` in `main.rs`). A private
  daemon that dies mid-session is restarted once, and only for that call.
* **`acos serve` now handles `SIGTERM`/`SIGINT`** (`crates/acos-cli/src/serve.rs`):
  a handler sets an atomic flag, the accept loop breaks and runs the ordinary
  `engine.shutdown()` (closes every ART guest and reaps its process group) and
  removes the socket. Minimal and self-contained (the `serve-leak-reap` branch
  touches the same area; this is a one-static/one-function addition).

### 2.2 Legacy is opt-in only

`ACOS_MCP_ENGINE=legacy` (also accepted: `rust-vm`) keeps the frozen in-process
backend; it is logged as FROZEN at startup and documented as
diagnostics-only in `docs/MCP.md` and `docs/AGENT_GUIDE.md`.
`ACOS_SESSION_DAEMON=1` is unchanged: an **external** `acos serve` at
`ACOS_SESSION_SOCKET` wins when it is listening; otherwise the server falls back
to its own private ART daemon.

### 2.3 Unique default session ids

`DEFAULT_SESSION = "s_mcp"` is gone. `session_backend::default_session_id()`
returns `mcp-<pid>` (process-wide `OnceLock`), filled into `base.session_id`
**before forwarding**, so two agents sharing an external daemon can never
address each other's session. The daemon's own fallback uses the same function.
A top-level `session_id` (the older smoke-client form) is accepted as
`base.session_id` for compatibility.

### 2.4 `acos_verify`: engine advertised, ART by default

* `crates/acos-protocol/src/commands.rs`: `VerifyRequest.engine:
  VerifyEngine` (`art` | `rust-vm`), `Default = Art`; re-exported from the crate
  root. `tools/list` now advertises `properties.engine` with
  `default: "art"` and the enum.
* `crates/acos-mcp/src/backend.rs`: with no `engine` argument the tool passes
  `--engine art` (the CLI's own default is the frozen Rust VM). `engine:
  "rust-vm"` is the only way to get the frozen VM, and the reply's fidelity is
  then `native-runtime`. A `reference` replay keeps the CLI default (ART cannot
  be combined with `--reference`).
* The existing verify rule is honoured by construction: a `required_fidelity:
  "art-reference"` run on a non-ART engine ends `UNSUPPORTED_BACKEND`
  ("achieved fidelity does not satisfy required"), never a pass.

**Deviation from the dispatch:** the task suggested the tool enum be
`art|native-preferred|native-only|reference-only`. Those last three are the
scenario YAML `engine:` values, which the CLI's `--engine` does **not** accept
and ART verification does not read. Advertising them would have been a lie, so
the tool enum is the real CLI set (`art` | `rust-vm`, default `art`) and both
docs now say once and clearly that the YAML field is a different, legacy
native-vs-reference preference. This removes the contradiction the fresh agent
hit without inventing engines that do not exist.

### 2.5 Docs

`docs/MCP.md` §1b "Which engine do the tools run?" (default ART, lifecycle,
legacy opt-in, external daemon, session-id uniqueness, `engine` argument vs the
YAML field) plus the environment table. `docs/AGENT_GUIDE.md`: the MCP/CLI
engine split at the `launch` step, the verify step (ART default,
`required_fidelity` behaviour), the CLI notes bullet, a note under the scenario
example, and a Limits bullet describing the private daemon and its teardown.
`crates/acos-mcp/tests/guide.rs` stays green.

### 2.6 `tools/release/mcp_smoke.py`

Extended (still stdlib-only) to prove the release path with **only default
settings**:

* launch must report `fidelity: art-reference`, `result.engine: "art"`,
  `stage_reached: first_frame` and a fully-ok `stages` ladder;
* `counter` mode: `0` + `Increment` → tap → `1` → `acos_reset` with
  `data_cleared: true` → tree `0` again;
* `gaten` mode: launch, require ≥5 real texts, pick a tab label (or the
  onboarding action) from the tree, tap it — React Native reports
  `clickable/visible: false` on this channel, so the tap uses the node's bounds
  centre — and require the tree to change;
* after `acos_destroy`, `acos_status` must be non-pass with
  `SESSION_NOT_FOUND`;
* `acos_verify` is called **without** an engine argument and must come back
  `pass`, `fidelity: art-reference`, `verdict.engine: art`.

---

## 3. Acceptance

Every ART run and every `acos-cli`/`acos-mcp` test went through
`/home/Qui/Projects/acos/tools/art-runtime/run-exclusive.sh --task REL3a --
tools/guarded.sh --mem 12G --time … --`; builds through `tools/guarded.sh --mem
14G --vmem 40000000 --time 900 --`.

### A. Tests — **PASS**

`cargo test --release --offline -p acos-mcp -p acos-cli -- --test-threads=1`:
all suites green (acos-mcp 113 unit + 45 integration across the target files,
acos-cli all suites), 0 failed.

New `crates/acos-mcp/tests/art_default.rs` (4 tests, drives the real
`acos-mcp` binary with a fake `acos` daemon over a real Unix socket):

| test | asserts |
| --- | --- |
| `default_session_tools_use_the_private_art_daemon` | a session tool with no engine/daemon env spawns `serve --socket <rt>/acos/mcp-<pid>-…/session.sock --engine art`; reply is `art`/`art-reference`; socket ≤100 bytes and under `XDG_RUNTIME_DIR` |
| `default_session_ids_are_unique_per_server` | two servers default to `mcp-<pid>` and never collide; the id is filled by the server before forwarding |
| `verify_schema_advertises_engine_and_defaults_to_art` | `tools/list` `acos_verify.properties.engine.default == "art"` and the enum contains `art` and `rust-vm`; a no-engine verify runs `--engine art` and returns `art-reference` |
| `private_daemon_is_reaped_when_the_server_exits` | on stdin EOF the server exits 0, the daemon pid is gone and its socket directory is removed |

### B. Installed prefix, outside the repo, defaults only — **PASS**

```
tools/release/install.sh --from-target WT/.cache/h4-target \
  --prefix WT/.cache/rel3/prefix --version 0.2.0      # installed ACOS 0.2.0
```

Run from `/home/Qui/Projects/acos-rel3-outside` (empty except the copies) with
`env -i` (HOME, USER, LANG, PATH with `<prefix>/bin`, XDG_RUNTIME_DIR,
DBUS_SESSION_BUS_ADDRESS, `ACOS_ALLOWED_ROOTS=/home/Qui`,
`ACOS_ARTIFACTS_DIR=<outside>/artifacts`). **No `ACOS_SESSION_DAEMON`, no
`ACOS_BIN`, no `engine` argument.** Transcript:
`results/release/mcp-art-default-roundtrip.jsonl` (117 frames, three
`{"smoke": "ok"}` lines).

| run | evidence |
| --- | --- |
| counter APK | launch `pass`, `fidelity art-reference`, `engine art`, `stage_reached first_frame`, stages `process/activity_resumed/first_frame` all ok; tree `["0","Increment","Reset","Counter",…]`; tap Increment → `state_changed true` → tree `["1",…]`; `acos_reset` → `pass`, `data_cleared: true`, `isolation fresh-guest-process` → tree `0`; `acos_destroy` → `destroyed: true`, `teardown.verified true`, `survivors: []`; `acos_status` → `error SESSION_NOT_FOUND` |
| Gaten APK | launch `pass`, `fidelity art-reference`, `engine art`, ladder to `first_frame`; tree has 36 real Gaten texts ("Look up Japanese anywhere", "Turn on lookup mode", "Study with Anki", "1 / 3", …); tapped the onboarding action read from the tree ("Skip", bounds centre 810,2189) → `accepted true`, `state_changed true`, tree changed; destroy + status gone |
| `acos_verify` (copy of `scenarios/gaten-home.yaml`, `app:` rewritten, **no engine arg**) | `status pass`, `fidelity art-reference`, `verdict.engine art`, 14 steps, 0 unsupported |
| leftovers after the MCP process exits | `pgrep -af 'acos serve|acos-art launch|dalvikvm64|bwrap'` filtered to the scratch paths: none |

### C. Dev-path gates `--engine art` — **PASS** (`results/release/gates-art.txt`)

| scenario | result |
| --- | --- |
| `scenarios/v2-art-counter.yaml` | pass, art-reference, art, 11 steps |
| `scenarios/w22-counter-reset.yaml` | pass, art-reference, art, 9 steps |
| `scenarios/gaten-home.yaml` | pass, art-reference, art, 14 steps |

### Goal 4 (ABI) — **NOT met, recorded honestly**

`fixtures/build/apps/flutter-hello-arm64.apk` (only `lib/arm64-v8a/`) on
`acos launch --engine art` returns `status error`, code **`GUEST_EXITED`** with
`details` (`art_code`, `session_exit_path`, `stderr_tail`, `engine`, `trust`) —
a structured harness classification, **not `INTERNAL`**. The launcher
deliberately logs `UNSUPPORTED_ABI` and lets the guest run
(`crates/acos-art/src/launch.rs` matches `NativeLibInstall::UnsupportedAbi` and
continues), so the envelope never carries `UNSUPPORTED_ABI`. Making the launch
refuse before the guest needs an `acos-art` error variant + CLI classification
that returns `ErrorCode::UnsupportedAbi`; that is the next smallest action and
was out of this track's time box. Nothing was weakened to hide it.

---

## 4. Decisions

* Private daemon per MCP server rather than a shared one: no cross-agent state,
  no shared-id collision, and teardown is deterministic. The external
  `ACOS_SESSION_DAEMON=1` path remains for callers that want a CLI-owned daemon.
* Lazy spawn on the first *session* call (not at startup) so `acos_inspect`,
  `acos_verify`, `acos_doctor` and `acos_help` start instantly and never pay a
  daemon.
* `PR_SET_PDEATHSIG` + explicit shutdown + the serve `SIGTERM` handler are all
  needed: the first for a killed server, the second for the normal EOF path, the
  third so `SIGTERM` closes ART guests instead of orphaning them.
* The verify tool enum is the real engine set (`art`/`rust-vm`), not the YAML
  `engine:` values (see §2.4).
* `session_id` at the top level is accepted as `base.session_id` so older
  agents/smoke clients keep working and land on the *server's* default id.

## 5. Remains

* Goal 4/ABI: `acos-art` should fail an arm64-only APK with `UNSUPPORTED_ABI`
  before booting a guest (`acos_art::Error` variant + `engine/art.rs`
  classification). Today it is `GUEST_EXITED`, structured, never `INTERNAL`.
* RN trees report `clickable`/`visible` as false; `acos_tap {"text": …}` refuses
  them as `ELEMENT_OFFSCREEN` (honest, pre-existing). The smoke taps bounds.
  A separate track should map RN accessibility flags.
* `serve-leak-reap` may want to reuse/extend the new `install_termination_handler`
  in `crates/acos-cli/src/serve.rs`.
* `~/.local` was not touched and no Claude/pi MCP registration was changed
  (the orchestrator owns that); the installed-copy verification used the
  scratch prefix only.

## 6. Cleanup

Scratch prefix `WT/.cache/rel3/prefix` (1.7 GB), the outside directory
`/home/Qui/Projects/acos-rel3-outside` and the per-test runtime dirs were
deleted after the evidence was copied into `results/release/`. No daemon, guest
or bwrap of ours was left running (`pgrep -af 'acos serve'` clean). Disk stayed
at 84 % (`/`, 753 G of 945 G used); host RAM stayed below ~60 % (60 GiB total,
MemAvailable ≥ 30 GiB during the runs).
