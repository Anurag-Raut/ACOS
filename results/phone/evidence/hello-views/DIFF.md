### hello-views

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Enter your name, Greet, Hello ACOS, Hello Views, clicks: 0 | **text-set** |
| 1 | wait_for | `{"id": "result"}` | pass / clicks: 0 / [42,612][175,663] | pass / clicks: 0 / [40,566][166,612] | - | - | **bounds** |
| 2 | assert_text | `{"id": "title"}` | pass / Hello ACOS / [42,317][231,368] | pass / Hello ACOS / [40,288][215,334] | - | - | **bounds** |
| 3 | assert_text | `{"id": "input"}` | pass / Enter your name / [42,368][1038,486] | pass / Enter your name / [40,334][1040,446] | - | - | **bounds** |
| 4 | assert_text | `{"id": "btn"}` | pass / Greet / [42,486][273,612] | pass / Greet / [40,446][260,566] | - | - | **bounds** |
| 5 | assert_text | `{"id": "result"}` | pass / clicks: 0 / [42,612][175,663] | pass / clicks: 0 / [40,566][166,612] | - | - | **bounds** |
| 6 | assert_log | `-` | pass / - / - | pass / - / - | - | Enter your name, Greet, Hello ACOS, Hello Views, clicks: 0 | **text-set** |
| 7 | tap | `{"id": "input"}` | pass / Enter your name / [42,368][1038,486] | pass / Enter your name / [40,334][1040,446] | - | - | **bounds** |
| 8 | type | `{"id": "input"}` | pass / World / [42,368][1038,486] | pass / Enter your name / [40,334][1040,446] | - | - | **state** |
| 9 | tap | `{"id": "btn"}` | pass / Greet / [42,486][273,612] | pass / Greet / [40,446][260,566] | - | - | **bounds** |
| 10 | assert_log | `-` | pass / - / - | pass / - / - | - | Greet, Hello ACOS, Hello Views, Hello, World! clicks: 1, World | **text-set** |
| 11 | assert_text | `{"id": "result"}` | pass / Hello, World! clicks: 1 / [42,612][392,663] | pass / Hello, World! clicks: 1 / [40,566][372,612] | - | - | **bounds** |
| 12 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 13 | tap | `{"id": "btn"}` | pass / Greet / [42,486][273,612] | pass / Greet / [40,446][260,566] | - | - | **bounds** |
| 14 | assert_log | `-` | pass / - / - | pass / - / - | - | Greet, Hello ACOS, Hello Views, Hello, World! clicks: 2, World | **text-set** |
| 15 | assert_text | `{"id": "result"}` | pass / Hello, World! clicks: 2 / [42,612][392,663] | pass / Hello, World! clicks: 2 / [40,566][378,612] | - | - | **bounds** |
| 16 | rotate | `-` | unsupported / - / - | pass / - / - | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **status** |
| 17 | assert_log | `-` | - / - / - | pass_with_failure / - / - | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 18 | assert_text | `{"id": "input"}` | - / - / - | pass / World / [148,266][2360,378] | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 19 | assert_text | `{"id": "result"}` | - / - / - | pass / clicks: 2 / [148,498][273,544] | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 20 | rotate | `-` | - / - / - | pass / - / - | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 21 | assert_log | `-` | - / - / - | pass_with_failure / - / - | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 22 | assert_text | `{"id": "result"}` | - / - / - | pass / clicks: 2 / [40,566][165,612] | - | Greet, Hello ACOS, Hello Views, World, clicks: 2 | **missing-side** |
| 23 | back | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 24 | assert_log | `-` | - / - / - | pass_with_failure / - / - | - | - | **missing-side** |
| 25 | assert_no_crash | `-` | - / - / - | pass / - / - | - | - | **missing-side** |

Counts: bounds=10, match=1, missing-side=9, state=1, status=1, text-set=4
