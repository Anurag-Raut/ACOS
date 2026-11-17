# MPROC1 step 2 evidence — the intent capture target

## What the code now does

- **Capture** (`crates/acos-services/src/intents.rs`, wired into
  `AmsState::start_activity`): an implicit `ACTION_SEND`/`SEND_MULTIPLE`
  (text/image), `ACTION_VIEW` (http/https), `ACTION_PICK`/`ACTION_GET_CONTENT`
  with **no installed target** is recorded verbatim — action, data, MIME type,
  categories, extras, intent flags, request code, `for_result` — and answered
  `START_SUCCESS` with the synthetic component `com.acos.capture/.Capture`
  (no process). Every record says `handled_by: "captured"`.
  Anything else stays the structured `START_NOT_FOUND` (an unresolvable explicit
  component is never captured).
- **Control op**: the session protocol `ctl` command +
  `acos_service_ctl {module:"intents", op}` (daemon/MCP tool) +
  `acos session ctl --module intents --op last|list|clear|result [--args JSON]`
  (CLI). `op:"result"` scripts the answer a picker returns
  (`{result_code, data, mime_type, action, extras}`), writes it to the session's
  own `<run>/out/launch/activity-result.json` and waits (<= 2 s) for the
  guest's ack.
- **Delivery in the guest** (`LaunchMain.startResultWatcher` /
  `deliverScriptedResult`): the scripted answer becomes a real
  `ActivityResultItem` `ClientTransaction` (`ResultInfo` built through the
  framework's own `CREATOR`) delivered to the real `ActivityThread` — so
  `onActivityResult` runs as on a device. Nothing is polled by the app.
- **Guest routing** (`LaunchMain.installStartActivityGuard`): an implicit start
  is no longer short-circuited in the guest; the host package manager decides
  (that short-circuit is what made the host unable to see the intent at all).
  An accepted component-less start logs `ACOSLAUNCH MARK start-activity-captured`.
- **Wire fix (root cause, whole class)**: `android.content.ClipData`. AOSP's
  `Instrumentation.execStartActivity` calls `Intent.migrateExtraStreamToClipData`,
  so **every** `ACTION_SEND` text/image intent carries a ClipData on the wire.
  `read_intent` refused it (`UNSUPPORTED_PARCELABLE`), i.e. every share failed
  with `IllegalArgumentException` before the host ever saw it. The reader now
  consumes the body field by field (ClipDescription: `CharSequence` label,
  String16 MIME list, PersistableBundle extras, long timestamp, boolean,
  classification int, confidences Bundle; icon presence; item count; per item:
  `CharSequence` text, String8 html, four typed-object presences), so the fields
  **after** it (`contentUserHint`, extras, `originalIntent`) still decode.
  Layout pinned from the image (`dexdump` of `ClipData.writeToParcel`,
  `ClipDescription.writeToParcel`) and from a guest's own
  `ClipData.writeToParcel` bytes (`fixtures/apps/intent-probe`).

## Evidence

### Capture + scripted result round trip (one live session)

`python3 tools/mproc1-intent-drive.py` drives a real interactive ART session:
launch, `ctl last`, `ctl result`, `ctl logs`.

```
ready ok= True package= dev.acos.fixtures.intentprobe
last: {"capture": {"action": "android.intent.action.GET_CONTENT",
  "categories": ["android.intent.category.OPENABLE"],
  "component": "com.acos.capture/.Capture", "data": null, "extras": {},
  "flags": 0, "for_result": true, "handled_by": "captured",
  "mime_type": "image/*", "request_code": 42, "scripted_result": null,
  "seq": 2, ...}}
result: {"delivered": true, "ok": true, "result": {"capture_seq": 2,
  "data": "content://com.acos.capture.images/pick.png", "delivered": true,
  "delivered_utc_ms": 1794807871741,
  "extras": {"dev.acos.capture.NOTE": "scripted picker"},
  "mime_type": "image/png", "request_code": 42, "result_code": -1}}
LOG AcosIntentProbe ACOSPROBE shared action=android.intent.action.SEND type=text/plain text=Shared from ACOS
LOG AcosIntentProbe ACOSPROBE pick-request action=android.intent.action.GET_CONTENT type=image/* request=42
LOG AcosIntentProbe ACOSPROBE result request=42 result=-1 data=content://com.acos.capture.images/pick.png type=image/png
```

`ACOSPROBE shared` / `pick-request` are printed only **after** the framework
call returned, so the app really did not get `ActivityNotFoundException`; the
last line is the app's own `onActivityResult` for the scripted pick.

Full capture list (share first, then pick), from `acos session ctl --op list`
on a live daemon session:

```
seq 1 action=android.intent.action.SEND       mime=text/plain request=-1 for_result=false
      extras={android.intent.extra.SUBJECT: "ACOS MPROC1",
              android.intent.extra.TEXT: "Shared from ACOS"}
      component=com.acos.capture/.Capture handled_by=captured
seq 2 action=android.intent.action.GET_CONTENT mime=image/*   request=42  for_result=true
      categories=[android.intent.category.OPENABLE]
      component=com.acos.capture/.Capture handled_by=captured
```

### Tests (fail before, pass after)

`cargo test -p acos-services --test mproc_intents` → **5 passed**:

```
test an_unresolved_share_is_captured_and_reported_as_captured ... ok
test an_unresolved_explicit_component_is_not_captured ... ok
test a_pick_is_captured_with_its_request_code_and_result_round_trips ... ok
test the_real_clip_data_body_is_consumed_exactly ... ok
test a_clip_data_item_with_a_payload_is_a_structured_refusal ... ok
```

`the_real_clip_data_body_is_consumed_exactly` replays the 120 bytes the ART
guest's own `ClipData.writeToParcel` produced and asserts the reader stops
exactly on the sentinel after them. Before this change `read_intent` refused the
body (so every share failed); with a 4-byte misalignment the sentinel check
fails. `a_clip_data_item_with_a_payload_is_a_structured_refusal` pins the honest
refusal for the shapes ACOS does not model.

Full suites: `cargo test -p acos-services -p acos-protocol` → all green
(66 + 90 + the rest of the files, 0 failed).

### Scenario

`acos verify scenarios/mproc1-intents.yaml --engine art` →
`verify: pass (ok=true, backend=acos-native, fidelity=art-reference)`, 6 steps.

## Limits (structured, never silent)

- A captured share whose ClipData item carries a **stream** (a `content://` Uri
  item, `ACTION_SEND` with `EXTRA_STREAM`) is a structured
  `UNSUPPORTED_PARCELABLE`: the text-share and pick shapes decode, a stream
  item's payload does not. Documented in `read_clip_data`.
- The scripted result is a **metadata** answer: the `content://` URI is real
  intent data, but no provider serves its bytes yet (the ContentProvider module,
  v0.4.0). An app that opens the URI gets the same wall a phone app gets from a
  provider that does not exist; the capture record and the result round trip
  (code + URI + type + extras) are what this step delivers.
- The result is delivered to the session's resumed Activity (`sCurrentToken`);
  a stack where the *caller* is below a newly launched activity would need the
  caller's token. One-activity fixtures (this one) are exact.
