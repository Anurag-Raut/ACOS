# METRO — the React Native dev loop on ACOS (v0.5.0)

**Status: METRO1 landed the network leg; METRO2c root-caused the reload leg (RN legacy-arch race + one ACOS pool-leak bug, fixed) and proved Fast Refresh as the deterministic path — `results/handoff/METRO2/REPORT.md`. METRO3 wired the scenario dev loop; METRO4 (redbox as data), METRO5 (dev menu / reload without a shake, `debug_http_host`, `adb reverse` equivalent) and METRO6 (symbolication) are wired with unit + mock-Metro tests; `scenarios/metro-redbox.yaml` passes on live ART with RN 0.81.4 (outcomes below).**
Owner context: `docs/PLAN_2026-11-16.md` §3.3 (network module, host-port opt-in),
ADR-0030 (+ amendments 1–3) and `docs/OWNER_DIRECTION_2026-11-18.md`
(~100 % compatibility, beat the stock emulator, attachable building blocks,
parallel sessions).

The point of this track is not "an RN app can make HTTP requests". It is that an
agent can **develop an app the way a developer does**: edit JS, reload, read the
error, set a breakpoint — and that every one of those steps has an
*agent-readable* verdict instead of a screenshot an agent has to squint at.

That is why the plan is broken into eight steps, each with its own acceptance
test and each independently useful. METRO1 is the first one and the only one
that needs the network module; the rest are runtime/dev-server work.

## What React Native actually does (the facts METRO1 pinned down)

Sources: the pinned `react-native@0.81.4` npm package (`.cache/rn-0.81.4/rnjs`)
and Metro 0.83.8 / `@react-native-community/cli-server-api` 20.0.0 in the same
cache. Every claim below is from that source, not from memory.

| what | where it comes from |
|---|---|
| where Metro is | `PackagerConnectionSettings.debugServerHost`: the `debug_http_host` key of the app's **default SharedPreferences**; else `AndroidInfoHelpers.getServerHost()` → `metro.host` system property → Genymotion (`Build.FINGERPRINT` contains `vbox`) → **stock emulator** (`Build.FINGERPRINT` starts with `google/sdk_gphone`) = `10.0.2.2:8081` → else `localhost:8081` |
| ACOS's answer | `crates/acos-vm/src/natives/os_props.rs` derives `ro.build.fingerprint` = `google/sdk_gphone64_x86_64/emu64xa:14/…`, so RN's default is **10.0.2.2:8081** — an address no ACOS session routes to (the session network is `10.163.N.0/30`, gateway `10.163.N.1`) |
| bundle URL | `DevServerHelper.getDevServerBundleURL`: `http://<host>/index.bundle?platform=android&dev=true&minify=false&app=<name>&modulesOnly=false&runModule=true` |
| readiness check | `PackagerStatusCheck` → `GET /status` (expects `packager-status:running`) |
| reload / dev menu | `DevSupportManagerBase.reload()` opens a `JSPackagerClient` websocket to `ws://<host>/message` (protocol 2) and registers a dynamic receiver for `<package>.RELOAD_APP_ACTION`. The CLI's `r` key is `POST /reload` on the dev server, which broadcasts `{"version":2,"method":"reload"}` to every `/message` client (`cli-server-api/build/index.js`, `websocket/createMessageSocketEndpoint.js`) |
| fast refresh | `ws://<host>/hot` (`metro/src/index.flow.js`, `HmrServer`); the client side is `DevServerHelper`'s HMR client |
| redbox | `DevSupportManagerBase.showNewJSError` → a `RedBoxDialog` whose content is a JSON payload (`{message, stack, name, componentStack, id}`) plus the log; the same payload is what `LogBox` reads |
| source maps | `GET /index.map?platform=android&dev=true…` and `POST /symbolicate` on the dev server |
| debugger | Hermes exposes CDP over `DevServerHelper`'s inspector URL (`/inspector/device`, `/json/list`), i.e. what `chrome://inspect` uses |

## The eight steps

### METRO1 — bundle fetch over the network module (**this step**)
A debug RN 0.81 app in an ACOS session fetches its bundle from a Metro dev
server on the host and renders it.

- **Network**: `network:real` (ADR-0030 relay), host loopback port 8081 opted in
  **before the guest exists** — a debug app fetches during `Application.onCreate`,
  so a post-launch opt-in is always too late. `acos-netd hostport add 8081`
  (relay-level) is now applied to every session at attach
  (`apply_relay_host_ports`), which also keeps NET3's per-session form
  (`hostport add 8081 --session acos-<id>`).
- **App**: `tools/build/build-rn-hello.sh debug` → `fixtures/build/apps/rn-debug.apk`:
  no embedded `assets/index.android.bundle`, `getUseDeveloperSupport() == true`,
  and `debug_http_host = localhost:8081` (the `adb reverse` shape: `acos-netd`'s
  per-session forwarder binds `127.0.0.1:8081` inside the session netns).
- **Acceptance**: `scenarios`-style driver `tools/metro/metro1-dev-loop.sh` —
  launch, assert the bundle's text; edit a JS file; reload; assert the new text.
- **Status**: bundle fetch **PASS** (`results/handoff/METRO1/`), reload **FAIL**
  with the cause located (see §"METRO1 result" below).

### METRO2 — reload that actually replaces the screen
The reload half of METRO1. Today the reload *runs* (packager command arrives, a
new JS context starts, the new bundle is fetched and executed: the guest log
shows `CatalystInstanceImpl.destroy() end` and a second
`The app is running using the Legacy Architecture` line) but the mounted view is
stale: `NativeViewHierarchyManager` logs `Trying to add a root view with an
explicit id (11) already set`, the tree doubles (8 → 16 nodes) and both copies
still show the pre-edit text.

- Scope: `ReactRootView` re-attach on reload (add/remove root view, the tag
  bookkeeping in `NativeViewHierarchyManager`, and the window layer's old view
  on the same `ViewRootImpl`) **and** the thread hand-off that follows it: the
  guest log's `Tried to enqueue runnable on already finished thread: 'js…'
  dropping Runnable.` says the new context's mount work is dropped instead of
  running on the new JS/UI thread. This is a runtime fidelity bug, not a Metro
  bug: the same app on a stock emulator replaces the text.
- Acceptance: one session, one process, `assert_text` sees exactly one copy of
  the new string and `assert_not_visible`/tree assertions see **no** copy of the
  old one; a regression scenario pins the single-root invariant (a reload may
  not grow the tree).
- Verdict data: `{reload: {delivered: true, context: "new", first_frame_after_ms,
  tree_nodes_before/after, stale_roots: 0}}` — a reload that leaves a stale root
  is a named failure, never a pass.

#### METRO2c outcome (2026-11-18)

The "stale tree" had three causes, only one of them in ACOS: (a) the driver reloaded
before Metro had rebuilt (fixed: wait until the dev server serves the new text);
(b) RN 0.81's **legacy-architecture** reload races the old tree's JS unmount against
the new context's `addRootView` on the same `ReactRootView` — lost by a few ms on
ACOS because context creation is fast (measured, §2 of the report), so the tree
doubles; (c) a real ACOS bug, the 16-entry layer-handle pool was never refilled, so
the guest aborted on the 4th reload (fixed, `surface_client.rs`). RN's recommended
path for a component edit, **Fast Refresh over `/hot`**, needs no context restart and
passes 5/5 (`tools/metro/metro2-verdict.py`). An explicit full reload stays
best-effort on the legacy arch and is reported with `stale_roots`/`explicit_id_errors`.

### METRO3 — fast refresh (HMR) over the `/hot` websocket
Same wiring as METRO2 but the incremental path: an edit that only touches a
component body arrives as an HMR update and does not reset state.

- Scope: the `/hot` websocket through the host-port forwarder (it is the same
  gated path, so no new network work), `HmrClient`'s apply-update call, and
  Metro's delta bundling in dev mode.
- Acceptance: edit a leaf component → new text with **state preserved** (the
  counter keeps its value); edit a hook signature → full reload path (METRO2)
  and the verdict says `hmr: {applied: false, reason: "signature"}`.
- Not a pass: an update that only reaches the JS context but not the view tree
  (that is METRO2's symptom, and it must be reported as `unsupported`, not
  `pass`).

#### METRO3b outcome (2026-11-22)

Evidence: `results/handoff/METRO3/README.md`.

- Scenario step `dev_server` (`start` / `edit` / `wait_serves` / `fast_refresh`
  / `reload` / `stop`) and `assert_tree` (`record` / `not_grown_since`) are
  wired in `acos-protocol`, `acos-verify`, `acos-cli`, and documented in
  `crates/acos-protocol/schema/scenario.json` + `docs/AGENT_GUIDE.md`.
- `DeferredArtSession` defers ART guest spawn until `launch` when a scenario
  starts `dev_server` first, so `acos-netd`'s host-port forwarder and Metro are
  live before `Application.onCreate` fetches the initial bundle.
- `scenarios/metro-reload.yaml` (Fast Refresh default) passes **5/5** on real
  ART (`29 → 18` nodes, single root, zero orphan `node` processes);
  `scenarios/negative/metro-reload-wrong-text.yaml` **FAILS** cleanly at
  `assert_text` (`ELEMENT_NOT_FOUND`) with automatic Metro teardown;
  `scenarios/metro-reload-full.yaml` stays marked **best-effort** and excluded
  from gate matrices per Owner Decision (A) (passed 5/5 in METRO3b's 5-run
  sample with strict assertions unchanged).

### METRO4 — the redbox as agent-readable data
An error overlay an agent can assert on, not a picture of a red screen.

- Scope: `DevSupportManagerBase.showNewJSError`'s payload is already a JSON
  object (`message`, `stack` with `file`/`line`/`column`, `componentStack`,
  `id`). ACOS should surface it in the verdict as
  `{js_error: {name, message, frames: [{file, line, column, fn}],
  component_stack, log_tail}}`, and `assert_log`/a new `assert_js_error` step
  should be able to say "a SyntaxError at index.js:26 is on screen".
- Also: the *native* redbox path (a crash in a native module) and the
  "bundle could not be fetched" error (`catalyst_reload_error`), which must be a
  distinct verdict (`bundle_unreachable`, with the URL and the errno) — the
  single most common v0.4.0 dev-loop failure.
- Acceptance: a scenario that ships a deliberately broken JS file asserts the
  error text + frame, then fixes the file and asserts the reload clears it.

#### METRO4 outcome (2026-11-28)

- Parser: `crates/acos-verify/src/rn_errors.rs` reads React Native's own report
  shapes (pinned to `react-native@0.81.4` sources): `JSStackTrace.format`
  (`<message>, stack:\n<fn>@<line>:<col>`) from `ExceptionsManagerModule` /
  `DevSupportManagerBase.logJSException`, `Error.stack` text with React's
  `This error is located at:` component stack on `ReactNativeJS`, a JSON redbox
  payload, Metro build errors (`DebugServerException.parse`: source-level
  `SyntaxError` at `file:line:column`), and the redbox dialog's texts (gated on
  its Dismiss/Reload buttons so an app's own "Error: ..." text never counts).
  Only React Native tags are read; a non-RN app gets no `rn` block.
- `bundle_unreachable` is a separate record and a separate code
  (`BUNDLE_UNREACHABLE`, status `blocked`): `Could not connect to development
  server`, `Unable to load script`, `catalyst_reload_error`, `ECONNREFUSED`,
  with the bundle URL and the low-level errno. A JS-error assertion on an
  unreachable bundle is `BUNDLE_UNREACHABLE`, never `ASSERTION_FAILED`.
- `assert_js_error` (`acos-protocol` + `schema/scenario.json`, `acos-verify`):
  `name`/`message`/`file`+`line`/`symbolicated`, `none: true`,
  `bundle_unreachable: true`, `since_previous_step`, `since_last_js_assert`,
  `timeout_ms` (the wait ends only when the whole assertion holds). `verdict.json`
  carries `rn: {js_error, bundle_unreachable}`; the live `acos_logs` reply
  carries the same `rn` block. New codes: `JS_ERROR`, `BUNDLE_UNREACHABLE`.

- Live (RN 0.81.4, dev build): `__DEV__` `ExceptionsManager.reportException`
  never calls the native `ExceptionsManager`, so none of the native shapes above
  appear for a JS throw, and the native redbox is not shown. The error is one
  `console.error` line (`[Name: message]`, no stack) plus the LogBox dialog.
  The log line is the primary signal; the dialog (recognised by view structure,
  `LogBoxDialog`'s own window holding an RN root view) supplies the symbolicated
  frames and the fatal kind to the error with the same message. A dialog still
  on screen fails `none: true`. No guest hook or protocol change.

### METRO5 — the dev menu without a shake, and the adb-reverse equivalent
A phone gets the dev menu from a shake/`Ctrl+M` and its debug server from
`adb reverse`. ACOS has neither, so it needs first-class equivalents.

- Scope: (a) a documented ACOS verb that opens the dev menu
  (`DevSettings`/`DevSupportManagerBase.showDevOptionsDialog`) — the receiver RN
  already registers (`<package>.RELOAD_APP_ACTION`) plus an ACOS-side intent
  broadcast path, so `r`/`d` have the same effect as on a device; (b) `acos net
  hostport` is the `adb reverse` equivalent — name it as such in the guide, with
  `localhost:PORT` as the shape an app must use (ACOS is not `10.0.2.2`);
  (c) the "Where is Metro?" affordance: an ACOS-side way to set
  `debug_http_host` (today: the app writes its own default in the debug build).
- Acceptance: from a verdict, an agent can open the dev menu, trigger a reload,
  and read the menu's own items (`reload`, `toggle inspector`, `dev settings`).

#### METRO5 outcome (2026-11-28)

- Scenario: `dev_server: {op: dev_menu}` (Metro `/message` broadcast `devMenu`,
  the CLI's `d`; `getpeers` first, so "no app connected" is a failure) next to
  the existing `{op: reload}` (`POST /reload`).
- Live session / scenario `service_ctl`: `acos_service_ctl {module:
  "react_native", op: "reload" | "dev_menu", args: {metro_port?}}`. `reload` is a
  host-originated `<package>.RELOAD_APP_ACTION` broadcast through the session's
  activity manager (the launcher's `rn_reload` peer op); zero receivers is
  `UNSUPPORTED_RN`. `dev_menu` is `KEYCODE_MENU` (`ReactActivity.onKeyUp` ->
  `showDevOptionsDialog`). With `metro_port` both also send the Metro command.
- `debug_http_host`: `dev_server: {op: start, debug_http_host: true}` (before
  `launch`) or `ACOS_RN_DEBUG_HTTP_HOST=localhost:PORT` makes the launcher write
  `<string name="debug_http_host">localhost:PORT</string>` into
  `/data/app-data/<pkg>/shared_prefs/<pkg>_preferences.xml` before the guest
  boots (merging with existing prefs, XML-escaped; `crates/acos-art/src/rn_prefs.rs`).
  Unset is the default and does no work. No `10.0.2.2` emulation exists.
- The `adb reverse` equivalent is the relay host-port opt-in, `acos_net
  {action: "hostport", port: PORT}` (or `acos-netd hostport add PORT`), with the
  app dialling `localhost:PORT` (`docs/AGENT_GUIDE.md`, Metro dev loop).

### METRO6 — source maps in stack traces
Every JS stack ACOS reports should point at the *source* line an agent can edit,
not at `index.bundle:1:483920`.

- Scope: fetch/consume `index.map` (or `POST /symbolicate`) for the dev bundle,
  symbolicate the frames in runtime errors and `console.error`, cache the map
  per bundle hash in the session artifacts, and put `file:line:column` in the
  verdict. Fall back honestly (`symbolicated: false, reason: "no map"`) when the
  map is missing.
- Acceptance: a JS throw at a known source line reports that exact `file:line`
  in both the live reply and `verdict.json`.

#### METRO6 outcome (2026-11-28)

- `crates/acos-verify/src/symbolicate.rs`: Metro's `POST /symbolicate` first
  (0-based columns, Metro's convention), then `GET /index.map?...` decoded here
  (source map v3, base64 VLQ) and cached per map SHA-256 in
  `<artifacts>/source-maps/` (`index.json` maps bundle revision + map path to
  the hash), so an error of the same revision still maps after Metro stopped. A
  refresh keeps the URL, so the scenario runner renews the revision on every
  `start` and `edit`, and never maps an older error with a newer bundle. The live
  `acos_logs` path does not know the revision: it uses no disk cache, remembers
  results per logged error, and contacts Metro after releasing the session lock
  (4 s budget per call). Every request has a
  strict timeout (5 s while Metro runs, 0.5 s after it stopped, 2 s on the live
  `acos_logs` path); a dead Metro answers within the connect timeout.
- Honest fallback: frames stay untouched with `symbolicated: false` and a reason
  (`no metro configured`, `symbolicate_unreachable: HTTP 404`, `no map: ...`).
  Mapped paths are made relative to the project copy (`App.js`).
- Applied to `assert_js_error`, to `verdict.json`'s `rn.js_error` (before Metro is
  stopped) and to the live `acos_logs` reply (Metro port read from the bundle URL
  the app logged; nothing guessed).

### METRO7 — the Hermes debugger (optional)
Attach a debugger to the guest's Hermes runtime over the inspector protocol
(CDP), so `console.log` and breakpoints can be driven by an agent.

- Scope: `DevServerHelper`'s inspector URL (`/inspector/device`) and Hermes' CDP
  endpoints proxied through the host-port forwarder; a `debugger` control op
  (`attach`, `evaluate`, `breakpoints`, `detach`) whose results land in the
  verdict. Explicitly optional: out of scope for the v0.5.0 gate, kept behind an
  `experimental` label, and never a prerequisite for METRO1..6.
- Acceptance (when built): `evaluate("module.exports.__marker")` returns the
  value the bundle set, and a breakpoint pauses on a known line.

### METRO8 — many sessions, one Metro
N=20 density is a v0.5.0 gate; the dev loop must not be the thing that breaks it.

- Scope: one Metro for the whole host, N sessions each with their own forwarder
  and `debug_http_host` (per-session `hostport` opt-in stays the default; the
  relay-level opt-in exists for launch-time fetches), no cross-session
  interference (session A's reload and edits never touch session B's tree), and
  the relay's per-session rules proving isolation.
- Acceptance: N=5 sessions each fetch the bundle and render their own text;
  editing + reloading session A leaves B unchanged; RAM/first-frame delta over
  module-off within the documented budget.

## METRO1 result (2026-11-18)

Evidence: `results/handoff/METRO1/` (`REPORT.md` + `run/`).

```
pasta 2026_10_02.cba3570                                   # pinned binary, sha256 matched
metro up port=8081 project=…/.cache/metro1/app2            # RN 0.81 CLI dev server
host_bundle http=200 bytes=4937649                         # Metro serves the dev bundle
netd_up rc=0 pasta_version=2026_10_02.cba3570              # ADR-0030 relay
hostport_add rc=0 host_ports=[8081]                        # relay-level opt-in, before launch
launch rc=0 stage=pass                                     # ART + network:real
text_found 'Hello ACOS' after 0s                           # the bundle's text, from Metro
reload http=200 body=OK                                    # POST /reload (the CLI's `r`)
host_bundle_after_edit served=NEW_TEXT                     # Metro rebuilt; server is right
TIMEOUT 'Hello Metro' after 90s                            # the guest did not re-render it
```

The guest log names the exact point it stops:

```
E NativeViewHierarchyManager Trying to add a root view with an explicit id (11)
                               already set. React Native uses the id field to
                               track react tags and will overwrite this field.
W ReactNative CatalystInstanceImpl.destroy() end
W ReactNative Tried to enqueue runnable on already finished thread: 'js…' dropping Runnable.
W ReactNativeJS The app is running using the Legacy Architecture. …      ← new context, new bundle
```

before → after trees: 8 nodes → 16 nodes, the two halves identical (same bounds,
same text), both showing the **pre-edit** string — while the host's own copy of
the same bundle URL (`bundle-probes.json`: `has_hello_acos: 0, has_hello_metro: 1`)
proves Metro served the new text. So: network ✓, bundle fetch ✓, reload command
✓, new JS context ✓, **view-tree replacement ✗** (METRO2, with the dropped
`js`-thread runnable as the first suspect).

The `debug_http_host` decision is worth calling out because it is the shape every
future dev-loop scenario will use: ACOS does **not** report itself as a stock
emulator to React Native (that would make RN dial the unreachable `10.0.2.2`), so
the debug variant writes RN's own documented preference to `localhost:8081` and
the relay's per-session forwarder provides the `adb reverse` equivalent. No
`10.0.2.2` emulation was added to the network module: an alias that only React
Native uses would be a fake.

## Rebuild / reproduce

```sh
export ACOS_FONT_DIR=/home/Qui/Projects/acos/.cache/boot/android-34/fonts
tools/build/build-rn-hello.sh debug                  # fixtures/build/apps/rn-debug.apk
# pinned pasta (one file, GPL-2, sha256 in research/spike-net/artifacts/passt-build.txt):
git clone --depth 1 -b 2026_10_02.cba3570 https://passt.top/passt .cache/metro1/build/passt-src
make -C .cache/metro1/build/passt-src static -j8 VERSION=2026_10_02.cba3570
cargo build --release -p acos-cli -p acos-netd -p acos-art
cp -al .cache/rn-0.81.4/jsproject .cache/metro1/app2     # Metro project (JS from the fixture)
tools/art-runtime/run-exclusive.sh --task METRO1 -- tools/guard/guarded.sh --mem 10G --time 700 -- \
  bash tools/metro/metro1-dev-loop.sh "$PWD/results/handoff/METRO1/run"
```
