### form-validation

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Email, Form Validation, Name, Submit | **text-set** |
| 1 | wait_for | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 2 | assert_text | `{"id": "name"}` | pass / Name / [42,317][1038,435] | pass / Name / [40,288][1040,400] | - | - | **bounds** |
| 3 | assert_text | `{"id": "email"}` | pass / Email / [42,435][1038,553] | pass / Email / [40,400][1040,512] | - | - | **bounds** |
| 4 | assert_text | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 5 | assert_log | `-` | pass / - / - | pass / - / - | - | Email, Form Validation, Name, Submit | **text-set** |
| 6 | tap | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 7 | assert_log | `-` | pass / - / - | pass / - / - | - | Email, Form Validation, Name, Name is required, Submit | **text-set** |
| 8 | assert_text | `{"id": "error"}` | pass / Name is required / [42,679][323,730] | pass / Name is required / [40,632][312,678] | - | - | **bounds** |
| 9 | tap | `{"id": "name"}` | pass / Name / [42,317][1038,435] | pass / Name / [40,288][1040,400] | - | - | **bounds** |
| 10 | type | `{"id": "name"}` | pass / Ada / [42,317][1038,435] | pass / Ada / [40,288][1040,400] | - | - | **bounds** |
| 11 | tap | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 12 | assert_log | `-` | pass / - / - | pass / - / - | - | Ada, Email, Email is required, Form Validation, Submit | **text-set** |
| 13 | assert_text | `{"id": "error"}` | pass / Email is required / [42,679][316,730] | pass / Email is required / [40,632][307,678] | - | - | **bounds** |
| 14 | tap | `{"id": "email"}` | pass / Email / [42,435][1038,553] | pass / Email / [40,400][1040,512] | - | - | **bounds** |
| 15 | type | `{"id": "email"}` | pass / not-an-email / [42,435][1038,553] | pass / not-an-email / [40,400][1040,512] | - | - | **bounds** |
| 16 | tap | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | - | - | **bounds** |
| 17 | assert_log | `-` | pass / - / - | pass / - / - | - | Ada, Form Validation, Invalid email, Submit, not-an-email | **text-set** |
| 18 | assert_text | `{"id": "error"}` | pass / Invalid email / [42,679][248,730] | pass / Invalid email / [40,632][242,678] | - | - | **bounds** |
| 19 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 20 | type | `{"id": "email"}` | pass / ada@example.com / [42,435][1038,553] | pass / ada@example.com / [40,400][1040,512] | - | - | **bounds** |
| 21 | tap | `{"id": "submit"}` | pass / Submit / [42,553][273,679] | pass / Submit / [40,512][260,632] | Ada, Form Validation, Submit, ada@example.com | - | **bounds** |
| 22 | assert_log | `-` | pass / - / - | pass / - / - | - | OK, Submitted: Ada, Success | **text-set** |
| 23 | assert_text | `{"id": "message"}` | pass / Submitted: Ada / [70,1157][1010,1214] | pass / Submitted: Ada / [67,1182][1013,1236] | Ada, Form Validation, Submit, ada@example.com | - | **bounds** |
| 24 | capture | `-` | pass / - / - | pass / - / - | Ada, Form Validation, Submit, ada@example.com | - | **text-set** |
| 25 | assert_region | `-` | pass / - / - | not_compared / - / - | Ada, Form Validation, Submit, ada@example.com | - | **text-set** |
| 26 | assert_region | `-` | pass / - / - | not_compared / - / - | Ada, Form Validation, Submit, ada@example.com | - | **text-set** |
| 27 | assert_no_crash | `-` | pass / - / - | pass / - / - | - | OK, Submitted: Ada, Success | **text-set** |

Counts: bounds=17, match=1, text-set=10
