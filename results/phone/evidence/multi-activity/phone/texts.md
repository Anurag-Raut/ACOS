# multi-activity — phone visible text dumps

## step 0 launch [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
labels: ["Message", "Multi Activity", "Send"]

## step 1 wait_for [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Message", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Message", "Multi Activity", "Send"]

## step 2 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Message", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
assertion: {"kind": "text", "expected": "Message", "mode": "exact", "passed": true, "expected_bounds": [42, 317, 1038, 435]}
labels: ["Message", "Multi Activity", "Send"]

## step 3 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Send", "identifier": "dev.acos.fixtures.multiactivity:id/send", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 400, 260, 520]}
assertion: {"kind": "text", "expected": "Send", "mode": "exact", "passed": true, "expected_bounds": [42, 435, 273, 561]}
labels: ["Message", "Multi Activity", "Send"]

## step 4 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
assertion: {"kind": "log", "expected": ["^Main.onCreate$", "^Main.onStart$", "^Main.onResume$"], "passed": true, "matched": ["^Main.onCreate$", "^Main.onStart$", "^Main.onResume$"], "lines": ["I/AcosMulti( 6600): Main.onCreate", "I/AcosMulti( 6600): Main.onStart", "I/AcosMulti( 6600): Main.onResume"]}
labels: ["Message", "Multi Activity", "Send"]

## step 5 tap [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Message", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Message", "Multi Activity", "Send"]

## step 6 type [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Hello", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Hello", "Multi Activity", "Send"]

## step 7 tap [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.SecondActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Send", "identifier": "dev.acos.fixtures.multiactivity:id/send", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 400, 260, 520]}
labels: ["Finish", "Multi Activity", "Received: Hello"]

## step 8 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.SecondActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
assertion: {"kind": "log", "expected": ["^Main.send message=Hello$", "^Second.onCreate$", "^Second.received=Hello$"], "passed": true, "matched": ["^Main.send message=Hello$", "^Second.onCreate$", "^Second.received=Hello$"], "lines": ["I/AcosMulti( 6600): Main.send message=Hello", "I/AcosMulti( 6600): Main.onPause", "I/AcosMulti( 6600): Second.onCreate", "I/AcosMulti( 6600): Second.received=Hello", "I/AcosMul
labels: ["Finish", "Multi Activity", "Received: Hello"]

## step 9 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.SecondActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Received: Hello", "identifier": "dev.acos.fixtures.multiactivity:id/received", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 282, 334]}
assertion: {"kind": "text", "expected": "Received: Hello", "mode": "exact", "passed": true, "expected_bounds": [42, 317, 295, 368]}
labels: ["Finish", "Multi Activity", "Received: Hello"]

## step 10 capture [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.SecondActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
labels: ["Finish", "Multi Activity", "Received: Hello"]

## step 11 tap [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Finish", "identifier": "dev.acos.fixtures.multiactivity:id/finish", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 334, 260, 454]}
labels: ["Hello", "Multi Activity", "Send"]

## step 12 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
assertion: {"kind": "log", "expected": ["^Second.finish$"], "passed": true, "matched": ["^Second.finish$"], "lines": ["I/AcosMulti( 6600): Second.finish", "I/AcosMulti( 6600): Second.onPause", "I/AcosMulti( 6600): Second.onStop", "I/AcosMulti( 6600): Second.onDestroy"]}
labels: ["Hello", "Multi Activity", "Send"]

## step 13 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Hello", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
assertion: {"kind": "text", "expected": "Hello", "mode": "exact", "passed": true, "expected_bounds": [42, 317, 1038, 435]}
labels: ["Hello", "Multi Activity", "Send"]

## step 14 rotate [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
labels: ["Hello", "Multi Activity", "Send"]

## step 15 assert_log [pass_with_failure]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
assertion: {"kind": "log", "expected": ["^Main.onSaveInstanceState message=Hello$"], "passed": false, "matched": [], "lines": ["I/AcosMulti( 6600): Main.onDestroy", "I/AcosMulti( 6600): Main.restored message=Hello"]}
labels: ["Hello", "Multi Activity", "Send"]

## step 16 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Hello", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": null, "clickable": null, "bounds": [148, 220, 2360, 332]}
assertion: {"kind": "text", "expected": "Hello", "mode": "exact", "passed": true, "expected_bounds": [170, 231, 2358, 349]}
labels: ["Hello", "Multi Activity", "Send"]

## step 17 rotate [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
labels: ["Hello", "Multi Activity", "Send"]

## step 18 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.multiactivity/dev.acos.fixtures.multiactivity.MainActivity", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
target: {"text": "", "content_desc": "Hello", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
assertion: {"kind": "text", "expected": "Hello", "mode": "exact", "passed": true, "expected_bounds": [42, 317, 1038, 435]}
labels: ["Hello", "Multi Activity", "Send"]

## step 19 back [pass]
screen: {"activity": "com.motorola.launcher3/com.android.launcher3.CustomizationPanelLauncher", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
labels: []

## step 20 assert_no_crash [pass]
screen: {"activity": "com.motorola.launcher3/com.android.launcher3.CustomizationPanelLauncher", "package": "dev.acos.fixtures.multiactivity", "app_bundle_in_snapshot": "dev.acos.fixtures.multiactivity"}
assertion: {"kind": "no_crash", "passed": true, "matched": []}
labels: []
