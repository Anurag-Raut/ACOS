# ACOS physical-device vs ART-reference comparison

**Tier: `physical-device`.** This table compares the final state captured from the physical phone (`moto g35 5G`, Android 15 / API 35, 1080x2400 @ 400 dpi) with the emulator `art-reference` captures in `fixtures/reference/<app>/art-api34-x86_64/` (API 34, 1080x2400 @ 420 dpi).

A `physical-device` pass never satisfies an `art-reference` requirement (ADR-0005). The two tiers are not interchangeable and are never mixed silently.

## Expected differences that are labelled, not failures

* **Density 400 dpi vs 420 dpi** — all pixel bounds differ by the density ratio (400/420 ≈ 0.952) and by the different status/action-bar insets. Text values are unaffected.
* **API 35 vs API 34** — edge-to-edge enforcement and status-bar inset changes shift the content origin; text and resource ids are stable.
* The scenario's expected bounds are calibrated to the API-34 420 dpi oracle, so a device bounds mismatch is expected and is reported as `bounds_differ_density`, never as a text failure.


## Table

| app | element (resource id) | scenario expected text | device final text | emulator final text | device bounds | emulator bounds | text verdict | bounds verdict | note |
|---|---|---|---|---|---|---|---|---|---|
| counter | `android:id/decor_content_parent` | - | — | — | [0,0][1080,2400] | [0,0][1080,2337] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| counter | `android:id/action_bar_container` | - | - | — | - | [0,128][1080,275] | element_absent_on_device | - |  |
| counter | `android:id/action_bar` | - | - | — | - | [0,128][1080,275] | element_absent_on_device | - |  |
| counter | `android:id/content` | - | — | — | [0,248][1080,2400] | [0,275][1080,2337] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| counter | `dev.acos.fixtures.counter:id/count` | 0 | 0 | 0 | [40,288][61,334] | [42,317][63,368] | text_match | bounds_differ_density | density 400 vs 420 dpi; matches scenario |
| counter | `dev.acos.fixtures.counter:id/inc` | Increment | Increment | Increment | [40,334][262,454] | [42,368][273,494] | text_match | bounds_differ_density | density 400 vs 420 dpi; matches scenario |
| counter | `dev.acos.fixtures.counter:id/reset` | Reset | Reset | Reset | [40,454][260,574] | [42,494][273,620] | text_match | bounds_differ_density | density 400 vs 420 dpi; matches scenario |
| counter | `android:id/statusBarBackground` | - | - | — | - | [0,0][1080,128] | element_absent_on_device | - |  |
| counter | `android:id/navigationBarBackground` | - | - | — | - | [0,2337][1080,2400] | element_absent_on_device | - |  |
| todo-list | `android:id/decor_content_parent` | - | — | — | [0,0][1080,2400] | [0,0][1080,2337] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| todo-list | `android:id/action_bar_container` | - | - | — | - | [0,128][1080,275] | element_absent_on_device | - |  |
| todo-list | `android:id/action_bar` | - | - | — | - | [0,128][1080,275] | element_absent_on_device | - |  |
| todo-list | `android:id/content` | - | — | — | [0,248][1080,2400] | [0,275][1080,1517] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| todo-list | `dev.acos.fixtures.todolist:id/input` | New item | New item | New item | [40,288][1040,400] | [42,317][1038,435] | text_match | bounds_differ_density | density 400 vs 420 dpi; matches scenario |
| todo-list | `dev.acos.fixtures.todolist:id/add` | Add | Add | Add | [40,400][260,520] | [42,435][273,561] | text_match | bounds_differ_density | density 400 vs 420 dpi; matches scenario |
| todo-list | `dev.acos.fixtures.todolist:id/clear` | Clear all | Clear all | Clear all | [260,400][480,520] | [273,435][504,561] | text_match | bounds_differ_density | density 400 vs 420 dpi; matches scenario |
| todo-list | `dev.acos.fixtures.todolist:id/list` | - | — | — | [40,520][1040,2360] | [42,561][1038,1475] | text_match | bounds_differ_density | density 400 vs 420 dpi |
| todo-list | `android:id/text1` | - | bread | bread | [40,520][1040,640] | [42,561][1038,687] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| todo-list | `android:id/statusBarBackground` | - | - | — | - | [0,0][1080,128] | element_absent_on_device | - |  |
| todo-list | `android:id/navigationBarBackground` | - | - | — | - | [0,2337][1080,2400] | element_absent_on_device | - |  |
| multi-activity | `android:id/decor_content_parent` | - | — | — | [0,0][1080,2400] | [0,0][1080,2337] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| multi-activity | `android:id/action_bar_container` | - | - | — | - | [0,128][1080,275] | element_absent_on_device | - |  |
| multi-activity | `android:id/action_bar` | - | - | — | - | [0,128][1080,275] | element_absent_on_device | - |  |
| multi-activity | `android:id/content` | - | — | — | [0,248][1080,2400] | [0,275][1080,2337] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| multi-activity | `dev.acos.fixtures.multiactivity:id/message` | Hello | Hello | Hello | [40,288][1040,400] | [42,317][1038,435] | text_match | bounds_differ_density | density 400 vs 420 dpi; matches scenario |
| multi-activity | `dev.acos.fixtures.multiactivity:id/send` | Send | Send | Send | [40,400][260,520] | [42,435][273,561] | text_match | bounds_differ_density | density 400 vs 420 dpi; matches scenario |
| multi-activity | `android:id/statusBarBackground` | - | - | — | - | [0,0][1080,128] | element_absent_on_device | - |  |
| multi-activity | `android:id/navigationBarBackground` | - | - | — | - | [0,2337][1080,2400] | element_absent_on_device | - |  |
| form-validation | `android:id/content` | - | — | — | [67,1051][1013,1457] | [70,1018][1010,1447] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| form-validation | `android:id/parentPanel` | - | — | — | [67,1051][1013,1457] | [70,1018][1010,1447] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| form-validation | `android:id/topPanel` | - | - | — | - | [70,1018][1010,1157] | element_absent_on_device | - |  |
| form-validation | `android:id/title_template` | - | - | — | - | [70,1018][1010,1136] | element_absent_on_device | - |  |
| form-validation | `android:id/alertTitle` | - | Success | Success | [127,1096][953,1162] | [133,1065][947,1136] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| form-validation | `android:id/titleDividerNoCustom` | - | - | — | - | [70,1136][1010,1157] | element_absent_on_device | - |  |
| form-validation | `android:id/contentPanel` | - | - | — | - | [70,1157][1010,1283] | element_absent_on_device | - |  |
| form-validation | `android:id/scrollView` | - | — | — | [67,1182][1013,1236] | [70,1157][1010,1214] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| form-validation | `android:id/message` | Submitted: Ada | Submitted: Ada | Submitted: Ada | [67,1182][1013,1236] | [70,1157][1010,1214] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift; matches scenario |
| form-validation | `android:id/buttonPanel` | - | — | — | [67,1302][1013,1457] | [70,1283][1010,1447] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |
| form-validation | `android:id/button1` | - | OK | OK | [823,1312][983,1447] | [810,1294][978,1436] | text_match | bounds_differ_density | density 400 vs 420 dpi + API 35 edge-to-edge/inset shift |

## Per-app scenario outcomes

* **counter** — 23 scenario steps captured; 0 action(s) failed; 0 issue(s). Final captured state: `assert_text:{"id": "count"}`.
  * note: the `back` step at seq 21 closed the app as the scenario expects.
* **todo-list** — 29 scenario steps captured; 0 action(s) failed; 0 issue(s). Final captured state: `assert_no_crash`.
  * note: the `back` step at seq 27 did not close the app; the app was still foreground after it (keyboard/back-stack difference vs the API-34 oracle). `agent-device close` closed it afterwards.
* **multi-activity** — 21 scenario steps captured; 0 action(s) failed; 0 issue(s). Final captured state: `assert_no_crash`.
  * note: the `back` step at seq 19 did not close the app; the app was still foreground after it (keyboard/back-stack difference vs the API-34 oracle). `agent-device close` closed it afterwards.
* **form-validation** — 26 scenario steps captured; 0 action(s) failed; 0 issue(s). Final captured state: `assert_no_crash`.
