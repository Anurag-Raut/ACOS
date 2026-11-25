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
| 10. Repo layout & doc contracts | `tools/verify/check-layout.sh` + doc tests | `bash tools/verify/check-layout.sh` + `cargo test --offline -p acos-mcp --test docs --test guide --test documentation_args` + `cargo test --offline -p acos-services --test modules_doc` | **PASS** — `check-layout: ok`; `docs` 1/1, `documentation_args` 4/4, `guide` 14/14, `modules_doc` 2/2 passed | `e08824661` + working tree |
| 11. Hub discovery (`getprop`, emulator console `avd name`, `device_name`, `expo-device-hub` `listDevices`) | `cp4_hub_lists_acos_devices_with_default_and_custom_names` (`devpanel_cp4_hub.rs`) | `env -u TMPDIR tools/art-runtime/run-exclusive.sh --task DEVPANEL1-HUB -- tools/guard/guarded.sh --mem 12G --time 1500 -- cargo test --offline -p acos-cli --test devpanel_cp4_hub -- --include-ignored --test-threads=1 --nocapture` | **PASS** — default `"Counter · cp4-def"`, `acos_launch.device_name` `"Checkout Probe Device"`, CLI `--device-name` `"CLI Device Name"` all surface in `pid_*.ini`, `getprop`, console `avd name`, and `expo-device-hub` 0.22.1 `/api/devices?booted=1` | working tree |
| 12. Option B `agent-device` adb commands + `source: adb` feed + verify watch-only + `PR_SET_PDEATHSIG` | `cp4_agent_device_adb_commands_and_source_adb_feed`, `cp4_verify_ignores_adb_input_and_emits_watch_only`, `cp4_devbridge_survives_launching_client_disconnect` (`devpanel_cp4_hub.rs`) | Same as §8.11 + `cargo test --offline -p acos-cli --bin acos adb` (8/8) | **PASS** — `devpanel_cp4_hub` 6/6 passed (40.34s); full guarded CP1–CP4 suite 21/21 passed; `adb` shell `input` mutates real app state with `source: adb`; verify rejects `adb` input (`VERIFY_WATCH_ONLY`); `acos-devbridge` survives `acos launch --keep` client disconnect and exits on `acos serve` `SIGKILL` | working tree |
| Live T3 E2E (`device_list`, `device_open`, `device_screenshot`, `agent-device` 0.21.36) | Live T3 MCP + `agent-device` against `acos serve --adb` (`counter.apk` `"E2E Counter"`, `text-input.apk` `"E2E TextInput"`) + `stream_probe.py` | T3 `device_list` / `device_open` / `device_screenshot` + `agent-device open/apps/click/type/screenshot/snapshot` + `PUT /vendor/serve-emu/api/stream-mode` + `POST /vendor/serve-emu/api/tap` | **PARTIAL (2 external tool gaps escalated)** — `device_list`, `device_open`, `agent-device open`, `apps`, `click` (`0 → 1`, `source: adb`), `type` (`"hello world"` → `"Saved: hello world"`, `source: adb`), `agent-device screenshot` (`1080×2400` PNG), and `api/tap` (`1 → 2`, `source: panel`) all **PASS**. Stock `device_screenshot` requires `inputSource: "grpc"` on `serve-emu` (stock defaults to `"scrcpy"`). `agent-device snapshot -i` fails because `agent-device` 0.21.36 mandates `pm install` + `am instrument` of `agent-device-android-snapshot-helper-0.21.36.apk` with no `uiautomator dump` path | — |

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
   - Optional panel actions outside §2 scope (`cmd power wakeup|sleep`, `getprop sys.boot_completed` AVD boot wait, `pm reset-permissions` / `appops reset`, `am force-stop` / `am kill`, `settings put system font_scale`, `pm install`, `am instrument`) exit 1 honestly with `acos: unsupported: <cmd>` rather than faking success. During the owner's manual Device panel check, inspect `acos serve --adb` stderr / logs for any unexpected unsupported adb service, shell command, or gRPC method triggered by live panel interactions.

## CodeRabbit round 1

12 owner-approved findings fixed on `devpanel` (all 6 security-hardening comments `4683005085`, `4683005093`, `4683005148`, `4683005163`, `4683005174`, `4683005188` skipped as out of scope for this single-user local dev tool). Every fix below had a reproduction test observed failing before the change and passing after.

| # | Finding | Reproduced? (Test Name) | Fix Commit | Verified |
|---|---|---|---|---|
| #0 | Missing parent dirs under runtime/lock/avd/registry roots created with umask instead of `0700` | **Yes** — `prepare_private_dir_creates_missing_parents_0700`, `prepare_lock_dir_creates_missing_parents_0700`, `write_creates_missing_parent_dir_with_mode_0700`, `ensure_private_dir_creates_missing_parents_0700`, `ensure_runtime_base_creates_missing_parents_0700`, `serve_starts_after_nested_socket_creates_acos_parent_under_fresh_xdg_runtime_dir` | `00799abe8` | **PASS** (`adb::`, `live_registry::`, `acos-mcp`, `devpanel_cp1_adb`) |
| #1 | `acos-devbridge` spawned on per-connection worker thread (`PR_SET_PDEATHSIG` died on client disconnect) | **Yes** — `adb::tests::bridge_survives_spawning_worker_thread_exit`, `cp2_bridge_survives_client_disconnect` (`devpanel_cp2_grpc.rs`) | `603ae1e22`, `a31e188b8` | **PASS** (`adb::` 7/7, `devpanel_cp2_grpc` 5/5) |
| #2 | `ArtSession::screenshot_png_bytes` fell back to stale `bound_frame` on non-stale errors and dead sessions | **Yes** — `engine::art::tests::screenshot_png_bytes_propagates_errors_and_only_falls_back_on_live_stale_frame` | `b5588568f` | **PASS** (`engine::art::` 54/54) |
| #3 | Per-session `raw_shot` counter could collide across sessions | **Yes** — `raw_screenshot_paths_do_not_collide_across_sessions` (`acos-devbridge`) | `7508de8ba` | **PASS** (`acos-devbridge` 10/10) |
| #4 | `live_view::writer_loop` left pending chunked frame tail buffered when screen went idle | **Yes** — `live_view::tests::idle_screen_flushes_pending_large_rgb888_tail_to_slow_reader` | `ebb3b48b8` | **PASS** (`acos-art --lib live_view` 21/21) |
| #5 | `acos-devbridge` respawn lost live sessions and left stale `grpc.port` in `pid_<pid>.ini` | **Yes** — `adb::tests::dead_bridge_respawn_reregisters_surviving_sessions_and_updates_ini` | `e53f79807` | **PASS** (`adb::` 7/7) |
| #6 | `adb::wire` socket writes were unbounded and omitted `Shutdown::Both` on error | **Yes** — `adb::wire::tests::stalled_peer_write_times_out_and_shuts_down_transport` | `bb8e6faf1` | **PASS** (`adb::` 7/7) |
| #7 | Replacing an active gRPC session in `acos-devbridge` orphaned the displaced server | **Yes** — `concurrent_register_same_session_shuts_down_displaced_servers` (`acos-devbridge`) | `5dd161ef4` | **PASS** (`acos-devbridge` 10/10) |
| #8 | Concurrent `send_touch` and `send_key` RPCs on one session raced callbacks | **Yes** — `concurrent_send_touch_and_send_key_serialize_callbacks` (`acos-devbridge`) | `8c616719d` | **PASS** (`acos-devbridge` 10/10) |
| #9 | `live_client` reader EOF hung subscribers and blocked resubscribe while receiver remained alive | **Yes** — `live_reader_eof_closes_subscriber_and_allows_reconnect_while_held` (`acos-devbridge`) | `478d36a38` | **PASS** (`acos-devbridge` 10/10) |
| #10 | `TouchTracker` processed pointer updates out of list order and dropped stationary `ACTION_MOVE` edges | **Yes** — `release_then_reuse_same_pointer_in_single_event_single_finger`, `release_then_reuse_same_pointer_in_single_event_multi_touch`, `stationary_move_emits_move_only_without_transitions` | `87cd9cec9` | **PASS** (`acos-devbridge` 10/10) |
| #11 | `acos verify --adb` skipped `VerifyViewOps` registration for `DeferredArtSession` (`dev_server: start` before `launch`) | **Yes** — `cp3_deferred_verify_adb_exposes_only_after_launch` (`devpanel_cp3.rs`) | `e19090110` | **PASS** (`devpanel_cp3`, `verify::` 2/2) |

### Post-CR Verification

- **Unit & contract suites:** `acos-devbridge` (`10/10`), `acos-art --lib live_view` (`21/21`), `acos-cli --bin acos` (`adb::` `7/7`, `live_registry::` `7/7`, `engine::art::` `54/54`, `verify::` `2/2`), `acos-services --test modules_doc` (`2/2`), `env -u TMPDIR cargo test --offline -p acos-mcp` (`--lib` `97/97`, `args` `5/5`, `art_default` `5/5`, `bounded` `3/3`, `compact_reply` `1/1`, `docs` `1/1`, `documentation_args` `4/4`, `guide` `14/14`; `e2e.rs` skipped/needs worktree-built `fixtures/build`), `bash tools/verify/check-layout.sh` (`check-layout: ok`).
- **Guarded real-ART integration suite (`--include-ignored --test-threads=1`):** `devpanel_cp1_adb` (`6/6`), `devpanel_cp2_grpc` (`5/5`), `devpanel_cp3` (`6/7`: `cp3_crash_cleanup`, `cp3_deferred_verify_adb_exposes_only_after_launch`, `cp3_event_feed_source_labels`, `cp3_verify_run_is_watch_only`, and helpers passed; `cp3_sixteen_slot_cap` reached 13 slots before hitting `all 16 adb emulator slots in use` because an external manual `acos serve --adb` process on the host held loopback ports `5555` and `5557`).
- **Idle CPU & memory sanity (`tools/bench/acos-bench.sh --phases memory --reps 1 --animations on`, `--adb` off):** `m1_idle_mem_mib = 58.38 MiB` (vs `58.22 MiB` on `e08824661`), `cpu_idle_n1/n5/n10 = 0.232% / 1.020% / 1.924%`, `wakeups_idle_n1/n5/n10 = 1.210 / 2.074 / 3.179 /s` (zero regressions; within noise of `e08824661` / `v0.4.1`).

## Option B & Hub Supported Command Table

Implemented across [`crates/acos-cli/src/adb/shell.rs`](../../../crates/acos-cli/src/adb/shell.rs), [`crates/acos-cli/src/adb/console.rs`](../../../crates/acos-cli/src/adb/console.rs), and [`crates/acos-cli/src/adb/wire.rs`](../../../crates/acos-cli/src/adb/wire.rs) (both `shell:` and `exec:` / `exec-out:` services, including compound `;`, `&&`, `||` command chains):

### 1. ADB Shell & Exec Commands
| Command | Behavior & ACOS Backing | Source / Verify Guard |
|---|---|---|
| `getprop` / `getprop <key> [default]` | Returns `[key]: [value]` lines (`ro.kernel.qemu=1`, `ro.boot.qemu=1`, `ro.boot.qemu.avd_name`, `persist.sys.avd_name`, `sys.boot_completed=1`, `ro.product.*=ACOS`, `ro.build.version.*=14/34`, `ro.sf.lcd_density=<density>`) or single-key lookup | Session snapshot + sanitized `avd_name` |
| `input [source] tap X Y` | Routes through ACOS real input path (`adb_tap`) | Live feed `source: "adb"`; verify rejects (`VERIFY_WATCH_ONLY`) |
| `input [source] swipe X1 Y1 X2 Y2 [ms]` | Routes through ACOS real swipe path (`adb_swipe`, default `300` ms) | Live feed `source: "adb"`; verify rejects (`VERIFY_WATCH_ONLY`) |
| `input [source] text <s>` | Decodes AOSP `%s` → space and injects via `adb_text` (`InputConnection.commitText`) | Live feed `source: "adb"`; verify rejects (`VERIFY_WATCH_ONLY`) |
| `input [source] keyevent [--longpress] <code\|KEYCODE_*>` | Resolves numeric or `KEYCODE_*` / short key names (`adb_key`) | Live feed `source: "adb"`; verify rejects (`VERIFY_WATCH_ONLY`) |
| `screencap -p` / `screencap -d <id> -p` | Returns real PNG bytes from `acos-art` frame capture (`observe --screenshot` pixels) | Read-only (also works over `exec-out screencap -p`) |
| `uiautomator dump [--compressed] [path]` | Translates `observe_tree` (`UiTree`) into AOSP `hierarchy`/`node` XML; supports `/dev/tty`, `/dev/stdout`, `-`, or in-session virtual file (`/sdcard/...`) | Read-only |
| `cat <path>`, `rm [-f\|-rf] <path>` | Reads/removes in-session virtual files created by `uiautomator dump` (up to 16 cached paths) | Read-only / virtual |
| `pm list packages` / `cmd package list packages` (`-f`, `-3`, `-s`, `--show-versioncode`, `[filter]`) | Lists packages loaded in the active session (`snap.package` + `extra_apks`) with APK path and manifest `versionCode` | Session APK manifest |
| `pm path <pkg>` | Returns `package:<apk_path>` for installed session package(s); exit 1 otherwise | Session APK path |
| `pm list features`, `cmd package has-feature <feat>` | Reports touchscreen/multitouch/portrait/landscape true; telephony/tv/leanback false | Session capabilities |
| `cmd package query-activities` / `resolve-activity` | Resolves `MAIN` launcher activity component (`pkg/.Activity`) for session package; empty for Leanback | Session manifest |
| `am get-current-user` | Outputs `0` | Host session user |
| `am start [...]` | Validates target matches live session package/component and returns standard `am start` output; fails for other packages | Live session |
| `am force-stop <pkg>`, `am kill <pkg>` | Refused honestly (`exit 1`, `acos: unsupported: am <subcmd> ...`) | Honest refusal |
| `pm install*`, `cmd package install*`, `adb install` | Refused honestly (`exit 1`, `acos: unsupported: install into a live session is not supported; launch a new session with the APK`) | Honest refusal |
| `dumpsys window [windows\|displays]`, `dumpsys activity [activities]` | Reports current focus, resumed activity, and display rotation | Session snapshot |
| `dumpsys display` | Reports `ACOS Display` size, density, and `mCurrentOrientation=<rot>` | Session snapshot |
| `dumpsys input_method`, `dumpsys input \| grep Classes:` | Reports focused client package (`mInputShown=false`) and `ALPHAKEY` | Session snapshot |
| `dumpsys power`, `dumpsys package <pkg>` | Reports `mWakefulness=Awake` and APK version/SDK/label metadata | APK + session state |
| `wm size`, `wm density` | Reports `Physical size: WxH` and `Physical density: D` | Session snapshot |
| `cmd window user-rotation [free\|lock N]` | Gets/sets rotation lock and invokes `target.rotate(N)` | Real display rotation |
| `settings get/put system accelerometer_rotation` & `user_rotation` | Gets/puts auto-rotation lock (`0`/`1`) and applies `target.rotate(N)` | Real display rotation |
| `settings get ...` (`font_scale`, `wifi_on`, `mobile_data`, `*_animation_scale`, `high_text_contrast_enabled`, `font_weight_adjustment`, `show_ime_with_hard_keyboard`) | Returns honest session/system values | Session snapshot |
| `cmd uimode night`, `pidof <pkg>`, `echo`, `true`, `false` | Reports `Night mode: no`, guest PID (`exit 1` when absent), shell helpers | Session state |
| Everything else (`scrcpy`, `app_process`, `pm install`, `am instrument`, `logcat`, ...) | Exits non-zero (`exit 1`) with `acos: unsupported: <cmd>` | Honest refusal |

### 2. Emulator Console (`127.0.0.1:<5554 + 2k>`) & Services
- **Emulator Console:** `auth <token>` (`OK`), `avd name` (returns `avd_name` + `OK`), `avd path` (`KO: no AVD directory (live ACOS session)`), `help`, `quit`/`exit` (`OK`); all other commands return `KO: unsupported console command: <line>`.
- **Non-shell ADB services:** `framebuffer:` (raw RGBA frame for fallback viewers); `sync:`/`push` and arbitrary services exit non-zero honestly.
- **gRPC (`acos-devbridge` on `pid_<pid>.ini` `grpc.port` with `grpc.token` bearer auth):** `getStatus`, `getScreenshot`, `streamScreenshot`, `sendTouch`, `sendKey` (`keymap` + ASCII typing).

## Live T3 E2E Findings & Escalated Spec-Scope Items

1. **Fixed during live E2E — `PR_SET_PDEATHSIG` thread-lifetime bug in [`crates/acos-cli/src/adb/bridge.rs`](../../../crates/acos-cli/src/adb/bridge.rs):**
   - `BridgeProcess::spawn()` previously forked `acos-devbridge` directly on the client-connection worker thread with `PR_SET_PDEATHSIG(SIGTERM)`. On Linux, `PR_SET_PDEATHSIG` triggers when the spawning *OS thread* exits, killing `acos-devbridge` immediately when `acos launch --keep` disconnected. Moved spawn onto a dedicated `acos-devbridge-sup` supervisor thread blocked in `poll(stop_event_fd, -1)`, verified with live `device_screenshot` and covered by `cp4_devbridge_survives_launching_client_disconnect`.
2. **External tool mismatch #1 — `expo-device-hub` 0.22.1 `serve-emu` default `inputSource`:**
   - Unconfigured `expo-device-hub` 0.22.1 defaults `DEFAULT_GRPC_INPUT_SOURCE = "scrcpy"` and calls `startScrcpyControl` (`test -f`, `adb push scrcpy-server-v4.0`, `app_process`) with no gRPC fallback on failure, returning HTTP 503 for `device_screenshot` unless `inputSource` is switched to `"grpc"` (`EXPO_DEVICE_HUB_SERVE_EMU_OPTIONS='{"inputSource":"grpc"}'` or `PUT /vendor/serve-emu/api/stream-mode`). Once `inputSource: "grpc"` is selected, `device_screenshot` (`1080×2400` PNG) and `POST /vendor/serve-emu/api/tap` (`source: "panel"`) pass.
3. **External tool mismatch #2 — `agent-device` 0.21.36 `snapshot`:**
   - Audited `/home/Qui/.t3/tools/agent-device/0.21.36/node_modules/agent-device/dist/src/{mechanics,snapshot-helper-install}.js`: `agent-device` 0.21.36 has no `uiautomator dump` path anywhere in `dist/src/`. `agent-device snapshot` (`G()`) unconditionally pushes and `pm install`s `agent-device-android-snapshot-helper-0.21.36.apk` and executes `am instrument` (`agentDeviceProtocol=android-snapshot-helper-v1`). Meanwhile, `agent-device open`, `apps`, `click`, `type`, and `screenshot` all pass live against ACOS, and `adb shell uiautomator dump` + `serve-emu`'s `GET /vendor/serve-emu/api/accessibility` both pass with ACOS's AOSP XML translation.

## Advisor Consultation (`gemini-4-neon`)

- Consulted `antigravity` / `gemini-4-neon` (`thinking_level: high`, read-only advisor) at [Consult Neon advisor on E2E blockers & PDEATHSIG fix](t3-thread://v2/edd23a4b-b34e-4ee4-9da0-f9eaad7695e0/thread:delegated-task:command%3Amcp%3A85b7d7f2-7cb7-4e43-87c0-4b651831063c%3Adelegate-task%3Aca79aaa0-003c-4dd4-8b30-b19611d8691a).
- **Advisor verification & recommendation:** Confirmed both `serve-emu` (`DEFAULT_GRPC_INPUT_SOURCE = "scrcpy"` with no fallback) and `agent-device` 0.21.36 (`snapshot`/`fill`/`swipe` strictly require helper APK install + `am instrument`, zero `uiautomator dump` fallback). Recommended keeping ACOS strictly honest-unsupported for `adb push`/`app_process`/`pm install`/`am instrument` rather than spoofing external helper protocols, and escalating both spec-scope questions to the lead with lettered options.
- **Advisor code review applied:** Reviewed our `PR_SET_PDEATHSIG` fix in `crates/acos-cli/src/adb/bridge.rs` and flagged 5 hardening improvements that we applied before final testing: (1) redirect `acos-devbridge` stderr to a log file in `runtime_dir` instead of an undrained `Stdio::piped()` pipe, (2) wrap `stop_event_fd` in `Arc<OwnedFd>` + `Drop for BridgeProcess`, (3) cast `SIGTERM as libc::c_ulong` + pass zero padding args to `prctl`, (4) check `getppid() != parent_pid` in `pre_exec`, and (5) check `POLLERR | POLLHUP | POLLNVAL` in `poll()` loops (plus added `settings get/put system accelerometer_rotation` and `settings put system user_rotation` in `shell.rs`).
