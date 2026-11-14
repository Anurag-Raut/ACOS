# form-validation — acos visible text dumps

## step 0 launch [pass]
screen: {}
labels: []

## step 1 wait_for [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 2, "content_hash": "001a6665b419e7ee6a2908d6354cf081"}
target: {"text": "Submit", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 553, 273, 679]}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 2 assert_text [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 2, "content_hash": "001a6665b419e7ee6a2908d6354cf081"}
target: {"text": "Name", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/name", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Name"}, "message": null}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 3 assert_text [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 2, "content_hash": "001a6665b419e7ee6a2908d6354cf081"}
target: {"text": "Email", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/email", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 435, 1038, 553]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Email"}, "message": null}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 4 assert_text [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 2, "content_hash": "001a6665b419e7ee6a2908d6354cf081"}
target: {"text": "Submit", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 553, 273, 679]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Submit"}, "message": null}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 5 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 6 tap [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 4, "content_hash": "e4d877086930eab3a63c8d091eaebe32"}
target: {"text": "Submit", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 553, 273, 679]}
labels: ["Email", "Form Validation", "Name", "Name is required", "Submit"]

## step 7 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 8 assert_text [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 4, "content_hash": "e4d877086930eab3a63c8d091eaebe32"}
target: {"text": "Name is required", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/error", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 679, 323, 730]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Name is required"}, "message": null}
labels: ["Email", "Form Validation", "Name", "Name is required", "Submit"]

## step 9 tap [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 6, "content_hash": "e4d877086930eab3a63c8d091eaebe32"}
target: {"text": "Name", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/name", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Email", "Form Validation", "Name", "Name is required", "Submit"]

## step 10 type [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 7, "content_hash": "948384e79b82610d575259951530aa52"}
target: {"text": "Ada", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/name", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 317, 1038, 435]}
labels: ["Ada", "Email", "Form Validation", "Name is required", "Submit"]

## step 11 tap [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 9, "content_hash": "2ca7498fc12171a93e7fee5010b639a1"}
target: {"text": "Submit", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 553, 273, 679]}
labels: ["Ada", "Email", "Email is required", "Form Validation", "Submit"]

## step 12 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 13 assert_text [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 9, "content_hash": "2ca7498fc12171a93e7fee5010b639a1"}
target: {"text": "Email is required", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/error", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 679, 316, 730]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Email is required"}, "message": null}
labels: ["Ada", "Email", "Email is required", "Form Validation", "Submit"]

## step 14 tap [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 11, "content_hash": "e89b3aeb54ca41d02d1c2b4570ab366b"}
target: {"text": "Email", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/email", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 435, 1038, 553]}
labels: ["Ada", "Email", "Email is required", "Form Validation", "Submit"]

## step 15 type [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 12, "content_hash": "10dc03a3b9d48a0f872f8f7710b0c92f"}
target: {"text": "not-an-email", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/email", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 435, 1038, 553]}
labels: ["Ada", "Email is required", "Form Validation", "Submit", "not-an-email"]

## step 16 tap [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 14, "content_hash": "a6f5edaaa7384fc3eb37cfa4036cafc3"}
target: {"text": "Submit", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 553, 273, 679]}
labels: ["Ada", "Form Validation", "Invalid email", "Submit", "not-an-email"]

## step 17 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 18 assert_text [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 14, "content_hash": "a6f5edaaa7384fc3eb37cfa4036cafc3"}
target: {"text": "Invalid email", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/error", "enabled": true, "checked": false, "clickable": false, "bounds": [42, 679, 248, 730]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Invalid email"}, "message": null}
labels: ["Ada", "Form Validation", "Invalid email", "Submit", "not-an-email"]

## step 19 capture [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 15, "content_hash": "a6f5edaaa7384fc3eb37cfa4036cafc3"}
labels: ["Ada", "Form Validation", "Invalid email", "Submit", "not-an-email"]

## step 20 type [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 29, "content_hash": "fcc872022adeb7d313e0d9470d41e171"}
target: {"text": "ada@example.com", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/email", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 435, 1038, 553]}
labels: ["Ada", "Form Validation", "Invalid email", "Submit", "ada@example.com"]

## step 21 tap [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 31, "content_hash": "410a2b4bc3760c224d878f9142b6e3a3"}
target: {"text": "Submit", "content_desc": "", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": false, "clickable": true, "bounds": [42, 553, 273, 679]}
labels: ["Ada", "Form Validation", "OK", "Submit", "Submitted: Ada", "Success", "ada@example.com"]

## step 22 assert_log [pass]
screen: {}
assertion: {"kind": "log", "passed": true, "expected": null, "message": null}
labels: []

## step 23 assert_text [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 31, "content_hash": "410a2b4bc3760c224d878f9142b6e3a3"}
target: {"text": "Submitted: Ada", "content_desc": "", "identifier": "android:id/message", "enabled": true, "checked": false, "clickable": false, "bounds": [70, 1157, 1010, 1214]}
assertion: {"kind": "text", "passed": true, "expected": {"exact": "Submitted: Ada"}, "message": null}
labels: ["Ada", "Form Validation", "OK", "Submit", "Submitted: Ada", "Success", "ada@example.com"]

## step 24 capture [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 32, "content_hash": "410a2b4bc3760c224d878f9142b6e3a3"}
labels: ["Ada", "Form Validation", "OK", "Submit", "Submitted: Ada", "Success", "ada@example.com"]

## step 25 assert_region [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 33, "content_hash": "909bf1e4b76d9f7584e7a2c67abcfb02"}
assertion: {"kind": "region", "passed": true, "expected": null, "message": "display region [200,1038,200,40] (frame [200,910]) matched (hash=e753006d4f666db1697d1562fda113a024aa37c5dc76e492514832ff346ae79c)"}
labels: ["Ada", "Form Validation", "OK", "Submit", "Submitted: Ada", "Success", "ada@example.com"]

## step 26 assert_region [pass]
screen: {"package": "dev.acos.fixtures.formvalidation", "frame_seq": 34, "content_hash": "909bf1e4b76d9f7584e7a2c67abcfb02"}
assertion: {"kind": "region", "passed": true, "expected": null, "message": "display region [90,1128,40,100] (frame [90,1000]) matched (hash=a72e051873a520049475589e02061ef34a2445a43be991d44228bb3a2cc9be29)"}
labels: ["Ada", "Form Validation", "OK", "Submit", "Submitted: Ada", "Success", "ada@example.com"]

## step 27 assert_no_crash [pass]
screen: {}
assertion: {"kind": "no_crash", "passed": true, "expected": null, "message": "no crash recorded"}
labels: []
