//! MODFILL: the ART-path `IClipboard` clip transactions, byte level.
//!
//! The clip bytes are verbatim from an ART guest's own
//! `ClipData.writeToParcel(p, 0)` (the same capture `mproc_intents.rs` uses);
//! the argument order after the clip is the image's `IClipboard$Stub`
//! (`setPrimaryClip(ClipData, String pkg, String attributionTag, int userId,
//! int deviceId)`, `getPrimaryClip(pkg, tag, userId, deviceId)`).
//! Fail-before: before MODFILL `setPrimaryClip` answered
//! `bad request ... no parcelable codec for android.content.ClipData`.

use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;
use acos_services::clipboard::{ClipboardService, ClipboardState};
use acos_services::host::ServiceObject;
use std::sync::{Arc, Mutex};

const DESCRIPTOR: &str = "android.content.IClipboard";
/// `ClipData.writeToParcel` of `ClipData.newPlainText(null, "Shared from ACOS")`.
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

fn code(method: &str) -> u32 {
    acos_aidl::lookup_by_name(DESCRIPTOR, method)
        .unwrap_or_else(|| panic!("{method} is in the AIDL table"))
        .code
}

fn caller_tail(parcel: &mut Parcel) {
    parcel.write_string16("dev.acos.test").unwrap();
    parcel.write_string16_opt(None).unwrap();
    parcel.write_int32(0); // userId
    parcel.write_int32(0); // deviceId
}

fn request_set(clip: &[u8]) -> Parcel {
    let mut p = Parcel::new();
    p.write_int32(1); // writeTypedObject presence
    p.write_raw_bytes(clip);
    caller_tail(&mut p);
    p.set_data_position(0);
    p
}

fn request_plain() -> Parcel {
    let mut p = Parcel::new();
    caller_tail(&mut p);
    p.set_data_position(0);
    p
}

fn service() -> (ClipboardService, Arc<Mutex<ClipboardState>>) {
    let state = Arc::new(Mutex::new(ClipboardState::new()));
    (ClipboardService::new(Arc::clone(&state)), state)
}

#[test]
fn set_then_get_replays_the_framework_bytes_verbatim() {
    let clip = hex_bytes(REAL_CLIP_DATA);
    let (mut service, state) = service();
    let mut reply = Parcel::new();
    assert!(service
        .raw_transact(DESCRIPTOR, code("setPrimaryClip"), &mut request_set(&clip), &mut reply)
        .unwrap());
    assert!(state.lock().unwrap().has_primary_clip());
    assert!(
        state.lock().unwrap().has_clipboard_text(),
        "the text is read from the clip, not guessed"
    );

    let mut reply = Parcel::new();
    assert!(service
        .raw_transact(DESCRIPTOR, code("getPrimaryClip"), &mut request_plain(), &mut reply)
        .unwrap());
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "typed object present");
    let rest = reply.marshall()[8..].to_vec();
    assert_eq!(rest, clip, "the clip comes back byte for byte");
}

#[test]
fn the_description_is_the_clips_leading_bytes() {
    let clip = hex_bytes(REAL_CLIP_DATA);
    let (mut service, state) = service();
    let mut reply = Parcel::new();
    service
        .raw_transact(DESCRIPTOR, code("setPrimaryClip"), &mut request_set(&clip), &mut reply)
        .unwrap();
    let facts = state.lock().unwrap().description().cloned().unwrap();
    assert_eq!(facts.mime_types, vec!["text/plain".to_string()]);
    assert_eq!(facts.item_count, 1);
    // label(null: 1,-1) mime(1,"text/plain") extras(0|-1) timestamp isStyled
    // classification confidences — everything before the icon word.
    assert_eq!(&facts.description_bytes[..], &clip[..facts.description_bytes.len()]);
    let at = facts.description_bytes.len();
    assert_eq!(&clip[at..at + 4], &0i32.to_le_bytes(), "icon word follows the description");
    assert_eq!(&clip[at + 4..at + 8], &1i32.to_le_bytes(), "then the item count");

    let mut reply = Parcel::new();
    service
        .raw_transact(
            DESCRIPTOR,
            code("getPrimaryClipDescription"),
            &mut request_plain(),
            &mut reply,
        )
        .unwrap();
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0);
    assert_eq!(reply.read_int32().unwrap(), 1);
    assert_eq!(&reply.marshall()[8..], &clip[..at], "ClipDescription.CREATOR reads the same bytes");
}

#[test]
fn a_listener_gets_one_oneway_change_per_set_and_one_per_real_clear() {
    let clip = hex_bytes(REAL_CLIP_DATA);
    let (mut service, state) = service();
    state.lock().unwrap().add_listener(Some(BinderId(77)));
    let mut reply = Parcel::new();
    service
        .raw_transact(DESCRIPTOR, code("setPrimaryClip"), &mut request_set(&clip), &mut reply)
        .unwrap();
    let deliveries = service.drain_raw_deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].target, BinderId(77));
    assert_eq!(deliveries[0].interface, "android.content.IOnPrimaryClipChangedListener");
    assert_eq!(deliveries[0].code, 1);
    assert!(service.drain_raw_deliveries().is_empty(), "drained once");

    let mut reply = Parcel::new();
    service
        .raw_transact(DESCRIPTOR, code("clearPrimaryClip"), &mut request_plain(), &mut reply)
        .unwrap();
    assert_eq!(service.drain_raw_deliveries().len(), 1);
    assert!(!state.lock().unwrap().has_primary_clip());
    // Clearing an empty clipboard changes nothing, so nothing is announced.
    let mut reply = Parcel::new();
    service
        .raw_transact(DESCRIPTOR, code("clearPrimaryClip"), &mut request_plain(), &mut reply)
        .unwrap();
    assert!(service.drain_raw_deliveries().is_empty());
}

#[test]
fn an_empty_clipboard_answers_null_for_the_clip_and_the_description() {
    let (mut service, _state) = service();
    for method in ["getPrimaryClip", "getPrimaryClipDescription"] {
        let mut reply = Parcel::new();
        service
            .raw_transact(DESCRIPTOR, code(method), &mut request_plain(), &mut reply)
            .unwrap();
        reply.set_data_position(0);
        assert_eq!(reply.read_int32().unwrap(), 0, "{method}: no exception");
        assert_eq!(reply.read_int32().unwrap(), 0, "{method}: null object");
    }
}

#[test]
fn a_uri_item_is_stored_and_an_icon_is_refused_by_name() {
    // The real clip with the first item's Uri slot (4th int from the end:
    // intent, uri, activityInfo, textLinks) set to a StringUri.
    let mut clip = hex_bytes(REAL_CLIP_DATA);
    let len = clip.len();
    clip.truncate(len - 12); // drop uri/activityInfo/textLinks (all 0)
    clip.extend_from_slice(&1i32.to_le_bytes()); // uri present
    clip.extend_from_slice(&1i32.to_le_bytes()); // StringUri
    let uri = "content://dev.acos.test/file";
    clip.extend_from_slice(&(uri.len() as i32).to_le_bytes());
    let mut text = uri.as_bytes().to_vec();
    text.push(0);
    while text.len() % 4 != 0 {
        text.push(0);
    }
    clip.extend_from_slice(&text);
    clip.extend_from_slice(&0i32.to_le_bytes()); // activityInfo
    clip.extend_from_slice(&0i32.to_le_bytes()); // textLinks
    let (mut service, state) = service();
    let mut reply = Parcel::new();
    service
        .raw_transact(DESCRIPTOR, code("setPrimaryClip"), &mut request_set(&clip), &mut reply)
        .unwrap();
    assert!(state.lock().unwrap().description().unwrap().has_uri);

    // An icon Bitmap is a named refusal, not a misread.
    let mut with_icon = hex_bytes(REAL_CLIP_DATA);
    let at = state.lock().unwrap().description().unwrap().description_bytes.len();
    with_icon[at..at + 4].copy_from_slice(&1i32.to_le_bytes());
    let mut reply = Parcel::new();
    let error = service
        .raw_transact(DESCRIPTOR, code("setPrimaryClip"), &mut request_set(&with_icon), &mut reply)
        .expect_err("an icon is not modelled");
    assert!(matches!(error, acos_services::ServiceError::UnsupportedParcelable { .. }));
}
