# DEVPANEL1 Report — ACOS Sessions in T3 Code's Device Panel

- **Branch:** `devpanel` (merged `origin/main` `40389d8a7` v0.4.1 cleanly at `cc532b86b`, branched from `origin/devpanel-wip` `f5fe4f409`)
- **Spec:** [`docs/DEVICE_PANEL_SPEC.md`](../../../docs/DEVICE_PANEL_SPEC.md)
- **ADR:** `ADR-0033` in [`DECISIONS.md`](../../../DECISIONS.md) and [`docs/internals/decisions.md`](../../../docs/internals/decisions.md) (amends ADR-0027 §5)

## §8 Acceptance Checklist

All automated checks ran against real `/usr/bin/adb` on a private `ANDROID_ADB_SERVER_PORT` (never touching host port `5037` or physical devices) and under `tools/art-runtime/run-exclusive.sh` + `tools/guard/guarded.sh`.

| §8 Item | Test / Benchmark Name | Command | Result (Exact Numbers) | Commit |
|---|---|---|---|---|
| 1. `adb devices` shows `emulator-55xx device` & disappears within 5 s of `acos_destroy` | `cp1_live_art_adb_and_hub_parsers` (`crates/acos-cli/tests/devpanel_cp1_adb.rs`) | `env -u TMPDIR tools/art-runtime/run-exclusive.sh --task DEVPANEL1 -- tools/guard/guarded.sh --mem 12G --time 900 -- cargo test --offline -p acos-cli --test devpanel_cp1_adb -- --include-ignored --test-threads=1 --nocapture` | **PASS** — 5/5 passed (5.71s); `SCAN_MS=5`, `GONE_AFTER_DESTROY_MS=295` (< 5000 ms) | `81953b5de`, `af2ebe49d` |
| 2. Hub §2 shell commands parse in `expo-device-hub` 0.22.1 parsers | `cp1_live_art_adb_and_hub_parsers` (runs vendored `serve-emu-dist/{adb,accessibility,app-info,device-capabilities,app-permissions}.js` under `node`) | Same as §8.1 | **PASS** — `listAllDevices`, `listDevices`, `isEmulatorSerial`, `availableStreamModesForSerial` (`grpc-screenshot`), `getDeviceSize` (`1080x2400`), `readEffectiveDisplaySize`, `getDisplayDensity` (`scale: 1`), `getDisplayRotation`/`Orientation` (`0`/`portrait` → `1`/`landscape_right`), `getUserRotation` (`free`/`lock`), `getFontScale` (`1`), `getNightMode` (`light`), `getNetworkStatus` (`wifi: enabled`), `getReduceMotion`, `getHighTextContrast`, `getFontWeight`, `getSoftwareKeyboard` (`hardwareKeyboard: true`), `screencapPng` (`89504e47`), `getForegroundApp` (`dev.acos.fixtures.counter`), `listPermissions`, `getAccessibilitySnapshot`, `setUserRotation` | `81953b5de`, `af2ebe49d` |
| 3. Unsupported shell command & `adb install` exit non-zero with `acos: unsupported…` | `cp1_live_art_adb_and_hub_parsers` (`adbMod.setFontScale`, `adb shell logcat`, `adb install counter.apk`) | Same as §8.1 | **PASS** — non-zero exit + stderr/rejection matches `acos: unsupported` | `81953b5de`, `af2ebe49d` |
| 4. gRPC `getScreenshot`, `streamScreenshot`, `sendTouch`, `sendKey`, bearer auth & `pid_<pid>.ini` | `cp2_live_grpc_stream_touch_key_and_auth` (`crates/acos-cli/tests/devpanel_cp2_grpc.rs`) + `acos-devbridge` unit tests | `env -u TMPDIR tools/art-runtime/run-exclusive.sh --task DEVPANEL1 -- tools/guard/guarded.sh --mem 12G --time 900 -- cargo test --offline -p acos-cli --test devpanel_cp2_grpc -- --include-ignored --test-threads=1 --nocapture` and `cargo test --offline -p acos-devbridge` | **PASS** — `devpanel_cp2_grpc` 4/4 passed (8.03s); `acos-devbridge` unit tests 3/3 passed (`keymap`, `touch` clamp + multi-touch). Pixel-exact RGB888 `getScreenshot` vs `acos observe --screenshot`, change-driven `streamScreenshot` (idle silent), `sendTouch` increments counter, `sendKey` enters text into `form.apk` EditText, bearer token enforced | `223a29c65` |
| 5. Event feed `source: panel` vs `source: agent` | `cp3_event_feed_source_labels` (`crates/acos-cli/tests/devpanel_cp3.rs`) | `env -u TMPDIR tools/art-runtime/run-exclusive.sh --task DEVPANEL1 -- tools/guard/guarded.sh --mem 12G --time 900 -- cargo test --offline -p acos-cli --test devpanel_cp3 -- --include-ignored --test-threads=1` | **PASS** — `devpanel_cp3` 6/6 passed (40.60s); event feed tags gRPC input `source: panel` and tool input `source: agent` | `f5fe4f409`, `f4121403c` |
| 6. `acos verify --adb` runs watch-only; panel input dropped & noted in feed | `cp3_verify_run_is_watch_only` (`devpanel_cp3.rs`) + `panel_input_ignored_emits_verify_watch_only_without_attaching` (`acos-art::live_view`) | Same as §8.5 + `cargo test --offline -p acos-art --lib live_view` | **PASS** — `cp3_verify_run_is_watch_only` passed (in 6/6, 40.60s); `live_view` unit tests 20/20 passed (10.56s) | `f4121403c` |
| 7. 17 concurrent sessions: 16 exposed, 17th runs with `exposed: false` | `cp3_sixteen_slot_cap` (`devpanel_cp3.rs`) | Same as §8.5 | **PASS** — 16 exposed on `emulator-5554..emulator-5584`, 17th succeeds with `device_panel: {"exposed": false, "reason": "all 16 adb emulator slots in use"}` | `f5fe4f409`, `f4121403c` |
| 8. Crash cleanup (`SIGKILL` `acos serve`) & stale `.ini` sweep | `cp3_crash_cleanup` (`devpanel_cp3.rs`) | Same as §8.5 | **PASS** — device disappears from `adb devices` after `SIGKILL`, next `acos serve --adb` sweeps stale `pid_<dead>.ini` before binding | `f5fe4f409`, `f4121403c` |
| 9. Benchmarks: `--adb` off matches v0.4.1 within noise; `--adb` on (no panel) idle CPU within noise | `acos_bench.py` (`memory` + `latency`, 2 reps × `reps=20`, `fast_reps=40`) & `ab-compare.py` | Release builds of `origin/main` (`40389d8a7`, v0.4.1) vs `devpanel` (`e08824661`) `--adb` off and `--adb` on (isolated `ANDROID_ADB_SERVER_PORT=59999`), run under `run-exclusive.sh --exclusive` | **PASS** — `--adb` off vs v0.4.1: 32/32 metrics within noise (M8 counter fast improved -13.87% p50 / -1.5% trimmed mean), 4/4 lattice metrics within noise. `--adb` on (no panel) vs v0.4.1: 19/19 metrics within noise (exact numbers below) | `e08824661` |
| 10. Repo layout & doc contracts | `tools/verify/check-layout.sh` + doc tests | `bash tools/verify/check-layout.sh` + `cargo test --offline -p acos-mcp --test docs --test guide --test documentation_args` + `cargo test --offline -p acos-services --test modules_doc` | **PASS** — `check-layout: ok`; `docs` 1/1, `documentation_args` 4/4, `guide` 14/14, `modules_doc` 2/2 passed | `e08824661` + report commit |
| Manual (owner) | Open T3 Code Device panel with a live `acos serve --adb` session | Launch session via `acos serve --adb` and open T3 Code Device panel | **Pending owner manual verification** | — |

## §8.9 Benchmark Measurements (v0.4.1 `40389d8a7` vs `devpanel` `e08824661`)

All runs used `animations=on`, release profile, 0 noisy phases across all arms (`loadavg1 < 1.8`, `MemAvailable > 49 GiB`).

### Arm 1: `devpanel` (`--adb` off) vs `v0.4.1`

`acos_bench.py --compare` (p50 / threshold gate — **0 regressions**):

| Metric | v0.4.1 | `devpanel` (`--adb` off) | Delta | Delta % | Threshold | Verdict |
|---|---:|---:|---:|---:|---:|---|
| `m1_idle_mem_mib` | 58.03 MiB | 58.22 MiB | +0.191 | +0.33% | ±3.0% / 0.50 | within-noise |
| `m1_idle_pss_mib` | 238.2 MiB | 238.3 MiB | +0.102 | +0.04% | ±3.0% / 0.50 | within-noise |
| `m2_per_session_anon_shmem_n1` | 56.05 MiB | 56.23 MiB | +0.185 | +0.33% | ±3.0% / 0.50 | within-noise |
| `m2_per_session_anon_shmem_n5` | 56.22 MiB | 56.38 MiB | +0.156 | +0.28% | ±3.0% / 0.50 | within-noise |
| `m2_per_session_anon_shmem_n10` | 56.29 MiB | 56.22 MiB | -0.075 | -0.13% | ±3.0% / 0.50 | within-noise |
| `m4_launch_strict_ms` | 285.7 ms | 286.1 ms | +0.368 | +0.13% | ±6.0% / 2.00 | within-noise |
| `m4_launch_fast_ms` | 150.6 ms | 149.5 ms | -1.055 | -0.70% | ±7.0% / 2.00 | within-noise |
| `m5_screenshot_ms` | 11.79 ms | 12.10 ms | +0.314 | +2.66% | ±5.0% / 2.00 | within-noise |
| `m6_reset_strict_ms` | 332.0 ms | 331.4 ms | -0.614 | -0.18% | ±5.0% / 2.00 | within-noise |
| `m6_reset_fast_ms` | 189.8 ms | 189.2 ms | -0.580 | -0.31% | ±5.0% / 2.00 | within-noise |
| `m7_tree_strict_ms` | 41.61 ms | 41.73 ms | +0.123 | +0.30% | ±5.0% / 2.00 | within-noise |
| `m7_tree_fast_ms` | 0.748 ms | 0.650 ms | -0.098 | -13.10% | ±5.0% / 2.00 | within-noise |
| `m8_tap_counter_strict_ms` | 952.2 ms | 953.9 ms | +1.720 | +0.18% | ±5.0% / 2.00 | within-noise |
| `m8_tap_counter_fast_ms` | 28.51 ms | 24.56 ms | -3.955 | -13.87% | ±5.0% / 2.00 | improved |
| `m8_tap_form_strict_ms` | 925.0 ms | 924.2 ms | -0.822 | -0.09% | ±5.0% / 2.00 | within-noise |
| `m8_tap_form_fast_ms` | 30.23 ms | 30.35 ms | +0.118 | +0.39% | ±5.0% / 2.00 | within-noise |
| `teardown_ms` | 21.00 ms | 21.00 ms | 0.000 | 0.00% | ±5.0% / 2.00 | within-noise |
| `cpu_idle_n1_pct` | 0.199% | 0.200% | +0.001% | +0.25% | ±17% / 0.20 | within-noise |
| `cpu_idle_n5_pct` | 0.915% | 0.882% | -0.033% | -3.60% | ±14% / 0.20 | within-noise |
| `cpu_idle_n10_pct` | 1.735% | 1.620% | -0.115% | -6.60% | ±11% / 0.20 | within-noise |
| `wakeups_idle_n1_per_s` | 1.212 /s | 1.228 /s | +0.016 | +1.32% | ±10.0% / 0.20 | within-noise |
| `wakeups_idle_n5_per_s` | 2.119 /s | 2.127 /s | +0.009 | +0.40% | ±10.0% / 0.20 | within-noise |
| `wakeups_idle_n10_per_s` | 3.133 /s | 3.142 /s | +0.009 | +0.27% | ±10.0% / 0.20 | within-noise |
| `cpu_active_tap_strict_pct` | 44.06% | 45.60% | +1.547% | +3.51% | ±10.0% / 0.20 | within-noise |

Lattice-aware A/B (`ab-compare.py`, pooled 5%-trimmed mean, n=80/80 per metric — **0 regressions**):
- `m4_launch_fast_ms`: `147.39` vs `145.76` ms (-1.63 ms, -1.1%), first slot `32%` vs `40%` → within-noise
- `m6_reset_fast_ms`: `191.08` vs `189.96` ms (-1.12 ms, -0.6%), first slot `1%` vs `2%` → within-noise
- `m8_tap_counter_fast_ms`: `25.15` vs `24.76` ms (-0.39 ms, -1.5%), first slot `49%` vs `50%` → within-noise
- `m8_tap_form_fast_ms`: `30.12` vs `30.38` ms (+0.26 ms, +0.9%), first slot `100%` vs `100%` → within-noise

### Arm 2: `devpanel` (`--adb` on, no panel connected) vs `v0.4.1`

`acos_bench.py --compare` (**0 regressions** across all 19 memory/CPU/wakeup metrics):

| Metric | v0.4.1 | `devpanel` (`--adb` on) | Delta | Delta % | Threshold | Verdict |
|---|---:|---:|---:|---:|---:|---|
| `m1_idle_mem_mib` | 58.03 MiB | 59.45 MiB | +1.425 | +2.46% | ±3.0% / 0.50 | within-noise |
| `m1_idle_pss_mib` | 238.2 MiB | 241.3 MiB | +3.123 | +1.31% | ±3.0% / 0.50 | within-noise |
| `m2_per_session_anon_shmem_n1` | 56.05 MiB | 57.47 MiB | +1.419 | +2.53% | ±3.0% / 0.50 | within-noise |
| `m2_per_session_anon_shmem_n5` | 56.22 MiB | 56.54 MiB | +0.316 | +0.56% | ±3.0% / 0.50 | within-noise |
| `m2_per_session_anon_shmem_n10` | 56.29 MiB | 56.36 MiB | +0.069 | +0.12% | ±3.0% / 0.50 | within-noise |
| `cpu_idle_n1_pct` | 0.199% | 0.216% | +0.017% | +8.29% | ±16% / 0.20 | within-noise |
| `cpu_idle_n5_pct` | 0.915% | 0.898% | -0.018% | -1.91% | ±10.0% / 0.20 | within-noise |
| `cpu_idle_n10_pct` | 1.735% | 1.683% | -0.052% | -2.97% | ±10.0% / 0.20 | within-noise |
| `wakeups_idle_n1_per_s` | 1.212 /s | 1.211 /s | -0.001 | -0.12% | ±10.0% / 0.20 | within-noise |
| `wakeups_idle_n5_per_s` | 2.119 /s | 2.093 /s | -0.026 | -1.25% | ±10.0% / 0.20 | within-noise |
| `wakeups_idle_n10_per_s` | 3.133 /s | 3.154 /s | +0.021 | +0.65% | ±10.0% / 0.20 | within-noise |
| `teardown_ms` | 21.00 ms | 21.50 ms | +0.500 | +2.38% | ±5.0% / 2.00 | within-noise |

Each exposed session's adb accept loop blocks in `libc::poll(..., -1)` (zero wakeups/s when idle), and `acos-devbridge` subscribes to live-view frames only while `streamScreenshot` is active, adding only ~1.4 MiB static RSS for the single shared bridge process across all 10 sessions (`+0.07 MiB/session` at `N=10`).

## §10 Open Risks & Measurements

1. **adb emulator scan timing (`SCAN_MS=5`, `GONE_AFTER_DESTROY_MS=295`):**
   - `crates/acos-cli/src/adb/mod.rs` (`notify_adb_server`) connects to `127.0.0.1:$ANDROID_ADB_SERVER_PORT` (default `5037`) immediately after binding `127.0.0.1:<5555 + 2k>` and sends `host:emulator:<adb_port>`. Because `adb` handles that notification synchronously rather than waiting for a server restart or periodic scan, `adbMod.listAllDevices()` discovered `emulator-55xx device` in **5 ms**.
   - **Risk:** If the host `adb` server is restarted *after* `acos serve --adb` has already launched sessions, `adb` scans local emulator ports `5555..5585` at server startup (standard Android emulator behavior), so listeners bound on `5555 + 2k` remain discoverable on reconnect/restart.
2. **Extra hub calls audited in `expo-device-hub` 0.22.1 vs §2:**
   - Audited `serve-emu-dist/{adb,device-grid,device-capabilities,app-info,app-permissions,app-management,emulator,grpc-session}.js`.
   - Beyond the baseline commands listed in §2, the hub invokes `dumpsys power` (`grpc-session.js: isDeviceAwake`) and `dumpsys package <pkg>` (`app-info.js`, `app-permissions.js: listPermissions`); both are implemented in `crates/acos-cli/src/adb/shell.rs` and verified in `cp1_live_art_adb_and_hub_parsers`.
   - Optional panel actions outside §2 scope (`cmd power wakeup|sleep`, `getprop sys.boot_completed` AVD boot wait, `pm reset-permissions` / `appops reset`, `pm list packages` / `monkey` / `am force-stop`, `settings put system font_scale`) exit 1 honestly with `acos: unsupported: <cmd>` rather than faking success. During the owner's manual Device panel check, inspect `acos serve --adb` stderr / logs for any unexpected unsupported adb service, shell command, or gRPC method triggered by live panel interactions.
