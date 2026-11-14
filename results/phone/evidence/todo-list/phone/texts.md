# todo-list — phone visible text dumps

## step 0 launch [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 1 wait_for [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "New item", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 2 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "New item", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
assertion: {"kind": "text", "expected": "New item", "mode": "exact", "passed": true, "expected_bounds": [42, 317, 1038, 435]}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 3 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "Add", "identifier": "dev.acos.fixtures.todolist:id/add", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 400, 260, 520]}
assertion: {"kind": "text", "expected": "Add", "mode": "exact", "passed": true, "expected_bounds": [42, 435, 273, 561]}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 4 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "Clear all", "identifier": "dev.acos.fixtures.todolist:id/clear", "enabled": true, "checked": null, "clickable": null, "bounds": [260, 400, 480, 520]}
assertion: {"kind": "text", "expected": "Clear all", "mode": "exact", "passed": true, "expected_bounds": [273, 435, 504, 561]}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 5 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
assertion: {"kind": "log", "expected": ["^onCreate$", "^onStart$", "^onResume$"], "passed": true, "matched": ["^onCreate$", "^onStart$", "^onResume$"], "lines": ["I/AcosTodo( 4985): onCreate", "I/AcosTodo( 4985): onStart", "I/AcosTodo( 4985): onResume"]}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 6 tap [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "New item", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 7 type [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "milk", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Add", "Clear all", "Todo List", "milk"]

## step 8 tap [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "Add", "identifier": "dev.acos.fixtures.todolist:id/add", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 400, 260, 520]}
labels: ["Add", "Clear all", "New item", "Todo List", "milk"]

## step 9 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
assertion: {"kind": "log", "expected": ["^add item=milk size=1$"], "passed": true, "matched": ["^add item=milk size=1$"], "lines": ["I/AcosTodo( 4985): add item=milk size=1"]}
labels: ["Add", "Clear all", "New item", "Todo List", "milk"]

## step 10 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "milk", "identifier": "android:id/text1", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 520, 1040, 640]}
assertion: {"kind": "text", "expected": "milk", "mode": "exact", "passed": true, "expected_bounds": [42, 561, 1038, 687]}
labels: ["Add", "Clear all", "New item", "Todo List", "milk"]

## step 11 capture [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
labels: ["Add", "Clear all", "New item", "Todo List", "milk"]

## step 12 tap [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "New item", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Add", "Clear all", "New item", "Todo List", "milk"]

## step 13 type [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "bread", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Add", "Clear all", "Todo List", "bread", "milk"]

## step 14 tap [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "Add", "identifier": "dev.acos.fixtures.todolist:id/add", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 400, 260, 520]}
labels: ["Add", "Clear all", "New item", "Todo List", "bread", "milk"]

## step 15 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
assertion: {"kind": "log", "expected": ["^add item=bread size=2$"], "passed": true, "matched": ["^add item=bread size=2$"], "lines": ["I/AcosTodo( 4985): add item=bread size=2"]}
labels: ["Add", "Clear all", "New item", "Todo List", "bread", "milk"]

## step 16 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "bread", "identifier": "android:id/text1", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 643, 1040, 763]}
assertion: {"kind": "text", "expected": "bread", "mode": "exact", "passed": true, "expected_bounds": [42, 690, 1038, 816]}
labels: ["Add", "Clear all", "New item", "Todo List", "bread", "milk"]

## step 17 tap [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "milk", "identifier": "android:id/text1", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 520, 1040, 640]}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 18 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
assertion: {"kind": "log", "expected": ["^remove item=milk size=1$"], "passed": true, "matched": ["^remove item=milk size=1$"], "lines": ["I/AcosTodo( 4985): remove item=milk size=1"]}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 19 assert_not_visible [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
assertion: {"kind": "not_visible", "passed": true, "expected_absent": {"text": {"exact": "milk"}}}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 20 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "bread", "identifier": "android:id/text1", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 520, 1040, 640]}
assertion: {"kind": "text", "expected": "bread", "mode": "exact", "passed": true, "expected_bounds": [42, 561, 1038, 687]}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 21 capture [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 22 rotate [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 23 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "bread", "identifier": "android:id/text1", "enabled": true, "checked": null, "clickable": null, "bounds": [148, 452, 2360, 572]}
assertion: {"kind": "text", "expected": "bread", "mode": "exact", "passed": true, "expected_bounds": [170, 475, 2358, 601]}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 24 assert_not_visible [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
assertion: {"kind": "not_visible", "passed": true, "expected_absent": {"text": {"exact": "milk"}}}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 25 rotate [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 26 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.todolist/dev.acos.fixtures.todolist.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
target: {"text": "", "content_desc": "bread", "identifier": "android:id/text1", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 520, 1040, 640]}
assertion: {"kind": "text", "expected": "bread", "mode": "exact", "passed": true, "expected_bounds": [42, 561, 1038, 687]}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 27 back [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
labels: []

## step 28 assert_no_crash [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.todolist", "app_bundle_in_snapshot": "dev.acos.fixtures.todolist"}
assertion: {"kind": "no_crash", "passed": true, "matched": []}
labels: []
