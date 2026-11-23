# results/

Run reports and measurement records. **Only reports and inputs are tracked.**

## Tracked

* `*.md` reports (the narrative verdicts that docs and the ledgers cite),
* harness code (`*.py`, `*.sh`, `*.rs`),
* the few data files that code or tests read:
  `aidl/`, `wm/`, `res/`, `render/corpus/`, `bench/BASELINE.json`, `bench/schema/`,
  `art-runtime/evidence/W31-artifacts/gaten-release-evidence/`,
  `parity/w193a/counter.{png,tree.json}`, `oracle/dexprogs-nondeterminism.txt`.

The exact rules live at the bottom of `/.gitignore`; `tools/verify/check-layout.sh` enforces them.

## Local-only (gitignored)

Raw evidence: frames and screenshots (`*.png`), logs, JSON/JSONL captures, markers,
trace dumps, bench run output, release/handoff attachments. Tools still write them
here (and the default `acos` run output goes to `out/`, also untracked); they simply
do not enter git. A report that cites one (`results/handoff/X/frame.png`) is describing
a run, not a fetchable file.

The 44.7k files that used to be tracked under `results/` and `out/` were archived once
(2026-11-23, on the v0.4.0 base) as `~/acos-evidence/evidence-results-and-out-20261123.tar.zst`
on the maintainer's machine (extract from the repo root to restore the original paths).
They also remain in git history before the cleanup commit.

To keep a new piece of evidence durable, summarise it in a `.md` report here and keep the
heavy artefacts out of git.
