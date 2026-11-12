# D-M5 — where one warm screenshot's 80 ms goes (per-phase table)

**Diagnosis only. No product behaviour was changed.** Env-gated trace-only
additions in `crates/acos-cli` / `crates/acos-art` (`DM5_TRACE=<path>`, inert when
unset) and one read-only-in-spirit `crates/acos-services/src/surface_client.rs`
trace patch that was applied **only** in the throwaway worktree
`.cache/dm5-wt` (patch kept as `results/art-runtime/evidence/D-M5/
surface_client-dm5-trace.patch`; the shared tree's `surface_client.rs` was never
touched). Scratch: `.cache/dm5/`, never `/tmp`. Legacy repo
`/home/Qui/Work/android-container` was read only.

**Result in one line.** The 80.09 ms is **73 % PNG encode**, and inside encode
**97 % is two hand-written checksums**: `crc32` is bit-serial (**42.96 ms p50**,
`crates/acos-art/src/launch.rs:2667`) and `adler32` does a `%` per byte
(**15.80 ms**, `launch.rs:2679`). The 9.5 MB PNG is "nearly uncompressed" because
the encoder emits zlib **stored** blocks — compressing is not the cost, the
scalar checksums are. The second-largest chunk is **hashing that is not needed
for the frame contract as written**: `hash_pixels` FNV-1a over 41.7 MB in C1
(7.04 ms, `crates/acos-services/src/surface_client.rs:2358`) and a **`md5sum`
fork+exec over the finished PNG** (9.63 ms, `launch.rs:1790`). The C1 blit the
brief suspected is 1.53 ms (2 %), the memfd→host copy 1.30 ms.

## 1. Method

One `acos_observe {png:true, screenshot:path}` per sample on an
**already-held, warm `acos serve --engine art` session** (counter APK, 8 s
settle, 2 warm-ups discarded), measured on the daemon's own Unix socket with the
client's own clock, joined to in-process phase stamps on **one clock**
(`CLOCK_REALTIME` µs in every process — `Instant` is not comparable across
processes, so the traces emit `t_us` realtime). Harness
`results/art-runtime/evidence/D-M5/probe.py` (a fresh-socket JSON-RPC client, not
`agent_loop.Daemon`, because the write/reply split needs its own stamps);
aggregator `report.py`.

Every sample is freshness-checked exactly as W57 does it (`frame_seq >
previous_seq`, PNG magic, non-zero bytes): **24/24 fresh** in each of the two
runs below.

Two runs, each its own `run-exclusive.sh` acquisition
(`tools/guarded.sh --mem 8G --time 400`), same binary
(`.cache/dm5-wt-target/release/acos`, HEAD `5b043b5` + trace hunks):

| run | traces | n (fresh) | wall p50 | wall p95 | spread |
|---|---|---:|---:|---:|---:|
| **A — reference** | off | 24 (24) | **80.09 ms** | 81.44 | 4.3 % |
| **B — phase table** | on | 24 (24) | **83.62 ms** | 85.64 | 4.3 % |

Run A reproduces W57's **80.08 ms** (BENCHMARKS §12.2) to 0.01 ms, so the probe
measures the same thing the D2 row measures. Run B's +3.53 ms (+4.4 %) is the
tracer's own cost (11 file-append events per request, ~0.13 ms each); the
per-phase numbers below are from run B, and every share is also stated against
the untraced 80.09 ms.

Host: loadavg1 ≤ 4 gate, MemAvailable ≥ 30 GiB, no rep excluded, 0 survivors.

## 2. The phase table (run B, n=24, microseconds)

Ordered as the brief asks. Rows marked `of which` nest inside the row above them,
so the nine brief phases are: **1** client write, **2** serve dispatch,
**3** request→launcher, **4** C1 composite/capture (with its own sub-table),
**5** frame transfer guest→host, **6** PNG encode (sub-table), **7a/7b** file
write, **8a/8b** hashing, **9** reply.

| # | phase | p50 | p95 | min | max | % of 80.09 ms |
|---|---|---:|---:|---:|---:|---:|
| 1 | client request write (`sendall` of the request line) | **5** | 6 | 3 | 7 | 0.01 |
| — | socket ready → request line parsed by serve | 152 | 204 | 97 | 205 | 0.19 |
| 2 | serve dispatch (`handle_request`, includes everything below) | **83 573** | 85 599 | 80 676 | 87 022 | 104.3 (nesting) |
| 3 | of which: request to the guest/launcher (`acos-art` round trip) | **82 030** | 84 197 | 79 229 | 85 601 | 102.4 |
| 4 | of which: **C1 composite/capture** | **10 199** | 10 429 | 9 596 | 10 875 | **12.7** |
| 5 | of which: frame transfer guest→host (`read_at` on the memfd, inside 4) | **1 302** | 1 485 | 1 096 | 1 570 | 1.6 |
| 6 | of which: **PNG encode** | **60 920** | 62 445 | 59 012 | 63 848 | **76.1** |
| 7a | of which: launcher writes its own frame file (`fs::write`, no fsync) | **924** | 1 172 | 843 | 1 364 | 1.2 |
| 8a | of which: **`md5sum` fork+exec over that file** | **9 631** | 10 170 | 9 014 | 10 306 | **12.0** |
| — | of which: pipe + launcher loop remainder (IPC) | 401 | 502 | 275 | 514 | 0.5 |
| 5/8b | of which: serve reads the frame file **back** (`read_png`) | **415** | 484 | 350 | 529 | 0.5 |
| 7b | of which: serve writes the caller's PNG path | **904** | 1 153 | 768 | 1 402 | 1.1 |
| — | of which: envelope build + JSON (accounting residual) | ≈115 | — | — | — | 0.1 |
| 9 | reply write (`write_all` + flush) | **10** | 10 | 7 | 11 | 0.01 |
| — | client read of the reply | 4 | 5 | 2 | 6 | 0.01 |

Wall p50 **83 620 µs** (traced) / **80 090 µs** (untraced reference).

### 2.1 C1 composite internals (patch, worktree only; n=24)

The live session composites **1 layer with a pixel buffer** out of 6 registry
layers into a **1080×2209** canvas (the W58 app area — not 1080×2400; the PNG is
1080×2209×4 B of pixels, so it is the app area, not the display).

| sub-phase | p50 | p95 | % of C1's 10.2 ms |
|---|---:|---:|---:|
| `kernel_rows` + `acos_sfgeom::compute` (B1b kernel) | 7 | 11 | 0.1 |
| `read_buffer_pixels` — **the guest→host frame transfer** (`read_at` from this process's dup of the `GraphicBuffer` memfd, 9.54 MB) | **1 302** | 1 485 | 12.8 |
| `blend_layer` — **the blit of layers into the canvas** | **1 529** | 1 581 | 15.0 |
| canvas `vec![0u8; …]` (9.54 MB) | 226 | 266 | 2.2 |
| **`hash_pixels` — FNV-1a over 41.7 MB + per-pixel non-zero scan** | **7 044** | 7 314 | **69.1** |

So the blit the brief pointed at (B1b's `capture_composite`) is **1.5 ms**; the
phase's real cost is its evidence hash.

### 2.2 PNG encode internals (n=24)

Encoder: hand-written RGBA8 writer, **filter type 0 per row**, zlib **"stored"
deflate blocks**, real CRC32/Adler-32 (`crates/acos-art/src/launch.rs:2542-2690`).
Output 9 545 882 B for 9 542 880 B of pixels (100.03 % — no compression at all,
by construction, not by a compression-level setting).

| sub-phase | p50 | p95 | % of encode's 60.9 ms |
|---|---:|---:|---:|
| filter-0 row copy (9.54 MB memcpy) | 220 | 683 | 0.4 |
| zlib stored-block framing (second 9.54 MB copy) | 611 | 728 | 1.0 |
| **`adler32` over 9.54 MB, `% 65_521` per byte** | **15 801** | 16 355 | **25.9** |
| IDAT chunk: 9.54 MB copy + `crc_input` copy + CRC32 | **44 049** | 45 478 | 72.3 |
| …of which **`crc32`, bit-serial (8 iterations/byte, no table)** | **42 964** | 44 411 | **70.5** |

**adler32 + crc32 = 58.8 ms = 96.5 % of the encode and 73.4 % of the whole M5
request.** Everything else the encoder does (all four full-buffer copies
included) is ≈1.0 ms.

## 3. Dominant phases, file:line

| rank | phase | p50 | file:line |
|---|---|---:|---|
| 1 | `crc32` — bit-serial PNG chunk CRC over the IDAT body | **42.96 ms** | `crates/acos-art/src/launch.rs:2667` (called at `:2648` `png_chunk`, from `:2598` `encode_png_rgba`) |
| 2 | `adler32` — `%` per byte, no NMAX chunking | **15.80 ms** | `crates/acos-art/src/launch.rs:2679` (called at `:2637`) |
| 3 | `md5sum` fork+exec + full 9.54 MB file read for `content_hash` | **9.63 ms** | `crates/acos-art/src/launch.rs:1790` (`md5_hex_file`, `Command::new("md5sum")`), called from `capture_c1` `:1738` |
| 4 | `hash_pixels` FNV-1a + non-zero scan over the canvas | **7.04 ms** | `crates/acos-services/src/surface_client.rs:2358` (callers `:1297` composite, `:1279` pass-through) |
| 5 | `blend_layer` blit into the canvas | **1.53 ms** | `crates/acos-services/src/surface_client.rs:2415` |
| 6 | frame transfer guest→host: `read_at` the memfd into a fresh `Vec` | **1.30 ms** | `crates/acos-services/src/surface_client.rs:2336` (`read_buffer_pixels`) |
| 7 | launcher writes the frame file | **0.92 ms** | `crates/acos-art/src/launch.rs:1734` (`fs::write`) |
| 8 | serve writes the caller's PNG | **0.90 ms** | `crates/acos-cli/src/engine/serve.rs:1083` |
| 9 | serve reads the frame file back (`read_png`) | **0.42 ms** | `crates/acos-cli/src/engine/art.rs:2110` |

Copy count for one pixel of canvas, before the caller has its file:
1. guest→host: `read_at` memfd → `Vec` (`surface_client.rs:2336`);
2. `blend_layer` canvas (`:2415`) — for a 1-layer full-window frame this is a
   plain row copy, i.e. a second full copy;
3. PNG row copy into `filtered` (`launch.rs:2600-2610`);
4. `filtered` → `zlib` stored blocks (`:2622-2634`);
5. `zlib` → `crc_input` for the CRC (`:2653-2656`);
6. `zlib` → `out` (`:2651`);
7. `fs::write` → page cache (`:1734`);
8. `md5sum` reads it back (`:1790`);
9. `read_png` reads it again (`art.rs:2110`);
10. `fs::write` the caller's path (`serve.rs:1083`).

## 4. What legacy ACVM did in 66.5 ms

Legacy M5 was measured as `self.acvm.run("shot", sid)` =
`subprocess.run([python3, agent/acvm, "shot", <id>])`
(`bench/score.py:495`, `bench/lib/harness.py:179-189`), warm, request-sequenced,
`fresh && non_black` gated. The path inside that call:

1. `commands.shot` → `redroid_backend.device_for(...).shot()` →
   `backend.capture(state, core, idx)` (`agent/acvm_lib/redroid_backend.py:1416`).
2. **One loopback TCP request** through the pre-forwarded port:
   `CAPTURE <idx> <seq> <token>\n` (`_capture_socket`, `:1343`).
3. Inside the container, the vdisp helper (`helper/vdisp/src/com/acvm/vdisp/
   VdService.java:188-208`) does `captureFrame(idx, seq, reqMs, 4000)`: it
   **forces one fresh redraw** by re-attaching the `VirtualDisplay` surface and
   waits for a cached frame whose `pixTs >= reqMs` **and** whose frame counter
   advanced — freshness bound to the request, deliberately preferring a redraw
   over a cached frame (`:303-330`).
4. `encodePng` = `Bitmap.createBitmap().copyPixelsFromBuffer()` +
   **`Bitmap.compress(PNG, 100)`** — native Skia/libpng **real deflate**
   (`:334-341`).
5. Header line + **raw PNG bytes on the socket**; the host reads them into a
   `bytearray`, writes **one** file (`cap_<seq>.png`), and takes
   `hashlib.sha256(data)[:16]` (native OpenSSL, `:1365-1389`).
6. The reply JSON is printed by the CLI; the number includes that process.

Consequences, and this is the honest comparison:

* **Legacy's 66.5 ms contains a whole Python process** (`python3 -c pass` was
  measured at **15.3 ms** on that host and the legacy repo's own
  `research/phase0/CLI-OVERHEAD.md` says "a bare `acvm` invocation pays ~15 ms
  before any container work"). **ACOS's 80.09 ms contains no process spawn at
  all** — it is one in-socket request on a held daemon. Measured like-for-like
  in process terms, ACOS's capture path is not 1.20× but roughly **2×** the
  legacy helper work. (The D2 row is still a FAIL; this is about where the time
  must be found, not a re-labelling.)
* Legacy does **native deflate** and a **native sha256**; ACOS does
  uncompressed stored blocks with **scalar, byte-at-a-time checksums** and
  shells out to **`md5sum`**. Both of the dominant ACOS costs are choices, not
  physics.
* Legacy's per-phase numbers (`waited_ms`, `enc_ms` — the helper even reports
  encode duration on the wire) were available but never recorded in a result
  file; the 66.5 ms is the only published value, so no legacy phase table can be
  reconstructed without re-running the frozen repo. It was not re-run.

## 5. Candidate improvements (not implemented)

Savings are against the **untraced 80.09 ms p50**; "byte-identical" means the
PNG bytes, the `content_hash`, the `frame_seq` binding and the W58 frame
geometry are all unchanged. Ranks 1-3 are the whole story: they remove 93 % of
the dominant phase.

| # | change | expected saving | correctness risk (freshness/frame binding, content hash, W58) |
|---|---|---|---|
| 1 | **Table-driven / slice-by-8 or pclmul CRC32** in `launch.rs:2667` (no new dep needed: `std::arch::x86_64::_mm_clmulepi64_si128`, or a 1 KiB table) | **−38 to −42 ms (47-52 %)**; 9.54 MB at 2-10 GB/s vs today's 241 MB/s | **none** — a correct CRC produces the identical 4 bytes; add a test against zlib's CRC of a known vector. Freshness, `content_hash` and W58 untouched |
| 2 | **adler32 with zlib's NMAX (5552-byte) deferred-modulo loop** (`launch.rs:2679`) | **−15.0 ms (19 %)** | **none** — identical output; same known-vector test |
| 3 | **Fold `hash_pixels` into a word-at-a-time pass** (read `u64`s for FNV and the non-zero test; `surface_client.rs:2358`) | **−6 ms (7.5 %)**; 41.7 MB at ~13 GB/s | **low/medium**: `pixels_fnv1a` is a published evidence field and `non_zero_pixels` proves "not a black frame" — the *algorithm* must stay FNV-1a over the same bytes in the same order to keep the value stable; the non-zero count must stay exact. No freshness/W58 risk (it hashes the canvas it already built) |
| 4 | **Stop forking `md5sum`; derive `content_hash` in-process** (`launch.rs:1790`). Best form: stream MD5/SHA-256 over the `png` `Vec` that is already hot, or (cheaper, semantic change) reuse the C1 pixel hash | **−7 to −9.6 ms (9-12 %)** | **medium-high**: today `tree.frame.content_hash == md5(PNG file bytes)` (`engine/mod.rs:569` ← `serve.rs:1069` ← `evidence.md5`). Hashing in-memory is semantics-preserving but a naive Rust MD5 (~400-600 MB/s) is *slower* than `md5sum`; it only wins once the PNG is small (row 7) or with a vendored fast hash. Dropping to a pixel hash changes the published value; `assert --changed-since`/`visual_change` (`art.rs:2480`) keep working (equality, not format) but stored baselines don't. Freshness unaffected |
| 5 | **Assemble the PNG in one buffer** — write sig/IHDR, then stored-block headers + rows straight from the canvas, CRC/Adler computed in the same pass (kills copies 3-6 of §3) | **−1.0 ms (1.3 %)** + ~29 MB less transient RSS | **low**: byte-identical if the block boundaries (65 535) and the per-row filter-0 byte are reproduced exactly; a byte-for-byte test against the current encoder is the gate |
| 6 | **One file, not three**: pass the caller's path to the launcher so the frame is written once (kills copies 7-10: launcher write, `read_png`, serve write; keeps the evidence copy as a hard link or keeps `md5sum` on the same file) | **−2.2 ms (2.8 %)** | **medium**: the launcher must validate the caller's path (it is host-provided, but `serve` currently owns the `ACOS_ALLOWED_ROOTS` check, `serve.rs:301-320`); the per-capture evidence file in `frames_dir` is part of the artifact trail — a hard link keeps it. The reply still names the same file |
| 7 | **Real deflate (level 1) instead of stored blocks / a "faster PNG level"** — the brief's hypothesis | **worse on latency**: +30-50 ms of `miniz_oxide`/`libz` CPU, in exchange for 9.54 MB → ~1.5-2 MB, which buys back only **−8 ms** of `md5sum`, **−1.8 ms** of the two file writes/one read, and a far smaller artifact | low on correctness (still a valid PNG), but **this is the wrong lever for M5**: after ranks 1-2 the encode is ~3 ms and the bytes are the only thing left. Worth doing for *evidence size*, not for the 66.5 ms bar |
| 8 | **Zero-copy / mmap the memfd and blend straight from the mapping** (`surface_client.rs:2336`) | **−1.3 ms (1.6 %)** | **low**: same pixels, same canvas; the mapping must be `MAP_SHARED|PROT_READ` on the dup'd fd and a short/failed map must stay a structured `CompositeError::Unreadable`. No freshness/W58 risk. Only worth it bundled with rank 3 |
| 9 | **Encode off the reply path** (reply after `fs::write`, encode/serve-read asynchronously) | at most the encode's 60.9 ms *if* the contract allows | **high**: `observe` reports `bytes`, `meaningful` and writes the caller's PNG before replying (`serve.rs:1060-1091`); the W57 harness (and every agent) treats "reply received" as "file readable". Deferring makes freshness a race — **not recommended**; ranks 1-2 make it pointless |
| 10 | *Rejected:* skip C1's `hash_pixels` | −7 ms | breaks `pixels_fnv1a`/`non_zero_pixels` evidence (the "not a black frame" proof) |

Expected after ranks 1-3 only (no semantic change anywhere): **80.09 →
≈21 ms p50** (−74 %), i.e. **3.2× faster than the 66.5 ms legacy bar**; with
ranks 5-8 as well, **≈17-18 ms**. Ranks 1-2 alone already take M5 from FAIL
(1.20×) to a comfortable PASS (≈2.6×).

## 6. Commands

```sh
CARGO_TARGET_DIR=.cache/dm5-target cargo build --release -p acos-cli -p acos-art --offline
git worktree add --detach .cache/dm5-wt 5b043b5        # + the DM5 trace hunks + the surface_client patch
cd .cache/dm5-wt && CARGO_TARGET_DIR=…/.cache/dm5-wt-target cargo build --release -p acos-cli -p acos-art --offline

# A: reference, traces off (reproduces W57 80.08 ms)
tools/art-runtime/run-exclusive.sh --task DM5 -- tools/guarded.sh --mem 8G --time 400 -- \
  python3 -I .cache/dm5/probe.py --acos .cache/dm5-wt-target/release/acos \
  --outdir .cache/dm5/runref --no-trace --samples 24 --warmup 2 --settle 8

# B: phase table, traces on
tools/art-runtime/run-exclusive.sh --task DM5 -- tools/guarded.sh --mem 8G --time 400 -- \
  python3 -I .cache/dm5/probe.py --acos .cache/dm5-wt-target/release/acos \
  --outdir .cache/dm5/run1 --samples 24 --warmup 2 --settle 8

python3 .cache/dm5/report.py .cache/dm5/run1/probe.json      # the tables in §2
```

Evidence (raw per-sample JSON with each sample's own trace events, the trace log,
both runners/aggregator, the `surface_client.rs` patch):
`results/art-runtime/evidence/D-M5/`.

## 7. Residuals

1. The traced run is 83.62 ms vs 80.09 ms untraced; the +4.4 % is the tracer
   (11 appends/request). Per-phase *shares* are computed on the traced total and
   cross-checked against the untraced reference, so no conclusion depends on the
   tracer's cost: even charging all 3.53 ms to the encode, encode is 57.4 ms =
   72 % of the request, and CRC+Adler are 73 % of it.
2. `crc32`/`adler32` throughputs here (241 MB/s and 656 MB/s) are single-run
   scalar numbers on this host; the improvement estimates assume 2-10 GB/s for a
   slice-by-8/pclmul CRC and ~12 GB/s for a chunked Adler, both published figures
   for those implementations, not measured here.
3. Legacy's 66.5 ms could not be split into per-phase numbers: the frozen repo
   emits `waited_ms`/`enc_ms` on the wire but no result file in the tree records
   them, and re-running it was out of scope (read-only, frozen). The "~15 ms of
   Python startup" component is the legacy repo's own documented estimate
   (`research/phase0/CLI-OVERHEAD.md`), not a measurement of the 66.5 ms sample.
4. The canvas is **1080×2209** (the W58 app area), not the 1080×2400 display the
   brief cites; the PNG is 9 545 882 B because it is 100.03 % of the 1080×2209
   raw pixels. The row is still the same row.
