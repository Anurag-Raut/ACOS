//! W35: the `android.content.Intent` parcelable codec, pinned against the image's
//! `writeToParcel` order (W32 §3).
//!
//! Two traps are what these rows exist for:
//!
//! * Intent-level strings are **String8**, while `ComponentName`'s are String16 —
//!   a uniform String16 mis-decodes the action of every `startActivity`;
//! * `ComponentName` has **no presence word** here (a null component is one null
//!   String16), unlike the typed-object shape `parcelable.rs` carries for it.
//!
//! Teeth: writing the action/tail with the wrong string width shifts every field
//! after it, so the byte-level rows below fail against such a writer (and did,
//! on the pre-W35 "no Intent codec" host: `startActivity` never decoded at all).

use acos_parcel::Parcel;
use acos_services::parcelable::{
    self, read_payload, write_payload, INTENT_ACTION, INTENT_CATEGORIES, INTENT_CLASS,
    INTENT_CLIP_DATA, INTENT_CONTENT_USER_HINT, INTENT_DATA, INTENT_FLAGS, INTENT_IDENTIFIER,
    INTENT_ORIGINAL, INTENT_PACKAGE, INTENT_SELECTOR, INTENT_TYPE, MAX_INTENT_CATEGORIES,
    MAX_INTENT_DEPTH,
};
use acos_services::ServiceError;
use acos_services::{Bundle, Parcelable, Value};

const COMPONENT: &str = "android.content.ComponentName";
const RECT: &str = "android.graphics.Rect";

fn component(package: &str, class: &str) -> Parcelable {
    Parcelable::ordered(
        COMPONENT,
        vec![
            Value::Str(Some(package.into())),
            Value::Str(Some(class.into())),
        ],
    )
}

fn rect(l: i32, t: i32, r: i32, b: i32) -> Parcelable {
    Parcelable::ordered(
        RECT,
        vec![Value::Int(l), Value::Int(t), Value::Int(r), Value::Int(b)],
    )
}

/// A minimal intent (what a component launch carries).
fn minimal(action: Option<&str>, component: Option<Parcelable>) -> Parcelable {
    Parcelable::ordered(
        INTENT_CLASS,
        vec![
            Value::Str(action.map(str::to_string)),
            Value::Str(None),
            Value::Str(None),
            Value::Str(None),
            Value::Int(0),
            Value::Str(None),
            Value::Parcelable(component.unwrap_or(Parcelable::Null)),
            Value::Parcelable(Parcelable::Null),
            Value::StringList(None),
            Value::Parcelable(Parcelable::Null),
            Value::Parcelable(Parcelable::Null),
            Value::Int(-2),
            Value::Bundle(None),
            Value::Parcelable(Parcelable::Null),
        ],
    )
}

fn values(intent: &Parcelable) -> &Vec<Value> {
    match intent {
        Parcelable::Ordered { class, values } => {
            assert_eq!(class, INTENT_CLASS);
            values
        }
        other => panic!("expected an Intent, got {other:?}"),
    }
}

#[test]
fn a_full_intent_round_trips_field_for_field() {
    let mut extras = Bundle::new();
    extras.put("android.intent.extra.TEXT", Value::Str(Some("hi".into())));
    let nested = minimal(Some("android.intent.action.MAIN"), None);
    let intent = Parcelable::ordered(
        INTENT_CLASS,
        vec![
            Value::Str(Some("android.intent.action.SEND".into())),
            Value::Str(None),
            Value::Str(Some("text/plain".into())),
            Value::Str(Some("identifier-1".into())),
            Value::Int(0x1400_0000),
            Value::Str(Some("dev.acos.fixtures".into())),
            Value::Parcelable(component("dev.acos.fixtures", ".MainActivity")),
            Value::Parcelable(rect(1, 2, 30, 40)),
            Value::StringList(Some(vec![
                Some("android.intent.category.LAUNCHER".into()),
                Some("android.intent.category.DEFAULT".into()),
            ])),
            Value::Parcelable(Parcelable::Null),
            Value::Parcelable(Parcelable::Null),
            Value::Int(-2),
            Value::Bundle(Some(extras)),
            Value::Parcelable(nested),
        ],
    );
    let mut parcel = Parcel::new();
    write_payload(&mut parcel, &intent).expect("an Intent writes");
    parcel.set_data_position(0);
    let decoded = read_payload(INTENT_CLASS, &mut parcel).expect("an Intent reads");
    assert_eq!(decoded, intent, "field-for-field round trip");
    assert_eq!(
        parcel.data_position(),
        parcel.data_size(),
        "the codec consumed exactly the payload"
    );
    let decoded = values(&decoded);
    assert_eq!(
        decoded[INTENT_ACTION].as_str(),
        Some("android.intent.action.SEND")
    );
    assert_eq!(decoded[INTENT_TYPE].as_str(), Some("text/plain"));
    assert_eq!(decoded[INTENT_IDENTIFIER].as_str(), Some("identifier-1"));
    assert_eq!(decoded[INTENT_FLAGS].as_int(), Some(0x1400_0000));
    assert_eq!(decoded[INTENT_PACKAGE].as_str(), Some("dev.acos.fixtures"));
    assert_eq!(decoded[INTENT_CONTENT_USER_HINT].as_int(), Some(-2));
    assert!(matches!(
        &decoded[INTENT_CATEGORIES],
        Value::StringList(Some(items)) if items.len() == 2
    ));
}

#[test]
fn the_intent_wire_is_string8_for_its_own_fields_and_string16_for_the_component() {
    // Byte level, in the image's `writeToParcel` order: `writeString8(action)`,
    // `writeInt(0)` (no data Uri), `writeString8(type)`, `writeString8(identifier)`,
    // `writeInt(flags)`, `writeString8(package)`, then the component's two
    // String16s. If the writer used String16 for the action, the very first
    // `read_string8` below returns something else (2-byte chars are not a valid
    // String8 length), so this row fails.
    let intent = Parcelable::ordered(
        INTENT_CLASS,
        vec![
            Value::Str(Some("ab".into())),          // action, String8
            Value::Str(None),                       // data Uri, null
            Value::Str(Some("text/x".into())),      // type, String8
            Value::Str(None),                       // identifier, String8
            Value::Int(0x10),                       // flags
            Value::Str(Some("pkg.n".into())),       // package, String8
            Value::Parcelable(component("p", "c")), // String16 + String16
            Value::Parcelable(Parcelable::Null),    // sourceBounds
            Value::StringList(None),                // categories
            Value::Parcelable(Parcelable::Null),    // selector
            Value::Parcelable(Parcelable::Null),    // clipData
            Value::Int(0),                          // contentUserHint
            Value::Bundle(None),                    // extras
            Value::Parcelable(Parcelable::Null),    // originalIntent
        ],
    );
    let mut parcel = Parcel::new();
    write_payload(&mut parcel, &intent).expect("writes");
    parcel.set_data_position(0);
    assert_eq!(parcel.read_string8().expect("action"), Some("ab".into()));
    assert_eq!(parcel.read_int32().expect("data"), 0);
    assert_eq!(parcel.read_string8().expect("type"), Some("text/x".into()));
    assert_eq!(parcel.read_string8().expect("identifier"), None);
    assert_eq!(parcel.read_int32().expect("flags"), 0x10);
    assert_eq!(
        parcel.read_string8().expect("package"),
        Some("pkg.n".into())
    );
    assert_eq!(
        parcel.read_string16().expect("component pkg"),
        Some("p".into())
    );
    assert_eq!(
        parcel.read_string16().expect("component class"),
        Some("c".into())
    );
    assert_eq!(parcel.read_int32().expect("sourceBounds"), 0);
    assert_eq!(parcel.read_int32().expect("categories"), 0);
    assert_eq!(parcel.read_int32().expect("selector"), 0);
    assert_eq!(parcel.read_int32().expect("clipData"), 0);
    assert_eq!(parcel.read_int32().expect("contentUserHint"), 0);
    assert_eq!(parcel.read_int32().expect("extras length"), -1);
    assert_eq!(parcel.read_int32().expect("originalIntent"), 0);
    assert_eq!(parcel.data_position(), parcel.data_size());
}

#[test]
fn a_null_component_is_one_null_string16_with_no_presence_word() {
    // `ComponentName.writeToParcel(ComponentName, Parcel)` (the static writer)
    // writes a single null String16. A presence word here would make the
    // sourceBounds marker (1) land one field early; this row reads the marker
    // straight after the component and then the four Rect ints.
    let intent = Parcelable::ordered(
        INTENT_CLASS,
        vec![
            Value::Str(None),
            Value::Str(None),
            Value::Str(None),
            Value::Str(None),
            Value::Int(0),
            Value::Str(None),
            Value::Parcelable(Parcelable::Null), // component: no presence word
            Value::Parcelable(rect(7, 8, 9, 10)),
            Value::StringList(None),
            Value::Parcelable(Parcelable::Null),
            Value::Parcelable(Parcelable::Null),
            Value::Int(0),
            Value::Bundle(None),
            Value::Parcelable(Parcelable::Null),
        ],
    );
    let mut parcel = Parcel::new();
    write_payload(&mut parcel, &intent).expect("writes");
    parcel.set_data_position(0);
    assert_eq!(parcel.read_string8().expect("action"), None);
    assert_eq!(parcel.read_int32().expect("data"), 0);
    assert_eq!(parcel.read_string8().expect("type"), None);
    assert_eq!(parcel.read_string8().expect("identifier"), None);
    assert_eq!(parcel.read_int32().expect("flags"), 0);
    assert_eq!(parcel.read_string8().expect("package"), None);
    // The null component is the String16 marker itself, and the next word is the
    // sourceBounds presence marker.
    assert_eq!(parcel.read_int32().expect("null String16"), -1);
    assert_eq!(parcel.read_int32().expect("sourceBounds present"), 1);
    assert_eq!(parcel.read_int32().expect("left"), 7);
    assert_eq!(parcel.read_int32().expect("top"), 8);
    assert_eq!(parcel.read_int32().expect("right"), 9);
    assert_eq!(parcel.read_int32().expect("bottom"), 10);

    // And the reader agrees.
    parcel.set_data_position(0);
    let decoded = read_payload(INTENT_CLASS, &mut parcel).expect("reads");
    assert_eq!(decoded, intent);
}

#[test]
fn a_non_null_data_uri_is_written_as_the_images_string_uri() {
    // LAUNCHEXTRAS: this image's `Uri.parse` always builds a `StringUri`, whose
    // `writeToParcel` writes the type tag 1 and `writeString8(uriString)`; the
    // static `Uri.writeToParcel` writes `writeInt(0)` only for null. Both halves
    // are pinned from the framework dex (and the byte-level proof is the
    // real-Java differential in `acos-runtime/tests/client_transaction_java.rs`).
    let mut with_uri = minimal(Some("android.intent.action.VIEW"), None);
    if let Parcelable::Ordered { values, .. } = &mut with_uri {
        values[INTENT_DATA] = Value::Str(Some("content://acos/x".into()));
    }
    let mut parcel = Parcel::new();
    write_payload(&mut parcel, &with_uri).expect("a data Uri is written");
    parcel.set_data_position(0);
    // mAction (String8), then the Uri type tag and its String8.
    assert_eq!(parcel.read_string8().expect("action").as_deref(), Some("android.intent.action.VIEW"));
    assert_eq!(parcel.read_int32().expect("Uri type"), 1, "StringUri");
    assert_eq!(
        parcel.read_string8().expect("Uri string").as_deref(),
        Some("content://acos/x")
    );

    parcel.set_data_position(0);
    let decoded = read_payload(INTENT_CLASS, &mut parcel).expect("reads");
    assert_eq!(decoded, with_uri, "the data Uri round-trips");

    // A Uri type this image does not define is a structural error, never a
    // guessed string read that would misalign every field after it.
    let mut wire = Parcel::new();
    wire.write_string8_opt(Some("android.intent.action.VIEW"))
        .expect("action");
    wire.write_int32(99); // unknown Uri type
    wire.set_data_position(0);
    let err = read_payload(INTENT_CLASS, &mut wire).expect_err("an unknown Uri type is refused");
    assert!(
        matches!(err, ServiceError::BadRequest { .. }),
        "an unknown Uri type is malformed: {err:?}"
    );
}

#[test]
fn a_selector_and_an_original_intent_both_round_trip() {
    let selector = minimal(
        Some("android.intent.action.MAIN"),
        Some(component("sel.pkg", "Sel")),
    );
    let original = minimal(None, Some(component("orig.pkg", "Orig")));
    let intent = Parcelable::ordered(
        INTENT_CLASS,
        vec![
            Value::Str(Some("android.intent.action.VIEW".into())),
            Value::Str(None),
            Value::Str(None),
            Value::Str(None),
            Value::Int(0),
            Value::Str(None),
            Value::Parcelable(component("pkg", "Cls")),
            Value::Parcelable(Parcelable::Null),
            Value::StringList(None),
            Value::Parcelable(selector),
            Value::Parcelable(Parcelable::Null),
            Value::Int(0),
            Value::Bundle(None),
            Value::Parcelable(original),
        ],
    );
    let mut parcel = Parcel::new();
    write_payload(&mut parcel, &intent).expect("writes");
    parcel.set_data_position(0);
    let decoded = read_payload(INTENT_CLASS, &mut parcel).expect("reads");
    assert_eq!(decoded, intent);
    assert!(matches!(
        values(&decoded)[INTENT_SELECTOR],
        Value::Parcelable(Parcelable::Ordered { .. })
    ));
    assert!(matches!(
        values(&decoded)[INTENT_ORIGINAL],
        Value::Parcelable(Parcelable::Ordered { .. })
    ));
}

#[test]
fn hostile_intents_are_refused_not_panicked() {
    // 1. Truncated after the action.
    let mut wire = Parcel::new();
    wire.write_string8_opt(Some("a")).expect("action");
    wire.set_data_position(0);
    assert!(read_payload(INTENT_CLASS, &mut wire).is_err());

    // 2. A category count bomb: refused before any allocation.
    let mut wire = Parcel::new();
    wire.write_string8_opt(None).expect("action");
    wire.write_int32(0);
    wire.write_string8_opt(None).expect("type");
    wire.write_string8_opt(None).expect("identifier");
    wire.write_int32(0);
    wire.write_string8_opt(None).expect("package");
    wire.write_string16_opt(None).expect("component");
    wire.write_int32(0);
    wire.write_int32(MAX_INTENT_CATEGORIES + 1);
    wire.set_data_position(0);
    let err = read_payload(INTENT_CLASS, &mut wire).expect_err("count bomb");
    assert!(matches!(err, ServiceError::BadRequest { .. }), "{err:?}");

    // 3. Selector recursion deeper than the cap.
    let mut deepest = minimal(None, None);
    for _ in 0..=MAX_INTENT_DEPTH {
        let mut wrapped = minimal(None, None);
        if let Parcelable::Ordered { values, .. } = &mut wrapped {
            values[INTENT_SELECTOR] = Value::Parcelable(deepest);
        }
        deepest = wrapped;
    }
    let mut parcel = Parcel::new();
    write_payload(&mut parcel, &deepest).expect("writes (the cap is on reading)");
    parcel.set_data_position(0);
    let err = read_payload(INTENT_CLASS, &mut parcel).expect_err("deep nesting");
    assert!(matches!(err, ServiceError::BadRequest { .. }), "{err:?}");

    // 4. A present clipData is refused rather than skipped.
    let mut with_clip = minimal(None, None);
    if let Parcelable::Ordered { values, .. } = &mut with_clip {
        values[INTENT_CLIP_DATA] = Value::Int(1);
    }
    assert!(write_payload(&mut Parcel::new(), &with_clip).is_err());
}

#[test]
fn the_shared_payload_entry_dispatches_to_the_intent_codec() {
    // `codec.rs` decodes a `WireOp::TypedObject{class:"android.content.Intent"}`
    // through `read_payload`, which is what makes
    // `IActivityTaskManager.startActivity` decode host-side.
    let intent = minimal(
        Some("android.intent.action.MAIN"),
        Some(component("dev.acos.fixtures", ".MainActivity")),
    );
    let mut parcel = Parcel::new();
    write_payload(&mut parcel, &intent).expect("writes");
    parcel.set_data_position(0);
    assert_eq!(
        read_payload("android.content.Intent", &mut parcel).expect("reads"),
        intent
    );
    // The Intent is not a flat spec; the dynamic codec owns it.
    assert!(parcelable::spec_for(INTENT_CLASS).is_none());
    assert!(parcelable::has_codec("android.view.DisplayInfo"));
}
