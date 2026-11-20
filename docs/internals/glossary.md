# ACOS Glossary

Canonical vocabulary for ACOS architecture, code, and documentation. Use these terms consistently so agents and contributors never confuse the Linux machine with per-session processes.

| Term | Canonical Meaning | Avoid / Deprecated |
|---|---|---|
| **Host** | The single Linux machine running ACOS (**always 1 per machine**). Runs the Linux kernel, holds the shared read-only AOSP page pool, and hosts Host-shared daemons (`acos-netd`, `acos-webview`, optional Host Zygote). | Do **not** call the per-session Rust daemon "the host" or "Rust host". |
| **Host-Shared Service** | A singleton daemon or read-only resource allocated **once per Host** and safely shared across all concurrent Sessions (e.g., `acos-netd`, `acos-webview`, read-only AOSP page pool, `angle-swiftshader` `.so`). | — |
| **Session** | One isolated virtual Android device on the Host. Owns private `/data`, `bwrap` namespaces, seccomp/Landlock confinement, one Session Controller, one primary Guest App Process, optional per-session `netns`/`veth`, and optional Chromium `BrowserContext`. Many Sessions run in parallel on one Host. | "emulator", "VM" (ACOS does not boot a VM) |
| **Session Controller** | The per-session Rust control plane (`acos-services` + `acos-wm` + `acos-binder-peer` + `acos-art` launcher) that answers Binder transactions, composites windows, enforces settle rules, and serves CLI/MCP requests for one Session. | ~~"Rust host"~~, ~~"host daemon"~~ |
| **Guest App Process** | One Android PID (`dalvikvm64` + `framework.jar` + app DEX/native `.so`) executing inside a Session's `bwrap` sandbox. | — |
| **Lazy System App** | A real Android system APK (`com.android.permissioncontroller`, `com.android.systemui`, `com.android.webview`) whose process is forked inside a Session **only when triggered** (0 MiB RAM until used). | — |
| **Binder Seam (`artshim.so`)** | The `LD_PRELOAD` C shim (`crates/acos-art/shim/artshim.c`) inside the guest process that intercepts `/dev/binder` (`open`/`ioctl`/`mmap`) in userspace and tunnels `BC_*`/`BR_*` transactions over a Unix socket to the Session Controller. | — |
| **C1 Compositor** | ACOS's lightweight Rust software window compositor (`crates/acos-wm` + `crates/acos-sfgeom`) that replaces `SurfaceFlinger`, manages AOSP window z-order layers (`docs/WINDOW_LAYERS.md`), computes insets (`report` default), and captures frames. | — |
| **Module** | An attachable bundle of Android system pieces registered in `ModuleRegistry::standard()` (`crates/acos-services`, ADR-0029). Always-on services form the `core` module; 23 optional modules (`network`, `webview`, `accessibility`, `system.permissions`, `sensors`, etc.) attach at launch (`at-launch`) or live (`hot`). | — |
| **Profile** | The implementation level of an attached module: `off`, `stub`, `scripted`, or `real`. Verdicts always record every attached module's profile. | — |
| **Settle** | The proof that a Session has reached a stable state after an action or wait (ADR-0032): `strict` (pixel/event quiet), `content` (UI-tree signature unchanged for ~300 ms on animating screens; `pixel_evidence: false`), or `fast` (opt-in navigation mode; never backs a pass). | — |
| **Fidelity** | Mandatory metadata on every verdict stating what actually executed (e.g., `art-reference`, host Blink WebView, `stub`/`scripted` modules) so agents never mistake simulated behavior for device equivalence. | — |
| **Verdict** | The machine-readable JSON outcome (`status`, `ok`, `backend`, `fidelity`, `isolation`, `settle`, `modules`, `artifacts`, `resources`) returned by `acos verify` and MCP tools. | — |
| **Legacy Rust VM** | The frozen pre-ART Rust DEX interpreter track (`acos-vm`, `acos-runtime`, `acos-render`, `acos-jni`). Kept compiling as a reference/fallback; no new product features are built on it. | — |
