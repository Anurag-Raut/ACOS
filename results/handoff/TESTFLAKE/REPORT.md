# TESTFLAKE — v0.3.0 test hygiene: two isolation bugs, fixed at the cause

Branch `testflake` from `rel-030` `0e3719bf`. Test env for every run: private
`ACOS_RUNTIME_DIR=$WT/.cache/tfrt` **and** `ACOS_SESSION_SOCKET=$WT/.cache/tfrt/session.sock`
(the standard harness env — see §1, it is what the flake was), plus
`ACOS_FONT_DIR`, `CARGO_PROFILE_{DEV,TEST}_DEBUG=0`, `CARGO_INCREMENTAL=0`,
`CARGO_TARGET_DIR=$WT/.cache/testflake-target`, every `cargo test` inside
`tools/art-runtime/run-exclusive.sh --task TESTFLAKE -- tools/guarded.sh`.

**Status: DONE.** Both failures reproduce before the change and pass after, the
assertions are unchanged (the foreign-build refusal is still proven), and the
whole `-p acos-cli` suite is green 3× in a row **with the socket variable
exported** (it failed that way before).

| # | brief item | verdict |
|---|---|---|
| 1 | `rel3d_daemon_identity::…foreign_daemon…` fails only in the full run; find the shared state, fix isolation at the cause, never weaken the assertion | **PASS** — cause: the arms overrode `ACOS_RUNTIME_DIR` but inherited `ACOS_SESSION_SOCKET`, which **wins** in the client, so every child `acos` talked to the owner's shared daemon and the arm's fake was never consulted (`evidence/00-before-fix-with-socket-env.log` vs `01-rel3d-with-hostile-env.log`) |
| 2 | `live_registry::…leaves_room_for_a_unix_socket` fails for a long `ACOS_VIEW_TEST_DIR`; make the test pick a short root (preferred) or fail naming the var | **PASS** — one shared rule (`acos_test_support::short_scratch_root`) + a budget **derived from the deepest name** (the old 72-byte budget passed its own assert and still produced a 114-byte socket path); a too-long var is reported once and ignored |
| 3 | `cargo test -p acos-cli` 3× green; `cargo test -p acos-art --lib` green with a long `ACOS_VIEW_TEST_DIR` and with none | **PASS** — `evidence/10-cli-full-run-{1,2,3}.log` (24/24 targets, exit 0 each), `evidence/20-art-lib-long-view-dir.log` (163 passed, 118-byte var), `evidence/21-art-lib-no-view-dir.log` (163 passed) |

---

## 1. The rel3d flake: an inherited shared-socket path, not a race

`rel3d_daemon_identity` runs the real `acos` binary against a *fake* daemon it
starts in a scratch directory. Its helper set `ACOS_RUNTIME_DIR` only:

```rust
Command::new(acos()).env("ACOS_RUNTIME_DIR", dir).env("ACOS_NO_DAEMON", "0")
```

but the client resolves its socket as `ACOS_SESSION_SOCKET` **first**
(`acos_mcp::daemon::socket_path`: "`ACOS_SESSION_SOCKET` wins"), and the worktree
test env exports exactly that (one private socket for the whole suite, so no test
touches `$HOME`). So during a full-suite run every `acos session list` in the test
went to the *owner's* shared daemon of the same build:

* no `DAEMON_VERSION_MISMATCH` warning (same build) → `stderr` empty →
  `assert!(warning)` at the first arm failed;
* `session list` was `pass` with the owner's sessions, not the fake's.

Standalone runs were made in a shell without the variable, so the test passed
(6/6) and only the suite failed. Proof, both directions:

| run | command | result |
|---|---|---|
| before | `ACOS_SESSION_SOCKET=… cargo test -p acos-cli --test rel3d_daemon_identity` (8-byte-identical code, `env_remove` line disabled) | **FAILED** — `a structured warning must be printed; stderr=` (`evidence/00-before-fix-with-socket-env.log`) |
| even earlier | same, unmodified `rel-030` inside the full suite | **FAILED** (SETTLE5 §7) |
| after | same command, fixed test | **PASS** 3 passed (`evidence/01-rel3d-with-hostile-env.log`) |

### The fix (isolation at the cause, assertions untouched)

* every arm gets its **own** directory from a guard (`Scratch`): 0700, unique per
  arm, removed on `Drop`, and the daemons the arm caused to auto-start are
  stopped by the pid the daemon reports about itself (`stop_daemon_at`) — that
  used to leave `acos serve` processes holding deleted runtime dirs;
* the child environment is fully explicit: `ACOS_RUNTIME_DIR=<arm dir>` **and**
  `env_remove("ACOS_SESSION_SOCKET")`, so no inherited path can redirect the
  client; `ACOS_NO_DAEMON=0`/`ACOS_SESSION_IDLE_SECS` are still set per run;
* the arm directory is **short by construction**: a build-private socket lives at
  `<dir>/build-<build id>/session.sock` and `sun_path` is 108 bytes, so the root
  is chosen from candidates (`$ACOS_VIEW_TEST_DIR` when it fits,
  `<cwd>/.cache/rel3d`, `/dev/shm/acos-rel3d-tests`, `$TMPDIR/...`) and the
  deepest socket is measured before the directory is created; nothing fits → a
  clear panic naming the variable and the budget;
* new regression test `rel3d_arms_ignore_an_inherited_shared_socket`: it starts
  a live same-build daemon elsewhere, then runs the four arms **in a child
  process** with `ACOS_SESSION_SOCKET` pointing at it and requires success. It
  fails on the old code (started the daemon on the hostile socket, no warning),
  passes now; no assertions on the daemon behaviour were relaxed.

Same class, swept (each helper that pointed a child `acos` at its own runtime dir
now also refuses to inherit the shared socket): `fb1_live_input`,
`negative_verify`, `rel3a_settle_status`, `rel3e_handled`, `rel3s_scenario`,
`rel3s_swipe`, `serve_daemon`, `w50_serve_art` (×2), `w55_dead_guest`
(`gl1_kept_session` already set `ACOS_SESSION_SOCKET` explicitly).

## 2. The long `ACOS_VIEW_TEST_DIR`: one rule, sized from the real deepest name

The old code (`live_registry::test_scratch_root`, `live_view::scratch_root`) took
`ACOS_VIEW_TEST_DIR` when it was **≤ 72 bytes** and *asserted* otherwise. Two
bugs in one: a 67-byte root (this worktree's `.cache/lv`) passed the 72-byte check
and still produced a **114-byte** socket path (SETTLE5's failure: root +
`/<pid>-frames-and-events-stream/producer.sock` = 114), and a genuinely long root
turned into a hard failure of every test that binds a socket.

Now there is one rule for the class, in `crates/acos-test-support`:

```rust
pub const SUN_PATH_MAX: usize = 107;                      // 108 with the NUL
const DEEPEST_BELOW_ROOT: &str = "-frames-and-events-stream/producer.sock";
pub const SCRATCH_ROOT_BUDGET: usize = SUN_PATH_MAX - 1 - 7 /* pid */ - DEEPEST_BELOW_ROOT.len(); // 60
pub fn short_scratch_root(env_var, candidates, fallback_name) -> PathBuf
pub fn pick_scratch_root(requested, candidates, fallback) -> (PathBuf, Option<PathBuf>) // pure, testable
```

Order: `$ACOS_VIEW_TEST_DIR` when it fits the budget → the checkout's own
`.cache/...` when that fits → `/dev/shm/<name>` → temp dir. A too-long variable is
used **never** and reported once on stderr with its length and the budget
(visible with `--nocapture` / on failure), because the caller cannot shorten a CI
worktree path and these tests must not fail for that.

Adopted by `acos-cli` (`live_registry::test_scratch_root`, and `view::hub`'s tests
now go through it instead of reading the variable themselves) and by `acos-art`
(`live_view::scratch_root`; `acos-test-support` added as a dev-dependency).

New/changed tests (`evidence/23-test-support.log`):

* `a_root_that_cannot_hold_a_socket_falls_through_to_a_short_one` (114-byte-style
  root → the checkout candidate, and the rejected value is reported);
* `the_budget_is_derived_from_the_deepest_name_and_not_a_round_number` (a root of
  exactly the budget still fits; one byte more does not — the old number was
  "round" and overflowed);
* `no_usable_candidate_falls_back_to_a_short_ram_disk_root`,
  `a_short_requested_root_is_honoured`;
* `live_registry::scratch_tests::the_test_scratch_root_leaves_room_for_a_unix_socket`
  (kept, now `<= SUN_PATH_MAX` and budget-derived),
  `…::a_long_view_test_dir_narrows_to_a_root_that_still_fits`;
* `live_view::tests::the_scratch_root_leaves_room_for_the_stream_socket`.

None of them mutates the process environment (the decision is a pure function
over `Option<&Path>`), so they cannot race with sibling tests — the "no
process-global env mutation without a guard" rule.

## 3. Evidence

| file | what it shows |
|---|---|
| `evidence/00-before-fix-with-socket-env.log` | the failing run: inherited `ACOS_SESSION_SOCKET`, `env_remove` disabled → arm 1 fails, and the new regression test names the cause |
| `evidence/01-rel3d-with-hostile-env.log` | the fixed test with the same inherited variable → 3 passed |
| `evidence/10-cli-full-run-1…3.log` | `cargo test -p acos-cli`, socket var exported, 3 runs: 24/24 targets `ok`, exit 0 |
| `evidence/11-cli-full-run-long-view-dir.log` | the same full suite with a 118-byte `ACOS_VIEW_TEST_DIR` → 24/24 `ok` |
| `evidence/20-art-lib-long-view-dir.log` | `cargo test -p acos-art --lib` with the 118-byte variable → 163 passed |
| `evidence/21-art-lib-no-view-dir.log` | `cargo test -p acos-art --lib`, variable unset → 163 passed |
| `evidence/22-cli-bin-long-view-dir.log` | `-p acos-cli --bin acos` with the 118-byte variable → 193 passed (both `live_registry` scratch tests ok) |
| `evidence/23-test-support.log` | the shared SUN_LEN rule's own unit tests → 4 passed |
| `evidence/24-rel3d-standalone-6x.log` | the rel3d target alone, 6 runs, socket var exported → 6× ok |

Before the change, in the same environment: `-p acos-art --lib` with an 89-byte
`ACOS_VIEW_TEST_DIR` was **12 failed / 5 passed** (every socket-binding test), and
`-p acos-cli --bin acos live_registry` with a 67-byte one failed
`the_test_scratch_root_leaves_room_for_a_unix_socket` at 114 bytes (the SETTLE5
report).

## 4. Reproduce

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
export ACOS_RUNTIME_DIR=$PWD/.cache/tfrt ACOS_SESSION_SOCKET=$PWD/.cache/tfrt/session.sock
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR=$PWD/.cache/testflake-target

# 1: green now, red on rel-030
cargo test --offline -p acos-cli --test rel3d_daemon_identity
cargo test --offline -p acos-cli                    # 3× in a row

# 2: long variable
LONG=/tmp/acos-view-test-dir-that-is-far-too-long-for-a-unix-socket-path-$(printf '0123456789%.0s' 1 2 3 4 5)
ACOS_VIEW_TEST_DIR=$LONG cargo test --offline -p acos-art --lib
cargo test --offline -p acos-test-support
```

## 5. Notes for the orchestrator

* `acos-mcp`'s integration tests have the same `ACOS_RUNTIME_DIR`-only pattern in
  a few places; they are outside this brief's two failures and were not touched
  (same one-line `env_remove` applies if they ever flake under the suite env).
* The repo is not `cargo fmt`-clean with the installed rustfmt (a bare
  `cargo fmt --all` rewrites ~500 files); the change is formatted by hand in the
  surrounding style, no reformat churn is included.
* `target`/scratch deleted at the end (`df -h /` 74 % → 74 %); a full
  `-p acos-cli` run stayed inside `guarded.sh --mem 12G` and host `free -g`
  showed 15 GiB used at peak (shared host, other agents included).
