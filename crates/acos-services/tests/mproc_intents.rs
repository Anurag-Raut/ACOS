//! MPROC1 step 2: the intent **capture target** and the `ClipData` wire codec.
//!
//! Two behaviours the multi-package/multi-process track depends on:
//!
//! 1. An implicit `ACTION_SEND`/`VIEW`/`PICK`/`GET_CONTENT` with no installed
//!    target is **captured** (recorded verbatim, `START_SUCCESS` to the caller),
//!    and anything else stays the structured `START_NOT_FOUND` — never a fake
//!    capture.
//! 2. The `android.content.ClipData` body `Instrumentation.execStartActivity`
//!    puts on every `ACTION_SEND` intent is consumed field by field, so the
//!    fields **after** it (`contentUserHint`, `extras`, `originalIntent`) still
//!    decode. Before this the whole share was `UNSUPPORTED_PARCELABLE` and the
//!    app's `startActivity` threw.
//!
//! The 120 clip bytes in [`REAL_CLIP_DATA`] are verbatim from an ART guest's own
//! `ClipData.writeToParcel(p, 0)` for the fixture share
//! (`fixtures/apps/intent-probe`, `ACTION_SEND` text/plain, EXTRA_TEXT
//! "Shared from ACOS"). See `results/handoff/MPROC1/evidence-step2.md`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use acos_parcel::Parcel;
use acos_services::ams::{AmsState, Extra, IntentFacts, START_NOT_FOUND, START_SUCCESS};
use acos_services::intents::{IntentState, CAPTURE_PACKAGE};

/// The exact ClipData payload the framework wrote in the guest.
const REAL_CLIP_DATA: &str =
    "01000000ffffffff010000000a00000074006500780074002f0070006c00610069006e00\
00000000ffffffff00000000000000000000000001000000000000000000000001000000\
01000000100000005368617265642066726f6d2041434f5300000000ffffffff00000000\
000000000000000000000000";

fn hex_bytes(hex: &str) -> Vec<u8> {
    let cleaned: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    (0..cleaned.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cleaned[i..i + 2], 16).expect("hex"))
        .collect()
}

fn send_facts() -> IntentFacts {
    let mut extras = BTreeMap::new();
    extras.insert(
        "android.intent.extra.TEXT".to_string(),
        Extra::String("Shared from ACOS".into()),
    );
    IntentFacts {
        action: Some("android.intent.action.SEND".into()),
        mime_type: Some("text/plain".into()),
        extras,
        ..IntentFacts::default()
    }
}

fn state_with_capture() -> Arc<Mutex<IntentState>> {
    let intents = Arc::new(Mutex::new(IntentState::new()));
    let mut ams = AmsState::new();
    ams.intents = Some(Arc::clone(&intents));
    // The state machine the test uses is the one the service host builds; keep
    // it alive through the Arc above.
    let mut ams = ams;
    let code = ams
        .start_activity(&send_facts(), 0, 0, -1)
        .expect("a SEND with no installed target is accepted by the capture target");
    assert_eq!(code, START_SUCCESS);
    intents
}

#[test]
fn an_unresolved_share_is_captured_and_reported_as_captured() {
    let intents = state_with_capture();
    let state = intents.lock().unwrap();
    let capture = state.last().expect("one capture");
    assert_eq!(capture.action, "android.intent.action.SEND");
    assert_eq!(capture.mime_type.as_deref(), Some("text/plain"));
    assert_eq!(capture.component, "com.acos.capture/.Capture");
    assert!(capture.component.starts_with(CAPTURE_PACKAGE));
    assert_eq!(
        capture.extras.get("android.intent.extra.TEXT"),
        Some(&Extra::String("Shared from ACOS".into()))
    );
    assert!(!capture.for_result, "startActivity passes requestCode -1");
    let json = capture.to_json();
    assert_eq!(json["handled_by"], "captured");
}

#[test]
fn an_unresolved_explicit_component_is_not_captured() {
    let intents = Arc::new(Mutex::new(IntentState::new()));
    let mut ams = AmsState::new();
    ams.intents = Some(Arc::clone(&intents));
    let err = ams
        .start_activity(
            &IntentFacts::explicit("dev.acos.nope", "dev.acos.nope.Main"),
            0,
            0,
            -1,
        )
        .expect_err("an explicit component that does not resolve is the caller's bug");
    assert!(matches!(
        err,
        acos_services::ServiceError::IllegalState { .. }
    ));
    assert!(intents.lock().unwrap().last().is_none());
    assert_eq!(START_NOT_FOUND, -91);
}

#[test]
fn a_pick_is_captured_with_its_request_code_and_result_round_trips() {
    let intents = Arc::new(Mutex::new(IntentState::new()));
    let mut ams = AmsState::new();
    ams.intents = Some(Arc::clone(&intents));
    let pick = IntentFacts {
        action: Some("android.intent.action.GET_CONTENT".into()),
        mime_type: Some("image/*".into()),
        categories: vec!["android.intent.category.OPENABLE".into()],
        ..IntentFacts::default()
    };
    ams.start_activity(&pick, 0, 0x7000, 42).expect("captured");
    let mut state = intents.lock().unwrap();
    let (seq, request_code, for_result) = {
        let capture = state.last().expect("capture");
        (capture.seq, capture.request_code, capture.for_result)
    };
    assert_eq!(request_code, 42);
    assert!(for_result, "requestCode >= 0 waits for a result");
    let scripted = state
        .script_result(
            None,
            None,
            Some("content://com.acos.capture.images/pick.png".into()),
            Some("image/png".into()),
            None,
            BTreeMap::from([("dev.acos.capture.NOTE".to_string(), "scripted".to_string())]),
        )
        .expect("scripted result");
    assert_eq!(scripted.request_code, 42);
    assert_eq!(scripted.result_code, -1, "RESULT_OK");
    assert!(state.undelivered_result(seq).is_some());
    assert!(state.mark_delivered(seq, 7));
    assert!(state.undelivered_result(seq).is_none());
    let json = state.last().unwrap().to_json();
    assert_eq!(json["scripted_result"]["delivered"], true);
    assert_eq!(
        json["scripted_result"]["data"],
        "content://com.acos.capture.images/pick.png"
    );
}

#[test]
fn the_real_clip_data_body_is_consumed_exactly() {
    // The payload, then a sentinel: if the reader consumed one byte too few or
    // too many, the sentinel read fails or returns something else.
    let mut bytes = hex_bytes(REAL_CLIP_DATA);
    let sentinel: i32 = 0x00ab_cdef;
    bytes.extend_from_slice(&sentinel.to_le_bytes());
    let mut parcel = Parcel::new();
    parcel.unmarshall(&bytes);
    acos_services::parcelable::skip_clip_data(&mut parcel).expect("the real ClipData decodes");
    assert_eq!(parcel.data_position(), bytes.len() - 4);
    assert_eq!(parcel.read_int32().unwrap(), sentinel);
}

#[test]
fn a_clip_data_item_with_a_payload_is_a_structured_refusal() {
    // Flip the first item object's presence word to 1 (a stream share carries a
    // Uri item). ACOS does not model that payload, so it must be a structured
    // UNSUPPORTED_PARCELABLE, never a guessed skip that desynchronises the rest.
    let mut bytes = hex_bytes(REAL_CLIP_DATA);
    let len = bytes.len();
    // Last four ints are the item's object presences; the first is at len-16.
    bytes[len - 16..len - 12].copy_from_slice(&1i32.to_le_bytes());
    let mut parcel = Parcel::new();
    parcel.unmarshall(&bytes);
    let err = acos_services::parcelable::skip_clip_data(&mut parcel)
        .expect_err("a non-null item payload is not modelled");
    assert!(matches!(
        err,
        acos_services::ServiceError::UnsupportedParcelable { .. }
    ));
}
