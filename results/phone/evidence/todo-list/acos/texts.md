# todo-list — acos visible text dumps

## step 0 launch [pass]
screen: {}
labels: []

## step 1 wait_for [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 2, "content_hash": "05bc2dbf8d28bc03a231a46934f7e1a2"}
target: {"text": "New item", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 2 assert_text [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 2, "content_hash": "05bc2dbf8d28bc03a231a46934f7e1a2"}
target: {"text": "New item", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "New item"}, "message": null}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 3 assert_text [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 2, "content_hash": "05bc2dbf8d28bc03a231a46934f7e1a2"}
target: {"text": "Add", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/add", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 435, 273, 561]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Add"}, "message": null}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 4 assert_text [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 2, "content_hash": "05bc2dbf8d28bc03a231a46934f7e1a2"}
target: {"text": "Clear all", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/clear", "enabled": true, "checked": false, "clickable": true, "bounds": [273, 435, 504, 561]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Clear all"}, "message": null}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 5 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 6 tap [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 4, "content_hash": "05bc2dbf8d28bc03a231a46934f7e1a2"}
target: {"text": "New item", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Add", "Clear all", "New item", "Todo List"]

## step 7 type [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 5, "content_hash": "8ef09fbe75c3d70e9ea9e7db40dc3db8"}
target: {"text": "milk", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Add", "Clear all", "Todo List", "milk"]

## step 8 tap [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 7, "content_hash": "facefebe22778b40df8d258ea65430f1"}
target: {"text": "Add", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/add", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 435, 273, 561]}
labels: ["Add", "Clear all", "New item", "Todo List", "milk"]

## step 9 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 10 assert_text [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 7, "content_hash": "facefebe22778b40df8d258ea65430f1"}
target: {"text": "milk", "content_desc": "", "identifier": "android:id/text1", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 561, 1038, 687]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "milk"}, "message": null}
labels: ["Add", "Clear all", "New item", "Todo List", "milk"]

## step 11 capture [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 8, "content_hash": "aa183105de0a10d0da572ae6ffc4f7ef"}
labels: ["Add", "Clear all", "New item", "Todo List", "milk"]

## step 12 tap [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 10, "content_hash": "facefebe22778b40df8d258ea65430f1"}
target: {"text": "New item", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Add", "Clear all", "New item", "Todo List", "milk"]

## step 13 type [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 11, "content_hash": "a3cf8eff4d4f1d504b20197950df02d5"}
target: {"text": "bread", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/input", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Add", "Clear all", "Todo List", "bread", "milk"]

## step 14 tap [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 13, "content_hash": "f3b1f57ee9f283d302c6f712f1a01c2c"}
target: {"text": "Add", "content_desc": "", "identifier": "dev.acos.fixtures.todolist:id/add", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 435, 273, 561]}
labels: ["Add", "Clear all", "New item", "Todo List", "bread", "milk"]

## step 15 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 16 assert_text [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 13, "content_hash": "f3b1f57ee9f283d302c6f712f1a01c2c"}
target: {"text": "bread", "content_desc": "", "identifier": "android:id/text1", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 690, 1038, 816]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "bread"}, "message": null}
labels: ["Add", "Clear all", "New item", "Todo List", "bread", "milk"]

## step 17 tap [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 14, "content_hash": "a5e134739072918bd7339e009405a95c"}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 18 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 19 assert_not_visible [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 14, "content_hash": "a5e134739072918bd7339e009405a95c"}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 20 assert_text [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 14, "content_hash": "a5e134739072918bd7339e009405a95c"}
target: {"text": "bread", "content_desc": "", "identifier": "android:id/text1", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 561, 1038, 687]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "bread"}, "message": null}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 21 capture [pass]
screen: {"package": "dev.acos.fixtures.todolist", "frame_seq": 15, "content_hash": "a5e134739072918bd7339e009405a95c"}
labels: ["Add", "Clear all", "New item", "Todo List", "bread"]

## step 22 rotate [unsupported]
screen: {}
labels: []
