# hello-views — acos visible text dumps

## step 0 launch [pass]
screen: {}
labels: []

## step 1 wait_for [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 2, "content_hash": "5df5291229d894e1fe6ec119164dad7d"}
target: {"text": "clicks: 0", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 612, 175, 663]}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 2 assert_text [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 2, "content_hash": "5df5291229d894e1fe6ec119164dad7d"}
target: {"text": "Hello ACOS", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/title", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 317, 231, 368]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Hello ACOS"}, "message": null}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 3 assert_text [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 2, "content_hash": "5df5291229d894e1fe6ec119164dad7d"}
target: {"text": "Enter your name", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/input", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 368, 1038, 486]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Enter your name"}, "message": null}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 4 assert_text [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 2, "content_hash": "5df5291229d894e1fe6ec119164dad7d"}
target: {"text": "Greet", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/btn", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 486, 273, 612]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Greet"}, "message": null}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 5 assert_text [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 2, "content_hash": "5df5291229d894e1fe6ec119164dad7d"}
target: {"text": "clicks: 0", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 612, 175, 663]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "clicks: 0"}, "message": null}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 6 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 7 tap [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 4, "content_hash": "5df5291229d894e1fe6ec119164dad7d"}
target: {"text": "Enter your name", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/input", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 368, 1038, 486]}
labels: ["Enter your name", "Greet", "Hello ACOS", "Hello Views", "clicks: 0"]

## step 8 type [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 5, "content_hash": "004391bac1374febcb13b822b109fd68"}
target: {"text": "World", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/input", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 368, 1038, 486]}
labels: ["Greet", "Hello ACOS", "Hello Views", "World", "clicks: 0"]

## step 9 tap [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 7, "content_hash": "b2c17978115e82d17b0d1a4b09e82bd2"}
target: {"text": "Greet", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/btn", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 486, 273, 612]}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 1", "World"]

## step 10 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 11 assert_text [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 7, "content_hash": "b2c17978115e82d17b0d1a4b09e82bd2"}
target: {"text": "Hello, World! clicks: 1", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 612, 392, 663]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Hello, World! clicks: 1"}, "message": null}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 1", "World"]

## step 12 capture [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 8, "content_hash": "b2c17978115e82d17b0d1a4b09e82bd2"}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 1", "World"]

## step 13 tap [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 10, "content_hash": "f22f8c3bf2324716228d2dc42a347c95"}
target: {"text": "Greet", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/btn", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 486, 273, 612]}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 2", "World"]

## step 14 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 15 assert_text [pass]
screen: {"package": "dev.acos.fixtures.helloviews", "frame_seq": 10, "content_hash": "f22f8c3bf2324716228d2dc42a347c95"}
target: {"text": "Hello, World! clicks: 2", "content_desc": "", "identifier": "dev.acos.fixtures.helloviews:id/result", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 612, 392, 663]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Hello, World! clicks: 2"}, "message": null}
labels: ["Greet", "Hello ACOS", "Hello Views", "Hello, World! clicks: 2", "World"]

## step 16 rotate [unsupported]
screen: {}
labels: []
