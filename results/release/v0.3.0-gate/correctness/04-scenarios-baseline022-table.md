# Baseline — the same 17 non-declared scenarios on v0.2.2 (tag v0.2.2 binary, same scenarios/APKs)

| scenario | expected | got | error code | steps | verdict path | as declared |
|---|---|---|---|---|---|---|
| `gaten-lesson.yaml` | pass | pass |  | 28 | `04-scenarios-baseline022/gaten-lesson/artifacts/verdict.json` | OK |
| `hello-views.yaml` | pass | timeout | TIMEOUT | 24 | `04-scenarios-baseline022/hello-views/artifacts/verdict.json` | **NOT OK** |
| `multi-activity.yaml` | pass | timeout | TIMEOUT | 12 | `04-scenarios-baseline022/multi-activity/artifacts/verdict.json` | **NOT OK** |
| `negative/multi-wrong-after-back.yaml` | fail | timeout | TIMEOUT | 7 | `04-scenarios-baseline022/negative__multi-wrong-after-back/artifacts/verdict.json` | **NOT OK** |
| `negative/offscreen-pager-wait.yaml` | fail | timeout | TIMEOUT | 3 | `04-scenarios-baseline022/negative__offscreen-pager-wait/artifacts/verdict.json` | **NOT OK** |
| `s8-hostile-region.yaml` | pass | timeout | TIMEOUT | 2 | `04-scenarios-baseline022/s8-hostile-region/artifacts/verdict.json` | **NOT OK** |
| `s8-hostile-untrusted.yaml` | pass | fail | INSUFFICIENT_HOST_EVIDENCE | 2 | `04-scenarios-baseline022/s8-hostile-untrusted/artifacts/verdict.json` | **NOT OK** |
| `suite/T07-form-good.yaml` | pass | fail | ELEMENT_OFFSCREEN | 9 | `04-scenarios-baseline022/suite__T07-form-good/artifacts/verdict.json` | **NOT OK** |
| `svg-viewbox.yaml` | pass | timeout | TIMEOUT | 2 | `04-scenarios-baseline022/svg-viewbox/artifacts/verdict.json` | **NOT OK** |
| `w15-broken-crash.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w15-broken-missing-symbol.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w25-gaten-launch.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w26-gaten-debug-launch.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w28-gaten-debug-launch.yaml` | pass | error | GUEST_EXITED |  | `` | **NOT OK** |
| `w44-hostile-forest-untrusted.yaml` | pass | fail | INSUFFICIENT_HOST_EVIDENCE | 2 | `04-scenarios-baseline022/w44-hostile-forest-untrusted/artifacts/verdict.json` | **NOT OK** |
| `w44-hostile-parcel.yaml` | pass | timeout | TIMEOUT | 2 | `04-scenarios-baseline022/w44-hostile-parcel/artifacts/verdict.json` | **NOT OK** |
| `w44-hostile-probe-spam.yaml` | pass | fail | INSUFFICIENT_HOST_EVIDENCE | 2 | `04-scenarios-baseline022/w44-hostile-probe-spam/artifacts/verdict.json` | **NOT OK** |

17 rows, 1 as declared, 16 not as declared
