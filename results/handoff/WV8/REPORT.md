# WV8 — WebView: module + integrate

Branch `wv8` from `wv7` @ `52af3ead`; `modules` (`a97d71b1`, PIFILLIN, BINDERREF2,
A11YNATIVE2, NET6) merged in after the merge regression was fixed.

Status: **DONE** — the `webview` module exists and the scenario passes on real ART
with the module attached by `services:` alone (19/19 steps, `verify: pass`), both
viewport arms are measured, and the WebView region of the ACOS capture holds the
page (pixel check below).

## 1. Commits

| sha | one line |
|---|---|
| `6ccd867f` | one PMS factory, one runtime-permission table (the wv7 merge regression) |
| `94ef9351` | merge `modules` into the WebView branch (resolutions, both sides kept) |
| `e2003a64` | the `webview` module (ADR-0029) — provider package + lazy broker at launch |
| `d08e42e6` | the viewport arms (`useWideViewPort` / `loadWithOverviewMode`), measured |
| `fee249de` | `webview-probe` passes on ART with the module attached, both viewport arms |

## 2. The merge regression (fixed first, per the brief)

`both_check_permission_paths_read_the_one_store` failed with PMS `-1` / ATMS `0`
after the `system.permissions` grant. **Two causes**, both from wv7's own merge:

1. `registry.rs` registered **two** `package`/`IPackageManager` factories: MODNOTIF2's
   (the one wired to the shared `PermissionStore`) and, later, the ADR-0031
   `webviewupdate` peer's own `PmsState::hello_views()`. The later factory won, so
   the serving PMS had no runtime table at all. There is now **one** PMS factory
   (`pms_shared` + the one `permissions` store, `set_install_time` on first use),
   and `webviewupdate` is published from that same state — never as a second
   factory.
2. `pms.rs`'s `checkPermission` had taken MPROC1's per-package answer and dropped
   the table lookup. `PmsState::check_permission_for(package, permission)` now
   reads the one runtime table first (it describes the session's app under test,
   and a package name the session has no record of cannot have a second table) and
   falls back to the **named** package's manifest grants, so MPROC1's multi-package
   behaviour is kept and a runtime grant cannot leak to a second installed package.

`cargo test -p acos-services --test modnotif2_permissions`: 4/4 pass (on the wv7
tree `both_check_permission_paths_read_the_one_store` failed; the other three
already passed, so the one behaviour fixed is visible as the one test that
flipped).

## 3. The `webview` module (ADR-0029, modelled on `network`)

`crates/acos-services/src/modules/webview.rs`:

* **profiles**: `off` (default, unattached) is Android's own *no provider* path —
  no `com.android.webview` package, `hasSystemFeature("android.software.webview")`
  false, `WebViewFactory` throwing, no socket; `real` is the provider package plus
  this session's gate socket, with the one shared host Chromium still started by
  the first guest connect. **No `stub`/`scripted`**: a scripted page would be a
  fake success (the module's own message says so).
* **attach**: `at-launch` (the provider package must exist before the guest boots
  and the socket path is baked into the sandbox). A live attach is refused with
  the `acos_launch` call, like `network`.
* **manifest**: `system_apps: ["com.android.webview"]`, `requires_host_tools:
  ["acos-webview", "chrome-headless-shell"]`, `security: host-tool`, no
  `services` (see below), budget `ram_mib: 4` / `startup_ms: 11` from WV6's
  measurements, and the fixed ~140 MiB per host reported in `status`/`details`
  because it is not per session.
* **why no `ServiceManager` name**: `webviewupdate` and `hasSystemFeature` are
  answered by `core` **from the package table**, which is exactly the table the
  provider install writes. A module re-registering `webviewupdate` would be a
  second source of truth — the very regression fixed in §2.
* **plumbing**: `module::system_apks(specs)` resolves a manifest's `system_apps`
  to host APKs, so `acos-art`'s launch stages the provider from `services:` and not
  from a hand-written `extra_apks:` line;
  `modules::webview::broker_requested(specs)` is what makes the launcher bind the
  gate (module `off` ⇒ no socket at all); `ServiceHost::webview_gate` publishes the
  endpoints the launcher bound, and the module's `status` control op answers from
  them (one control path: `acos_service_ctl` over MCP and the CLI reach the same
  `Module::control` arm; the manifest's ops are the advertised set).
* **kit/conformance**: `the_webview_module_passes_the_conformance_kit` (manifest,
  attach/detach twice back to baseline, honest report, negative, budget) and
  `crates/acos-services/tests/webview_module.rs` (provider APK contribution, gate
  request, attach/refusals, `status` from host facts, every advertised op
  dispatched + unknown op refused + detached refusal).

Verdict row (from the passing run, `evidence/pass/verdict.json`):

```json
{"id": "webview", "profile": "real", "fidelity": "real", "state": "attached",
 "attach": "at-launch",
 "reason": "real WebView: provider package com.android.webview installed from …/webview-provider.apk, one shared host Chromium behind the session's gate socket (started on the first connect)",
 "details": {"provider_package": "com.android.webview", "frame_resolution": "engine CSS pixels", "fixed_host_ram_mib": 140}}
```

## 4. Wide viewport (decision, implemented, measured)

Recorded in `DECISIONS.md` (ADR-0031 note) and `results/handoff/WVB/WIRE.md`:

| switch | meaning | measured |
|---|---|---|
| `useWideViewPort(false)` (default) | layout viewport = the View's width in CSS px; `<meta viewport>` ignored (Chromium `mobile:false`) | `A viewport -> "343x1.00"` (900 device px / 2.625) |
| `useWideViewPort(true)` | honour `<meta viewport>`; without one Chromium's Android default 980 CSS px (`mobile:true`) | `B viewport#0 -> "640x0.54"`, `B viewport#1 -> "980x0.35"` |
| `loadWithOverviewMode(true)` | visual scale `cssWidth/layoutWidth` (`Emulation.setPageScaleFactor`); layout unchanged | the `0.54` / `0.35` above |
| back to the default | 343 CSS px, scale 1.00 | `B viewport#2 -> "343x1.00"` |

Both arms are measured twice: against the engine directly
(`crates/acos-webview/tests/wv8_viewport.rs`, pages with and without a meta tag,
`document.documentElement.clientWidth` + `visualViewport.scale`) and through the
real guest provider in the ART scenario (pages with/without a meta tag, the switch
set before the first load — replayed right after `view.create` — and changed again
at runtime).

A defect the measurement found and fixed at the cause: a page scale left over from
the previous document became the **next** document's initial scale, so the 980 px
page laid out at 343 under the old 0.35 scale. The broker now resets the scale
before a navigation and re-fits after it; both new-document cases are asserted in
the engine test.

## 5. Frames stay at the engine's CSS resolution (decision)

Unchanged from WV7 and now recorded: frames are the engine's CSS pixels, the wire
reports the frame's real size, and the guest scales into the View. This pinned
`chrome-headless-shell` cannot capture at `deviceScaleFactor` under
`--enable-begin-frame-control` (WV7 `evidence/engine-capture-facts.txt`).

The **"a frame that arrives while nothing else draws is painted only on the next
draw"** defect is the LOOPERWAKE track's and is **not** worked around here. The
scenario keeps an explicit tolerant step (the provider's `frame decoded` line with
`timeout_ms: 30000`) and its comment says why the captures come after the steps
that do force a traversal. A *second*, related symptom was measured while wiring
the scenario and is reported, not hidden: two engine targets loading pages at the
same time serialise their frame pulls on the broker and the guest's UI thread
blocks in `onDraw` (`Choreographer: Skipped 119 frames`), which delayed the tap's
ack to 2.0 s and lost the click. The fixture therefore runs the viewport arms
**after** the input steps (started from the page's own `acosBridge.report("tapped")`),
and the scenario says so.

## 6. Acceptance

| item | result | evidence |
|---|---|---|
| `cargo test -p acos-services` | **PASS** (153 lib + every integration binary, incl. `modnotif2_permissions` 4/4 and the new `webview_module` 6/6 + kit row) | command in §8 |
| `cargo test -p acos-art` | **PASS** (169 lib + 11 integration binaries) | command in §8 |
| `cargo test -p acos-webview` | **PASS** (17 lib, `gate_engine`, `gate_lazy` 3, `gate_cost`, `engine_gate`, `engine_bare`, `wv8_viewport` 2 — the engine tests opted in with `ACOS_WEBVIEW_TEST=1`) | command in §8 |
| `git merge modules`, both sides kept | **DONE** — `94ef9351` (`SessionMain` native lists, `IntentFacts` `mime_type`+`identifier`, `AudioAttributes` + `Location` codecs beside REL3P2's, extra-APK key/annotate call sites) | commit |
| wv7's broken `acos-runtime` test fixed on the way | `parcelable_java.rs::package_info_arrays_value` was truncated by wv7's own merge (unclosed delimiter, the file did not compile); restored whole | commit `94ef9351` |
| `webview-probe` on ART with the module attached by `services:` | **PASS**, 19/19 steps, `verify: pass` | `evidence/pass/verdict.json`, `evidence/pass/steps/`, `evidence/pass/logcat.txt` |
| pixel check: the WebView region holds the page | **PASS** — region `x0 y420 w900 h560`: 475 colours, 7 158 dark glyph pixels, 33 037 px changed between the first-frame and post-tap captures (WV6 measured 95.9 % dark / 2 colours for the black-region defect) | `evidence/pixels.json`, `evidence/region-A-first.png`, `evidence/region-A-tapped.png`, `evidence/pass/acos-tapped-frame.png` |
| viewport arms through the guest | **PASS** — `A viewport -> "343x1.00"`, `B viewport#0 -> "640x0.54"`, `B viewport#1 -> "980x0.35"`, `B viewport#2 -> "343x1.00"` | `evidence/pass/logcat.txt` |
| no-engine arm stays honest | **PASS (expected fail)** — module `real` + `ACOS_WEBVIEW_GATE=0`: `WebView FAILED: IllegalStateException … ENGINE_UNAVAILABLE … /run/acos/webview.sock: No such file or directory`, `ASSERTION_FAILED` at "WebView created:" | `evidence/no-engine/verdict.json`, `evidence/no-engine/logcat.txt`, `results/handoff/WV8/webview-probe-no-engine.yaml` |
| module `off` is Android's no-provider path | **PASS** (by construction and by the module test: no provider package staged, no gate bound; `webviewupdate`/`hasSystemFeature` answer from the package table) | `webview_module.rs`, `only_the_real_profile_asks_for_the_session_gate` |

## 7. What remains / orchestrator decisions

1. **LOOPERWAKE**: the deferred paint (and, it now appears, the guest UI thread
   blocking in `onDraw` while the broker serialises frames across targets) is the
   same family. The measured 2 s input stall with two loading targets is written
   down in the scenario; it is the smallest next thing to fix there.
2. The provider APK is a **fixture** built by `tools/build-apps.sh`
   (`ACOS_APP_ONLY="webview-provider"`), not a shipped system app. A release needs
   it produced by the image build; the module's `status` names the path it used, so
   a wrong build shows up in the reply.
3. Everything else in this track is done: wire v2, the module, the viewport arms,
   the frame decision and the honest negative arms.

## 8. Commands

```sh
# acceptance (all under the shared lock + guard)
ACOS_VIEW_TEST_DIR=$PWD/.cache/lv run-exclusive.sh --task WV8-tests -- \
  guarded.sh --mem 14G --time 2400 -- cargo test --offline -p acos-art -p acos-webview
  # (add ACOS_WEBVIEW_TEST=1 ACOS_WEBVIEW_CACHE=…/webview to start the engine)
ACOS_VIEW_TEST_DIR=$PWD/.cache/lv guarded.sh --mem 12G --time 1800 -- \
  cargo test --offline -p acos-services

# the scenario (module attached by `services:`)
ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts \
ACOS_RUNTIME_DIR=$PWD/.cache/wv8-run ACOS_SESSION_SOCKET=$PWD/.cache/wv8-run/session.sock \
ACOS_VIEW_TEST_DIR=$PWD/.cache/lv ACOS_WEBVIEW_CACHE=/home/Qui/Projects/acos/.cache/webview \
ACOS_ART_BIN=$PWD/.cache/wv8-target/release/acos-art \
run-exclusive.sh --task WV8 -- guarded.sh --mem 10G --time 900 -- \
  ./.cache/wv8-target/release/acos verify scenarios/webview-probe.yaml --engine art \
  --artifacts .cache/wv8-run/ev

# the negative arm
ACOS_WEBVIEW_GATE=0 … acos verify results/handoff/WV8/webview-probe-no-engine.yaml …
```

## 9. Measurements / host

Peak RAM observed ~16 GiB used of 60 (builds + one ART run); disk 72 → 75 % used
at the worst point (≈230 GiB free). Cleaned up: `.cache/wv8-target` (14 GiB), the
`.cache/wv8-run` scratch, the `.cache/lv` test dir and the `fixtures/build` copy;
the shared broker was stopped and its stale endpoint removed (`ps -eo pid,args`
shows no `acos-webview`/`chrome-headless-shell` process, `ss -lx` no `acos-wv`
socket). Left alone: the shared engine cache
`/home/Qui/Projects/acos/.cache/webview` (used by every WebView run, never a
per-track copy).
