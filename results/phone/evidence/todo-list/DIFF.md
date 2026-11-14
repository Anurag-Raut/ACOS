### todo-list

| step | kind | selector | ACOS status/target/bounds | phone status/target/bounds | text only ACOS | text only phone | class |
|---|---|---|---|---|---|---|---|
| 0 | launch | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List | **text-set** |
| 1 | wait_for | `{"id": "input"}` | pass / New item / [42,317][1038,435] | pass / New item / [40,288][1040,400] | - | - | **bounds** |
| 2 | assert_text | `{"id": "input"}` | pass / New item / [42,317][1038,435] | pass / New item / [40,288][1040,400] | - | - | **bounds** |
| 3 | assert_text | `{"id": "add"}` | pass / Add / [42,435][273,561] | pass / Add / [40,400][260,520] | - | - | **bounds** |
| 4 | assert_text | `{"id": "clear"}` | pass / Clear all / [273,435][504,561] | pass / Clear all / [260,400][480,520] | - | - | **bounds** |
| 5 | assert_log | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List | **text-set** |
| 6 | tap | `{"id": "input"}` | pass / New item / [42,317][1038,435] | pass / New item / [40,288][1040,400] | - | - | **bounds** |
| 7 | type | `{"id": "input"}` | pass / milk / [42,317][1038,435] | pass / milk / [40,288][1040,400] | - | - | **bounds** |
| 8 | tap | `{"id": "add"}` | pass / Add / [42,435][273,561] | pass / Add / [40,400][260,520] | - | - | **bounds** |
| 9 | assert_log | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, milk | **text-set** |
| 10 | assert_text | `{"text": {"exact": "milk"}}` | pass / milk / [42,561][1038,687] | pass / milk / [40,520][1040,640] | - | - | **bounds** |
| 11 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 12 | tap | `{"id": "input"}` | pass / New item / [42,317][1038,435] | pass / New item / [40,288][1040,400] | - | - | **bounds** |
| 13 | type | `{"id": "input"}` | pass / bread / [42,317][1038,435] | pass / bread / [40,288][1040,400] | - | - | **bounds** |
| 14 | tap | `{"id": "add"}` | pass / Add / [42,435][273,561] | pass / Add / [40,400][260,520] | - | - | **bounds** |
| 15 | assert_log | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread, milk | **text-set** |
| 16 | assert_text | `{"text": {"exact": "bread"}}` | pass / bread / [42,690][1038,816] | pass / bread / [40,643][1040,763] | - | - | **bounds** |
| 17 | tap | `{"text": {"exact": "milk"}}` | pass / - / - | pass / milk / [40,520][1040,640] | - | - | **match** |
| 18 | assert_log | `-` | pass / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread | **text-set** |
| 19 | assert_not_visible | `{"text": {"exact": "milk"}}` | pass / - / - | pass / - / - | - | - | **match** |
| 20 | assert_text | `{"text": {"exact": "bread"}}` | pass / bread / [42,561][1038,687] | pass / bread / [40,520][1040,640] | - | - | **bounds** |
| 21 | capture | `-` | pass / - / - | pass / - / - | - | - | **match** |
| 22 | rotate | `-` | unsupported / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread | **status** |
| 23 | assert_text | `{"text": {"exact": "bread"}}` | - / - / - | pass / bread / [148,452][2360,572] | - | Add, Clear all, New item, Todo List, bread | **missing-side** |
| 24 | assert_not_visible | `{"text": {"exact": "milk"}}` | - / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread | **missing-side** |
| 25 | rotate | `-` | - / - / - | pass / - / - | - | Add, Clear all, New item, Todo List, bread | **missing-side** |
| 26 | assert_text | `{"text": {"exact": "bread"}}` | - / - / - | pass / bread / [40,520][1040,640] | - | Add, Clear all, New item, Todo List, bread | **missing-side** |
| 27 | back | `-` | - / - / - | pass / - / - | - | - | **missing-side** |
| 28 | assert_no_crash | `-` | - / - / - | pass / - / - | - | - | **missing-side** |

Counts: bounds=13, match=4, missing-side=6, status=1, text-set=5
