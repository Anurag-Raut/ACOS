# ARTGC — bounded artifacts without losing evidence

**Branch:** `artgc-retention` off `main` `389429a`
**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/k5`
**Experiment:** `EXPERIMENTS.md` E9.3
**Date:** 2026-11-15

## Why

Every `acos verify`/gate run wrote 0.5–2 GB under `out/<scenario>/…`; one
integration worktree's `out/` reached 20 GB and the host disk hit 85 % three
times in one day. A release was about to ship to the owner's other agents, who
would hit the same wall.

## Measurement (before)

| run | total | breakdown |
|---|---|---|
| `gaten-home` (pass) | 435 MiB | `art-frames/live-frames` 311 MiB (30 captures), `frames/` 114 MiB (11 PNGs), `tree/` 10 MiB, `events.jsonl` 0.1 MiB |
| `gaten-content-welcome` (pass) | 2246 MiB | `live-frames` 2126 MiB (only 3 of 11 `frames/` PNGs were declared), `frames/` 114 MiB |
| `gaten-home-wrong` (fail) | 132 MiB | `live-frames` 73 MiB, `frames/` 62 MiB, `tree/` 3 MiB |

Every PNG is exactly **10 371 258 bytes** at 1080×2400: the ART capture path
writes zlib *stored* blocks with filter 0, i.e. an unfiltered raw RGBA buffer in
a PNG wrapper. The "raw buffers" are the frames themselves, which is why pruning
alone could not reach the 70 % target: the retained, *declared* frames were the
bulk.

## What shipped

1. `crates/acos-verify/src/retention.rs` — retention as a pure function of the
   run status plus a keep-set walker.
   * `decision(status, mode)`: only `pass` is ever pruned.
   * Pass keeps: every `steps[].frame_seq` (`frames/<seq>.png` +
     `tree/<seq>.json`), every `evidence[]` path, `steps/`, `logs/`, reports,
     first and last frame; drops intermediate frames and all but the first/last
     mirrored capture.
   * Reference-wins pre-scan: every retained JSON is searched for each candidate
     path; a hit un-prunes it. Nothing a report names is ever deleted.
   * `art-frames/live-session.json` is rewritten (`png` → `png_pruned: true`).
     Because the launcher writes that file at shutdown (after pruning),
     `retention::finalize` reconciles it again from `verify::annotate`.
   * Retained PNGs are re-encoded losslessly (`png` crate, real deflate + Sub
     filtering): same pixels, ~10× smaller.
   * `retention.json` records mode, decision, kept/pruned counts and bytes,
     re-encode savings, the pruned list (deletion receipt) and
     `kept_because_referenced`. The verdict `notes` summarise it.
2. `acos verify --keep-artifacts all|referenced|none-on-pass` + env
   `ACOS_KEEP_ARTIFACTS` (flag > env > default `referenced`). A bogus flag is
   `INVALID_ARGUMENT`; a bogus env value falls back and is recorded in the
   verdict. `none-on-pass` also strips frame declarations from the pass's
   verdict so "no frames" cannot dangle or invalidate the pass.
3. `acos artifacts gc [--dir] [--older-than] [--keep-last] [--dry-run] [--json]`
   — per-scenario ordering, newest `--keep-last` (default 1) protected, non-pass
   runs younger than `--older-than` (default 24h) protected, unreadable verdicts
   never pruned, freed bytes printed.
4. `tools/check-artifact-references.py` — walks run directories, scans every
   JSON/JSONL for artifact paths and resolves them; `retention.json`'s `pruned`
   key is a deletion receipt and is skipped. Non-zero exit on any dangling path.
5. Docs: `docs/AGENT_API.md` §10.2 (retention), §10.3 (`gc`), §17 verify usage,
   §1 loop rule; `docs/MCP.md` (`ACOS_KEEP_ARTIFACTS`, artifacts section).

## Measurement (after)

From each run's own `retention.json` (`before = kept + pruned + reencode_saved`):

| scenario | status | before | after | reduction |
|---|---|---|---|---|
| `v2-art-counter` | pass | 109.0 MiB | 0.3 MiB | −99.7 % |
| `gaten-home` | pass | 195.3 MiB | 8.8 MiB | −95.5 % |
| `negative/gaten-home-wrong` | fail `ASSERTION_FAILED` | 131.8 MiB | 131.8 MiB | 0 % (by design) |

## Gates and flows (status + code, retention on)

| scenario | expected | observed |
|---|---|---|
| `v2-art-counter`, `w22-counter-reset`, `w54-dialog-pixels`, `w42-back-frame`, `w15-dialog-menu`, `form-validation`, `suite/T03-render-good`, `v1-offscreen-pager`, `w7-insets-report`, `se1-expect-change-tap` | pass | pass |
| `suite/T03-render-wrong`, `w7-insets-carve-negative`, `negative/se1-expect-change-noop` | fail `ASSERTION_FAILED` | fail `ASSERTION_FAILED` |
| `negative/offscreen-pager-visible` | fail `ELEMENT_OFFSCREEN` | fail `ELEMENT_OFFSCREEN` |
| `p11-progress-timeout` | timeout `TIMEOUT` | timeout `TIMEOUT` |
| `scenarios/gaten-{home,tabs,settings,search,lesson}` | pass | pass |
| `scenarios/negative/gaten-*-wrong` (5) | fail `ASSERTION_FAILED` | fail `ASSERTION_FAILED` |
| `scenarios/gaten-{support,kanji,recognition,backstack}` (GR4) | pass | pass |
| `scenarios/negative/gaten-{support,kanji,recognition,backstack}-wrong` (GR4 negatives) | fail `ASSERTION_FAILED` | fail `ASSERTION_FAILED` |

Pixel gates keep their frames because a gate's frames are exactly what its
verdict declares, and declared evidence is never a prune candidate.

## Tests

`cargo test --release --offline -p acos-cli -p acos-verify` — green.
New: 11 retention unit tests (decision per status incl. `crash`/`blocked`,
referenced-frame preservation, first/last frame, `none-on-pass` + verdict
rewrite, live-session rewrite, reference-wins, lossless re-encode round trip,
capture ordering) and 5 `gc` tests (keep-last/ordering, recent-failure
protection, dry-run parity, unreadable verdict, nested scenario dirs).

## Decisions and what is not done

* Default `referenced`, not `all`: a green run keeps every file its own verdict
  names, so the removed bytes are only those no report ever mentioned.
* No compression change on the ART capture path itself (it is on the screenshot
  critical path; the benchmark row D-M5 depends on it). Re-encoding happens in
  the evidence pass only.
* `gc` has no size budget (`--max-bytes`); `--older-than` + `--keep-last` is the
  policy so far.
* Retention is applied in the executor (one place every entry point funnels
  through: CLI, serve, MCP-spawned CLI), with the post-teardown reconciliation
  in `verify::annotate`.
