# SHOTZ — a screenshot is a compressed (lossless) PNG, and an MCP image reply is bounded

**Branch:** `screenshot-compress` off `rel3-int` `19db6b8` (v0.2.0 candidate).
**Worktree:** `/home/Qui/.paseo/worktrees/3oxa6xao/gr4`.
**Decision:** ADR-0027 #6 (`DECISIONS.md`, main `5f20b82` — the ADR text is on
`main`, which is ahead of `rel3-int`, so it is quoted here rather than carried in
this branch's `DECISIONS.md`).
**Experiment:** `EXPERIMENTS.md` E9.4.
**Date:** 2026-11-14/15.

> **ADR-0027 #6.** *Screenshots: compress.* Default capture becomes a compressed
> lossless PNG (fast deflate + filter), still exact for pixel assertions, with M5
> at parity or better (smaller write + hash should offset the encode). Images
> returned to agents over MCP are additionally size-bounded (downscaled lossy
> preview + path to the lossless file), since a full-resolution PNG in a tool
> reply is wasted context.

---

## 1. Why (measured, not assumed)

ARTGC (`results/handoff/ARTGC.md`) found that every capture PNG was **exactly
10 371 258 B** at 1080x2400: the capture path wrote zlib *stored* blocks with
filter 0 — an unfiltered 9.9 MB raw buffer in a PNG wrapper. The capture path
paid for it twice: `crates/acos-art/src/launch.rs`'s `encode_write_and_hash`
(1) encoded 9.5 MB (the deflate "encode" was a memcpy) and (2) wrote **and
MD5'd** 9.9 MB to disk, ~11 ms of the ~24 ms M5.

## 2. What shipped

### 2.1 Capture encoder — real deflate, still lossless (`acos-art`)

`encode_png_rgba_stream` keeps its shape (one pass, PNG bytes handed to the caller
as they are produced, so `encode_write_and_hash` still writes and MD5s the same
bytes it encodes) and changes the payload:

* **fdeflate** (the fast deflate tuned for PNG; fixed-Huffman), not stored blocks;
* **per-row adaptive filter**, Sub (1) or Up (2), chosen by the sum of absolute
  deltas **sampled every 8th byte** (`FILTER_SAMPLE_STEP = 8`) — the exhaustive
  sum picks the same filter on these frames for ~1/8 the cost;
* filtered rows are written **straight into a 64 KiB deflate block** (no
  row-sized scratch buffer, no frame-sized filtered copy);
* IDAT is emitted in ≤1 MiB chunks (a real frame's zlib stream is one chunk);
* **floor**: if deflate cannot shrink a frame (pure high-entropy noise — fixed
  Huffman expands ~9 %), the encoder emits the stored-block layout instead, so a
  capture is never materially bigger than before. Real UI frames are not in
  that regime (they shrink ~130x);
* `content_hash` is **still the MD5 of exactly the file's bytes** — unchanged
  meaning, unchanged value for a given file, and md5sum-identical (tested);
* the historic stored-block encoder is kept under `cfg(test)` as
  `encode_png_stored_stream`, compared byte-for-byte against an independent
  oracle so the "before" numbers below come from the code v0.2.0 shipped.

Pixel assertions are unaffected by construction: `assert_region` /
`assert_screenshot` decode the PNG (`crates/acos-verify/src/screenshot.rs`), and
the decoded pixels are bit-identical.

### 2.2 MCP image replies are bounded (`acos-mcp`)

`crates/acos-mcp/src/preview.rs` (new):

* an image in a reply is inlined as a **preview**: long side ≤
  `ACOS_MCP_PREVIEW_MAX_SIDE` (default **1280**) and file ≤
  `ACOS_MCP_MAX_IMAGE_BYTES` (default **512 KiB**). 1080x2400 → **576x1280**;
* if a preview would still exceed the byte cap (high-entropy frame), the long
  side is halved until it fits — never a multi-MB blob;
* the reply also carries a text line with both sizes and the path of the
  **lossless** file, and the file is still emitted as a `resource_link`, so the
  exact pixels stay one read away;
* a file that does not decode as a PNG, and JPEG/WebP (no encoder available
  offline — only `jpeg-decoder` is in the lock), is **linked, never inlined**;
* the Flutter backend's `png_base64` is bounded the same way and reports
  `png_preview {scaled, width, height, source_*, inline}`; `png_bytes` remains
  the real file size.

So the lossy-JPEG half of the ADR is deliberately **not** shipped: the workspace
has no offline JPEG *encoder*, and JPEG cannot be pixel-honest anyway. A PNG
preview of a real capture is ~20-40 KB, which is bounded and readable; the
downscale is what saves the context, the codec saves little.

### 2.3 ARTGC's re-encode is now a no-op (`acos-verify`)

`retention::reencode_retained_frames` only re-encodes frames whose IDAT is a
**stored-block** zlib stream (`retention::is_stored_zlib_png`, one byte of the
first deflate block header). A frame this encoder wrote is already deflated, so
the pass skips it — no decode + encode per frame, and the frame the verdict's
`md5` names is left byte-identical.

---

## 3. Size table

Measured in-tree by the `#[ignore]`d test
`launch::shotz_png_tests::png_encoder_candidates_on_real_frames` on the five real
1080x2400 ART captures in `results/parity/w198a/` (each file decodes to exactly
10 368 000 B of RGBA):

```text
frame                   stored_B compressed_B  stored_ms    comp_ms     size
counter                 10371258       75489       6.22       1.95     0.7%
dialog-menu             10371258       84450       5.86       1.94     0.8%
form-validation         10371258       77587       5.83       1.89     0.7%
hello-views             10371258       88555       5.73       1.94     0.9%
text-input              10371258       85396       5.80       1.96     0.8%
```

| | before | after | Δ |
|---|---:|---:|---:|
| capture PNG, 1080x2400 UI frame | 10 371 258 B | **75–89 KB** | **−99.2 %** (~120–137x) |
| encode (filter + deflate, in process) | 5.7–6.2 ms | **1.9 ms** | −68 % |
| write + MD5 of the file | ~11 ms over 9.9 MB | **~0.1 ms over ~80 KB** | −99 % |
| `content_hash` | MD5 of the file's bytes | same | unchanged |

Candidate sweep that chose it (same frames, single-frame harness, deflate alone
included the filter):

| candidate | bytes (counter / dialog) | encode |
|---|---:|---:|
| stored (shipped before) | 10 371 258 / 10 371 258 | 33–36 ms (2 passes) |
| fdeflate + Sub | 94 382 / 107 975 | 0.8 ms |
| fdeflate + Up | 178 747 / 190 509 | 0.9 ms |
| fdeflate + Adaptive (exhaustive) | 72 954 / 81 700 | 5.9 ms |
| **fdeflate + Adaptive (sampled 1/8)** | **75 519 / 84 308** | **1.9 ms** |
| flate2 L1 + Adaptive | 76 489 / 84 864 | 6.8 ms |
| flate2 L3 + Adaptive | 29 719 / 37 057 | 15.0 ms |
| flate2 L6 + Adaptive | 29 256 / 36 517 | 16.1 ms |

The shipped choice is the knee: full-adaptive quality for a third of the encode,
and 3x cheaper than the stored framing it replaces. L3/L6 buy another ~2.5x
smaller file for ~8x the encode — the capture is on the screenshot critical path
and 80 KB is already far below the 1 MB target, so that trade is not taken.

Real UI frames of an *opaque* app are one colour per band; the frames above are
the fixtures' worst realistic case (text, form fields, a dimmed dialog).

## 4. M5 and the rest of the latency metrics (interleaved A/B)

See §6 for the run. Method is `results/bench/INT-KG-AB.md`'s:
`tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300" --exclusive`,
3 reps per arm, alternating arms, main arm = a build of `rel3-int` in place,
branch arm = this branch; medians of per-rep p50s; gate +5 % / +2 ms.

Interleaved, `tools/bench/acos-bench.sh --reps 1 --groups "a:latency:8G:300" --task SHOTZAB`,
3 reps per arm first (base, fix, base, fix, base, fix), then one more alternating
pair to resolve a noise-level row: **4 reps per arm**, base = `19db6b8` built in a
linked worktree (`$WT/.cache/shotz-base`, `.cache/base-target`), fix = this branch
built in `$WT/.cache/gr4-target`. Medians of per-rep p50s; gate +5 % **or** +2 ms.
Raw runs and per-arm JSON: `results/bench/SHOTZ-evidence/`.

| metric | base (rel3-int 19db6b8) | fix | Δ |
|---|---:|---:|---:|
| M4 launch strict | 305.90 ms | 305.73 ms | -0.1 % (-0.17 ms) |
| M4 launch fast | 155.21 ms | 155.82 ms | +0.4 % (+0.61 ms) |
| M5 screenshot (PNG) | 24.09 ms | 11.68 ms | -51.5 % (-12.40 ms) |
| M6 reset strict | 339.92 ms | 341.20 ms | +0.4 % (+1.27 ms) |
| M6 reset fast | 198.60 ms | 195.86 ms | -1.4 % (-2.74 ms) |
| M7 tree strict | 41.38 ms | 41.34 ms | -0.1 % (-0.04 ms) |
| M7 tree fast | 0.56 ms | 0.54 ms | -4.1 % (-0.02 ms) |
| M8 tap counter strict | 956.03 ms | 948.42 ms | -0.8 % (-7.61 ms) |
| M8 tap counter fast | 47.71 ms | 49.45 ms | +3.6 % (+1.73 ms) |
| M8 tap form strict | 948.86 ms | 927.58 ms | -2.2 % (-21.27 ms) |
| M8 tap form fast | 45.24 ms | 32.35 ms | -28.5 % (-12.89 ms) |
| tap ack | 5.00 ms | 5.00 ms | +0.0 % (+0.00 ms) |
| teardown | 13.00 ms | 13.00 ms | +0.0 % (+0.00 ms) |
| teardown wall | 19.15 ms | 19.10 ms | -0.2 % (-0.05 ms) |
| active CPU (strict taps) | 46.61% | 45.60% | -2.2 % (-1.01 ms) |

M5 **halves** (−12.4 ms): the 9.9 MB write+MD5 that used to overlap a ~7 ms
encode is gone, so what remains is the capture itself plus a ~2 ms encode and a
~0.1 ms write of ~80 KB. Every other metric is inside the gate.

Reading note: on the first 3 reps `m8_tap_counter_fast` sat at +5.3 % (+2.54 ms)
while its two arms overlapped at the edges (base 46.5-48.7, fix 48.6-50.5); the
4th alternating pair (fix 47.7 ms — the base median) put the 4-rep median at
+3.6 % (+1.73 ms), inside the gate. `m8_tap_form_fast` moving −28.5 % in the same
cohort is the mirror image of the same session-level noise: neither path touches
the capture encoder. Nothing in this change is on the tap path at all.


## 5. Acceptance evidence

Raw logs: `.cache/shotz-gates.log` / `.cache/shotz-gates2.log`, verdicts in
`.cache/shotz-gates/artifacts/<scenario>/`, summaries
`.cache/shotz-gates/summary{,2}.tsv`, bench evidence
`results/bench/SHOTZ-evidence/` (`base-r{1..4}.json`, `fix-r{1..4}.json`, run
logs), capture sizes re-measured from the run artifacts.

**Gates — 15 as expected, 0 false passes.**

| gate | expected | got |
|---|---|---|
| `v2-art-counter` | pass | pass |
| `form-validation` | pass | pass |
| `w54-dialog-pixels` *(pixel gate)* | pass | pass |
| `w42-back-frame` *(pixel gate)* | pass | pass |
| `w15-dialog-menu` | pass | pass |
| `suite/T03-render-good` *(pixel gate)* | pass | pass |
| `v1-offscreen-pager` | pass | pass |
| `w7-insets-report` | pass | pass |
| `p11-activity` | pass | pass |
| `p11-list-smooth` | pass | pass |
| `suite/T03-render-wrong` | fail `ASSERTION_FAILED` | fail `ASSERTION_FAILED` |
| `w7-insets-carve-negative` | fail `ASSERTION_FAILED` | fail `ASSERTION_FAILED` |
| `negative/se1-expect-change-noop` | fail `ASSERTION_FAILED` | fail `ASSERTION_FAILED` |
| `negative/offscreen-pager-visible` | fail `ELEMENT_OFFSCREEN` | fail `ELEMENT_OFFSCREEN` |
| `p11-progress-timeout` | `TIMEOUT` | `timeout` / `TIMEOUT` |

The three pixel gates read decoded PNG pixels (and `T03-render-wrong` must still
*fail* on them): they pass/fail exactly as before, which is the proof that the
compression is pixel-exact. Two further scenarios I tried as pass gates
(`counter.yaml`, `hello-views.yaml`) exit `unsupported` / `UNSUPPORTED_BACKEND`
on `input.rotate` — ART has no rotate command in this tree, pre-existing and
unrelated, so they are not gates; `p11-activity` and `p11-list-smooth` are the
two that complete the ten.

**Gaten — 5 flows pass, 5 negatives `ASSERTION_FAILED`**, APK
`/home/Qui/Projects/acos/.cache/gaten/gaten-dev-f344272-x86_64-release-unsigned.apk`:
`gaten-home`, `gaten-tabs`, `gaten-settings`, `gaten-search`, `gaten-lesson` pass;
`negative/gaten-{home,tabs,settings,search,lesson}-wrong` each fail
`ASSERTION_FAILED`. 0 false passes.

**Capture sizes measured on the gate runs themselves** (302 PNGs written by the
new encoder across the 15 gates + 5 Gaten flows + 5 negatives):

| | bytes |
|---|---:|
| counter frames (16) | 69 284 (median), max 69 539 |
| form-validation frames (16) | 85 441 (median), max 103 487 |
| dialog frames (14) | 83 649 (median), max 103 487 |
| Gaten home frames (24) | 194 329 (median), max 239 032 |
| all 302 | p10 74 698 · p50 156 104 · p90 226 943 · max 431 595 |
| all 302, files ≥ 1 MB | **0** |
| any file at the old 10 371 258 B | **none** |

**Tests:** `cargo test --release --offline -p acos-art -p acos-cli -p acos-mcp
-p acos-verify` → rc=0, **46 suites, 0 failures** (log
`.cache/shotz-tests-final.log`): acos-art 121 lib tests + 8
integration (`cleanup`), acos-mcp 59 lib, acos-verify retention 12, plus the
acos-cli suites — green. Two ART-spawning integration tests
(`launch_hang::guest_exits_before_milestone…`,
`cleanup::log_spam_keeps_the_launcher_peak_rss_under_256_mib`) fail only when the
whole suite runs under host load and pass in isolation (0.26 s; 55.6 MiB peak vs
the 256 MiB limit) — see §6; INT-KG-AB recorded the same `launch_hang` flake on
`main` (LHFLAKE).


## 6. Decisions and what is left

* **Sampled adaptive filter** (1/8) instead of the exhaustive sum: same filter
  choice on every real frame measured, ~1/8 the cost. A frame whose rows tie is
  unaffected (ties pick Sub, as the exhaustive version did).
* **PNG preview, not JPEG**: no offline JPEG encoder in this workspace; a PNG
  preview of a real capture is already 20-40 KB. If an assistant-side consumer
  ever needs a *lossy* preview, `image` + `jpeg-encoder` would be the change, and
  it belongs behind another ADR because it makes the preview lossy by design.
* **Stored-block floor**: a pathological high-entropy frame is heavier in
  fixed-Huffman deflate, so the encoder falls back to the stored layout. Cost: one
  extra pass, only in that case.
* **`content_hash` semantics unchanged**: still the MD5 of the file's bytes.
  Hashing raw pixels instead would be ~free but would change the meaning of every
  recorded `md5`/`content_hash` and break `frame-diff`/`parity` comparisons, so it
  was not done.
* **Not done:** the retention pass still *decodes* a stored frame to re-encode it
  (unavoidable); nothing else re-encodes.
* **Left open (pre-existing, filed):** `acos-art`'s
  `launch_hang::guest_exits_before_milestone…` and
  `cleanup::log_spam_keeps_the_launcher_peak_rss_under_256_mib` fail only when
  the whole suite runs under host load (both pass in isolation: 0.26 s and
  55.6 MiB peak against the 256 MiB limit); INT-KG-AB recorded the same
  `launch_hang` flake on `main` and queued it as LHFLAKE.
