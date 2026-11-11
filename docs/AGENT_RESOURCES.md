# Resources every ACOS subagent must use (policy)

Every dispatch packet must tell the agent to read this file first. It lists what to consult BEFORE guessing, and the habits that cut mistakes. Last updated 2026-11-11 (advisor recommendations get folded in below when received).

## 1. Read first (in this order)
1. `docs/OPERATING_DIRECTIVES.md` — rules, safety limits, one-hour rule.
2. `docs/OWNER_DIRECTION_2026-11-11.md` and `docs/ENGINEERING_POLICY.md` — AOSP-first ladder; no custom replacement of a working Google component without escalation.
3. `STATUS.md` and the newest `results/art-runtime/*.md` for your area (W3, W4, G2, S6, V3-session-loop, C1, I1, ...). Do not redo solved walls; do not trust an old report over a newer one.

## 2. Reference sources (use before guessing)
| Resource | Where | Use for |
|---|---|---|
| AOSP source, tag android-14.0.0_r75 (read-only) | `.cache/aosp/` (guide: `docs/AOSP_REFERENCE.md`; recreate: `tools/fetch-aosp-reference.sh`; extend: `git -C .cache/aosp/<dir> sparse-checkout add <path>`) | behaviour, parcel/wire order, binder protocol, JNI registration, BLAST/SurfaceComposerClient, hwui |
| The API-34 system image (ground truth for what actually runs) | `.cache/art/sysroot/` (built by `tools/art-sysroot.sh`) | exact layouts: `acos-dex dexdump` for Java/AIDL, `nm -D` / `objdump -d` for native. The tag differs from the image: when they disagree the image wins. |
| Prior findings | `results/art-runtime/`, `research/art-feasibility/`, `docs/FINDINGS.md`, `DECISIONS.md` | known walls, root causes, measured numbers |
| Real device | moto g35 over adb (`tools/phone-parity.sh`) | parity evidence only, via the `agent-device` tooling, with owner-approved scope |
| Probes / harness | `tools/art-runtime/{leakcheck.sh,bench-n.sh,bench.sh}`, `tools/guarded.sh`, `tools/ramguard.sh`, `research/art-feasibility/*/scripts` | guarded runs, leak checks, byte-level replays |

Do not copy AOSP source into the repo. Do not rely on memory of Android internals when the source or the image can answer.

## 3. Habits that prevent mistakes
- **Evidence before claims.** State a root cause only after a measurement (log line, byte replay, gdb trace). Label anything else "hypothesis". A wrong hypothesis presented as fact has cost whole tasks (W2's "88 bytes late" was disproved by W3's byte replay).
- **Real time.** Record `date -u` at start and at every checkpoint, and report real elapsed from those timestamps. Self-estimated durations have been inflated 2-3x.
- **Smallest reproducer first**, then the fix, then a regression test that fails without the fix ("teeth").
- **Serial ART runs only**, always through `tools/guarded.sh --mem N`, scratch under `.cache/`, never `/tmp`.
- **Stay in your files.** Edit only the paths your packet lists. If you need another file changed, write the exact request in your report. Never revert or reformat other agents' edits; never `git add -A`; do not commit unless told.
- **Report format:** changed (files), verified (command + result), not verified, next wall (precise stack/log line), real elapsed.
- **Stop rules:** at 45 min send status; at 60 min report done / blocking / why hard / smallest next action and wait. Never fake success; unsupported behaviour returns a structured `UNSUPPORTED_*` error.

## 4. Advisor-derived rules (codex gpt-6.1-sol, 2026-11-11)
- **ART runs go through the lock:** `tools/art-runtime/run-exclusive.sh --task <name> -- tools/guarded.sh --mem 8G <cmd>`. One ART guest at a time host-wide; each call gets unique `ACOS_ART_DATA`; start/finish/queue-wait are logged to `.cache/art-locks/runs.log`. Intentional N-session benchmarks hold the lock and manage their own concurrency.
- **Time is recorded by the dispatcher, not by you.** Do not compute elapsed time in prose; report only `date -u` stamps.
- **Hypothesis ledger** (required in every report that names a cause):
  `Observation (first error, artifact/offset/stack) | Hypothesis | Prediction if true | Cheapest falsifying experiment | Result | Verdict: untested/supported/rejected/confirmed | Remaining alternatives`. Analyse the chronologically FIRST error, including swallowed exceptions and error returns, not the last crash.
- **Image evidence over tag.** Record wire/ABI facts you verified in `docs/IMAGE_ABI_NOTEBOOK.md` (contract | reader/writer location in the image | evidence | replay fixture). Contract tests need negative cases (null, object-table membership, stability records, trailing bytes).
- **Probe pre-flight** before any stress/60-cycle run: layout evidence identified; ownership/refcounts/teardown defined; blocking calls and thread scheduling defined; crop/dimensions/stride/queue limits explicit; every stage has a deadline and a diagnostic marker; one cycle passes first; the checker is independent and a deliberate failure is shown to be caught. Keep "stock BufferQueue graph" distinct from "BLAST exercised".
- **Outcome packets with a continuation envelope:** OUTCOME (observable milestone) / READ / OWN / DO NOT TOUCH / BASELINE (command + failing artifact) / ACCEPT (commands + assertions + forbidden shortcuts) / CONTINUE (fix adjacent causes inside your ownership and the approved design) / STOP (60-min deadline, ownership conflict, owner gate) / HANDOFF (diff, evidence, ledger, residuals, next command). Naming the next wall is a checkpoint, not completion, unless diagnosis was the outcome.
- **Three separate verdicts** in reports: Task (accepted/checkpoint/blocked), Component (DONE/PARTIAL/BLOCKED/NOT STARTED), Evidence scope (unit / isolated stock probe / real APK lifecycle / interactive E2E). PREVIEW evidence stays separate.
- **Ownership includes Cargo.toml/Cargo.lock/mod declarations/STATUS.md** — the integrator (orchestrator) owns those; request changes in your report. Record `git status` at start and end.
- **Reports are short:** verdict, evidence table, ledger, residuals.

### Evidence bundles and ownership checks (added 2026-11-11)
- `tools/accept-bundle.sh --task NAME -- <cmd...>` runs one command and writes `results/bundles/<task>-<UTCstamp>/bundle.txt` (task, base commit, start `git status --porcelain`, source digest before/after, image-manifest digest, command, cwd, exit code, UTC start/finish, run-log path+sha256) plus the combined `run.log`.
- The source digest hashes `git diff HEAD` for `crates/ tools/ scenarios/ docs/` plus the contents of untracked non-ignored files there; `results/`, `fixtures/`, `research/` and `.cache/` are excluded so command output cannot dirty the evidence.
- `source_status: SOURCE_CHANGED_DURING_RUN` means the command edited sources mid-run; the tool exits with the command's rc and warns on stderr, so callers must check the marker before trusting a bundle.
- The image-manifest digest hashes `.cache/art/sysroot/READY` plus the generated `MANIFEST.txt` (sorted path+size+sha256[:16]); it is `none` when no sysroot exists.
- This tool never launches ART itself: wrap ART callers as `tools/art-runtime/run-exclusive.sh --task NAME -- tools/accept-bundle.sh --task NAME -- tools/guarded.sh ...`.
- `tools/own-check.sh --own '<glob>'... [--base <commit>]` lists changed and untracked files since `--base` (default HEAD, i.e. the working tree) and exits 1 printing offenders for paths outside every own-glob or in the integrator-owned set.
- Integrator-owned, matched by filename only: `Cargo.toml`, `Cargo.lock`, `STATUS.md`, `docs/OPERATING_DIRECTIVES.md`, `crates/*/Cargo.toml`, `**/lib.rs` (lib.rs contents are never inspected; do not edit mod declarations).
- Globs use bash pattern semantics (`*` crosses `/`); a wildcard-free glob matches a path or a directory prefix. `ACOS_REPO_ROOT` overrides the repo root for tests only.
- Tests are plain bash: `tools/tests/test-accept-bundle.sh` and `tools/tests/test-own-check.sh` build throwaway repos under `.cache/tests/` and print PASS/FAIL (negative cases: source changed during a run, out-of-ownership file).
