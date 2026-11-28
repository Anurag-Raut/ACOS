# METRO4/5/6: React Native dev-loop polish (redbox as data, dev menu without a shake, symbolication)

Branch `feat/tier1-metro-polish`. Plan: `docs/METRO_PLAN.md` §§ METRO4–6 (each step's outcome is recorded there).

## What landed

| step | surface | where |
|---|---|---|
| METRO4 | `JsError` / `BundleUnreachable` / `RnDevState` protocol types; `verdict.json` `rn` block; codes `JS_ERROR` (fail) and `BUNDLE_UNREACHABLE` (blocked, `host_guard`, retryable) | `crates/acos-protocol/src/{rn,error,verdict}.rs`, `schema/*.json` |
| METRO4 | `assert_js_error` step (`name`, `message`, `file`+`line`, `symbolicated`, `none`, `bundle_unreachable`, `since_previous_step`, `timeout_ms`) with validation | `crates/acos-protocol/src/scenario.rs`, `schema/scenario.json` |
| METRO4 | parser for React Native's report shapes (JSStackTrace.format, `Error.stack` + component stack, JSON redbox payload, Metro transform errors, the redbox dialog's texts) and the bundle-fetch failures | `crates/acos-verify/src/rn_errors.rs` |
| METRO4 | executor: wait + evaluate `assert_js_error` (logs plus the redbox tree when present); verdict `rn`; live `acos_logs` `rn` block | `crates/acos-verify/src/executor.rs`, `crates/acos-cli/src/engine/serve.rs` |
| METRO5 | `dev_server: {op: dev_menu}` (Metro `/message` `devMenu` after `getpeers`; no app connected = failure) | `crates/acos-verify/src/dev_server.rs` |
| METRO5 | `acos_service_ctl {module: react_native, op: reload \| dev_menu, args: {metro_port?}}`: a host-originated `<pkg>.RELOAD_APP_ACTION` broadcast (launcher peer op `rn_reload`; zero receivers = `UNSUPPORTED_RN`) and `KEYCODE_MENU` | `crates/acos-art/src/launch.rs`, `crates/acos-cli/src/engine/art.rs` |
| METRO5 | `debug_http_host`: `dev_server: {op: start, debug_http_host: true}` (before `launch`) or `ACOS_RN_DEBUG_HTTP_HOST` makes the launcher write RN's own preference before the guest boots (merge, XML-escaped); no `10.0.2.2` emulation | `crates/acos-art/src/rn_prefs.rs`, `crates/acos-cli/src/verify.rs` |
| METRO5 | `adb reverse` equivalent documented: `acos_net {action: hostport, port}` / `acos-netd hostport add PORT`, app dials `localhost:PORT` | `docs/AGENT_GUIDE.md` |
| METRO6 | symbolicator: Metro `POST /symbolicate` first, then `index.map` (VLQ source map v3), cached per map SHA-256 under `<artifacts>/source-maps/`; strict timeouts; honest `symbolicated: false` + reason | `crates/acos-verify/src/symbolicate.rs` |
| acceptance arm | `scenarios/metro-redbox.yaml` (runtime ReferenceError → assert error + App.js frame → fix → `none: true`) | passes live (see below) |

Zero cost when unused: the parser reads React Native tags only and runs on demand; symbolication runs only for an
unsymbolicated JS error; `debug_http_host` is written only when asked for; no new process or thread exists unless a
dev-loop step asks for it.

## Verification (this branch, 2026-11-28)

- `tools/verify/check-layout.sh`: ok.
- `cargo test -p acos-protocol` (with `ACOS_ALLOW_ART_SKIP=1`: the host has no built `a11y-names-probe.apk` /
  `rn-debug.apk`, so 76 scenario files are counted skips; `metro-redbox.yaml` was validated separately with
  `Scenario::from_yaml_str`): 102 + 10 + 9 + 7 + 3 + 3 + 2 pass. Schemas regenerated (`UPDATE_SCHEMA=1`).
- `cargo test -p acos-verify`: 147 lib (12 `rn_errors`, 6 `symbolicate` against a mock Metro, 3 new `dev_server`)
  + 49 `tests/scenarios.rs` (4 new METRO4 end-to-end) pass.
- `cargo test -p acos-mcp`: everything passes, including `docs`, `guide` and `documentation_args`. Seven tests
  failed first only because of a fixtures symlink outside the worktree (`UNSAFE_PATH`); they pass with a hard-linked
  copy.
- `cargo test -p acos-cli --bin acos` (the crate has no lib target): 276 pass (3 `zygote_host` tests need a short
  `TMPDIR` for AF_UNIX paths).
- `cargo test -p acos-art --lib rn_`: 5 pass (prefs write/merge/escape, `rn_reload` receiver count).
- `cargo clippy --no-deps -p acos-protocol -p acos-verify -p acos-cli -p acos-mcp -p acos-art --all-targets`: no
  warning on any line this branch adds.

## Not done / open

- Dev-build errors logged as `console.error(error)` get no frames (LogBox shows only a toast), and live `acos_logs`
  reads the log alone, so it reports name and message without frames for these errors.
- `throw 'text'`: the message and an empty name, no frames, `kind: soft`. LogBox shows `Unknown` and only React
  Native's own frames for it, so the dialog is not matched to the log line.
- The LogBox frames exist only while the dialog is open; a dismissed dialog leaves name and message.
- LogBox recognition is pinned to React Native 0.81.4's dialog; other versions are unverified. A native (not JS)
  dialog that puts an RN root view directly in its content and draws LogBox's exact texts would be read as LogBox.
- `console.error('text', error)` (extra arguments of the app's own) is not read.
- `react_native` control ops have unit coverage of their outcome logic only (no fake `ArtSession` harness exists).
- Indexed (`sections`) source maps are refused with a reason; Metro does not emit them for dev bundles.

## Second review round (PR #16, 9 threads)

- Source maps are bound to a bundle revision (renewed by every `dev_server` `start` and `edit`); an older error is
  never mapped by newer maps or sent to a newer Metro. Live `acos_logs` does not know the revision, so it keeps no disk
  cache, remembers results per logged error, and contacts Metro after releasing the session lock (2 s per request,
  4 s per call, failures retried after 10 s). A first poll made after a refresh is still answered by Metro's current
  bundle.
- An `assert_js_error` wait maps the latest error and applies the step's own full check.
- Redbox title classification, `httpClient.ts`, bare `index.bundle` names and chunked-reply framing are fixed.
- New scope `since_last_js_assert`; `metro-redbox.yaml` uses it for the no-error assertion.

Live check with an uncommitted variant of the scenario that throws a shape the parser reads
(`console.error(new Error('acosLiveProbe').stack)`):

| run | result |
|---|---|
| probe | pass, 19 steps; `symbolicated: true` by `metro /symbolicate`, `App.js:32`, same answer in `verdict.json` after the fix edits |
| a second error logged while the fix refreshes | fail (`a JS error is still reported: Error: acosRecoveryProbe`) |
| the same, with the old `since_previous_step` scope | pass (the false pass the new scope removes) |

## RN 0.81.4 dev errors (fix for the live failure)

What React Native does (`react-native@0.81.4` sources): in `__DEV__`, `ExceptionsManager.reportException` never calls
the native `NativeExceptionsManager`. It calls `LogBox.addException` and `console.error(e)`; the console polyfill
prints the object without a stack. So there is no native ExceptionsManager signal and no native redbox. No guest hook
or protocol change was needed.

| error | `E ReactNativeJS` line (live) | LogBox |
|---|---|---|
| uncaught in a handler | `[ReferenceError: Property 'acosMissingFunction' doesn't exist]` | opens, "Uncaught Error" |
| thrown in render | `{ [TypeError: acosRender]\n  componentStack: '...', isComponentError: true }` | opens |
| `console.error(error)` in a component | `[RangeError: acosSoft], { [Stack] name: 'Stack' }` (React DevTools appends the second part) | toast only |
| `throw 'text'` | `{ [acosThrownString] name: '' }` | opens, message "Unknown" |

Signals used (`crates/acos-verify/src/rn_errors.rs`):

1. The log record, only on tag `ReactNativeJS` at level E, and only when the whole record is that object: name,
   message, component stack.
2. The LogBox dialog from the UI tree, recognised by structure: a `TYPE_APPLICATION` window whose
   `android:id/content` holds `ReactRootView` / `ReactSurfaceView` directly, then header, title, message and footer in
   order. It adds the Metro-symbolicated frames and the fatal kind to the logged error with the same message. LogBox's
   views have no resource ids (JS `id` props are not in the tree). A dialog with no logged error in scope is itself
   the error, so `none: true` fails while it is up.

`assert_js_error`'s wait now reads the screen when the log alone does not settle it, and `verdict.json` keeps what the
step read after the dialog is gone. The native-redbox parser is unchanged.

Negative coverage: error values on `console.log` / `console.warn` or another tag; quoted, array, multi-argument and
sentence forms; the same dialog texts in the activity window, in an app `<Modal>`, or without the inspector's fixed
parts.

Verification:

- `cargo test --offline`: `-p acos-verify` 181 lib + 54 `tests/scenarios.rs`; `-p acos-protocol`
  (`ACOS_ALLOW_ART_SKIP=1`) 136, 42 counted skips; `-p acos-cli --bin acos` 277; `-p acos-mcp --test docs
  --test guide --test documentation_args` 1 + 14 + 4. All pass.
- `tools/verify/check-layout.sh`: ok. Clippy: no warning on a changed line.

Live (ART + Metro, RN 0.81.4 debug APK; the three committed scenarios unmodified):

| scenario | result |
|---|---|
| `metro-redbox.yaml` | pass, 19 steps; `ReferenceError`, `kind: fatal`, `App.js:32` column 23, `react native logbox`, same in `verdict.json` after the fix |
| `metro-reload.yaml` | pass, 12 steps, no `rn` block |
| `metro-reload-full.yaml` | pass, 12 steps, no `rn` block |
| probe: log/warn of error values + `<Modal>` with LogBox's texts | `none: true` passes |
| probe: render error | pass; `TypeError`, `App.js:21`, component stack |
| probe: `throw 'text'` | pass on message; name empty, no frames |
| probe: `console.error(new RangeError())` | pass on name and message |
| probe: `none: true` while LogBox is up | fails as intended (`a JS error is still reported`) |

The probes are uncommitted scenario variants.

