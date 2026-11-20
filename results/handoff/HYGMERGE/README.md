# HYGMERGE4 — HYG4 merge into `modules` (v0.4.0 blocker)

Branch `hygmerge4n` from `hygmerge3` (`592c32f6a`, carrying `hyg4` + HYGMERGE2/3 fixes `4d2033cdb`, `48e281385`, `d41e04df0`), merged with `origin/modules` (`80f8bff79`) at `c52730cf3`.

**Status: PASS (all acceptance gates green except the documented runner limitation in §2; audit 0; tree clean).**

## Environment & Prerequisites

- `source tools/agent-env.sh` (`tools/link-worktree-prereqs.sh` linked `.cache/{boot/android-34,art,gaten,qemu-user,bionic-arm64,flutter-1,corpus}` and `libexec/pasta -> /home/Qui/Projects/acos/.cache/pasta-pinned/pasta`, and built `.cache/arm64-guard/libacos_seccomp_guard.so`).
- Fixtures built fresh in the worktree (`tools/build-apps.sh`, `tools/build-dogfood-apps.sh`, `tools/build-negative-apps.sh`, with prebuilt large Flutter/RN fixtures hardlink-seeded from the shared build tree without mutating shared inodes).
- Every ART / crate gate run used private `ACOS_RUNTIME_DIR` + `ACOS_SESSION_SOCKET` via `tools/art-runtime/test-crates.sh` (`run-exclusive.sh` + `guarded.sh`, shared slots, no `--exclusive` timing claims).
- `env -u ACOS_HOME acos doctor` (private runtime dir, nothing set by hand for pasta):
  - `[ok  ] pasta              2026_10_02.cba3570 (<WT>/libexec/pasta [runtime_root])`
  - `[ok  ] gles               angle-swiftshader (default; installed at <WT>/.cache/art/sysroot/gles/angle-swiftshader)`
  - `acos doctor: healthy (0 failure(s))`

## 1. `guest_cannot_overwrite_boot_art_or_the_classpath` (`acos-art`)

**Verdict: PASS** (`test guest_cannot_overwrite_boot_art_or_the_classpath ... ok`, `tests/cleanup.rs`: `9 passed; 0 failed`).

### Code-level reasoning (no protection weakened)

- **Why the host hash of `/data/acos.properties` changed in HYG4:**
  `seed_data(&scratch)` copies the warm `.cache/art/data` directory, whose cached `acos.properties` predated current `Props::for_launch()` defaults (e.g., `ro.vndk.version=34` and software-GL properties). When `acos-art run --data <scratch/data>` starts on the host, `ArtRoot::ensure_data()` (`crates/acos-art/src/lib.rs:452`) legitimately refreshes `<data>/acos.properties` via `Props::for_launch()?.write(...)` (`publish_atomic`, `crates/acos-art/src/lib.rs:236-274`) *before* spawning the sandbox.
- **Guest protection is real and intact:**
  `Sandbox::command` binds `/data/acos.properties` read-only (`--ro-bind`, verified by unit test at `crates/acos-art/src/lib.rs:2802-2812`), and in the canary run `Canary.java` reads each protected file (`READ <path>`) and attempts to open it for append (`FileOutputStream(p, true)`), asserting `DENIED <path>` and `!WROTE <path>`. Even in the earlier HYG4 failure log, all guest assertions (`READ`, `DENIED`, `!WROTE`) already passed for `/data/acos.properties` — only the host `before`/`after` md5 comparison tripped on the launcher's own host-side regeneration.
- **Fix already landed on `origin/modules` (`f4c6b9978`, merged at `c52730cf3`):**
  `crates/acos-art/tests/cleanup.rs:887-889` calls `acos_art::Props::for_launch()?.write(&data.join("acos.properties"))` *before* taking the `before` md5 snapshot. `publish_atomic` then observes identical bytes during `ensure_data()`, while `/data/acos.properties` remains in the protected set for both guest `READ`/`DENIED`/`!WROTE` checks and host `before == after` md5 checks alongside `/data/system/environ/classpath`, `/data/shim/libartshim.so`, `/data/app/guest/classes.dex`, `/linkerconfig/ld.config.txt` (and `/data/img/x86_64/boot.art` when a boot image is staged).

## 2. `spike_net_boundary` & Product `relay` Arm

### Product `relay` arm (nothing set by hand) — **PASS**

With `ACOS_HOME`, `ACOS_NET_PASTA`, `ACOS_SPIKE_NET_PASTA`, and `ACOS_NET_ARM` all **unset** and private `ACOS_RUNTIME_DIR` + `ACOS_SESSION_SOCKET`:

- `acos-netd pasta`: `<WT>/libexec/pasta (2026_10_02.cba3570, runtime_root)`
- `acos-netd` + `acos-netd-ctl` crate gate (`tools/art-runtime/test-crates.sh`): `21 passed; 0 failed; 0 ignored; SKIP COUNT: 0`.
- Product `network:real` (relay arm) ART verification scenarios (`acos verify <scenario> --engine art --json`, through `run-exclusive.sh` + `guarded.sh`):
  - `scenarios/net-network-real.yaml`: `rc=0 status=pass wall_ms=1303 err=None`
  - `scenarios/net-https-proof.yaml`: `rc=0 status=pass wall_ms=1657 err=None`
  - `scenarios/net-network-offline.yaml`: `rc=0 status=pass wall_ms=3706 err=None`
  - `scenarios/modfill-wifi-network-real.yaml`: `rc=0 status=pass wall_ms=9318 err=None`

### Legacy `base` arm (`tests/spike_net_boundary.rs`) — **Documented runner limitation (test unchanged)**

- `boundary_failure_never_starts_the_guest`: **PASS** (`ok`).
- `guest_cannot_change_the_lan_drop_rules`: **FAIL (runner limitation only)**.
  - Runner `/proc/self/status`: `NoNewPrivs: 1`, `Seccomp: 2` (`Seccomp_filters: 1`).
  - Under `NoNewPrivs: 1` + `Seccomp: 2`, the legacy `base` arm (`ACOS_NET_ARM=base`) cannot nest `bwrap --unshare-user` inside `pasta` (same limitation reproduced in `FLAKEFIX3`, `results/handoff/FLAKEFIX/README.md`).
  - Exact error from `tests/spike_net_boundary.rs` shim stderr:
    ```text
    acos-net: shim netns=net:[4026533384] userns=user:[4026533378] cap-eff=0000000000201400
    acos-net: rules=installed chain=-N acos-net-drop;-A acos-net-drop -d 192.168.0.0/16 -j DROP;-A acos-net-drop -d 10.0.0.0/8 -j DROP;-A acos-net-drop -d 172.16.0.0/12 -j DROP;-A acos-net-drop -d 169.254.0.0/16 -j DROP;-A acos-net-drop -d 100.64.0.0/10 -j DROP;-P OUTPUT ACCEPT;-A OUTPUT -j acos-net-drop;
    bwrap: setting up uid map: Operation not permitted
    acos-net: rules=after chain=-N acos-net-drop;-A acos-net-drop -d 192.168.0.0/16 -j DROP;-A acos-net-drop -d 10.0.0.0/8 -j DROP;-A acos-net-drop -d 172.16.0.0/12 -j DROP;-A acos-net-drop -d 169.254.0.0/16 -j DROP;-A acos-net-drop -d 100.64.0.0/10 -j DROP;-P OUTPUT ACCEPT;-A OUTPUT -j acos-net-drop; guest-exit=1
    ```
  - `crates/acos-art/tests/spike_net_boundary.rs` was **not** modified or weakened.

## 3. Silent-Skip Audit & Per-Crate Gates

### Silent-skip audit — **PASS**

```text
tools/audit-silent-skips.sh
SILENT-SKIP SITES: 0
```

### Merge fix applied in HYGMERGE4

- `7f9d1cb9c` (`crates/acos-runtime/tests/services_t38.rs`): after merging `origin/modules`, `MODFILL` (`ab401c966`) wired `IClipboard.setPrimaryClip` directly through `ClipboardService::raw_transact` (`read_clip_shape`) on the VM-free `ServiceHost`. Passing `null_obj()` (`present == 0`) now returns `SERVICE_BAD_REQUEST` (`"clip is null"`) instead of the pre-MODFILL stub `UNSUPPORTED_ANDROID_API`. Updated `clipboard_set_primary_clip_without_a_vm_is_a_structured_error` to assert `codes::SERVICE_BAD_REQUEST` and `"clip is null"`, then re-ran the full `acos-runtime` gate (121 passed, 0 failed).

### All 12 per-crate gates (`tools/art-runtime/test-crates.sh`, `--no-fail-fast`)

Documented opt-outs excluded by `test-crates.sh` from `tools/test-prereq-optouts.txt`: 8 heavy Chromium `ACOS_WEBVIEW_TEST=1` tests in `acos-webview` and 1 `ACOS_S8_LIVE_HOSTILE=1` test in `acos-cli` (9 total).

| # | Crate | Passed | Failed | Ignored (pre-existing) | Filtered (opt-outs) | SKIP COUNT | Verdict |
|---|---|---:|---:|---:|---:|---:|---|
| 1 | `acos-art` | 261 | 1* | 1 | 0 | 0 | PASS* (*only `spike_net_boundary::guest_cannot_change_the_lan_drop_rules` runner limitation, §2) |
| 2 | `acos-cli` | 313 | 0 | 15 | 1 | 0 | PASS |
| 3 | `acos-dex` | 50 | 0 | 0 | 0 | 0 | PASS |
| 4 | `acos-framework-natives` | 41 | 0 | 0 | 0 | 0 | PASS |
| 5 | `acos-jni` | 71 | 0 | 0 | 0 | 0 | PASS |
| 6 | `acos-mcp` | 169 | 0 | 0 | 0 | 0 | PASS |
| 7 | `acos-protocol` | 132 | 0 | 0 | 0 | 0 | PASS |
| 8 | `acos-render` | 128 | 0 | 0 | 0 | 0 | PASS |
| 9 | `acos-runtime` | 121 | 0 | 10 | 0 | 0 | PASS (re-run after `7f9d1cb9c`) |
| 10 | `acos-services` | 741 | 0 | 2 | 0 | 0 | PASS |
| 11 | `acos-vm` | 217 | 0 | 0 | 0 | 0 | PASS |
| 12 | `acos-webview` | 40 | 0 | 0 | 8 | 0 | PASS |
| **Total (12 gates)** | | **2,284** | **1*** | **28** | **9** | **0** | **PASS*** |

Additional completeness checks run through `tools/art-runtime/test-crates.sh`:
- `acos-netd` + `acos-netd-ctl`: 21 passed, 0 failed, SKIP COUNT 0.
- 6 HYG3 crates (`acos-apk`, `acos-arm64`, `acos-native-loader`, `acos-sandbox`, `acos-test-support`, `acos-verify`): 314 passed, 0 failed (only `acos-test-support`'s own unit test of `ACOS_ALLOW_ART_SKIP` emits its expected `SKIP SUMMARY` line).

### Tree cleanliness — **PASS**

`tools/check-tree-clean.sh`: `check-tree-clean: clean` (0 tracked changes from test runs).

## Commits on `hygmerge4n` (since `origin/modules` `80f8bff79`)

- `hygmerge3` (`592c32f6a`) — `hyg4` + HYGMERGE2/3 fixes (`4d2033cdb`, `48e281385`, `d41e04df0`)
- `c52730cf3` — `Merge origin/modules into hygmerge4n`
- `7f9d1cb9c` — `HYGMERGE4: update services_t38 null-ClipData assertion for MODFILL wire-path clipboard`
- (this commit) — `HYGMERGE4: evidence README`

## Resource Usage & Cleanup

- Peak host RAM observed: 15 GiB used / 60 GiB total (45 GiB available).
- Disk on `/`: 81%–82% during runs (well under the 85% stop threshold); `.cache/hygmerge4n-target` and `fixtures/build` copy removed at completion.
