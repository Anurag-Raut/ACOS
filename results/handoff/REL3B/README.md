# REL3B handoff evidence

`provenance-smoke.yaml` — the tiny real-ART scenario (launch counter, wait for
the count, assert "0").

Acceptance run (real ART, engine `art`), `--json` envelope: `envelope.json`,
stderr: `stderr.txt`, artifacts + verbatim verdict: `artifacts-json/`.
A second copy of the run whose human summary is quoted in the report:
`artifacts/`.

Evidence that the provenance block is not a CLI-only decoration:

* `result.provenance.scenario.path` is absolute
  (`…/results/handoff/REL3B/provenance-smoke.yaml`), `sha256_12` = `4ac610c4ce06`
  (= `sha256sum provenance-smoke.yaml`).
* `result.provenance.apk.path` is absolute
  (`…/fixtures/build/apps/counter.apk`), `sha256_12` = `8170835e70f2`
  (= `sha256sum fixtures/build/apps/counter.apk`).
* `engine: art`, `fidelity: art-reference`, `warnings: []`.
* The same block is in `artifacts-json/verdict.json` under `provenance`, and
  `reproducer.scenario_path` is the same absolute path (no relative path).

Human summary line of the text run (`artifacts/`):

```
verify: pass (ok=true, backend=acos-native, fidelity=art-reference)
  engine: art (mode: live)
  provenance: scenario /home/Qui/.t3/worktrees/acos/t3-0293800a-c011ab7a54eee486/results/handoff/REL3B/provenance-smoke.yaml (sha 4ac610c4ce06), apk /home/Qui/.t3/worktrees/acos/t3-0293800a-c011ab7a54eee486/fixtures/build/apps/counter.apk (sha 8170835e70f2), engine art, fidelity art-reference
  artifacts: results/handoff/REL3B/artifacts
  steps: 3
```
