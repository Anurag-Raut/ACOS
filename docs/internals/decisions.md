# Active Architectural Decisions

A curated index of the architectural decisions that govern ACOS today, grouped by subsystem. For complete historical rationale, amendments, and superseded pre-ART Rust-VM ADRs (ADR-0001 through ADR-0024), see the append-only log in [`../../DECISIONS.md`](../../DECISIONS.md).

## Runtime & Execution

- **Real Google ART inside unprivileged `bwrap` (supersedes ADR-0012):** ACOS executes stock API-34 `dalvikvm64`, `framework.jar`, `libandroid_runtime`, and CPU Skia (`libhwui`) inside an unprivileged Linux user+mount namespace with `artshim.so` userspace `/dev/binder` interception (`crates/acos-art`). The Rust VM (`acos-vm`, `acos-runtime`) is frozen.
- **Boot image & production Zygote (ADR-0026):** Default boot image is `shared-fixed`, built once and cached on disk so read-only pages share cleanly across Sessions. Production Zygote runs persistent warm-fork with per-session run roots and memory limits (cutting per-session private RAM from ~37 MiB to ~23 MiB).
- **Window compositor & insets (ADR-0026):** C1 Rust software compositor (`acos-wm`) with AOSP geometry (`acos-sfgeom`) stays the default (stock `SurfaceFlinger` dropped). Default window insets mode is `report` (phone-like edge-to-edge).
- **Software GLES (ADR-0025 + GLSHIP):** Guest GLES/EGL runs in software (`angle-swiftshader` read-only `.so` bind-mounted into guest + `acos-gles` substrate). Zero RAM cost when unused, 100% parallel across Sessions, never shares the Host GPU address space.

## Services & Modules

- **Attachable service modules (ADR-0029):** System services are organized as `core` + 23 attachable modules in `ModuleRegistry::standard()` (`crates/acos-services`). Profiles are `off`, `stub`, `scripted`, `real`. Unattached known services return structured `UNSUPPORTED_ANDROID_API` tombstones (never silent fake success). See [`../MODULES.md`](../MODULES.md).
- **Multi-process & launch origin (ADR-0029 amendment, MERGEFIX2):** Secondary and lazy system apps (`permissioncontroller`, `systemui`, `webview`) fork on demand and are idle-reclaimed. `PendingLaunch.origin` distinguishes `Guest` launches (materialized by the guest's `ActivityThread`) from `Host` launches (delivered by the Session Controller for notification taps and permission dialogs) so a launch never spawns duplicate Activities.
- **ServiceManager notifications (FINDINGS / MODTAIL2–3):** `registerForNotifications` rejects undeclarable names with `EX_ILLEGAL_ARGUMENT` instead of hanging forever; `isDeclared` answers from loaded module manifests.

## Host-Shared Infrastructure

- **Real internet via Host-shared `acos-netd` (ADR-0030 + SPIKE-NET amendment):** One rootless `pasta` + relay netns per Host (~26 MiB flat across N=1..20) serves isolated per-session `netns` + `veth` interfaces and in-guest `/dev/socket/dnsproxyd`. `pasta` runs with `-T none -U none --no-splice --map-host-loopback none`; session netfilter drops RFC1918, link-local, and CGNAT (guest lacks `CAP_NET_ADMIN` and seccomp blocks `AF_NETLINK`). Host ports (e.g., Metro `8081`) require explicit opt-in.
- **WebView via Host-shared `acos-webview` (ADR-0031 + SPIKE-WV2 + WVFAIR + WVSCOPE + WVTAIL):** One pinned `chrome-headless-shell` per Host (~130–140 MiB lazy once) serves isolated per-session `BrowserContext`s behind a gate socket (guests never touch raw DevTools).
  - **Fairness (WVFAIR / WVTAIL):** Only `beginFrame` capture serializes through `crates/acos-webview/src/fair.rs` (750 ms cap while another view waits, 2 s solo, 1 coalesced pull/view, input frames preempt via `Yield`, off the connection worker pool). Engine restarts report `ENGINE_RESTARTED`, never `NO_VIEW`.
  - **Lifetime & version isolation (WVSCOPE):** Broker runs in its own transient systemd scope (`systemd-run --user --scope --collect`) so one Session's cgroup teardown never kills WebView for other Sessions. `attach:2` verifies build fingerprints and isolates mismatched builds on separate sockets (`broker-<source8>.sock`).

## Verification, Honesty & Confinement

- **Content-aware settle & placeholder window (ADR-0032 + GATEFIX):** `wait_for` and tree/text/state assertions default to `auto` (`strict` first, then `content` stability on animating screens, explicitly reporting `pixel_evidence: false`). Actions and pixel assertions remain `strict`. A timed-out settle is always `TIMEOUT`, never `pass`. `PLACEHOLDER_CONTENT` requires stability across the full `ACOS_ART_FIRST_FRAME_MS` window.
- **Fail-closed confinement (ADR-0022, [`../SECURITY.md`](../SECURITY.md)):** Landlock, seccomp, namespaces, and rlimits cannot be weakened by env vars. `AF_INET`/`AF_INET6` is unlocked only when `network` is attached.
- **Host RAM safety (ADR-0018):** Never exceed 90% Host RAM (target ≤ 85%); run builds/tests/guest executions under `tools/guarded.sh`.
- **Live view & screenshots (ADR-0027):** `acos view` is a local view-only browser hub (`127.0.0.1` + token) with zero overhead when no viewer is connected (single atomic load). Screenshots default to compressed lossless PNG (downscaled preview + lossless file path over MCP).
- **Lattice-aware benchmark gating (ADR-0028):** Fast-settle metrics sit on a ~16.4 ms frame lattice and are gated by pooled 5%-trimmed mean (`tools/bench/ab-compare.py`), while strict metrics keep median-of-p50s gating.
