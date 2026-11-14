# hello-views — phone visible text dumps

## step 0 launch [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 1 wait_for [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "clicks: 0", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 566, 166, 612]}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 2 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "Hello ACOS", "identifier": "dev.acos.fixtures.helloviews:id/title", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 215, 334]}
assertion: {"kind": "text", "expected": "Hello ACOS", "mode": "exact", "passed": true, "expected_bounds": [42, 317, 231, 368]}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 3 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "Enter your name", "identifier": "dev.acos.fixtures.helloviews:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 334, 1040, 446]}
assertion: {"kind": "text", "expected": "Enter your name", "mode": "exact", "passed": true, "expected_bounds": [42, 368, 1038, 486]}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 4 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "Greet", "identifier": "dev.acos.fixtures.helloviews:id/btn", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 446, 260, 566]}
assertion: {"kind": "text", "expected": "Greet", "mode": "exact", "passed": true, "expected_bounds": [42, 486, 273, 612]}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 5 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "clicks: 0", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 566, 166, 612]}
assertion: {"kind": "text", "expected": "clicks: 0", "mode": "exact", "passed": true, "expected_bounds": [42, 612, 175, 663]}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 6 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
assertion: {"kind": "log", "expected": ["^onCreate$", "^onStart$", "^onResume$"], "passed": true, "matched": ["^onCreate$", "^onStart$", "^onResume$"], "lines": ["I/AcosFixture( 5882): onCreate", "I/AcosFixture( 5882): onStart", "I/AcosFixture( 5882): onResume"]}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 7 tap [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "Enter your name", "identifier": "dev.acos.fixtures.helloviews:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 334, 1040, 446]}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 8 type [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "World", "identifier": "dev.acos.fixtures.helloviews:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 334, 1040, 446]}
labels: ["Greet", "Hello ACOS", "Hello Views", "World", "clicks: 0"]

## step 9 tap [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "Greet", "identifier": "dev.acos.fixtures.helloviews:id/btn", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 446, 260, 566]}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 1", "World"]

## step 10 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
assertion: {"kind": "log", "expected": ["^greet name=World clicks=1$"], "passed": true, "matched": ["^greet name=World clicks=1$"], "lines": ["I/AcosFixture( 5882): greet name=World clicks=1"]}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 1", "World"]

## step 11 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "Hello, World! clicks: 1", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 566, 372, 612]}
assertion: {"kind": "text", "expected": "Hello, World! clicks: 1", "mode": "exact", "passed": true, "expected_bounds": [42, 612, 392, 663]}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 1", "World"]

## step 12 capture [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 1", "World"]

## step 13 tap [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "Greet", "identifier": "dev.acos.fixtures.helloviews:id/btn", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 446, 260, 566]}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 2", "World"]

## step 14 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
assertion: {"kind": "log", "expected": ["^greet name=World clicks=2$"], "passed": true, "matched": ["^greet name=World clicks=2$"], "lines": ["I/AcosFixture( 5882): greet name=World clicks=2"]}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 2", "World"]

## step 15 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "Hello, World! clicks: 2", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 566, 378, 612]}
assertion: {"kind": "text", "expected": "Hello, World! clicks: 2", "mode": "exact", "passed": true, "expected_bounds": [42, 612, 392, 663]}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 2", "World"]

## step 16 rotate [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
labels: ["Greet", "Hello ACOS", "Hello Views", "World", "clicks: 2"]

## step 17 assert_log [pass_with_failure]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
assertion: {"kind": "log", "expected": ["^onPause$", "^onStop$", "^onSaveInstanceState clicks=2$", "^onDestroy$", "^onCreate$", "^onStart$", "^onResume$"], "passed": false, "matched": ["^onPause$", "^onStop$", "^onSaveInstanceState clicks=2$", "^onDestroy$"], "lines": ["I/AcosFixture( 5882): onPause", "I/AcosFixture( 5882): onStop", "I/AcosFixture( 5882): onSaveInstanceState clicks=2", "I/AcosFixture( 5882):
labels: ["Greet", "Hello ACOS", "Hello Views", "World", "clicks: 2"]

## step 18 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "World", "identifier": "dev.acos.fixtures.helloviews:id/input", "enabled": true, "checked": null, "clickable": null, "bounds": [148, 266, 2360, 378]}
assertion: {"kind": "text", "expected": "World", "mode": "exact", "passed": true, "expected_bounds": [170, 282, 2358, 400]}
labels: ["Greet", "Hello ACOS", "Hello Views", "World", "clicks: 2"]

## step 19 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "clicks: 2", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": null, "clickable": null, "bounds": [148, 498, 273, 544]}
assertion: {"kind": "text", "expected": "clicks: 2", "mode": "exact", "passed": true, "expected_bounds": [170, 526, 303, 577]}
labels: ["Greet", "Hello ACOS", "Hello Views", "World", "clicks: 2"]

## step 20 rotate [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
labels: ["Greet", "Hello ACOS", "Hello Views", "World", "clicks: 2"]

## step 21 assert_log [pass_with_failure]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
assertion: {"kind": "log", "expected": ["^onPause$", "^onStop$", "^onSaveInstanceState clicks=2$", "^onDestroy$", "^onCreate$", "^onStart$", "^onResume$"], "passed": false, "matched": [], "lines": []}
labels: ["Greet", "Hello ACOS", "Hello Views", "World", "clicks: 2"]

## step 22 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.helloviews/dev.acos.fixtures.helloviews.MainActivity", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
target: {"text": "", "content_desc": "clicks: 2", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 566, 165, 612]}
assertion: {"kind": "text", "expected": "clicks: 2", "mode": "exact", "passed": true, "expected_bounds": [42, 612, 175, 663]}
labels: ["Greet", "Hello ACOS", "Hello Views", "World", "clicks: 2"]

## step 23 back [pass]
screen: {"activity": "com.motorola.launcher3/com.android.launcher3.CustomizationPanelLauncher", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
labels: []

## step 24 assert_log [pass_with_failure]
screen: {"activity": "com.motorola.launcher3/com.android.launcher3.CustomizationPanelLauncher", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
assertion: {"kind": "log", "expected": ["^onPause$", "^onStop$", "^onSaveInstanceState clicks=2$", "^onDestroy$"], "passed": false, "matched": [], "lines": []}
labels: []

## step 25 assert_no_crash [pass]
screen: {"activity": "com.motorola.launcher3/com.android.launcher3.CustomizationPanelLauncher", "package": "dev.acos.fixtures.helloviews", "app_bundle_in_snapshot": "dev.acos.fixtures.helloviews"}
assertion: {"kind": "no_crash", "passed": true, "matched": []}
labels: []
