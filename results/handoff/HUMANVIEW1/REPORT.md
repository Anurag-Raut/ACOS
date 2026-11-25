# HUMANVIEW1 Report — Interactive `acos view`

**Branch:** `humanview` (from `v0.4.1` / `40389d8a7`, **no PR opened, not merged**)
**Decision:** ADR-0033 (`DECISIONS.md`) amends ADR-0027 §5
**Spec:** `docs/HUMAN_VIEW_SPEC.md`
**Status:** Automated CP1–CP5 + Fix Round 1 + Live T3 & Playwright Browser E2E complete & pushed; **Owner one-time Gaten check** remains manual (runbook in §7).

---

## 1. Checkpoints & Commits

| CP | Commit | Pushed | Summary |
|---|---|---|---|
| Spec | `aa6579dea` | yes | `docs/HUMAN_VIEW_SPEC.md` baseline |
| CP1 | `b64240dca` | yes | Port `source` labels (`agent`/`panel`), verify-run `watch_only`, `device_name`, and `panel_*` dispatch |
| CP2 | `83f8e5d99` | yes | Hub RFC 6455 `/api/input/<id>` WebSocket, Host/token/cap enforcement, ordered per-session worker, keycodes, disconnect `cancel`, live tests |
| CP3 | `8747ab7d9` | yes | Interactive page UI (`page.rs`), pointer→touch stream, wheel→scroll, keyboard→text/key, Back/Home/Recents/Rotate toolbar, verify banner, upstream upgrade/revert |
| CP4 | `896003a19` | yes | Producer rate-limiter fix on empty polls, 1.0× direct copy (`rgb_scratch`/`encode_scratch` reuse), interactive upgrade to 1.0× PNG @ 30 fps, latency harness (`humanview_cp4.rs`), 4-rep A/B & idle-CPU benchmarks |
| CP5 | *(this commit)* | yes | ADR-0033 (`DECISIONS.md`), doc updates (`LIVE_VIEW.md`, `AGENT_GUIDE.md`, `MCP.md`, `README.md`, `docs/internals/{decisions,overview}.md`, `HUMAN_VIEW_SPEC.md`), `results/handoff/HUMANVIEW1/REPORT.md` |

---

## 2. Acceptance Matrix (`docs/HUMAN_VIEW_SPEC.md` §6)

| # | Acceptance Item | Status | Evidence / Exact Numbers |
|---|---|---|---|
| 1a | WebSocket tap on `counter.apk` "Increment" increments counter (via `observe_tree`) | **PASS** | `humanview_cp2::cp2_ws_live_tap_text_back_and_source_labels` |
| 1b | Drag scrolls a list (`scroll.apk`, visible first row changes) | **PASS** | `humanview_cp3::cp3_live_drag_fling_long_press_and_rotate` |
| 1c | Long press opens context menu | **PASS** | `humanview_cp3::cp3_live_drag_fling_long_press_and_rotate` |
| 1d | Typed text via `/api/input` appears in `EditText` (`form-validation.apk`) | **PASS** | `humanview_cp2::cp2_ws_live_tap_text_back_and_source_labels` |
| 1e | `KEYCODE_BACK` finishes second activity (`multi-activity.apk`) | **PASS** | `humanview_cp2::cp2_ws_live_tap_text_back_and_source_labels` |
| 1f | Rotate command changes display dimensions (`1080x2400` ↔ `2400x1080`) | **PASS** | `humanview_cp3::cp3_live_drag_fling_long_press_and_rotate` |
| 2 | Human input labelled `source: panel`, agent input `source: agent` in `/api/events` | **PASS** | `humanview_cp1::cp1_event_feed_source_labels_and_app_label`, `humanview_cp2::cp2_ws_live_tap_text_back_and_source_labels` |
| 3 | Verify run under `acos view` refuses input (`VERIFY_WATCH_ONLY`); verdict unchanged | **PASS** | `humanview_cp1::cp1_verify_run_publishes_watch_only_and_records_ignored_panel_input`, `humanview_cp3::cp3_verify_run_watch_only_refused_under_view` |
| 4 | Token, Host header (`127.0.0.1:<port>`), and connection cap enforced on `/api/input` (wrong token/Host → `403`) | **PASS** | `humanview_cp2::cp2_ws_input_auth_host_and_cap_enforced` |
| 5 | Gesture fidelity: drag delivers monotonic `MOVE` stream; fling scrolls past release point | **PASS** | `humanview_cp3::cp3_live_drag_fling_long_press_and_rotate` |
| 6 | Input-to-changed-frame latency (50 reps): p50 ≤ 100 ms, p95 ≤ 200 ms | **PASS** | `humanview_cp4::cp4_latency_tap_to_changed_frame_50_reps`: **p50 = 66.70 ms**, **p95 = 68.90 ms** (min 62.97 ms, max 69.37 ms, mean 66.56 ms) |
| 7a | `acos_bench.py --phase memory,latency --animations on` vs `v0.4.1` (4 interleaved reps) | **PASS** | `compare_rc=0` (**within-noise** on all 32 metrics), `ab_rc=0` (**within-noise** on all 4 fast-settle lattice metrics) |
| 7b | Idle CPU with `acos view` open & idle vs closed (4 interleaved reps, 60 s window) | **PASS** | Serve CPU median **0.233%** (closed) vs **0.242%** (open-idle, +0.009 pp); `acos view` 0.00 s CPU ticks |
| 8 | `tools/verify/check-layout.sh` passes; only `.md` reports under `results/` | **PASS** | `check-layout: ok` |
| E2E | Live E2E in T3 Code's browser tab (`preview_open`, Increment/form/Back/Home, screenshots) | **PENDING** | Deferred to browser subagent; see runbook in §6 |
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

Raw 50 samples (ms):
`[69.3, 66.96, 62.97, 66.06, 66.67, 66.97, 68.9, 63.75, 66.7, 66.46, 67.36, 66.5, 65.53, 67.15, 66.81, 67.46, 66.15, 66.21, 66.21, 66.84, 67.17, 66.0, 67.37, 64.92, 69.37, 65.0, 67.35, 67.32, 67.46, 64.95, 66.54, 66.71, 66.27, 68.12, 66.03, 67.38, 67.37, 66.8, 66.46, 66.13, 67.01, 66.8, 64.0, 67.15, 66.26, 66.39, 67.87, 66.76, 65.33, 64.89]`.

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
3. **Browser E2E & Owner Gaten check remain pending** (see §6–§7 below).
4. **No PR opened / not merged:** per assignment instructions ("No PR, never merge"), commits are pushed directly to `origin/humanview`.

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

