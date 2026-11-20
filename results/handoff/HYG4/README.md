# HYG4 — silent-skip conversion, continued from HYG3c

Branch `hyg4` from `hyg3c` (26d034f2e). Every commit is one crate (or one tools change), each
verified by its own gate before it was committed.

**Status: PARTIAL.**
- Acceptance "audit count reduced to 0": **met** (`tools/audit-silent-skips.sh` → `SILENT-SKIP SITES: 0`).
  Note the audit was widened in this pass to also match multi-line prints. The starting list (187 sites,
  old one-line rule) is kept in `audit-start-187.txt`; the widened audit found 24 more sites, all converted.
- Per-crate logs with zero missing prerequisites: **met for 11 of 12 gates**. `acos-art` is **red** (3 failing
  tests), see "Open items". No gate was run with `--exclusive` or with timing claims.
- The HYG3 `audit-remaining.txt` is updated to the final state.

## Commits (hyg4, 26d034f2e..HEAD)

| commit | crate / change | what it does | gate evidence |
|---|---|---|---|
| b5eb2cff4 | tools | `tools/link-worktree-prereqs.sh` links qemu-user, bionic-arm64, flutter-1, corpus (+boot, art, gaten) and builds the arm64 guard, idempotently; `agent-env.sh` calls it; `test-crates.sh` exports a private `ACOS_RUNTIME_DIR` / `ACOS_SESSION_SOCKET` (SUN_LEN checked) | — |
| 9f92ec9ad | acos-framework-natives | 22 sites → `require_some` / `require_prereq` | `logs/hyg4-acos-framework-natives.log`: 41 passed, 0 skipped |
| 9e8c09ca8 | acos-art (landlock) | 1 site | `logs/hyg4-acos-art-landlock.log`: 4 passed |
| 237c456d1 | acos-services | 1 site | `logs/hyg4-acos-services.log`: 694 passed, 2 ignored (pre-existing) |
| d0ef6cc28 | acos-dex | 7 sites; **decision (a)**: unreadable/malformed input fails the run; boot jars default to the worktree boot cache; one multi-line site the audit could not see | `logs/hyg4-acos-dex.log` (50 passed), `hyg4-acos-dex-nocapture.log` (89 boot dex files + 5 legacy verified) |
| a74e20cf7 | acos-webview | 8 `ACOS_WEBVIEW_TEST` / bwrap gates → `require_prereq`; **decision (b)**: the six heavy Chromium tests are listed in `tools/test-prereq-optouts.txt` | see the webview rows below |
| beaf78c4b | acos-render | 26 sites incl. 3 multi-line (corpus, reference_diff, text_coverage) | `logs/hyg4-acos-render.log`: 128 passed, SKIP COUNT 0 |
| c964fb1a9 | acos-cli | 22 + 4 multi-line sites; `ACOS_S8_LIVE_HOSTILE` is an opt-in (see tools commit) | `logs/hyg4-acos-cli-stdout.log` (`--no-fail-fast`): 310 passed, SKIP COUNT 0 |
| 2d59939c8 | tools | audit sees multi-line prints; allowlist (3 product messages + binder build script, with reasons); opt-out list (heavy webview, engine_gate, fair_pulls, S8); `test-s8-live.sh` | — |
| 2645907b2 | acos-runtime | 48 sites; **`ParcelableCompareMain.java` repaired** (missing `}` in `linkProperties()`, so both Java parcelable tests had silently skipped as "toolchain unavailable") | `logs/hyg4-acos-runtime-stdout.log`: 121 passed, SKIP COUNT 0 |
| a6220f27a | tools (test-crates) | the gate sets `ACOS_SNAPSHOT_FULL_REAL=1`: the acos-vm snapshot real-corpus tests are release-only by default and would otherwise fail every debug gate (forced run: 3 passed, ~22 s) | — |
| 843039654 | acos-vm | 57 sites; **`OsNativesMain.java` repaired** (checked `ErrnoException` undeclared, so it had silently skipped); **sink race fixed** (`javabridge` native-root test read the process-wide log sink without the guard) | `logs/hyg4-acos-vm-stdout.log`: 217 passed, SKIP COUNT 0 |
| fb6878d6b | acos-jni | 9 sites (found by the widened audit) | `logs/hyg4-acos-jni.log`: 71 passed, SKIP COUNT 0 |
| 97a18cbff | acos-protocol | 1 site (scenario runner skipped a scenario whose fixture was absent) | `logs/hyg4-acos-protocol.log`: 131 passed, SKIP COUNT 0 |
| a3620e7ce | acos-art | the w31 evidence test and landlock; **gate red**, see below | `logs/hyg4-acos-art-stdout.log`: 254 passed, 3 failed |
| 925bd0eea | tools (webview heavy) | engine cache moved to `.cache/wv` (socket paths must fit SUN_LEN; the long cache path made `engine_gate` fail) | — |
| c2c7af386 | acos-webview | `engine_gate`, `fair_pulls` env gates → `require_prereq` (both listed as opt-outs) | default: `logs/hyg4-acos-webview-gate*.log` (SKIP COUNT 0); heavy: `logs/hyg4-acos-webview-heavy-stdout.log` (8 ok, skip_lines 0) |

HEAD: see `git log -1` on `hyg4` (the final commit of this pass is this README's commit).

## Gates (what each one proves)

Every gate runs through `tools/art-runtime/test-crates.sh` (ART lock, guard, private runtime dir, opt-outs
excluded, `SKIP COUNT` checked) with `--no-fail-fast` so one failure does not hide the rest.

| crate | final result | failing / notes |
|---|---|---|
| acos-webview (default, via test-crates) | rc 0, SKIP 0 (40 passed) | — |
| acos-services, acos-dex, acos-framework-natives | run with direct `cargo test` (private runtime env for services/natives), all passed, 0 skipped; not through the ART gate (no ART needed) | — |
| acos-cli | rc 0, 310 passed, SKIP 0 | S8 live test is an opt-in, run by `tools/art-runtime/test-s8-live.sh` (rc 0, skip_lines 0, real acos-art, `logs/hyg4-s8live-20261118T093906Z.log`) |
| acos-render | rc 0, 128 passed, SKIP 0 | — |
| acos-runtime | rc 0, 121 passed, SKIP 0 | — |
| acos-vm | rc 0, 217 passed, SKIP 0 | — |
| acos-jni, acos-protocol | rc 0, SKIP 0 | — |
| acos-mcp (rerun with private `ACOS_RUNTIME_DIR` / `ACOS_SESSION_SOCKET`, as the HYG3c brief asked) | rc 0, 169 passed, SKIP 0 | `logs/hyg4-acos-mcp-stdout.log` |
| acos-webview heavy (`tools/art-runtime/test-webview-heavy.sh`, env `ACOS_WEBVIEW_TEST=1`) | 8 ok, skip_lines 0 | `logs/hyg4-acos-webview-heavy-stdout.log` |
| acos-art | **3 failed**, 254 passed, SKIP 0 | see below |

## Open items (acos-art is red; not weakened)

1. `spike_net_boundary` (2 tests, LAN rules and fail-closed): `no pasta binary`. These were kept red in HYG2/HYG3
   (`tools/test-prereq-optouts.txt` does not list them). **Decision (c)** said: leave discovery to NETREALc;
   not touched here. Smallest next action: point discovery at `.cache/pasta-pinned/pasta` (or set
   `ACOS_SPIKE_NET_PASTA`) and rerun the acos-art gate.
2. `guest_cannot_overwrite_boot_art_or_the_classpath` (cleanup.rs:953): a host-side shared input
   (`/data/acos.properties`) changes hash during the canary run. It fails deterministically: alone, under the
   lock, and without the private runtime env (`logs/hyg4-art-canary-stdout.log`, `logs/hyg4-acos-art-stdout.log`).
   `src/lib.rs:241` says other launches rewrite `acos.properties`. I did not change any production code on this
   path (every changed `src/` line is inside a test module). **Confirmed pre-existing:** run alone at the HYG3
   base 26d034f2e (scratch worktree, same ART lock and guard) it fails with the same md5 values
   (`logs/hyg4-base-canary.log`). Smallest next action: decide whether the canary's premise (no writer during
   the run) or the launch path that rewrites `acos.properties` needs fixing; that is a test-design decision, so I
   did not change it.

## Other things the orchestrator should know

- The HYG3 handoff said the acos-webview `ACOS_WEBVIEW_TEST` tests printed SKIP. The audit did not see them
  (multi-line `println!`). They are converted now.
- `tools/art-runtime/test-crates.sh` now sets `ACOS_SNAPSHOT_FULL_REAL=1` (decision recorded in its commit).
- Heavy webview tests write evidence into the tracked `results/handoff/WVB/evidence/gate.json` and
  `results/handoff/WVFAIR/evidence/fair-pulls.json`. I restored both to HEAD after each run and did not commit the
  rewrites. The owners may want those evidence files refreshed from a clean run.
- Pre-existing, not changed: `boot_jars.rs` and `legacy_apks.rs` write scratch under `std::env::temp_dir()` (`/tmp`).
- `cargo fmt` was run on some crates early in this pass; it reformatted unrelated lines in a few test files. I
  reverted those in acos-webview. The other crates' commits may carry formatting-only hunks; the assertions were not changed.
- Reason strings produced by the fallback keep the test name as a prefix in a few places (cosmetic).
- Not run: `--exclusive` or any timed A/B (none requested).

## Evidence

- `logs/` — the final run of every gate (28 files, 1.1 MB), plus `hyg4-base-canary.log` (the canary at 26d034f2e).
- `audit-start-187.txt` — the audit output at the start of this pass (one-line rule).
- `../HYG3/audit-remaining.txt` — updated to `SILENT-SKIP SITES: 0`.
- Scratch and caches removed at the end: `.cache/hyg4-target`, `fixtures/build` copy, `.cache/wv` (engine, ~280 MB).
