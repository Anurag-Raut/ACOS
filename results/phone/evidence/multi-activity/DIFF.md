### multi-activity

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Message, Multi Activity, Send | **text-set** |
| 1 | wait_for | `{"id": "message"}` | pass / Message / [42,317][1038,435] | pass / Message / [40,288][1040,400] | - | - | **bounds** |
| 2 | assert_text | `{"id": "message"}` | pass / Message / [42,317][1038,435] | pass / Message / [40,288][1040,400] | - | - | **bounds** |
| 3 | assert_text | `{"id": "send"}` | pass / Send / [42,435][273,561] | pass / Send / [40,400][260,520] | - | - | **bounds** |
| 4 | assert_log | `-` | pass / - / - | pass / - / - | - | Message, Multi Activity, Send | **text-set** |
| 5 | tap | `{"id": "message"}` | pass / Message / [42,317][1038,435] | pass / Message / [40,288][1040,400] | - | - | **bounds** |
| 6 | type | `{"id": "message"}` | pass / Hello / [42,317][1038,435] | pass / Message / [40,288][1040,400] | - | - | **state** |
| 7 | tap | `{"id": "send"}` | pass / - / - | pass / Send / [40,400][260,520] | - | - | **match** |
| 8 | assert_log | `-` | pass / - / - | pass / - / - | - | Finish, Multi Activity, Received: Hello | **text-set** |
| 9 | assert_text | `{"id": "received"}` | pass / Received: Hello / [42,317][295,368] | pass / Received: Hello / [40,288][282,334] | - | - | **bounds** |
| 10 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 11 | tap | `{"id": "finish"}` | pass / - / - | pass / Finish / [40,334][260,454] | - | - | **match** |
| 12 | assert_log | `-` | pass / - / - | pass / - / - | - | Hello, Multi Activity, Send | **text-set** |
| 13 | assert_text | `{"id": "message"}` | pass / Hello / [42,317][1038,435] | pass / Hello / [40,288][1040,400] | - | - | **bounds** |
| 14 | rotate | `-` | unsupported / - / - | pass / - / - | - | Hello, Multi Activity, Send | **status** |
| 15 | assert_log | `-` | - / - / - | pass_with_failure / - / - | - | Hello, Multi Activity, Send | **missing-side** |
| 16 | assert_text | `{"id": "message"}` | - / - / - | pass / Hello / [148,220][2360,332] | - | Hello, Multi Activity, Send | **missing-side** |
| 17 | rotate | `-` | - / - / - | pass / - / - | - | Hello, Multi Activity, Send | **missing-side** |
| 18 | assert_text | `{"id": "message"}` | - / - / - | pass / Hello / [40,288][1040,400] | - | Hello, Multi Activity, Send | **missing-side** |
| 19 | back | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 20 | assert_no_crash | `-` | - / - / - | pass / - / - | - | - | **missing-side** |

Counts: bounds=6, match=3, missing-side=6, state=1, status=1, text-set=4
