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
| acceptance arm | `scenarios/metro-redbox.yaml` (runtime ReferenceError → assert error + App.js frame → fix → `none: true`) | not yet run live |

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

- No live ART + Metro run of `scenarios/metro-redbox.yaml`: the RN debug APK is not built on this host. The parser is
  pinned to RN 0.81.4 source formats, not to a recorded guest log; the first live run is the acceptance check, and any
  mismatch must be fixed in the parser, never by weakening the scenario.
- `react_native` control ops have unit coverage of their outcome logic only (no fake `ArtSession` harness exists).
- Indexed (`sections`) source maps are refused with a reason; Metro does not emit them for dev bundles.
