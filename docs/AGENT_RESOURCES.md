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
