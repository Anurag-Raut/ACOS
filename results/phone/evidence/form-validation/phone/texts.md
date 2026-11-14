# form-validation — phone visible text dumps

## step 0 launch [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 1 wait_for [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Submit", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 512, 260, 632]}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 2 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Name", "identifier": "dev.acos.fixtures.formvalidation:id/name", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
assertion: {"kind": "text", "expected": "Name", "mode": "exact", "passed": true, "expected_bounds": [42, 317, 1038, 435]}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 3 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Email", "identifier": "dev.acos.fixtures.formvalidation:id/email", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 400, 1040, 512]}
assertion: {"kind": "text", "expected": "Email", "mode": "exact", "passed": true, "expected_bounds": [42, 435, 1038, 553]}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 4 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Submit", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 512, 260, 632]}
assertion: {"kind": "text", "expected": "Submit", "mode": "exact", "passed": true, "expected_bounds": [42, 553, 273, 679]}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 5 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
assertion: {"kind": "log", "expected": ["^onCreate$", "^onStart$", "^onResume$"], "passed": true, "matched": ["^onCreate$", "^onStart$", "^onResume$"], "lines": ["I/AcosForm( 4121): onCreate", "I/AcosForm( 4121): onStart", "I/AcosForm( 4121): onResume"]}
labels: ["Email", "Form Validation", "Name", "Submit"]

## step 6 tap [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Submit", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 512, 260, 632]}
labels: ["Email", "Form Validation", "Name", "Name is required", "Submit"]

## step 7 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
assertion: {"kind": "log", "expected": ["^validation_failed reason=name message=Name is required$"], "passed": true, "matched": ["^validation_failed reason=name message=Name is required$"], "lines": ["I/AcosForm( 4121): validation_failed reason=name message=Name is required"]}
labels: ["Email", "Form Validation", "Name", "Name is required", "Submit"]

## step 8 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Name is required", "identifier": "dev.acos.fixtures.formvalidation:id/error", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 632, 312, 678]}
assertion: {"kind": "text", "expected": "Name is required", "mode": "exact", "passed": true, "expected_bounds": [42, 679, 323, 730]}
labels: ["Email", "Form Validation", "Name", "Name is required", "Submit"]

## step 9 tap [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Name", "identifier": "dev.acos.fixtures.formvalidation:id/name", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Email", "Form Validation", "Name", "Name is required", "Submit"]

## step 10 type [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Ada", "identifier": "dev.acos.fixtures.formvalidation:id/name", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 288, 1040, 400]}
labels: ["Ada", "Email", "Form Validation", "Name is required", "Submit"]

## step 11 tap [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Submit", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 512, 260, 632]}
labels: ["Ada", "Email", "Email is required", "Form Validation", "Submit"]

## step 12 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
assertion: {"kind": "log", "expected": ["^validation_failed reason=email message=Email is required$"], "passed": true, "matched": ["^validation_failed reason=email message=Email is required$"], "lines": ["I/AcosForm( 4121): validation_failed reason=email message=Email is required"]}
labels: ["Ada", "Email", "Email is required", "Form Validation", "Submit"]

## step 13 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Email is required", "identifier": "dev.acos.fixtures.formvalidation:id/error", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 632, 307, 678]}
assertion: {"kind": "text", "expected": "Email is required", "mode": "exact", "passed": true, "expected_bounds": [42, 679, 316, 730]}
labels: ["Ada", "Email", "Email is required", "Form Validation", "Submit"]

## step 14 tap [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Email", "identifier": "dev.acos.fixtures.formvalidation:id/email", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 400, 1040, 512]}
labels: ["Ada", "Email", "Email is required", "Form Validation", "Submit"]

## step 15 type [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "not-an-email", "identifier": "dev.acos.fixtures.formvalidation:id/email", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 400, 1040, 512]}
labels: ["Ada", "Email is required", "Form Validation", "Submit", "not-an-email"]

## step 16 tap [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Submit", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 512, 260, 632]}
labels: ["Ada", "Form Validation", "Invalid email", "Submit", "not-an-email"]

## step 17 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
assertion: {"kind": "log", "expected": ["^validation_failed reason=email_format message=Invalid email$"], "passed": true, "matched": ["^validation_failed reason=email_format message=Invalid email$"], "lines": ["I/AcosForm( 4121): validation_failed reason=email_format message=Invalid email"]}
labels: ["Ada", "Form Validation", "Invalid email", "Submit", "not-an-email"]

## step 18 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Invalid email", "identifier": "dev.acos.fixtures.formvalidation:id/error", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 632, 242, 678]}
assertion: {"kind": "text", "expected": "Invalid email", "mode": "exact", "passed": true, "expected_bounds": [42, 679, 248, 730]}
labels: ["Ada", "Form Validation", "Invalid email", "Submit", "not-an-email"]

## step 19 capture [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
labels: ["Ada", "Form Validation", "Invalid email", "Submit", "not-an-email"]

## step 20 type [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "ada@example.com", "identifier": "dev.acos.fixtures.formvalidation:id/email", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 400, 1040, 512]}
labels: ["Ada", "Form Validation", "Invalid email", "Submit", "ada@example.com"]

## step 21 tap [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Submit", "identifier": "dev.acos.fixtures.formvalidation:id/submit", "enabled": true, "checked": null, "clickable": null, "bounds": [40, 512, 260, 632]}
labels: ["OK", "Submitted: Ada", "Success"]

## step 22 assert_log [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
assertion: {"kind": "log", "expected": ["^validation_ok name=Ada email=ada@example.com$"], "passed": true, "matched": ["^validation_ok name=Ada email=ada@example.com$"], "lines": ["I/AcosForm( 4121): validation_ok name=Ada email=ada@example.com"]}
labels: ["OK", "Submitted: Ada", "Success"]

## step 23 assert_text [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
target: {"text": "", "content_desc": "Submitted: Ada", "identifier": "android:id/message", "enabled": true, "checked": null, "clickable": null, "bounds": [67, 1182, 1013, 1236]}
assertion: {"kind": "text", "expected": "Submitted: Ada", "mode": "exact", "passed": true, "expected_bounds": [70, 1157, 1010, 1214]}
labels: ["OK", "Submitted: Ada", "Success"]

## step 24 capture [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
labels: ["OK", "Submitted: Ada", "Success"]

## step 25 assert_region [not_compared]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
assertion: {"kind": "region", "passed": null, "note": "pixel region hash is calibrated to the ACOS frame geometry; not asserted on the phone"}
labels: ["OK", "Submitted: Ada", "Success"]

## step 26 assert_region [not_compared]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
assertion: {"kind": "region", "passed": null, "note": "pixel region hash is calibrated to the ACOS frame geometry; not asserted on the phone"}
labels: ["OK", "Submitted: Ada", "Success"]

## step 27 assert_no_crash [pass]
screen: {"activity": "dev.acos.fixtures.formvalidation/dev.acos.fixtures.formvalidation.MainActivity", "package": "dev.acos.fixtures.formvalidation", "app_bundle_in_snapshot": "dev.acos.fixtures.formvalidation"}
assertion: {"kind": "no_crash", "passed": true, "matched": []}
labels: ["OK", "Submitted: Ada", "Success"]
