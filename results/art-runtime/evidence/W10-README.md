# W10 evidence bundle

All paths are under `results/art-runtime/evidence/`.

| file | what it is |
|---|---|
| `W10-run.json` | `acos verify --engine art` envelope + verdict for `scenarios/v2-art-counter.yaml` (live pass, 11 steps) |
| `W10-artifacts/` | that run's artifacts: `verdict.json`, `steps/*`, `frames/*.png`, `tree/*.json`, `art-frames/live-session.json` |
| `W10-negative.json` | the negative control's envelope + verdict (live fail, `ASSERTION_FAILED`) |
| `W10-negative-selector.yaml` | the negative scenario (tap a view id that does not exist) |
| `W10-negative-artifacts/` | the negative run's artifacts |
| `W10-png-recompress.txt` | proof the PNGs were losslessly re-encoded for size; decoded RGBA sha256 identical |

## Labels

* **Screenshots are ACOS C1 buffer captures** (`capture_source: "acos-c1-buffer"` in
  `art-frames/live-session.json`): the pixels are read from the `GraphicBuffer`
  memfd the real app's BLAST producer posted and C1 stored. They are **not**
  physical-device captures and **not** the offline preview renderer.
* **UI trees are the in-guest probe**: `LaunchMain`'s periodic read-only
  `View` walk on the app's main thread (`ACOSPROBE` lines). They are not an
  offline hierarchy dump.
* `frames/*.png` are the engine's copies of the reply PNGs for the steps that
  captured one; `art-frames/live-frames/*.png` are the session's own captures
  (one file per capture command).
* The session PNGs were written by `acos-art`'s minimal RGBA PNG writer (zlib
  stored blocks: correct but uncompressed, ~9.5 MB each). For the committed
  bundle they were re-encoded losslessly with Pillow optimise; pixel bytes were
  compared before/after and are identical (`W10-png-recompress.txt`). The md5
  values inside `W10-run.json` / `live-session.json` are the md5s of the
  session's original files, as the run itself recorded them.

## Reproduce

```bash
tools/art-runtime/run-exclusive.sh --task W10 -- \
  tools/guarded.sh --mem 8G --time 900 -- \
  ./target/debug/acos verify --scenario scenarios/v2-art-counter.yaml \
    --apk fixtures/build/apps/counter.apk --engine art \
    --artifacts results/art-runtime/evidence/W10-artifacts --json
```
