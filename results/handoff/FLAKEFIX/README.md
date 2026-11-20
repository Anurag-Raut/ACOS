# FLAKEFIX — `modfill-wifi` callback ordering race, fresh-worktree ART failure, and per-entry `logd` timestamps

Branch `flakefix3` (completed from paused `flakefix2`), merged cleanly with `origin/modules` (`7dd03e50d`).

## 1. Root causes and fixes

### A. `modfill-wifi` intermittent ordering flake (`NETCB available` / `NETCB lost` vs `BCAST ...`)
* **Root causes (both layers):**
  1. **Guest binder driver (`crates/acos-binder-peer/native/binder_driver.c`):** The incoming queue owner dropped ownership as soon as `g.in_head == NULL` — *before* libbinder ran the handler for the transaction just delivered. A second binder looper thread could then claim the next host delivery and execute its handler concurrently, so a `NetworkCallback` and a broadcast sent in order by `net.wifi` raced on the guest.
  2. **App callback thread (`fixtures/apps/wifi-probe/src/dev/acos/fixtures/wifiprobe/MainActivity.java`):** `ConnectivityManager.registerDefaultNetworkCallback(cb)` (1-arg) dispatches on `ConnectivityThread`, while `BroadcastReceiver` runs on the main looper — logging from two separate threads raced against the scheduler even when binder deliveries arrived in order.
* **Fixes:**
  * Added frame-only flag `TF_ACOS_ORDERED` (`0x4000_0000`, `E3G_TF_ORDERED` in C) on host-originated `F_TXN` deliveries (`crates/acos-binder-peer/src/protocol.rs`, `peer.rs`). The driver strips `E3G_TF_ORDERED` before handing `tr.flags` to libbinder.
  * For `TF_ONE_WAY | E3G_TF_ORDERED` deliveries, `binder_driver.c` holds queue ownership (`g_owner_held`, `g_owner_since` guarded strictly under `g.mu`, with `cv_own` on `CLOCK_MONOTONIC`) until the owning looper thread returns to `read` (handler finished), bounded by a `250 ms` lease (`E3G_OWNER_LEASE_MS`) so a stuck handler cannot wedge the guest. Plain sync app→app deliveries do not extend the hold.
  * Updated `wifi-probe`'s `MainActivity.java` to pass `new Handler(Looper.getMainLooper())` to `registerDefaultNetworkCallback` and `main.post(...)` to `registerScanResultsCallback`, keeping all callback logging on one looper without weakening any assertion in `scenarios/modfill-wifi.yaml`.
* **Regression tests:**
  * `crates/acos-binder-peer/native/driver_test.c` check 6b (via `cargo test -p acos-binder-peer`): proves with two binder looper threads that both `TF_ONE_WAY` and `E3G_TF_ORDERED` deliveries serialize handler completion before the next delivery runs, and that a 900 ms stuck handler loses the 250 ms lease so the next delivery proceeds.

### B. Fresh-worktree ART failure (`derive_classpath`: `/data/shim/libartshim.so` not accessible)
* **Root causes (two interacting defects):**
  1. **Path-dependent build-cache keys (`crates/acos-art/src/{build_cache,lib,app,launch}.rs`):** `linkerconfig_cache_key`, `classpath_env_cache_key`, `provision_overlays`, `build_guest`, and `build_launch_guest` hashed absolute sysroot/source paths (`file_id` / raw source `PathBuf`). A fresh worktree reaching the identical shared image through its own `.cache/art` symlink missed the shared build cache.
  2. **Strict Treble `linkerconfig` regeneration (`crates/acos-art/src/{props,lib}.rs`):** `GATENE2E` added `ro.vndk.version=34` to `Props::for_launch()` (for libcutils memfd ashmem). When a cache miss triggered `linkerconfig` regeneration, the image's `linkerconfig` read `ro.vndk.version` and switched to strict Treble mode (`dir.system`, isolated default namespace excluding `/data/shim`), causing `derive_classpath` to fail linking `LD_PRELOAD=/data/shim/libartshim.so` (captured before fix in `evidence/before-fresh-worktree-counter/counter-before.json`).
* **Fixes:**
  * Added content-only `build_cache::content_id` (size + sha256, path/mtime-independent) for sysroot binaries (`linkerconfig`, `derive_classpath`, `idmap2`) and stripped `src_dir` prefixes for guest Java source paths; bumped cache key versions (`v2`/`v3`) and `linkerconfig_stamp()` (`+legacy-v1`) so stale strict-mode trees are never reused.
  * Added `Props::for_linkerconfig()` (omitting `LINKERCONFIG_MODE_KEYS = ["ro.vndk.version"]`), written to `acos.linkerconfig.properties` and read-only bound over `/data/acos.properties` solely during host `linkerconfig` generation. Guests still receive `ro.vndk.version=34`.
* **Regression tests:**
  * `crates/acos-art/tests/flakefix2_linkerconfig.rs`: runs the image's real `linkerconfig` with `ACOS_ART_BUILD_CACHE=off` into an empty data dir and asserts legacy `[legacy]` + `namespace.default.isolated = false` (no `dir.system` / `[system]`).
  * `crates/acos-art/src/lib.rs`: `the_derived_file_cache_keys_do_not_name_the_sysroot_path`, `a_strict_mode_stamp_from_an_older_build_is_not_reused`.
  * `crates/acos-art/src/props.rs`: `the_linkerconfig_table_hides_only_the_mode_keys`.

### C. Guest `logcat.txt` constant snapshot timestamp
* **Root cause:** `logd::LogEntry` carried no capture timestamp; `acos-cli` stamped every entry in a `{"cmd":"logs"}` snapshot with the single host read time (and `merge_final_logd` used `session-exit.json`'s `utc_ms`).
* **Fixes:**
  * Added `ts_ms: u64` (`#[serde(default)]`) to `acos_art::LogEntry`, stamped at capture time in `logd::parse_frame` and serialized in both live `logs` replies and `logd-final.jsonl`.
  * Updated `crates/acos-cli/src/engine/art.rs` (`log_records_from_reply`) and `crates/acos-cli/src/verify.rs` (`merge_final_logd`) to preserve per-entry `ts_ms` (falling back to read/exit time only when `0`/absent).
* **Regression tests:**
  * `acos_art::logd::tests::frame_roundtrip` (`ts_ms > 0`).
  * `acos_cli::engine::art::tests::log_snapshot_keeps_each_entrys_own_capture_time`.
  * `acos_cli::verify::final_logd_merge_tests::merge_appends_only_the_missing_tail`.

## 2. Verification results (merged HEAD with `origin/modules` `7dd03e50d`)

### Crate test suites (`cargo test --release --offline`, `CARGO_PROFILE_RELEASE_DEBUG=0`, `CARGO_INCREMENTAL=0`)
| Crate | Result | Notes |
|---|---|---|
| `acos-binder-peer` | **62/62 PASS** (3x pre-merge + post-merge) | Includes C `driver_test.c` check 6b (ordered delivery + 250 ms lease) |
| `acos-art` | **260/260 PASS** (1 pre-existing ignored) | Includes `flakefix2_linkerconfig` (1/1), `cleanup` (9/9), `sandbox_s5` (7/7), `sandbox_s6` (5/5), `w37_build_cache` (5/5), `w39_stage_cache` (5/5). Note: `spike_net_boundary::guest_cannot_change_the_lan_drop_rules` (legacy `base` arm) cannot nest `bwrap --unshare-user` inside `pasta` under the agent harness's `NoNewPrivs: 1` / `Seccomp: 2` (reproduced with bare `pasta --config-net -- bwrap --unshare-user ...`, unrelated to FLAKEFIX; product `relay` arm passes 10/10 below). |
| `acos-cli` | **309/309 PASS** (15 pre-existing ignored) | Run with `-u TMPDIR` so `zygote_host` unit test socket paths fit `SUN_LEN` |
| `acos-netd` | **11/11 PASS** | Post-merge check |
| `acos-services` (affected) | **10/10 PASS** | `wifi_network_gate`, `w42_layer_lifetime`, `window_surface_layer` |

### ART scenario proof runs (`results/handoff/FLAKEFIX/run-batch.sh`, no `ACOS_ART_SYSROOT` override, private `ACOS_RUNTIME_DIR` + `ACOS_SESSION_SOCKET`, via `run-exclusive.sh`)
| Scenario | Pre-merge (`a72255c9a`) | Merged HEAD (`6c6e07f1f`) | Median wall | Evidence |
|---|---|---|---|---|
| `modfill-wifi` | **30/30 PASS** | **30/30 PASS** | 13.81 s | `evidence/after-merged/modfill-wifi.json`, `modfill-wifi-logcat-excerpt.txt` |
| `modfill-wifi-network-real` | **10/10 PASS** | **10/10 PASS** | 5.31 s | `evidence/after-merged/modfill-wifi-network-real.json` |
| `counter` (fresh worktree, warm cache, no override) | **1/1 PASS** | **1/1 PASS** | 3.65 s | `evidence/after-merged/counter-fresh-worktree.json` |
| `gaten-bubble-overlay` (fresh worktree, warm cache, no override) | **1/1 PASS** | **1/1 PASS** | 16.19 s | `evidence/after-merged/gaten-bubble-overlay-fresh-worktree.json` |
| `counter` (fresh worktree, `ACOS_ART_BUILD_CACHE=off`, no override) | **1/1 PASS** | **1/1 PASS** | 3.69 s | `evidence/after-merged/counter-cold-cache.json` |
| `gaten-bubble-overlay` (fresh worktree, `ACOS_ART_BUILD_CACHE=off`, no override) | **1/1 PASS** | **1/1 PASS** | 16.18 s | `evidence/after-merged/gaten-bubble-overlay-cold-cache.json` |

* Zero binder queue lease-timeout warnings across all 80 wifi runs (60 `modfill-wifi` + 20 `modfill-wifi-network-real`).
* Warm fresh-worktree `counter` logged 9/9 `build-cache HIT`s (including `linkerconfig` tree and `derive_classpath`) with `ACOS_ART_SYSROOT` unset.
* Merged `modfill-wifi` `logcat.txt` recorded 33 `wifiprobe:` lines across 24 distinct per-entry millisecond capture timestamps (`evidence/after-merged/modfill-wifi-logcat-excerpt.txt`).
* Full summary JSON: `evidence/after-merged/summary.json`. Before-fix failure capture: `evidence/before-fresh-worktree-counter/counter-before.json`.
