# HUMANVIEW1 Report — Interactive `acos view`

**Branch:** `humanview` (from `v0.4.1` / `40389d8a7`, PR #6 open, not merged)
**Decision:** ADR-0033 (`DECISIONS.md`) amends ADR-0027 §5
**Spec:** `docs/HUMAN_VIEW_SPEC.md`
**Status:** Automated CP1–CP5 + Fix Rounds 1–3 + Live T3 & Playwright Browser E2E + Fix Round 4 (CodeRabbit PR #6 review fixes) complete; **Owner one-time Gaten check** remains manual (runbook in §7).

---

## 1. Checkpoints & Commits

| CP | Commit | Pushed | Summary |
|---|---|---|---|
| Spec | `aa6579dea` | yes | `docs/HUMAN_VIEW_SPEC.md` baseline |
| CP1 | `b64240dca` | yes | Port `source` labels (`agent`/`panel`), verify-run `watch_only`, `device_name`, and `panel_*` dispatch |
| CP2 | `83f8e5d99` | yes | Hub RFC 6455 `/api/input/<id>` WebSocket, Host/token/cap enforcement, ordered per-session worker, keycodes, disconnect `cancel`, live tests |
| CP3 | `8747ab7d9` | yes | Interactive page UI (`page.rs`), pointer→touch stream, wheel→scroll, keyboard→text/key, Back/Home/Recents/Rotate toolbar, verify banner, upstream upgrade/revert |
| CP4 | `896003a19` | yes | Producer rate-limiter fix on empty polls, 1.0× direct copy (`rgb_scratch`/`encode_scratch` reuse), interactive upgrade to 1.0× PNG @ 30 fps, latency harness (`humanview_cp4.rs`), 4-rep A/B & idle-CPU benchmarks |
| CP5 | `57882cacb` | yes | ADR-0033 (`DECISIONS.md`), doc updates (`LIVE_VIEW.md`, `AGENT_GUIDE.md`, `MCP.md`, `README.md`, `docs/internals/{decisions,overview}.md`, `HUMAN_VIEW_SPEC.md`), `results/handoff/HUMANVIEW1/REPORT.md` |
| Fix 1 | `8867ae192` | yes | Immediate MJPEG closing boundary flush, `<canvas>` post-`load` reader + DOM-stable hub grid for T3 preview, deterministic rate-limiter test, `has_home` plumbing, detach frame clear (§8) |
| Fix 2 | `67af8e877` | yes | Documented `has_home` in `LIVE_VIEW.md` and stopped infinite 400 ms frame retries on ended sessions (§9) |
| Fix 3 | `f9b903d28` | yes | Keyed compositor `BufferCache` by `(token, id)` with `uncacheBuffers` and process-death release (§10) |
| Fix 4 | *(this commit)* | yes | CodeRabbit PR #6 review fixes (11 Majors + Minors + pre-PR notes) and tightened live tests (§11) |

---

## 2. Acceptance Matrix (`docs/HUMAN_VIEW_SPEC.md` §6)

| # | Acceptance Item | Status | Evidence / Exact Numbers |
|---|---|---|---|
| 1a | WebSocket tap on `counter.apk` "Increment" increments counter (via `observe_tree`) | **PASS** | `humanview_cp2::cp2_ws_live_tap_text_back_and_source_labels` |
| 1b | Drag scrolls a list (`list-scroll.apk`, visible first row changes) | **PASS** | `humanview_cp3::cp3_live_drag_fling_long_press_and_rotate` |
| 1c | Long press triggers `GestureDetector.onLongPress` | **PASS** | `humanview_cp3::cp3_live_drag_fling_long_press_and_rotate` |
| 1d | Typed text via `/api/input` appears in `EditText` (`form-validation.apk`) | **PASS** | `humanview_cp2::cp2_ws_live_tap_text_back_and_source_labels` |
| 1e | `KEYCODE_BACK` finishes second activity (`multi-activity.apk`) | **PASS** | `humanview_cp2::cp2_ws_live_tap_text_back_and_source_labels` |
| 1f | Rotate command changes display dimensions (`1080x2400` ↔ `2400x1080`) | **PASS** | `humanview_cp3::cp3_live_drag_fling_long_press_and_rotate` |
| 2 | Human input labelled `source: panel`, agent input `source: agent` in `/api/events` | **PASS** | `humanview_cp1::cp1_event_feed_source_labels_and_app_label`, `humanview_cp2::cp2_ws_live_tap_text_back_and_source_labels` |
| 3 | Verify run under `acos view` refuses input (`VERIFY_WATCH_ONLY`); verdict unchanged | **PASS** | `humanview_cp1::cp1_verify_run_publishes_watch_only_and_records_ignored_panel_input`, `humanview_cp3::cp3_verify_run_watch_only_refused_under_view` |
| 4 | Token, Host header (`127.0.0.1:<port>`), and connection cap enforced on `/api/input` (wrong token/Host → `403`) | **PASS** | `humanview_cp2::cp2_ws_input_auth_host_and_cap_enforced` |
| 5 | Gesture fidelity: drag delivers pipelined `MOVE` stream; fling scrolls past release point | **PASS** | `humanview_cp3::cp3_live_drag_fling_long_press_and_rotate` |
| 6 | Input-to-changed-frame latency (50 reps): p50 ≤ 100 ms, p95 ≤ 200 ms | **PASS** | `humanview_cp4::cp4_latency_tap_to_changed_frame_50_reps` (§11, arrival-timed `received_at - t0`): **p50 = 63.06 ms**, **p95 = 64.39 ms** (min 61.12 ms, max 66.54 ms, mean 63.12 ms) |
| 7a | `acos_bench.py --phase memory,latency --animations on` vs `v0.4.1` (4 interleaved reps) | **PASS** | `compare_rc=0` (**within-noise** on all 32 metrics), `ab_rc=0` (**within-noise** on all 4 fast-settle lattice metrics) |
| 7b | Idle CPU with `acos view` open & idle vs closed (4 interleaved reps, 60 s window) | **PASS** | Serve CPU median **0.233%** (closed) vs **0.242%** (open-idle, +0.009 pp); `acos view` 0.00 s CPU ticks |
| 8 | `tools/verify/check-layout.sh` passes; only `.md` reports under `results/` | **PASS** | `check-layout: ok` |
| E2E | Live E2E in T3 Code's browser tab & Playwright Chromium (Increment/list/long-press/form/Back/Home/Rotate/verify) | **PASS** | Completed in Fix Round 1 (§8.3) and re-verified for `system.launcher:real` Home switching in Fix Round 3 (§10.4) |
| Owner | Owner one-time manual Gaten check in T3's browser tab | **PENDING** | See runbook in §7 |

---

## 3. Automated Test Runs (Exact Counts)

All release live-ART runs executed inside `env -u TMPDIR tools/art-runtime/run-exclusive.sh --task HUMANVIEW1 -- tools/guard/guarded.sh --mem 12G --time <sec> -- ...`:

1. `cargo test --offline -p acos-art --lib live_view`: **22 passed, 0 failed** (190 filtered out).
2. `cargo test --offline -p acos-cli --bin acos view`: **25 passed, 0 failed** (200 filtered out).
3. `cargo test --offline -p acos-cli --test humanview_cp1 --test humanview_cp2 --test humanview_cp3` (non-ignored):
   - `humanview_cp1`: **3 passed, 0 failed, 2 ignored**
   - `humanview_cp2`: **3 passed, 0 failed, 1 ignored**
   - `humanview_cp3`: **3 passed, 0 failed, 2 ignored**
4. `cargo test --release --offline -p acos-cli --test humanview_cp1 --test humanview_cp2 -- --ignored --nocapture`:
   - `humanview_cp1`: **2 passed, 0 failed** (`cp1_event_feed_source_labels_and_app_label`, `cp1_verify_run_publishes_watch_only_and_records_ignored_panel_input`)
   - `humanview_cp2`: **1 passed, 0 failed** (`cp2_ws_live_tap_text_back_and_source_labels`)
5. `cargo test --release --offline -p acos-cli --test humanview_cp3 --test humanview_cp4 -- --ignored --nocapture`:
   - `humanview_cp3`: **2 passed, 0 failed** (`cp3_live_drag_fling_long_press_and_rotate`, `cp3_verify_run_watch_only_refused_under_view`)
   - `humanview_cp4`: **1 passed, 0 failed** (`cp4_latency_tap_to_changed_frame_50_reps`)
6. Documentation & module registry checks (after CP5 edits):
   - `cargo test --offline -p acos-mcp --test docs --test guide --test documentation_args`: **docs 1 passed**, **documentation_args 4 passed**, **guide 14 passed**, **0 failed**.
   - `cargo test --offline -p acos-services --test modules_doc`: **2 passed, 0 failed**.
   - `tools/verify/check-layout.sh`: **`check-layout: ok`**.

---

## 4. CP4 Latency & Benchmark Evidence

### 4.1 Input-to-Changed-Frame Latency (`cp4_latency_tap_to_changed_frame_50_reps`)

Measured on `counter.apk` (`1080x2400` PNG @ `30` fps over `/api/input/<id>` + `/api/frames/<id>` MJPEG, 3 warmup taps + 50 measured taps spaced 50 ms apart, asserting `changed_pixels >= 16` per tap and `53` total increments in `observe_tree`):

| Stat | Value | Target | Result |
|---|---:|---:|---|
| `min_ms` | 62.97 ms | — | — |
| `p50_ms` | **66.70 ms** | ≤ 100 ms | **PASS** |
| `p95_ms` | **68.90 ms** | ≤ 200 ms | **PASS** |
| `max_ms` | 69.37 ms | — | — |
| `mean_ms` | 66.56 ms | — | — |
| `mean_encode_ms` (`ACOS_VIEW_STATS_FILE`) | 2.08 ms | — | 56,221 B/frame avg |
| `frames_dropped_latest` / `events_dropped` | 0 / 0 | — | — |
| `min_changed_pixels` / `max_changed_pixels` | 147 / 530 | ≥ 16 | 53/53 taps landed |

*(Summary stats only; see §11 for the arrival-timed `received_at - t0` re-run after Fix Round 4: `p50 = 63.06 ms`, `p95 = 64.39 ms`.)*

### 4.2 Parity Benchmarks: `humanview` vs `v0.4.1` (4 Interleaved Reps)

Run via `tools/bench/acos_bench.py --phase latency,memory --reps 1 --fast-reps 40 --animations on` alternating `v041` (`sha256:0c3a2815...`) and `humanview` (`sha256:d5107341...`), 4 reps each, pooled into `.cache/cp4-bench/{v041,humanview}-baseline.json` and compared with `acos_bench.py --compare` + `tools/bench/ab-compare.py`:
- **Provenance note:** the `humanview` binary (`d5107341...`) was compiled cleanly from the CP4 working tree before commit `896003a19` was recorded, so the generated JSON metadata stamps `HEAD = 8747ab7d9e87` while testing the CP4 code. In `v041` rep 4 memory phase, host load spiked briefly (`load1 = 4.280`, flagged `noisy: yes`; `humanview` had `0/8` noisy phases).
- **Overall verdict:** `compare_rc = 0` (**within-noise** across all 32 tracked metrics) and `ab_rc = 0` (**within-noise** across all 4 fast-settle lattice metrics).

#### Selected Old-Gate Medians (all 32 metrics `within-noise`)

| Metric | `v0.4.1` p50 | `humanview` p50 | Delta | Verdict |
|---|---:|---:|---:|---|
| `m1_idle_mem_mib` (N=1 anon+shmem) | 57.72 MiB | 57.50 MiB | -0.38% | within-noise |
| `m1_idle_pss_mib` (N=1 Pss) | 239.7 MiB | 241.3 MiB | +0.67% | within-noise |
| `m2_per_session_anon_shmem_n1` | 55.74 MiB | 55.51 MiB | -0.40% | within-noise |
| `m2_per_session_anon_shmem_n5` | 56.21 MiB | 55.94 MiB | -0.48% | within-noise |
| `m2_per_session_anon_shmem_n10` | 56.19 MiB | 56.27 MiB | +0.14% | within-noise |
| `m2_per_session_pss_n1` | 219.7 MiB | 221.1 MiB | +0.64% | within-noise |
| `m2_per_session_pss_n5` | 73.70 MiB | 74.05 MiB | +0.48% | within-noise |
| `m2_per_session_pss_n10` | 55.11 MiB | 55.48 MiB | +0.67% | within-noise |
| `m4_launch_strict_ms` | 290.7 ms | 299.9 ms | +3.16%¹ | within-noise |
| `m4_launch_fast_ms` | 152.0 ms | 151.5 ms | -0.38%¹ | within-noise |
| `m5_screenshot_ms` | 11.62 ms | 12.14 ms | +4.42%¹ | within-noise |
| `m6_reset_strict_ms` | 325.4 ms | 332.0 ms | +2.02%¹ | within-noise |
| `m6_reset_fast_ms` | 188.9 ms | 188.3 ms | -0.32%¹ | within-noise |
| `m7_tree_strict_ms` | 41.64 ms | 41.47 ms | -0.41%¹ | within-noise |
| `m7_tree_fast_ms` | 0.624 ms | 0.641 ms | +2.72%¹ | within-noise |
| `m8_tap_counter_strict_ms` | 948.9 ms | 949.7 ms | +0.09%¹ | within-noise |
| `m8_tap_counter_fast_ms` | 24.64 ms | 24.69 ms | +0.20%¹ | within-noise |
| `m8_tap_form_strict_ms` | 931.1 ms | 924.7 ms | -0.69%¹ | within-noise |
| `m8_tap_form_fast_ms` | 27.99 ms | 29.08 ms | +3.89%¹ | within-noise |
| `tap_ack_ms` | 5.000 ms | 5.000 ms | 0.00% | within-noise |
| `teardown_ms` | 21.00 ms | 21.00 ms | 0.00% | within-noise |
| `cpu_idle_n1_pct` | 0.240% | 0.249% | +3.53% | within-noise |
| `cpu_idle_n5_pct` | 1.139% | 1.070% | -6.02% | within-noise |
| `cpu_idle_n10_pct` | 2.005% | 1.971% | -1.70% | within-noise |
| `wakeups_idle_n1_per_s` | 1.212 /s | 1.203 /s | -0.74% | within-noise |
| `wakeups_idle_n5_per_s` | 2.112 /s | 2.091 /s | -1.02% | within-noise |
| `wakeups_idle_n10_per_s` | 3.164 /s | 3.087 /s | -2.45% | within-noise |
| `cpu_active_tap_strict_pct` | 38.59% | 42.60% | +10.40% (thr ±27%) | within-noise |

¹ Pooled p50 delta from `acos_bench.py`; `ab-compare.py` computes the old gate on median-of-per-rep-p50s (`m4_launch_strict_ms`: +1.65%, `m5_screenshot_ms`: +2.76%, `m8_tap_form_fast_ms`: +4.47%, etc.) — all `within-noise`.

#### Lattice Gate (`ab-compare.py`, 5%-Trimmed Mean, `n = 160` vs `160`)

| Metric | `v0.4.1` 5%-trim | `humanview` 5%-trim | Delta | First-slot share | Verdict |
|---|---:|---:|---:|---:|---|
| `m4_launch_fast_ms` | 156.31 ms | 152.68 ms | -3.62 ms (-2.32%) | 6.9% → 21.9% (+15.0 pp) | within-noise |
| `m6_reset_fast_ms` | 185.69 ms | 185.17 ms | -0.52 ms (-0.28%) | 6.3% → 7.5% (+1.2 pp) | within-noise |
| `m8_tap_counter_fast_ms` | 24.82 ms | 24.80 ms | -0.02 ms (-0.07%) | 50.0% → 50.0% (0.0 pp) | within-noise |
| `m8_tap_form_fast_ms` | 28.11 ms | 29.15 ms | +1.04 ms (+3.70%) | 100.0% → 100.0% (0.0 pp) | within-noise |

### 4.3 Idle-CPU 4-Rep Interleaved A/B (`closed` vs `open_idle`)

Run via `.cache/cp4-bench/idle_cpu_ab.py`: 4 interleaved pairs on `counter.apk` (`--animations on`), 60 s idle measurement window after 10 s settle. `closed` has no `acos view` process; `open_idle` runs `acos view` with `/api/frames/<id>` + `/api/events/<id>` open, `GET /api/sessions` every 2 s (`input_active: false`), and **no** `/api/input/<id>` WebSocket.
*(Note: the wrapper command hit its 600 s wall timeout right as `idle_cpu_ab.py` exited, after all 4 reps completed, `.cache/idle-ab-*` scratch dirs were cleaned up, `SUMMARY` printed, and `.cache/cp4-bench/idle_cpu_ab.json` was written.)*

| Rep | `closed` serve CPU % | `closed` wakeups/s | `open_idle` serve CPU % | `open_idle` total CPU % (incl. `acos view`) | `open_idle` serve / total wakeups/s |
|---:|---:|---:|---:|---:|---:|
| 1 | 0.217% | 1.200 /s | 0.258% | 0.258% | 1.225 / 1.644 |
| 2 | 0.200% | 1.217 /s | 0.242% | 0.242% | 1.209 / 1.644 |
| 3 | 0.250% | 1.200 /s | 0.226% | 0.226% | 1.225 / 1.660 |
| 4 | 0.250% | 1.217 /s | 0.242% | 0.242% | 1.209 / 1.628 |
| **Median** | **0.233%** | **1.208 /s** | **0.242%** | **0.242%** | **1.217 / 1.644** |
| **Mean** | **0.229%** | **1.208 /s** | **0.242%** | **0.242%** | **1.217 / 1.644** |

- `acos view` itself recorded `0.00 s` of CPU time over each 60 s window (`total_cpu_pct == serve_cpu_pct` in every rep), with +0.436/s extra wakeups only from the synthetic 2 s `/api/sessions` poll.

---

## 5. Honest Notes & Spec Deviations

1. **1.0× PNG @ 30 fps instead of 1.0× JPEG (`HUMAN_VIEW_SPEC.md` §4.2):**
   Spec §4.2 originally said *"Prefer JPEG at full size, to save encode time and bandwidth."* Benchmarking on this host showed scalar JPEG encoding in the `image` crate at `1080x2400` costs ~40+ ms/frame, whereas fast PNG takes **2.08 ms** mean encode (~56 KB/frame on `counter.apk`) with direct RGBA→RGB copy (`rgb_scratch`/`encode_scratch` reuse) and hits **p50 = 66.70 ms, p95 = 68.90 ms**. Interactive sessions therefore upgrade to `1.0` scale PNG @ `30` fps (`LIVE_VIEW.md` §7–§8, `ADR-0033`).
2. **`acos view --open` flag omitted (`HUMAN_VIEW_SPEC.md` §3):**
   `ViewArgs` (`crates/acos-cli/src/view/mod.rs`) exposes `--bind`, `--port`, `--once`, `--json`, and `--registry-dir`; `--open` (launching `xdg-open`) was not implemented because in T3 Code / headless workflows users paste or open the printed `http://127.0.0.1:<port>/?t=<token>` URL directly.
3. **Live Browser E2E complete; Owner Gaten check remains manual:** Live E2E in T3's browser tab and Playwright Chromium was completed in Fix Round 1 (§8.3) and re-verified for `system.launcher:real` Home switching in Fix Round 3 (§10.4). Only the one-time Owner Gaten check (§7) remains manual.
4. **PR #6 open (not merged):** `humanview` → `main` is open as PR #6 for review; not merged by agents.

---

## 6. Runbook — Later Live E2E in T3 Code's Browser Tab (for Browser Subagent)

Execute from `/home/Qui/Projects/acos/.cache/wt-humanview` using `CARGO_TARGET_DIR=$PWD/.cache/target`:

1. **Build or reuse release binaries** (already in `.cache/target/release/{acos,acos-art}`):
   ```bash
   source tools/dev/agent-env.sh
   export CARGO_TARGET_DIR=$PWD/.cache/target
   ```
2. **Start a clean serve daemon + live session (`counter.apk`)**:
   - Create a private registry dir `.cache/e2e-view/viewreg` (`chmod 700`) and start `acos serve --socket .cache/e2e-view/serve.sock` with `ACOS_VIEW_REGISTRY_DIR=.cache/e2e-view/viewreg` and `ACOS_ART_BIN=$CARGO_TARGET_DIR/release/acos-art`.
   - Launch `fixtures/build/apps/counter.apk` (`session_id: "e2e_counter"`, `device_name: "E2E Counter"`, `animations: "on"`), then `fixtures/build/apps/form-validation.apk` (`session_id: "e2e_form"`, `device_name: "E2E Form"`).
3. **Start the hub**:
   ```bash
   "$CARGO_TARGET_DIR/release/acos" view --port 0 --json --registry-dir .cache/e2e-view/viewreg
   ```
   Copy the printed `url` (`http://127.0.0.1:<port>/?t=<token>`).
4. **Drive T3 Code's built-in browser tab** (`preview_open` on that URL, then `preview_snapshot` / `preview_act`):
   - Confirm both `E2E Counter` and `E2E Form` appear in the session list.
   - Click `E2E Counter`:
     - Verify the live canvas renders the counter UI and `Increment` button.
     - Click `Increment` on the canvas twice; verify screen updates to `1` then `2`, and the event feed displays `panel` pills alongside the initial `agent` launch entry.
     - Click **Home** / **Back** / **Rotate** toolbar buttons and verify display orientation flips (`1080x2400` ↔ `2400x1080`) and Back/Home dispatches appear in the event feed.
   - Switch to `E2E Form`:
     - Click the name field, type text via keyboard input, click Submit, and verify typed text appears on screen and `/api/events` records `panel` key/text events (and masks passwords if a password field is focused).
   - Take preview screenshots (`preview_snapshot` / screenshot capture), summarize results, and cleanly shut down `acos view` and `acos serve` (closing the preview tab afterward).

---

## 7. Runbook — Owner's One-Time Gaten Check

1. Ensure binaries are built (or use `.cache/target/release/acos` + `acos-art` on `humanview`).
2. Start a Gaten session (either via MCP `acos_launch` with `device_name: "Gaten"` or via CLI against a running `acos serve --engine art` with the built Gaten debug APK in `/home/Qui/Projects/acos/.cache/gaten/...`).
3. In a terminal, start the viewer:
   ```bash
   acos view
   # prints: acos view: http://127.0.0.1:<port>/?t=<token>
   ```
4. Paste the printed URL into **T3 Code's preview pane** (or any local browser).
5. Click the `Gaten` session card and verify by hand:
   - Live screen is crisp (`1.0×` PNG once first interaction opens `/api/input`) and responsive during navigation;
   - Taps, vertical scroll/fling, typing into a form field, `Back`, `Home`, and `Rotate` behave like an emulator window;
   - Recent actions strip shows both `agent` and `panel` actions cleanly without interfering with a concurrent agent call.

---

## 8. Fix Round 1 — Verifier Findings & Live Browser E2E

All six verifier findings were resolved on `humanview` and verified end-to-end against live ART sessions in both T3 Code's built-in preview tab and headless Chromium (Playwright):

### 8.1 Fixes Applied (`file:line`)

1. **[HIGH] Immediate MJPEG closing boundary & flush** (`crates/acos-cli/src/view/mod.rs:443-475`, `crates/acos-cli/tests/humanview_cp4.rs:382-395`):
   - `stream_frames` writes `--acosframe\r\n` as the opening prefix only on frame 1, then appends `\r\n--acosframe\r\n` immediately after every frame's payload and calls `connection.flush()` before blocking for the next frame.
   - Regression test `view::tests::stream_frames_flushes_closing_boundary_before_blocking_for_next_frame` (`crates/acos-cli/src/view/mod.rs:1436-1573`) verifies frames 1 and 2 emit `\r\n--acosframe\r\n` before the producer blocks; `MjpegStream` in `humanview_cp4.rs` asserts the closing boundary immediately follows each part.
2. **[HIGH] T3 Code built-in browser tab support & DOM-stable hub grid** (`crates/acos-cli/src/view/page.rs:148,182-189,276-339,560-668`, `crates/acos-cli/src/view/mod.rs:1575-1625`):
   - Root cause of the 15 s `preview_snapshot` timeouts: (a) `<img src="/api/frames/...">` held the browser's `load` event open indefinitely on an endless multipart stream, and (b) `renderGrid()` wiped and rebuilt `.card` DOM nodes every 2 s, invalidating Playwright/T3 `aria-ref` locators mid-click.
   - Replaced `<img id="frame">` with `<canvas id="frame">` driven post-`load` (`afterLoad`) via `startFrameStream()` (`fetch()` + `ReadableStream` multipart parser + `createImageBitmap` + `drawImage`). Kept `/api/frames/<id>` MJPEG unchanged on the server.
   - Keyed `renderGrid()` via `lastGridSig` so 2 s `/api/sessions` refreshes update card data and `.age` text in-place without recreating `.card` elements.
3. **[MEDIUM] Deterministic producer rate-limiter test** (`crates/acos-art/src/live_view.rs:3152-3268`):
   - Replaced wall-sleep coordination in `empty_source_poll_does_not_rate_limit_next_frame` with an explicit `ParkWake` hook so the background `acos-live-view-tap` thread waits until signaled and only the test's `view.tick()` calls drive the source. Verified 20/20 consecutive passes.
4. **[MEDIUM] Launcher-aware Home button (`has_home`)** (`crates/acos-services/src/{atms.rs:2415-2427,host.rs:2311-2323}`, `crates/acos-art/src/{launch.rs:1920-1924,3069,7457,7636,7666,live_view.rs:160,170,555-557}`, `crates/acos-cli/src/{engine/art.rs,engine/serve.rs,verify.rs,live_registry.rs}`, `crates/acos-cli/src/view/{hub.rs:66,100,153,176,page.rs:241-265,416-420,694-710,898-901}`):
   - Plumbed `has_home` from `AtmsService::has_home_activity()` through `LaunchReady` + `LiveViewConfig` (`hello`) + `LiveEntry` to `/api/sessions` and SSE `state`, with a `HOME_UNAVAILABLE` WS error fallback.
   - Single-APK sessions without a launcher disable `#btn-home` with tooltip `"no launcher in this session"`; sessions with `system.launcher` keep `#btn-home` enabled and launch `com.acos.launcher`.
5. **[LOW] Clear cached frame on last-watcher detach** (`crates/acos-cli/src/view/hub.rs:712,2588-2658`):
   - `Slot::detach` clears `state.frame = None` when resetting slot state, verified by `view::hub::tests::detach_clears_cached_frame_so_reattach_waits_for_fresh_frame`.
6. **ADR numbering note** (`DECISIONS.md:1627`):
   - Retained `ADR-0033` for HUMANVIEW1 and noted that parked PR #5 (`devpanel`) must take the next free number (`ADR-0034`+) if revived.

### 8.2 Post-Fix Automated Checks (Exact Counts)

- `cargo test --offline -p acos-art --lib live_view`: **22 passed, 0 failed** (190 filtered out); 20/20 loop pass on `empty_source_poll_does_not_rate_limit_next_frame`.
- `cargo test --offline -p acos-cli --bin acos view`: **27 passed, 0 failed** (200 filtered out, includes both new regression tests).
- `cargo test --offline -p acos-cli --bin acos live_registry`: **8 passed, 0 failed** (219 filtered out).
- `cargo test --offline -p acos-cli --test humanview_cp1 --test humanview_cp2 --test humanview_cp3 --test humanview_cp4` (non-ignored): `cp1` **3 passed** (2 ignored), `cp2` **3 passed** (1 ignored), `cp3` **3 passed** (2 ignored), `cp4` **2 passed** (1 ignored).
- `cargo test --release --offline -p acos-cli --test humanview_cp4 -- --ignored --nocapture` (under `run-exclusive.sh` + `guarded.sh`): **1 passed, 0 failed** (`p50_ms = 66.72 ms`, `p95_ms = 67.88 ms`, `mean_ms = 66.94 ms`, `mean_encode_ms = 1.74 ms`, `frames_dropped_latest = 0`, `events_dropped = 0`).
- `cargo test --release --offline -p acos-cli --test humanview_cp1 --test humanview_cp2 --test humanview_cp3 -- --ignored --test-threads=1` (after fixes 1–6, before the final `renderGrid` DOM-stability tweak): `cp1` **2 passed**, `cp2` **1 passed**, `cp3` **2 passed**, **0 failed**.
- Docs & layout: `acos-mcp` (`docs`: **1 passed**, `documentation_args`: **4 passed**, `guide`: **14 passed**), `acos-services --test modules_doc` (**2 passed**), `tools/verify/check-layout.sh` (`check-layout: ok`).

### 8.3 Live Browser E2E Results

- **T3 Code built-in preview tab (`y`):** Tested directly against a live `counter.apk` session using `preview_open`, `preview_snapshot`, `preview_act`, `preview_evaluate`, and `t3_preview_close` (tabs closed cleanly):
  - `preview_snapshot` on both hub grid (~1–3 s) and `#s=t3_counter` detail view (< 1 s) succeeded after the `<canvas>` post-`load` fix.
  - `preview_act` clicking session card by `aria-ref` (after the `lastGridSig` DOM-stability fix) and clicking live canvas at `Increment` succeeded, upgrading `540x1200` (`631,260` non-black pixels, `#btn-home` disabled with `"no launcher in this session"`) -> `1080x2400` (`interactive`, `panel touch ok @ 158,430`). Sequential `preview_snapshot` + `Rotate` click also succeeded (one back-to-back concurrent `preview_snapshot`+`preview_evaluate` right after click timed out at 15 s; sequential calls all passed).
- **Playwright Headless Chromium E2E (`.cache/hv-e2e/e2e_browser.mjs` under `run-exclusive.sh` + `guarded.sh`):** All steps passed with `pageErrors: []`, `consoleErrors: []`, and inspected screenshots confirming rendered frames match the `acos_observe` UI tree:

| Step | Scenario / Action | Result | Frame Matches Tree | Evidence |
|---|---|---|---|---|
| a | `counter.apk` open idle (`540x1200`) -> click `Increment` x2 -> `1080x2400` (`count == "2"`, `Home` disabled `"no launcher in this session"`) | **PASS** | `y` | `.cache/hv-e2e/out/step_a_*.png` |
| b | `list-scroll.apk` drag + wheel scroll (`Row 0..3` -> `Row 35..38`) | **PASS** | `y` | `.cache/hv-e2e/out/step_b_*.png` |
| c | `touch-probe.apk` long press 800 ms (`status == "long-pressed"`) | **PASS** | `y` | `.cache/hv-e2e/out/step_c_*.png` |
| d | `form-validation.apk` focus `EditText` + type `"hello human"` | **PASS** | `y` | `.cache/hv-e2e/out/step_d_*.png` |
| e1 | `multi-activity.apk` open second activity -> `#btn-back` -> `#btn-rotate` (`1080x2400` -> `2400x1080`) | **PASS** | `y` | `.cache/hv-e2e/out/step_e_{back,rot}.png` |
| e2 | `hello-views.apk` + `system.launcher` (`real`) -> `#btn-home` enabled (`title="Home"`) -> opens `ACOS Home` | **PASS** | `y` | `.cache/hv-e2e/out/step_e_home.png` |
| verify | `acos verify` watch-only (`counter.apk`, 7 s wait) renders crisp `540x1200` idle frame immediately, `watch-only: verify run` banner visible, all toolbar buttons disabled, click ignored, `verifyOk: true` | **PASS** | `y` | `.cache/hv-e2e/out/step_verify_watch.png` |

---

## 9. Fix Round 2

Addressed the two LOW findings from re-verification:

1. **Documented optional `has_home` (`docs/LIVE_VIEW.md` §2, §5, §8):**
   - Added `has_home` to the `H` hello frame example & field table (§2), the `LiveEntry` registry JSON & field notes (§5), and the `/api/sessions` + SSE `state` hub summary (§8).
2. **Stopped infinite `startFrameStream()` 400 ms retries on destroyed sessions (`crates/acos-cli/src/view/{page.rs,mod.rs}`):**
   - Stops retrying and switches both `#dstate` and `#offline` to `"session ended"` (closing SSE and WebSocket as well) on HTTP `404`, when the session disappears from `/api/sessions`, or when `connected === false` after/while watching.
   - Added exponential backoff (`400 ms` up to `5 s`) with abort-aware sleep on other errors.
   - Added regression test `view::tests::page_contains_session_ended_stop_and_backoff`.

**Checks:**
- `cargo test --offline -p acos-cli --bin acos view`: **28 passed, 0 failed** (200 filtered out).
- `cargo test --offline -p acos-mcp --test docs --test guide --test documentation_args`: **docs 1 passed**, **documentation_args 4 passed**, **guide 14 passed**, **0 failed**.
- `tools/verify/check-layout.sh`: **`check-layout: ok`**.

---

## 10. Fix Round 3 — Multi-Process `BufferCache` Collision (`system.launcher:real` + App)

Fixed a pre-existing compositor bug (also present on `v0.4.1`) where launching `counter.apk` with `--service system.launcher:real` and pressing Home (`acos_key home` or `#btn-home` in `acos view`) switched the UI tree to `ACOS Home` while every screenshot and live-view frame remained stuck on Counter's pixels.

### 10.1 Root Cause & Fix (`file:line`)

- **Root cause:** Counter (`tag=0`) and Launcher (`tag=1`) run in separate `bwrap` PID namespaces, so `getpid()` matches and `GraphicBuffer::getId()` produces identical IDs (`0x2_0000_0000`, `0x2_0000_0001`, …) in both processes. `SurfaceState::buffer_cache` was keyed only by `u64` buffer id, `read_buffer_data` discarded `client_cache_t.token`, `cache_buffer` skipped insertion when the id already existed, and `apply_layer_state` resolved Launcher's cached-id submissions to Counter's cached `memfd`.
- **Compositor cache keying, collision replacement & `uncacheBuffers`** (`crates/acos-services/src/surface_client.rs`):
  - Added `cached_token: BinderId` to `LatestBuffer` (`:320`) and `BufferData` (`:1884`), and `uncached_buffers: Vec<(BinderId, u64)>` to `TransactionState` (`:1936`).
  - Keyed `SurfaceState::buffer_cache` (`:638`) and `buffer_cache_order` (`:640`) by `(BinderId, u64)` (`(cached_token, buffer_id)`), matching AOSP `ClientCache::getBuffer` (`{token, id}`).
  - Updated `destroy_layer_tree` (`:757-760`) to evict by `(buffer.cached_token, buffer.buffer_id)`.
  - Updated `SurfaceState::cache_buffer(state, owner, meta)` (`:815-859`) to replace stale entries on same-key collision instead of returning early, while bounding entries (`16` per owner, `64` total).
  - Added `SurfaceState::uncache_buffer` (`:862-870`), populated `uncached_buffers` in `read_transaction_state_body` (`:2918-2926,2970`), and applied evictions in `apply_transaction_state` (`:1765-1769`).
  - Retained `cached_token` in `read_buffer_data` (`:3318-3338`) and resolved/stored `(buffer.cached_token, buffer.cached_buffer_id)` in `apply_layer_state` (`:2167-2223`).
- **Process death / disconnect release:**
  - `SurfaceState::release_process_buffers` (`crates/acos-services/src/surface_client.rs:874-891`), registered via `install` -> `host.set_compositor` (`:1715`).
  - `ServiceHost::set_compositor` & `release_process_compositor_buffers` (`crates/acos-services/src/host.rs:1808-1829`), called from `ServiceHost::service_process_died` (`:1775`) and `Peer::disconnect_process` (`crates/acos-binder-peer/src/peer.rs:976-977`).

### 10.2 Automated Tests & `cp4` Latency (Exact Counts)

- **New unit test (`crates/acos-services/tests/surface_client.rs:1183-1469`):** `two_owners_with_same_buffer_id_keep_separate_cached_buffers` verifies separate cached `memfd`s for two owners with the same `buffer_id`, independent cached-id lookups, same-key replacement, and per-process cache release on `service_process_died`.
- **New live regression test (`crates/acos-cli/tests/launcher_home_frame.rs:429-604`, `#[ignore]`):** `pressing_home_with_system_launcher_switches_pixels_from_counter_to_launcher` launches `counter.apk` + `system.launcher:real`, asserts both `acos_observe` PNG and live `/api/frames` MJPEG switch from Counter to `ACOS Home` on `acos_key home` (`diff_counter_vs_launcher=115689` pixels), and switch back to Counter on tapping `app_row_0` (`diff_back_vs_launcher=115689`, `diff_back_vs_counter=0`).
- **Non-ignored suites:**
  - `cargo test --offline -p acos-services`: **782 passed, 0 failed** (lib 193 + integration crates incl. `surface_client` **13 passed**, `b1b_kernel_placement` 2, `display_events` 11, `sandbox_c1` 9, `w35_composite` 8, `w42_layer_lifetime` 6, `w54_dialog_surface_insets` 2, `window_surface_layer` 2, `mproc*` / `sysui*` / `launcher` / `binderpid` suites; 2 ignored doctests).
  - `cargo test --offline -p acos-binder-peer`: **62 passed, 0 failed**.
  - `env -u TMPDIR cargo test --offline -p acos-art --lib live_view`: **22 passed, 0 failed** (190 filtered).
  - `env -u TMPDIR cargo test --offline -p acos-cli --bin acos`: **228 passed, 0 failed**.
  - `env -u TMPDIR cargo test --offline -p acos-mcp --test docs --test guide --test documentation_args`: **19 passed, 0 failed** (1 + 14 + 4).
  - `humanview_cp1`–`cp4` & `launcher_home_frame` (non-ignored): `cp1` **3 passed** (2 ignored), `cp2` **3 passed** (1 ignored), `cp3` **3 passed** (2 ignored), `cp4` **2 passed** (1 ignored), `launcher_home_frame` **2 passed** (1 ignored).
- **Release live-ART suites (`run-exclusive.sh` + `guarded.sh`, `--ignored --test-threads=1`):**
  - `launcher_home_frame`: **1 passed, 0 failed** (5.49 s).
  - `humanview_cp1`: **2 passed, 0 failed** (19.02 s); `humanview_cp2`: **1 passed, 0 failed** (5.54 s); `humanview_cp3`: **2 passed, 0 failed** (20.32 s).
  - `humanview_cp4`: **1 passed, 0 failed** (10.86 s) — **p50 = 66.81 ms** (≤ 100 ms), **p95 = 68.68 ms** (≤ 200 ms), mean = 66.63 ms, min = 62.97 ms, max = 69.03 ms, `mean_encode_ms = 1.90 ms`, `frames_dropped_latest = 0`, `events_dropped = 0`.
  - `cr1fix_finish_in_oncreate`: **1 passed, 0 failed** (10.74 s).
- **Live ART scenarios (`acos verify --engine art`):** `launcher-home.yaml`, `mproc3-two-process.yaml`, `mproc8-bind-only.yaml`, `mproc1-multi-package.yaml`, `mproc1-intents.yaml` all **pass** (`negative/launcher-home-wrong.yaml` fails at step 4 `ASSERTION_FAILED`, exit 1 as expected).

### 10.3 Benchmarks (`--phases "latency memory" --animations on --reps 2 --fast-reps 40`)

Pooled across 2 clean reps (`0/4` noisy phases) on binary `736296e4...`:

| Metric | `v0.4.1` (CP4) | `humanview` (CP4) | `humanview` (Fix 3) | vs CP4 | vs `v0.4.1` |
|---|---:|---:|---:|---|---|
| `m1_idle_mem_mib` | 57.72 MiB | 57.50 MiB | 57.84 MiB | +0.6% (`within-noise`) | +0.22% (`within-noise`) |
| `m1_idle_pss_mib` | 239.7 MiB | 241.3 MiB | 241.9 MiB | +0.2% (`within-noise`) | +0.92% (`within-noise`) |
| `m2_per_session_anon_shmem_n1` | 55.74 MiB | 55.51 MiB | 55.86 MiB | +0.6% (`within-noise`) | +0.23% (`within-noise`) |
| `m2_per_session_anon_shmem_n5` | 56.21 MiB | 55.94 MiB | 55.82 MiB | -0.2% (`within-noise`) | -0.70% (`within-noise`) |
| `m2_per_session_anon_shmem_n10` | 56.19 MiB | 56.27 MiB | 55.98 MiB | -0.5% (`within-noise`) | -0.38% (`within-noise`) |
| `m2_per_session_pss_n1` | 219.7 MiB | 221.1 MiB | 216.7 MiB | -2.0% (`within-noise`) | -1.35% (`within-noise`) |
| `m2_per_session_pss_n5` | 73.70 MiB | 74.05 MiB | 72.96 MiB | -1.5% (`within-noise`) | -1.01% (`within-noise`) |
| `m2_per_session_pss_n10` | 55.11 MiB | 55.48 MiB | 54.71 MiB | -1.4% (`within-noise`) | -0.72% (`within-noise`) |
| `m5_screenshot_ms` | 11.62 ms | 12.14 ms | 11.87 ms | -1.2% (`within-noise`) | +2.15% (`within-noise`) |
| `m8_tap_counter_strict_ms` | 948.9 ms | 949.7 ms | 934.6 ms | -1.0% (`within-noise`) | -1.51% (`within-noise`) |
| `m8_tap_counter_fast_ms` | 24.64 ms | 24.69 ms | 24.24 ms | -0.3% (`within-noise`) | -1.64% (`within-noise`) |
| `m8_tap_form_strict_ms` | 931.1 ms | 924.7 ms | 923.3 ms | -0.1% (`within-noise`) | -0.84% (`within-noise`) |
| `m8_tap_form_fast_ms` | 27.99 ms | 29.08 ms | 30.09 ms | +2.7% (`within-noise`) | +1.90 ms trimmed (`within-noise`¹) |

¹ Against CP4 (`humanview-baseline.json`), `ab-compare.py` passes **32/32** old-gate metrics and **4/4** lattice metrics (`ab_rc = 0`). Against pre-reboot `v0.4.1-baseline.json`, `ab-compare.py` lattice gate passes **4/4** fast metrics (`m8_tap_form_fast_ms` +1.90 ms < 2.00 ms floor, 100% slot-0 in both), while non-interleaved old-gate `acos_bench.py --compare` flagged `m8_tap_form_fast_ms` (+2.10 ms pooled vs 2.00 ms floor) and `cpu_active_tap_strict_pct` (38.59% → 45.13%, +16.96% vs `v0.4.1`'s ±10% threshold; +5.9% `within-noise` vs CP4's 42.60%).

### 10.4 Live Browser Home Check (`system.launcher:real` + `counter.apk`)

- **T3 preview tab:** Opened `acos view` via `preview_open` (`127.0.0.1:<port>/?t=<token>`), drove card open + `#btn-home` click via `preview_evaluate`, and confirmed `#frame` switched pixels from Counter (`631,260` non-black) to Launcher (`2,523,960` non-black, `diff = 277,085` pixels), matching `acos_observe` (`ACOS Home` + `observe-2.png`). `preview_snapshot` and `preview_act` timed out at `15000ms` while T3's Electron preview pane reported `visible: false`, so the tab was closed with `t3_preview_close` and full mouse-driven browser verification was completed in headless Chromium (`playwright-core`, `errors: []`): Counter (`1`, `1080x2400`) → `#btn-home` → `ACOS Launcher / ACOS Home / Counter` → tap `Counter` row → `Counter (1)`.
- **Pre-existing unrelated note:** If the lazily spawned `com.acos.launcher` process (`tag=1`) sits idle long enough (~71 s) to be reaped by MPROC5c idle kill, a subsequent Home key calls `start_activity_from_host` → `enqueue_pending_resumes` for dead `tag=1` (`UNSUPPORTED_ANDROID_API: idle delivery to process 1 dropped`) rather than respawning the launcher task.

---

## 11. Fix Round 4 — CodeRabbit PR #6 Review Fixes

Triaged all CodeRabbit PR #6 inline/overflow findings and pre-PR verification notes:

### 11.1 Addressed Issues

- **Compositor cache generation guard (`crates/acos-services/src/surface_client.rs`, `tests/surface_client.rs`):** Added `cache_gen: u64` on `LatestBuffer`/`CachedBuffer` + `next_cache_gen` on `SurfaceState`; `destroy_layer_tree` only evicts `buffer_cache` when `cache_gen` matches and no other live layer still references the entry (regression test added).
- **ART launcher & live-view producer (`crates/acos-art/src/{launch.rs,live_view.rs}`, `guest/java/dev/acos/art/LaunchMain.java`):** Replaced `evidence.last()` reply guard in `"rotate" | "panel_rotate"` with a per-request `replied` bool; synchronized `attach_epoch` + `last_encoded_seq` under `inner.slot`, snapshotted `format` once per frame, and fixed stale JPEG comments; tightened `LaunchMain.java` delayed `StopActivityItem` guard for `A→B→C` transitions.
- **Session identity & registry serialization (`crates/acos-cli/src/{live_registry.rs,engine/serve.rs,serve.rs,launch.rs,verify.rs}`, `crates/acos-mcp/src/descriptors.rs`):** Added immutable per-guest `guest_token` published as `LiveEntry.launch_token`, serialized registry reads/updates/removals via `io_lock`, added token-checked `update_activity_if_token` / `update_display_if_token` / `remove_if_token`, validated `expected_launch_token` on `panel_*` calls (`SESSION_REPLACED`), fixed relative `serve_socket` canonicalization + `sun_path ≤ 107` byte guard, rejected `--device-name` on non-ART/non-keep launches, and scoped MCP `device_name` docs to ART launches.
- **Hub input worker & stream lifecycle (`crates/acos-cli/src/view/{hub.rs,ws.rs}`):** Evicted replaced session slots on `refresh_locked` and rejected stale input clients with `SESSION_REPLACED`; bounded `InputSlot` queue (`128` total / `112` normal) with `QUEUE_FULL`, `Touch Move` coalescing (`coalesced: true`), and priority disconnect cleanup; restricted `DaemonConn` single retry to pre-write failures only (never replaying post-write EOF/I/O errors); allocated unique `0..MAX_POINTERS` `wire_id`s across concurrent clients; closed race windows in `watch`/`open_input`/`sync_interactive`/`detach`/`pump`, guarded zero-movement scrolls + mid-scroll `ACTION_CANCEL`, and added `f32::is_finite()` post-narrowing checks (5 new hub unit tests).
- **Browser page (`crates/acos-cli/src/view/{page.rs,mod.rs}`):** Stopped treating transient `connected === false` as permanent session end (only `!updated`, HTTP `404`, or WS `SESSION_REPLACED` invoke `markSessionEnded`); bounded `wsQueue` with `touch move` coalescing + oldest non-terminal eviction; removed `Tab` from `KEY_MAP`, skipped `keydown` when `watch_only || sessionEnded`, and tracked `heldGuestKeys` for paired `keyup`; added `frameSize` orientation guards, `pointercancel` full cleanup, and `pageshow` bfcache stream restart.
- **Tests & Docs (`crates/acos-cli/tests/{humanview_cp1..4,launcher_home_frame}.rs`, `docs/{LIVE_VIEW,HUMAN_VIEW_SPEC}.md`, `REPORT.md`):** Fixed `cp4` latency measurement (`received_at: Instant` on `MjpegPart`, pre-tap drain, single per-tap deadline, `received_at >= t0`, `received_at - t0` excluding PNG decode/diff, exact `count` resource-id == `"53"`); added `ChildGuard` for `acos verify` children in `cp1`/`cp3`; added interruptible `EventProbe` in `cp1`, strict over-cap EOF/200 recovery + exact `count` id in `cp2`, strict wheel-scroll movement + fresh 540px PNG check during open WS in `cp3`, and 2×2 downscaled live MJPEG pixel equality assertions in `launcher_home_frame`; updated stale PNG/JPEG/env/page-size/watch-only doc wording and removed the raw 50-sample array from §4.1.

### 11.2 Documented False Positives / Out-of-Scope (Not Changed)

1. **`hub.rs` touch `UP` failure retention:** Keeping a failed `UP` pointer in `self.pointers` would leave a ghost pointer after the browser already released it, breaking subsequent single-finger taps and blocking `handle_scroll`.
2. **`hub.rs` key `UP` failure retention:** Keeping a failed `UP` key in `self.held_keys` risks stuck keys or stale late releases after disconnect.
3. **`launch.rs` rotation-specific guest ack:** `ProbeSample` currently has no `rotation` field; `panel_rotate` intentionally shares the pre-existing `ROT3` `rotate` protocol.

### 11.3 Fresh Verification (Exact Observed Counts)

- `tools/verify/check-layout.sh`: **`check-layout: ok`**.
- Non-ignored unit & integration suites:
  - `cargo test --offline -p acos-services --test surface_client`: **13 passed, 0 failed**.
  - `cargo test --offline -p acos-services --test modules_doc`: **2 passed, 0 failed**.
  - `cargo test --offline -p acos-binder-peer`: **62 passed, 0 failed** (8 + 2 + 13 + 11 + 26 + 2).
  - `cargo test --offline -p acos-art --lib live_view`: **22 passed, 0 failed** (190 filtered).
  - `env -u TMPDIR cargo test --offline -p acos-cli --bin acos`: **233 passed, 0 failed** (incl. 37 in `view::` + `live_registry::`, +5 new hub regression tests).
  - `cargo test --offline -p acos-mcp --test docs --test guide --test documentation_args`: **19 passed, 0 failed** (1 + 14 + 4).
  - `humanview_cp1`–`cp4` & `launcher_home_frame` (non-ignored): `cp1` **3 passed** (2 ignored), `cp2` **3 passed** (1 ignored), `cp3` **3 passed** (2 ignored), `cp4` **2 passed** (1 ignored), `launcher_home_frame` **2 passed** (1 ignored).
- Release live-ART suites (`env -u TMPDIR tools/art-runtime/run-exclusive.sh --task HVCR -- tools/guard/guarded.sh --mem 12G --time 300 -- ...`):
  - `launcher_home_frame` (`--ignored`): **1 passed, 0 failed** (7.92 s; `diff_counter_vs_launcher=115689`, `diff_back_vs_launcher=115689`, `diff_back_vs_counter=0`, live 540×1200 MJPEG matches 2×2 downscaled screenshots).
  - `humanview_cp1` (`--ignored`): **2 passed, 0 failed** (18.84 s).
  - `humanview_cp2` (`--ignored`): **1 passed, 0 failed** (5.47 s).
  - `humanview_cp3` (`--ignored`): **2 passed, 0 failed** (21.87 s).
  - `humanview_cp4` (`--ignored`): **1 passed, 0 failed** (10.68 s) — arrival-timed (`received_at - t0`): **p50 = 63.06 ms** (≤ 100 ms), **p95 = 64.39 ms** (≤ 200 ms), `min = 61.12 ms`, `max = 66.54 ms`, `mean = 63.12 ms`, `mean_encode_ms = 2.03 ms` (56,220 B/frame avg), `frames_dropped_latest = 0`, `events_dropped = 0`, `min/max_changed_pixels = 147 / 530`, exact `count` TextView == `"53"`.
  - `cr1fix_finish_in_oncreate`: **1 passed, 0 failed** (11.15 s).

---

## 12. Fix Round 5 — CodeRabbit PR #6 Round 2 Review Fixes

Verified and addressed all **18** CodeRabbit Round 2 actionable comments (5 Majors + 13 Minors, 0 false positives):

### 12.1 Addressed Issues

- **ART guest & live-view producer (`crates/acos-art/guest/java/dev/acos/art/LaunchMain.java`, `crates/acos-art/src/{live_view.rs,launch.rs}`):**
  - Re-resolved `activityForToken(token)` inside `synchronized (sFinishPrevious)` in the delayed stop guard (`LaunchMain.java`) so a same-token relaunch checks the live `Activity` instance's `mFinished`.
  - Tracked pending return-to-previous handoffs separately via `sPendingResumeTokens` and updated `sCurrentToken = previous` only after scheduling the resume transaction succeeds (`LaunchMain.java`).
  - Tagged every captured `Slot` in `live_view.rs` with `attach_epoch` (read before `self.scale()` in `tap_once`) and filtered out stale-epoch slots in `writer_loop`.
  - Added `"frame_seq": latest_generation(live)` to the `"rotate" | "panel_rotate"` reply (`launch.rs`) so clients can gate post-rotation input on frame generation.
- **Hub & HTTP/WebSocket server (`crates/acos-cli/src/view/{hub.rs,mod.rs}`):**
  - Added a non-secret monotonic `guest_gen: u64` per guest instance (`SessionView`, `Watcher`, SSE `state` + `replaced` events) and required `?g=<guest_gen>` on `/api/input/<session_id>` via `Hub::open_input_checked` (`400` if missing/invalid, `409 Conflict` with `SESSION_REPLACED` if stale; marks replaced watchers so SSE/MJPEG end cleanly).
  - Validated `Sec-WebSocket-Version: 13` in `handle_input_ws` before opening input, rejecting unsupported versions with `426 Upgrade Required` + `Sec-WebSocket-Version: 13`.
  - Serialized the no-input branch of `Hub::watch` (`slot.set_interactive(false)`) under `self.inner` so it cannot overwrite a concurrent `open_input` upgrade.
  - Restricted `InputClient::enqueue` touch-move coalescing to `state.queue.back_mut()` only (never scanning past intervening commands).
  - Updated `InputClient::enqueue` so if a terminal release (`Touch Up/Cancel`, `Key Up`) cannot be admitted at `INPUT_QUEUE_LIMIT` (128), the hub marks the client terminated, prunes its queued entries, schedules priority disconnect cleanup, replies `QUEUE_FULL` with `"disconnected": true`, and closes the WebSocket.
  - Distinguished pre-write vs post-write ambiguous errors (`DaemonCallErr::PostWriteAmbiguous`) in `InputWorker::call_daemon_outcome`; `handle_scroll` sends best-effort `ACTION_CANCEL` on ambiguous `ACTION_DOWN` EOF without replaying `ACTION_DOWN`.
  - Replaced the 40 ms sleep in `input_queue_coalesces_moves_and_bounds_backlog` with explicit Condvar synchronization (`received_first`) and added 4 new hub unit tests + `426`/`400`/`409` WS handler tests.
- **Browser page (`crates/acos-cli/src/view/page.rs`):**
  - Tracked `currentGuestGen` across `/api/sessions`, SSE `state`/`replaced`, frame stream reconnects, and `/api/input/<id>?t=<tok>&g=<guest_gen>` so replaced sessions end cleanly instead of routing clicks to a new guest.
  - Restricted connecting-queue (`wsQueue`) touch-move coalescing to the queue tail only and restricted overflow eviction to queued `touch move` entries (falling back to `closeWs()` rather than evicting a queued press).
  - Gated pointer and wheel input on post-rotation frame confirmation (`pendingRotate` using `X-Frame-Seq`, `frame_seq`, and orientation checks — covering 180° rotations too) and cancelled active pointers across rotation transitions.
  - Added `window.addEventListener("blur", releaseHeldGuestKeys)` to release held keys on window focus loss, and closed WS on `QUEUE_FULL` with `disconnected: true`.
- **Tests & Docs (`crates/acos-cli/tests/{humanview_cp2,humanview_cp3,humanview_cp4}.rs`, `docs/LIVE_VIEW.md`):**
  - Updated `WsClient::connect` in `humanview_cp2`–`cp4` to fetch `guest_gen` from `/api/sessions` and pass `&g=<guest_gen>`.
  - Replaced the single 150 ms sleep in `cp2` connection-cap recovery with bounded 5 s polling for HTTP `200`.
  - Failed `cp3` wheel-scroll verification with `obs_wheel` if no row is observed instead of defaulting to row `0`, and updated `cp3_verify_run_watch_only_refused_under_view` to wait for the SSE `tap ok` event and a post-tap changed frame while WS is still open before asserting half-scale PNG.
  - Aligned `docs/LIVE_VIEW.md` §2 and §8 to list exact verify watch-only marker phases (`touch down`, `scroll`, `key down`/`press`, `text`, `rotate`), qualify earlier JSON/queue errors, and document `Sec-WebSocket-Version: 13` and `&g=<guest_gen>`.

### 12.2 Fresh Verification (Exact Observed Counts)

- `tools/verify/check-layout.sh`: **`check-layout: ok`**.
- Non-ignored unit, doc & integration suites:
  - `cargo test --offline -p acos-art --lib live_view`: **22 passed, 0 failed** (190 filtered).
  - `env -u TMPDIR cargo test --offline -p acos-cli --bin acos`: **237 passed, 0 failed** (+4 new hub regression tests + `426`/`400`/`409` WS checks).
  - `cargo test --offline -p acos-mcp --lib`: **97 passed, 0 failed**; `--test docs --test guide --test documentation_args`: **19 passed, 0 failed** (1 + 14 + 4).
  - `cargo test --offline -p acos-services --test surface_client --test modules_doc`: **15 passed, 0 failed** (13 + 2).
  - `humanview_cp1`–`cp4` & `launcher_home_frame` (non-ignored): `cp1` **3 passed** (2 ignored), `cp2` **3 passed** (1 ignored), `cp3` **3 passed** (2 ignored), `cp4` **2 passed** (1 ignored), `launcher_home_frame` **2 passed** (1 ignored).
- Live ART integration suites (`env -u TMPDIR tools/art-runtime/run-exclusive.sh --task HVCR2 -- tools/guard/guarded.sh --mem 12G --time 300 -- ...`):
  - `launcher_home_frame` (`--ignored`): **1 passed, 0 failed** (9.68 s).
  - `humanview_cp1` (`--ignored`): **2 passed, 0 failed** (15.75 s).
  - `humanview_cp2` (`--ignored`): **1 passed, 0 failed** (6.06 s).
  - `humanview_cp3` (`--ignored`): **2 passed, 0 failed** (14.84 s).
  - `humanview_cp4` (`--release --ignored`): **1 passed, 0 failed** (10.69 s) — arrival-timed (`received_at - t0`): **p50 = 63.38 ms** (≤ 100 ms), **p95 = 64.77 ms** (≤ 200 ms), `min = 60.53 ms`, `max = 65.17 ms`, `mean = 63.06 ms`, `mean_encode_ms = 2.22 ms` (56,266 B/frame avg), `frames_dropped_latest = 0`, `events_dropped = 0`, `min/max_changed_pixels = 147 / 530`.
  - `cr1fix_finish_in_oncreate` (`--release`): **1 passed, 0 failed** (10.60 s).
