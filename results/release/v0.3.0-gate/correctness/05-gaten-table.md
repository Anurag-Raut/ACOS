# Item 4 — the 20 Gaten gate rows (10 positive + 10 negative), each in a FRESH artifact dir

| scenario | expected | got | error code | steps | verdict path | as declared |
|---|---|---|---|---|---|---|
| `gaten-home.yaml` | pass | pass |  | 14 | `05-gaten/gaten-home/artifacts/verdict.json` | OK |
| `gaten-tabs.yaml` | pass | pass |  | 22 | `05-gaten/gaten-tabs/artifacts/verdict.json` | OK |
| `gaten-settings.yaml` | pass | pass |  | 33 | `05-gaten/gaten-settings/artifacts/verdict.json` | OK |
| `gaten-search.yaml` | pass | pass |  | 22 | `05-gaten/gaten-search/artifacts/verdict.json` | OK |
| `gaten-lesson.yaml` | pass | timeout | TIMEOUT | 26 | `05-gaten/gaten-lesson/artifacts/verdict.json` | **NOT OK** |
| `gaten-kanji.yaml` | pass | pass |  | 33 | `05-gaten/gaten-kanji/artifacts/verdict.json` | OK |
| `gaten-recognition.yaml` | pass | pass |  | 33 | `05-gaten/gaten-recognition/artifacts/verdict.json` | OK |
| `gaten-support.yaml` | pass | pass |  | 27 | `05-gaten/gaten-support/artifacts/verdict.json` | OK |
| `gaten-backstack.yaml` | pass | pass |  | 25 | `05-gaten/gaten-backstack/artifacts/verdict.json` | OK |
| `gaten-content-welcome.yaml` | pass | pass |  | 4 | `05-gaten/gaten-content-welcome/artifacts/verdict.json` | OK |
| `negative/gaten-home-wrong.yaml` | fail | fail | ASSERTION_FAILED | 8 | `05-gaten/negative__gaten-home-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-tabs-wrong.yaml` | fail | fail | ASSERTION_FAILED | 8 | `05-gaten/negative__gaten-tabs-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-settings-wrong.yaml` | fail | fail | ASSERTION_FAILED | 13 | `05-gaten/negative__gaten-settings-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-search-wrong.yaml` | fail | fail | ASSERTION_FAILED | 17 | `05-gaten/negative__gaten-search-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-lesson-wrong.yaml` | fail | fail | ASSERTION_FAILED | 11 | `05-gaten/negative__gaten-lesson-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-kanji-wrong.yaml` | fail | fail | ASSERTION_FAILED | 20 | `05-gaten/negative__gaten-kanji-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-recognition-wrong.yaml` | fail | fail | ASSERTION_FAILED | 29 | `05-gaten/negative__gaten-recognition-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-support-wrong.yaml` | fail | fail | ASSERTION_FAILED | 14 | `05-gaten/negative__gaten-support-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-backstack-wrong.yaml` | fail | fail | ASSERTION_FAILED | 24 | `05-gaten/negative__gaten-backstack-wrong/artifacts/verdict.json` | OK |
| `negative/gaten-content-welcome-wrong.yaml` | fail `ASSERTION_FAILED/None` | fail | ASSERTION_FAILED | 2 | `05-gaten/negative__gaten-content-welcome-wrong/artifacts/verdict.json` | OK |

20 rows, 19 as declared, 1 not as declared
