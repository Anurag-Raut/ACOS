# LAUNCHER2 Handoff Evidence

Minimal guest Launcher (`com.acos.launcher`) and Settings stub (`com.android.settings`) system apps, opt-in ADR-0029 modules (`system.launcher`, `system.settings`), `KEYCODE_HOME` routing, `ResolveInfo.loadLabel(pm)` / `ParceledListSlice<ResolveInfo>` resolution, intent `mData` propagation, and cross-process task switching.

## Design
See [`docs/LAUNCHER_PLAN.md`](../../../docs/LAUNCHER_PLAN.md) and [`docs/MODULES.md`](../../../docs/MODULES.md).

## Guest APKs
- `fixtures/apps/launcher` (`com.acos.launcher/.LauncherActivity`, `MAIN`/`HOME`, `singleTask`)
- `fixtures/apps/settings` (`com.android.settings/.SettingsActivity`, `SETTINGS`, `WIFI_SETTINGS`, `APPLICATION_DETAILS_SETTINGS` with `package:` data URI, `singleTask`)
- `fixtures/apps/settings-probe` (`dev.acos.fixtures.settingsprobe/.MainActivity`, negative/positive probe for settings actions including unhandled `LOCATION_SOURCE_SETTINGS`)

## Real ART Scenarios
| Scenario | Expectation | Actual Verdict | Wall (ms) | Notes |
| --- | --- | --- | ---: | --- |
| `scenarios/launcher-home.yaml` | pass | `pass` | 7291 | `Hello Views` -> `key home` -> `ACOS Home` (lists `Counter`, `Hello Views`) -> tap `Counter` -> `count = 1` -> `key home` -> tap `Hello Views` -> `Hello ACOS` |
| `scenarios/settings-intents.yaml` | pass | `pass` | 16188 | `ACTION_SETTINGS`, `ACTION_APPLICATION_DETAILS_SETTINGS` (`Package: dev.acos.fixtures.settingsprobe`), `ACTION_WIFI_SETTINGS`, and unhandled `ACTION_LOCATION_SOURCE_SETTINGS` -> `ActivityNotFoundException` |
| `scenarios/negative/launcher-home-wrong.yaml` | fail | `fail` (`ASSERTION_FAILED`) | — | Expects nonexistent label on Launcher screen |
| `scenarios/negative/settings-intents-wrong.yaml` | fail | `fail` (`ELEMENT_NOT_FOUND`) | — | Expects Settings view when `system.settings` is off and `ACTION_SETTINGS` throws `ActivityNotFoundException` |
| `home-off-smoke` (scratch) | fail | `unsupported` (`UNSUPPORTED_ANDROID_API` / `HOME_UNAVAILABLE`) | — | `key home` with `system.launcher` Off stays an honest error |

## Regression ART Scenarios
- `scenarios/mproc1-intents.yaml`: `pass` (696 ms)
- `scenarios/mproc3-two-process.yaml`: `pass` (7514 ms)
- `scenarios/sysui1-permission-allow.yaml`: `pass` (3298 ms)
- `scenarios/sysui1-shade.yaml`: `pass` (2016 ms)
- `scenarios/w38-multi-activity.yaml`: `pass` (4187 ms)
- `scenarios/rot1-rotate.yaml`: `pass` (4743 ms)

## Cold-Start Benchmark (`scenarios/v1-hello-views.yaml`, `system.launcher`/`system.settings` Off)
- Pre-change checkout run (`out/v1-hello-views`): `session_start_ms=167`, `launch_ms=359`, `wall_ms=2930` (prior baseline: `launch_ms=373/372`, `wall_ms=2952.5/2954`).
- Post-change 4 runs:
  - Run 1: `session_start_ms=159`, `launch_ms=404`, `wall_ms=3090`
  - Run 2: `session_start_ms=164`, `launch_ms=384`, `wall_ms=3082`
  - Run 3: `session_start_ms=171`, `launch_ms=366`, `wall_ms=3045`
  - Run 4: `session_start_ms=162`, `launch_ms=402`, `wall_ms=3090`
- Post-change medians: `session_start_ms=163.0` (`-4.0 ms` vs `167`), `launch_ms=393.0`, `wall_ms=3086.0`.

## Cargo Tests
- `cargo test --offline -p acos-wm`: 79 + 10 + 4 + 2 = 95 passed, 0 failed
- `cargo test --offline -p acos-binder-peer`: 6 + 2 + 3 + 4 + 8 + 2 = 25 passed, 0 failed
- `cargo test --offline -p acos-runtime --lib --test activity_result_delivery --test ams_java --test parcelable_java`: 13 + 11 + 26 + 2 = 52 passed, 0 failed
- `cargo test --offline -p acos-services --test module_kit --test modules_doc --test mproc_intents --test mproc5_top_of_task --test rot8_resume --test sysui4_result_ordering`: 20 + 2 + 5 + 6 + 1 + 3 = 37 passed, 0 failed
- `ACOS_ALLOW_ART_SKIP=1 cargo test --offline -p acos-services --lib`: 190 passed, 0 failed (`pms::tests::from_apk_reads_provider_authorities` skipped because Gradle fixture `service-provider.apk` is not built by `tools/build-apps.sh`)
- `cargo test --offline -p acos-art --lib`: 204 passed, 0 failed, 1 ignored
- `cargo test --offline -p acos-cli --bins`: 212 passed, 0 failed
