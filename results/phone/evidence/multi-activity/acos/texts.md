# multi-activity — acos visible text dumps

## step 0 launch [pass]
screen: {}
labels: []

## step 1 wait_for [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 2, "content_hash": "315a9c0fd8e661741833b42cc28b791b"}
target: {"text": "Message", "content_desc": "", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Message", "Multi Activity", "Send"]

## step 2 assert_text [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 2, "content_hash": "315a9c0fd8e661741833b42cc28b791b"}
target: {"text": "Message", "content_desc": "", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Message"}, "message": null}
labels: ["Message", "Multi Activity", "Send"]

## step 3 assert_text [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 2, "content_hash": "315a9c0fd8e661741833b42cc28b791b"}
target: {"text": "Send", "content_desc": "", "identifier": "dev.acos.fixtures.multiactivity:id/send", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 435, 273, 561]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Send"}, "message": null}
labels: ["Message", "Multi Activity", "Send"]

## step 4 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 5 tap [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 4, "content_hash": "315a9c0fd8e661741833b42cc28b791b"}
target: {"text": "Message", "content_desc": "", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Message", "Multi Activity", "Send"]

## step 6 type [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 5, "content_hash": "fd2af905b97fa46eba23a8bf2aa1a884"}
target: {"text": "Hello", "content_desc": "", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Hello", "Multi Activity", "Send"]

## step 7 tap [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 7, "content_hash": "3f2ec47d46c3aafeb245d070dff58806"}
labels: ["Finish", "Multi Activity", "Received: Hello"]

## step 8 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 9 assert_text [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 7, "content_hash": "3f2ec47d46c3aafeb245d070dff58806"}
target: {"text": "Received: Hello", "content_desc": "", "identifier": "dev.acos.fixtures.multiactivity:id/received", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 317, 295, 368]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Received: Hello"}, "message": null}
labels: ["Finish", "Multi Activity", "Received: Hello"]

## step 10 capture [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 8, "content_hash": "3f2ec47d46c3aafeb245d070dff58806"}
labels: ["Finish", "Multi Activity", "Received: Hello"]

## step 11 tap [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 10, "content_hash": "652c4bdb392e70f90b21f0485ce4f31d"}
labels: ["Hello", "Multi Activity", "Send"]

## step 12 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 13 assert_text [pass]
screen: {"package": "dev.acos.fixtures.multiactivity", "frame_seq": 10, "content_hash": "652c4bdb392e70f90b21f0485ce4f31d"}
target: {"text": "Hello", "content_desc": "", "identifier": "dev.acos.fixtures.multiactivity:id/message", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Hello"}, "message": null}
labels: ["Hello", "Multi Activity", "Send"]

## step 14 rotate [unsupported]
screen: {}
labels: []
