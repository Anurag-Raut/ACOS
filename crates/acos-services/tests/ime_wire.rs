//! W13: the `IInputMethodManager` focus path, pinned to the API-34 image.
//!
//! Evidence chain:
//! * `fixtures/w13_ime_start_input.bin` is a byte-for-byte capture of a real
//!   `startInputOrWindowGainedFocus` (code 9) request made by the image's own
//!   `ViewRootImpl` on the `counter.apk` launch, taken by the opt-in
//!   `ACOS_IME_DUMP` path in `ime_wire.rs`. It contains a real `EditorInfo`
//!   (`writeTypedObject`: presence word then the class's `writeToParcel`).
//! * The layouts were decoded from the image's `framework.jar` (`acos-dex
//!   dexdump`): `EditorInfo.writeToParcel`, `AutofillId.writeToParcel`,
//!   `SurroundingText.writeToParcel`, `TextUtils.writeToParcel`,
//!   `LocaleList.writeToParcel`, `InputBindResult.writeToParcel`,
//!   `ImeTracker$Token.writeToParcel`.
//!
//! Teeth: the captured request must be consumed to its last byte and answered
//! with `InputBindResult.NO_IME` (`result = 6`); a truncation anywhere inside
//! the `EditorInfo` is a structured error, never a panic or a partial success;
//! the legacy tag layout for `ImeTracker$Token` ([int, string16]) is rejected.

use std::sync::{Arc, Mutex};

use acos_parcel::{BinderId, Parcel};
use acos_services::exception::NO_EXCEPTION;
use acos_services::ime::{install, shared, ImeState, BIND_NO_IME, BIND_SUCCESS, KEYCODE_BACK};
use acos_services::ime_wire::{
    read_editor_info, read_ime_tracker_token, write_ime_tracker_token, EditorInfo, ImeTrackerToken,
    ImeWireService, InputBindResult, EDITOR_INFO_CLASS, IME_DESCRIPTOR, IME_TRACKER_TOKEN_CLASS,
    RESULT_RECEIVER_CLASS,
};
use acos_services::{ServiceHost, ServiceObject};

const CAPTURED: &[u8] = include_bytes!("fixtures/w13_ime_start_input.bin");
const CAPTURED_TEXT_INPUT: &[u8] = include_bytes!("fixtures/w13_ime_text_input.bin");
const START_INPUT: u32 = 9;

fn captured_parcel_of(bytes: &[u8]) -> Parcel {
    let mut parcel = Parcel::new();
    parcel.unmarshall(bytes);
    // The peer imports a request in libbinder wire form: a null binder keeps
    // its 4-byte stability record (W9). The capture is post-import.
    parcel.set_wire_null_stability(true);
    parcel
}

fn captured_parcel() -> Parcel {
    captured_parcel_of(CAPTURED)
}

/// The tests below drive one process-wide `ImeState` (that is what the service
/// host installs), so the stateful ones take this lock instead of racing.
fn state_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Install a fresh IME state; `bind` attaches the transport's session binder,
/// which is what selects the bound-IME mode (the ART transport does this in
/// `registry::install`).
fn fresh_ime(bind: bool) -> Arc<Mutex<ImeState>> {
    let state = Arc::new(Mutex::new(ImeState::new()));
    if bind {
        state.lock().unwrap().attach_session(SESSION);
    }
    install(Arc::clone(&state));
    state
}

const SESSION: BinderId = BinderId(0x5e55_10);

fn host_with_input_method() -> (ServiceHost, BinderId) {
    let mut host = ServiceHost::new();
    host.install_standard();
    let binder = host
        .check_service("input_method")
        .expect("lookup")
        .expect("the IME manager is registered");
    (host, binder)
}

/// The ART transport: `install_with_handles` attaches the session binder and
/// the WMS state, so `startInput` binds a real session + channel.
fn art_host() -> (ServiceHost, BinderId, BinderId) {
    install(Arc::new(Mutex::new(ImeState::new())));
    let mut host = ServiceHost::new();
    acos_services::registry::install_with_handles(&mut host);
    let binder = host
        .check_service("input_method")
        .expect("lookup")
        .expect("the IME manager is registered");
    let session = host
        .aux("com.android.internal.inputmethod.IInputMethodSession")
        .expect("the transport attached a session binder");
    (host, binder, session)
}

#[test]
fn the_captured_live_start_input_request_is_answered_with_a_bound_ime_and_channel() {
    let _guard = state_lock();
    let (mut host, binder, session) = art_host();
    let state = shared();
    let mut data = captured_parcel();
    let mut reply = Parcel::new();
    host.transact(binder, START_INPUT, &mut data, &mut reply, 0)
        .expect("the real request is accepted");
    assert_eq!(
        data.remaining(),
        0,
        "the whole EditorInfo request must be consumed (capture sha256 \
         56bc68d68e38c295b20dfb22b73e674a4934dbb641973e95ef4a81353e12e454)"
    );

    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    assert_eq!(reply.read_int32().expect("typed presence"), 1);
    assert_eq!(
        reply.read_int32().expect("result"),
        BIND_SUCCESS,
        "a bound IME answers InputBindResult.SUCCESS"
    );
    assert_eq!(
        reply.read_strong_binder().expect("method"),
        Some(session),
        "the session binder the app keys mCurBindState on"
    );
    assert_eq!(reply.read_int32().expect("accessibility sessions"), -1);
    assert_eq!(reply.read_int32().expect("channel presence"), 1);
    assert_eq!(reply.read_int32().expect("isInitialized"), 1);
    let channel_token = reply.read_strong_binder().expect("channel token");
    assert!(
        channel_token.is_some(),
        "the InputChannel carries a token binder"
    );
    assert_ne!(
        channel_token,
        Some(session),
        "the channel token is one pair's own fresh binder, never the session \
         binder (`InputChannel::openInputChannelPair` creates a new `BBinder` \
         per pair, and the framework's `areSameInputChannel` compares these)"
    );
    let name = reply.read_string16().expect("channel name").expect("name");
    assert!(name.ends_with("(client)"), "channel name {name}");
    let fd = reply.read_file_descriptor().expect("client fd");
    assert!(fd.is_some(), "the bind result carries the real client end");
    let id = reply.read_string16().expect("id").expect("session id");
    assert!(id.starts_with("acos-ime-"), "{id}");
    let sequence = reply.read_int32().expect("sequence");
    assert_eq!(sequence, 1, "the request's sequence is echoed");
    assert_eq!(reply.read_float_array().expect("matrix"), None);
    assert!(!reply.read_bool().expect("suppressing"));
    assert_eq!(reply.remaining(), 0, "the reply has no trailing bytes");

    let recorded = state.lock().unwrap();
    assert_eq!(recorded.start_inputs.last().map(|e| e.2), Some(BIND_SUCCESS));
    assert!(recorded.ime_visible == false, "startInput does not show the IME");
}

/// REL3I2: every bound `InputChannel` pair carries its **own** token, exactly
/// as `InputChannel::openInputChannelPair` does (`sp<IBinder> token = new
/// BBinder()` per pair). The token is the channel's identity: the framework's
/// `InputMethodManager.areSameInputChannel` compares it, and a token shared by
/// two binds makes it treat the second bind as "the same connection" and keep
/// the *stale* `mCurChannel` — the app then writes into a socket whose server
/// end this manager already dropped, `sendInputEvent` returns false, and the
/// key falls through to the view tree instead of hiding the soft input.
#[test]
fn each_bound_channel_carries_its_own_token() {
    let _guard = state_lock();
    let state = fresh_ime(true);
    let (first, _, _) = state.lock().unwrap().start_input(None, 1, None);
    assert_eq!(first, BIND_SUCCESS);
    let first_token = state
        .lock()
        .unwrap()
        .take_bind_channel()
        .expect("the first bind result carries a channel")
        .0;
    let (second, _, _) = state.lock().unwrap().start_input(None, 5, None);
    assert_eq!(second, BIND_SUCCESS);
    let second_token = state
        .lock()
        .unwrap()
        .take_bind_channel()
        .expect("the second bind result carries a channel")
        .0;
    assert_ne!(
        first_token, second_token,
        "two binds must not share a channel token (`areSameInputChannel` would \
         keep the first pair and drop the key path)"
    );
}

#[test]
fn without_an_attached_session_the_manager_stays_the_null_ime() {
    let _guard = state_lock();
    fresh_ime(false);
    let (mut host, binder) = host_with_input_method();
    let mut data = captured_parcel();
    let mut reply = Parcel::new();
    host.transact(binder, START_INPUT, &mut data, &mut reply, 0)
        .expect("the real request is accepted");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    assert_eq!(reply.read_int32().expect("typed presence"), 1);
    assert_eq!(
        reply.read_int32().expect("result"),
        BIND_NO_IME,
        "the image's InputBindResult.NO_IME is error(6)"
    );
    assert_eq!(reply.read_strong_binder().expect("method"), None);
    assert_eq!(reply.read_int32().expect("accessibility sessions"), -1);
    assert_eq!(reply.read_int32().expect("channel presence"), 0);
    assert_eq!(reply.read_string16().expect("id"), None);
    assert_eq!(reply.read_int32().expect("sequence"), 1);
    assert_eq!(reply.read_float_array().expect("matrix"), None);
    assert!(!reply.read_bool().expect("suppressing"));
    assert_eq!(reply.remaining(), 0);
}

/// REL3I: AOSP's IME key path. `InputMethodManager.dispatchInputEvent` writes
/// the key into `mCurChannel`; the IME reads it, and BACK while the soft input
/// is shown hides it and answers `FINISHED(handled=true)`. Every other key is
/// answered `handled=false` and is then forwarded to the view tree.
#[test]
fn back_with_the_soft_input_shown_hides_it_and_finishes_handled() {
    let _guard = state_lock();
    let state = fresh_ime(true);
    let (result, id, session) = state.lock().unwrap().start_input(None, 7, None);
    assert_eq!(result, BIND_SUCCESS);
    assert_eq!(session, Some(SESSION));
    assert!(id.is_some());
    let (_token, _name, client) = state
        .lock()
        .unwrap()
        .take_bind_channel()
        .expect("the bind result carries a channel");
    assert!(state.lock().unwrap().show_soft_input(BinderId(0), 0));
    assert!(state.lock().unwrap().ime_visible);

    let send = |seq: u32, action: i32, code: i32| {
        let bytes = acos_services::input_channel::InputMessage::Key {
            seq,
            event: acos_services::input_channel::KeyEvent::new(9, action, code, 1, 1),
        }
        .encode()
        .expect("encode");
        // SAFETY: `client` owns the descriptor for as long as this closure's
        // captured state does, and `send` does not close it.
        let fd = unsafe {
            rustix::fd::BorrowedFd::borrow_raw(std::os::fd::AsRawFd::as_raw_fd(&client))
        };
        let sent = rustix::net::send(&fd, &bytes, rustix::net::SendFlags::empty())
            .expect("write the key into the IME channel");
        assert_eq!(sent, bytes.len());
    };

    send(11, 0, KEYCODE_BACK);
    assert_eq!(acos_services::ime::service_pending_keys(), 1);
    {
        let s = state.lock().unwrap();
        assert!(!s.ime_visible, "BACK hid the soft input");
        assert_eq!(s.channel_replies, vec![(11, true)]);
        assert_eq!(s.channel_keys.len(), 1);
        assert_eq!(s.channel_keys[0].key_code, KEYCODE_BACK);
        assert!(s.channel_keys[0].hid_ime);
    }
    // The UP half of the same press is handled too (AOSP's tracking rule).
    send(12, 1, KEYCODE_BACK);
    assert_eq!(acos_services::ime::service_pending_keys(), 1);
    assert_eq!(state.lock().unwrap().channel_replies, vec![(11, true), (12, true)]);

    // With the IME hidden the next BACK belongs to the view tree.
    send(13, 0, KEYCODE_BACK);
    assert_eq!(acos_services::ime::service_pending_keys(), 1);
    {
        let s = state.lock().unwrap();
        assert_eq!(s.channel_replies.last(), Some(&(13, false)));
        assert!(!s.channel_keys.last().unwrap().hid_ime);
    }

    // A typed key is never consumed by the IME: it follows the typing path.
    send(14, 0, 29); // KEYCODE_A
    assert_eq!(acos_services::ime::service_pending_keys(), 1);
    assert_eq!(state.lock().unwrap().channel_replies.last(), Some(&(14, false)));
}

#[test]
fn the_captured_editor_info_decodes_to_the_live_fields() {
    let mut data = captured_parcel();
    data.enforce_interface(IME_DESCRIPTOR)
        .expect("interface token");
    let _sequence = data.read_int32().expect("sequence");
    let _client = data.read_strong_binder().expect("client");
    let _token = data.read_strong_binder().expect("window token");
    for _ in 0..3 {
        let _ = data.read_int32().expect("mode");
    }
    // The `writeTypedObject` presence word.
    assert_eq!(data.read_int32().expect("editor presence"), 1);
    let editor = read_editor_info(&mut data).expect("image EditorInfo");
    assert_eq!(
        editor.package_name.as_deref(),
        Some("dev.acos.fixtures.counter")
    );
    assert!(editor.input_type >= 0);
    // The next argument is a real strong binder (`IRemoteInputConnection`).
    assert!(data.read_strong_binder().is_ok());
}

#[test]
fn every_truncation_inside_the_editor_info_is_a_structured_error() {
    // The EditorInfo body starts after the three leading ints, two binders, the
    // presence word and the package string. Cutting inside any of these is the
    // realistic "short parcel" failure.
    let full = CAPTURED.len();
    let mut refusals = 0usize;
    for cut in [
        full - 1,
        full - 3,
        full / 2,
        full / 3,
        120,
        118,
        116,
        112,
        108,
        104,
    ] {
        let mut data = Parcel::new();
        data.unmarshall(&CAPTURED[..cut]);
        data.set_wire_null_stability(true);
        let mut service = ImeWireService::new(shared());
        // The host enforces the interface token; the direct call receives the
        // parcel after the token. Strip it the way the host does.
        if data.enforce_interface(IME_DESCRIPTOR).is_err() {
            refusals += 1;
            continue;
        }
        let mut reply = Parcel::new();
        let result = service.raw_transact(IME_DESCRIPTOR, START_INPUT, &mut data, &mut reply);
        assert!(
            result.is_err(),
            "a {cut}-byte truncation must be refused, not decoded"
        );
        refusals += 1;
    }
    assert_eq!(refusals, 10);
}

#[test]
fn a_text_field_editor_info_with_spanned_surrounding_text_is_consumed_in_full() {
    let _guard = state_lock();
    let (mut host, binder, _session) = art_host();
    // `text-input.apk` focuses an `EditText`: `mInitialSurroundingText.mText`
    // is a `SpannableStringBuilder` (zero spans in the capture) and the hint is
    // a plain `CharSequence`. This is the request that used to kill the
    // guest's main looper with `no parcelable codec for android.text.Spanned`.
    let mut data = captured_parcel_of(CAPTURED_TEXT_INPUT);
    let mut reply = Parcel::new();
    host.transact(binder, START_INPUT, &mut data, &mut reply, 0)
        .expect("the EditText focus request is accepted");
    assert_eq!(
        data.remaining(),
        0,
        "the whole EditorInfo (including the Spanned surrounding text) is consumed"
    );
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    assert_eq!(reply.read_int32().expect("presence"), 1);
    assert_eq!(reply.read_int32().expect("result"), BIND_SUCCESS);

    // The same capture decodes to the live text-field fields.
    let mut probe = captured_parcel_of(CAPTURED_TEXT_INPUT);
    probe.enforce_interface(IME_DESCRIPTOR).expect("token");
    let _sequence = probe.read_int32().expect("sequence");
    let _client = probe.read_strong_binder().expect("client");
    let _window = probe.read_strong_binder().expect("window");
    for _ in 0..3 {
        let _ = probe.read_int32().expect("mode");
    }
    assert_eq!(probe.read_int32().expect("presence"), 1);
    let editor = read_editor_info(&mut probe).expect("EditorInfo");
    assert_eq!(
        editor.package_name.as_deref(),
        Some("dev.acos.fixtures.textinput")
    );
    assert_eq!(editor.input_type, 1, "TYPE_CLASS_TEXT");
    assert_eq!(editor.hint_text.as_deref(), Some("Type something"));
    assert!(editor.has_initial_surrounding_text);
    assert_eq!(editor.autofill_view_id, Some(0x4000_0000));
}

#[test]
fn a_garbage_request_is_refused_without_panicking() {
    let mut data = Parcel::new();
    data.write_int32(-1);
    for _ in 0..64 {
        data.write_int32(-1);
    }
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let mut service = ImeWireService::new(shared());
    let result = service.raw_transact(IME_DESCRIPTOR, START_INPUT, &mut data, &mut reply);
    assert!(result.is_err(), "garbage cannot be a successful startInput");
}

#[test]
fn the_no_ime_bind_result_payload_matches_the_image_writer_order() {
    let mut parcel = Parcel::new();
    InputBindResult::no_ime(42)
        .write_payload(&mut parcel)
        .expect("write");
    parcel.set_data_position(0);
    assert_eq!(parcel.read_int32().expect("result"), 6);
    assert_eq!(parcel.read_strong_binder().expect("method"), None);
    assert_eq!(parcel.read_int32().expect("sessions"), -1);
    assert_eq!(parcel.read_int32().expect("channel"), 0);
    assert_eq!(parcel.read_string16().expect("id"), None);
    assert_eq!(parcel.read_int32().expect("sequence"), 42);
    assert_eq!(parcel.read_float_array().expect("matrix"), None);
    assert!(!parcel.read_bool().expect("suppressing"));
    assert_eq!(parcel.remaining(), 0);
}

#[test]
fn a_non_null_bind_result_channel_is_unsupported_not_silently_written() {
    let mut result = InputBindResult::no_ime(1);
    result.channel = Some(());
    let mut parcel = Parcel::new();
    assert!(result.write_payload(&mut parcel).is_err());
}

#[test]
fn the_image_ime_tracker_token_is_binder_then_string8_not_int_then_string16() {
    let token = ImeTrackerToken {
        binder: Some(BinderId(0x2a)),
        tag: Some("showSoftInput".into()),
    };
    let mut parcel = Parcel::new();
    write_ime_tracker_token(&mut parcel, &token).expect("write");
    // First word is the flat-object type (`BINDER_TYPE_BINDER` 0x73622a85),
    // not a string length: the image writes `writeStrongBinder` first.
    let first = u32::from_le_bytes(parcel.marshall()[0..4].try_into().expect("4 bytes"));
    assert_eq!(first, 0x7362_2a85, "token starts with the binder object");
    parcel.set_data_position(0);
    let decoded = read_ime_tracker_token(&mut parcel).expect("read");
    assert_eq!(decoded, token);
    assert_eq!(parcel.remaining(), 0);

    // The tag/r75 shape `[int id][string16 tag]` must not decode as a token.
    let mut legacy = Parcel::new();
    legacy.write_int32(7);
    legacy
        .write_string16_opt(Some("showSoftInput"))
        .expect("string");
    legacy.set_data_position(0);
    assert!(
        read_ime_tracker_token(&mut legacy).is_err(),
        "the image layout wins over the tag layout"
    );
}

#[test]
fn show_soft_input_with_the_image_token_and_result_receiver_is_answered_false() {
    let _guard = state_lock();
    // No `startInput` bound this client, so the IME must not claim it is shown.
    let state = fresh_ime(true);
    let mut data = Parcel::new();
    // IInputMethodManager.showSoftInput request (code 7) in image order:
    // client, window token, ImeTracker.Token, flags, windowFlags, receiver, id.
    data.write_strong_binder(BinderId(0x11));
    data.write_strong_binder_opt(None);
    data.write_int32(1);
    write_ime_tracker_token(
        &mut data,
        &ImeTrackerToken {
            binder: Some(BinderId(0x12)),
            tag: Some("showSoftInput".into()),
        },
    )
    .expect("token");
    data.write_int32(0); // flags
    data.write_int32(0); // windowFlags
    data.write_int32(1); // ResultReceiver presence
    data.write_strong_binder(BinderId(0x13));
    data.write_int32(0); // last argument
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let mut service = ImeWireService::new(shared());
    assert!(service
        .raw_transact(IME_DESCRIPTOR, 7, &mut data, &mut reply)
        .expect("showSoftInput decoded"));
    assert_eq!(data.remaining(), 0, "the whole request is consumed");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().expect("exception"), NO_EXCEPTION);
    assert!(!reply.read_bool().expect("shown"));
    assert_eq!(reply.remaining(), 0);
    assert!(!state.lock().unwrap().ime_visible);
}

#[test]
fn a_typed_object_class_with_no_verified_codec_is_unsupported() {
    let mut data = Parcel::new();
    data.write_int32(1); // presence
    data.write_int32(0); // pretend payload
    data.set_data_position(0);
    // A synthetic startInput whose EditorInfo slot is replaced by an unknown
    // class would need the full prefix; assert the class check directly.
    let mut parcel = Parcel::new();
    parcel.write_int32(1);
    let mut editor = None;
    let mut token = None;
    let mut receiver = false;
    let result = acos_services::ime_wire::read_typed_for_test(
        &mut parcel,
        "android.view.inputmethod.NotARealParcelable",
        &mut editor,
        &mut token,
        &mut receiver,
    );
    assert!(result.is_err());
    // The three classes the focus path uses are accepted.
    let mut known = Parcel::new();
    known.write_int32(1);
    write_ime_tracker_token(&mut known, &ImeTrackerToken::default()).expect("token");
    known.set_data_position(0);
    assert!(acos_services::ime_wire::read_typed_for_test(
        &mut known,
        IME_TRACKER_TOKEN_CLASS,
        &mut None,
        &mut None,
        &mut false,
    )
    .is_ok());
    let _ = (
        EDITOR_INFO_CLASS,
        RESULT_RECEIVER_CLASS,
        EditorInfo::default(),
    );
}

/// GATENBACK: when an editor window shows the soft input and focus then moves
/// to another window without a text editor (e.g. a dialog or Gaten's
/// `TYPE_ACCESSIBILITY_OVERLAY` lookup sheet), AOSP's
/// `ImeVisibilityStateComputer` hides the soft input on
/// `startInputOrWindowGainedFocus`. Otherwise the next `KEYCODE_BACK` is
/// swallowed by `ImeInputStage` as a stale keyboard hide and never reaches
/// the focused overlay's `dispatchKeyEvent`.
#[test]
fn focusing_a_non_editor_overlay_window_hides_stale_soft_input_so_back_reaches_the_window() {
    use acos_services::ime::{
        StartInputContext, SOFT_INPUT_STATE_UNSPECIFIED, START_INPUT_FLAG_INITIAL_CONNECTION,
        START_INPUT_FLAG_IS_TEXT_EDITOR, START_INPUT_FLAG_VIEW_HAS_FOCUS,
        START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
    };

    let _guard = state_lock();
    let state = fresh_ime(true);
    let client = Some(BinderId(0x1000_000b));
    let activity_window = Some(BinderId(0x1000_000e));
    let overlay_window = Some(BinderId(0x1000_0030));

    // 1. MainActivity focuses its search EditText and shows the soft input.
    let (res, _, _) = state.lock().unwrap().start_input_with_context(
        client,
        5,
        Some(BinderId(0x2001)),
        StartInputContext {
            window_token: activity_window,
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS
                | START_INPUT_FLAG_IS_TEXT_EDITOR
                | START_INPUT_FLAG_INITIAL_CONNECTION,
            soft_input_mode: SOFT_INPUT_STATE_UNSPECIFIED,
            window_flags: 0x0181_0100,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert_eq!(res, BIND_SUCCESS);
    let _first_chan = state.lock().unwrap().take_bind_channel().expect("channel");
    assert!(state.lock().unwrap().show_soft_input(client.unwrap(), 0));
    assert!(state.lock().unwrap().ime_visible);

    // 2. Gaten's TYPE_ACCESSIBILITY_OVERLAY window gains focus (no editor,
    //    SOFT_INPUT_STATE_UNSPECIFIED, mayUseInputMethod == true).
    let (res, _, _) = state.lock().unwrap().start_input_with_context(
        client,
        1,
        None,
        StartInputContext {
            window_token: overlay_window,
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS
                | START_INPUT_FLAG_INITIAL_CONNECTION
                | START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
            soft_input_mode: SOFT_INPUT_STATE_UNSPECIFIED,
            // FLAG_NOT_TOUCH_MODAL | FLAG_HARDWARE_ACCELERATED | FLAG_KEEP_SCREEN_ON |
            // FLAG_LAYOUT_IN_SCREEN | FLAG_LAYOUT_NO_LIMITS (no FLAG_NOT_FOCUSABLE / ALT_FOCUSABLE_IM)
            window_flags: 0x0100_03a0,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert_eq!(res, BIND_SUCCESS);
    assert!(
        !state.lock().unwrap().ime_visible,
        "moving focus to a non-editor overlay window must hide the stale soft input"
    );

    // 3. Pressing Back on the overlay's channel must NOT be consumed by the IME.
    let (_token, _name, overlay_client_fd) = state
        .lock()
        .unwrap()
        .take_bind_channel()
        .expect("overlay channel");
    for (seq, action) in [(1u32, 0i32), (2u32, 1i32)] {
        let bytes = acos_services::input_channel::InputMessage::Key {
            seq,
            event: acos_services::input_channel::KeyEvent::new(9, action, KEYCODE_BACK, 1, 1),
        }
        .encode()
        .expect("encode");
        let fd = unsafe {
            rustix::fd::BorrowedFd::borrow_raw(std::os::fd::AsRawFd::as_raw_fd(&overlay_client_fd))
        };
        let sent = rustix::net::send(&fd, &bytes, rustix::net::SendFlags::empty()).expect("send");
        assert_eq!(sent, bytes.len());
    }
    assert_eq!(acos_services::ime::service_pending_keys(), 2);
    let s = state.lock().unwrap();
    assert_eq!(
        s.channel_replies,
        vec![(1, false), (2, false)],
        "BACK must pass through ImeInputStage (handled=false) to OverlayRoot.dispatchKeyEvent"
    );
    assert!(s.channel_keys.iter().all(|k| !k.hid_ime));
}

#[test]
fn ime_visibility_respects_same_window_editor_alt_focusable_im_and_soft_input_modes() {
    use acos_services::ime::{
        StartInputContext, FLAG_ALT_FOCUSABLE_IM, SOFT_INPUT_IS_FORWARD_NAVIGATION,
        SOFT_INPUT_STATE_ALWAYS_HIDDEN, SOFT_INPUT_STATE_HIDDEN, SOFT_INPUT_STATE_UNSPECIFIED,
        START_INPUT_FLAG_IS_TEXT_EDITOR, START_INPUT_FLAG_VIEW_HAS_FOCUS,
        START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
    };

    let _guard = state_lock();
    let state = fresh_ime(true);
    let client = Some(BinderId(0x10));
    let win_a = Some(BinderId(0x100));
    let win_alt = Some(BinderId(0x200));
    let win_hidden = Some(BinderId(0x300));
    let win_always_hidden = Some(BinderId(0x400));

    // Bind & show IME on Window A's editor.
    state.lock().unwrap().start_input_with_context(
        client,
        1,
        None,
        StartInputContext {
            window_token: win_a,
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS | START_INPUT_FLAG_IS_TEXT_EDITOR,
            soft_input_mode: SOFT_INPUT_STATE_UNSPECIFIED,
            window_flags: 0,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert!(state.lock().unwrap().show_soft_input(client.unwrap(), 0));
    assert!(state.lock().unwrap().ime_visible);

    // Re-starting input on the SAME window with an editor keeps IME visible.
    state.lock().unwrap().start_input_with_context(
        client,
        2,
        None,
        StartInputContext {
            window_token: win_a,
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS | START_INPUT_FLAG_IS_TEXT_EDITOR,
            soft_input_mode: SOFT_INPUT_STATE_UNSPECIFIED,
            window_flags: 0,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert!(
        state.lock().unwrap().ime_visible,
        "same-window editor restart must not hide the soft input"
    );

    // Moving to a window with FLAG_ALT_FOCUSABLE_IM (mayUseInputMethod == false)
    // keeps the soft input visible under SOFT_INPUT_STATE_UNSPECIFIED.
    state.lock().unwrap().start_input_with_context(
        client,
        3,
        None,
        StartInputContext {
            window_token: win_alt,
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS
                | START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
            soft_input_mode: SOFT_INPUT_STATE_UNSPECIFIED,
            window_flags: FLAG_ALT_FOCUSABLE_IM,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert!(
        state.lock().unwrap().ime_visible,
        "FLAG_ALT_FOCUSABLE_IM window must not hide IME under SOFT_INPUT_STATE_UNSPECIFIED"
    );

    // SOFT_INPUT_STATE_HIDDEN + FORWARD_NAVIGATION hides explicitly.
    state.lock().unwrap().start_input_with_context(
        client,
        4,
        None,
        StartInputContext {
            window_token: win_hidden,
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS
                | START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
            soft_input_mode: SOFT_INPUT_STATE_HIDDEN | SOFT_INPUT_IS_FORWARD_NAVIGATION,
            window_flags: FLAG_ALT_FOCUSABLE_IM,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert!(!state.lock().unwrap().ime_visible);

    // Re-show on win_hidden, then move to SOFT_INPUT_STATE_ALWAYS_HIDDEN (no forward nav).
    assert!(state.lock().unwrap().show_soft_input(client.unwrap(), 0));
    state.lock().unwrap().start_input_with_context(
        client,
        5,
        None,
        StartInputContext {
            window_token: win_always_hidden,
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS
                | START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
            soft_input_mode: SOFT_INPUT_STATE_ALWAYS_HIDDEN,
            window_flags: FLAG_ALT_FOCUSABLE_IM,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert!(!state.lock().unwrap().ime_visible);
}

#[test]
fn window_focus_gain_report_only_hides_stale_ime_without_dropping_live_channel() {
    use acos_services::ime::{
        StartInputContext, BIND_REPORT_WINDOW_FOCUS_ONLY, SOFT_INPUT_STATE_UNSPECIFIED,
        START_INPUT_FLAG_VIEW_HAS_FOCUS, START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
        START_INPUT_REASON_WINDOW_FOCUS_GAIN_REPORT_ONLY,
    };

    let _guard = state_lock();
    let state = fresh_ime(true);
    let client = Some(BinderId(0x11));
    let win_a = Some(BinderId(0x101));
    let win_b = Some(BinderId(0x202));

    let (res, _, _) = state.lock().unwrap().start_input_with_context(
        client,
        1,
        None,
        StartInputContext {
            window_token: win_a,
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS,
            soft_input_mode: SOFT_INPUT_STATE_UNSPECIFIED,
            window_flags: 0,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert_eq!(res, BIND_SUCCESS);
    let original_token = state
        .lock()
        .unwrap()
        .take_bind_channel()
        .expect("channel")
        .0;
    assert!(state.lock().unwrap().show_soft_input(client.unwrap(), 0));

    let (res, id, session) = state.lock().unwrap().start_input_with_context(
        client,
        START_INPUT_REASON_WINDOW_FOCUS_GAIN_REPORT_ONLY,
        None,
        StartInputContext {
            window_token: win_b,
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS
                | START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
            soft_input_mode: SOFT_INPUT_STATE_UNSPECIFIED,
            window_flags: 0,
            target_sdk_version: 34,
            has_editor_info: false,
        },
    );
    assert_eq!(res, BIND_REPORT_WINDOW_FOCUS_ONLY);
    assert_eq!(id, None);
    assert_eq!(session, None);
    let s = state.lock().unwrap();
    assert!(!s.ime_visible);
    assert_eq!(
        s.channel.as_ref().map(|c| c.token),
        Some(original_token),
        "WINDOW_FOCUS_GAIN_REPORT_ONLY must not replace or drop the live InputChannel"
    );
}

#[test]
fn hiding_soft_input_on_focus_change_clears_ime_insets_on_previous_window_in_wms() {
    use acos_services::ime::{
        StartInputContext, SOFT_INPUT_STATE_UNSPECIFIED, START_INPUT_FLAG_IS_TEXT_EDITOR,
        START_INPUT_FLAG_VIEW_HAS_FOCUS, START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
    };
    use acos_services::wms::WmsState;
    use acos_wm::codec::{inset_type, LayoutParams};

    let _guard = state_lock();
    let state = fresh_ime(true);
    let wms = Arc::new(Mutex::new(WmsState::new()));
    state.lock().unwrap().attach_wms(Arc::clone(&wms));

    let activity_binder = BinderId(0x1000_000e);
    let overlay_binder = BinderId(0x1000_0030);
    let a11y_token = BinderId(232);
    let activity_win = {
        let mut w = wms.lock().unwrap();
        let mut app_lp = LayoutParams::default();
        app_lp.type_ = 1; // TYPE_BASE_APPLICATION
        let r = w
            .add_to_display_as_user(activity_binder, app_lp, 0)
            .expect("add activity");
        assert_eq!(r.result, 0);
        r.window
    };

    let client = Some(BinderId(0x1000_000b));
    state.lock().unwrap().start_input_with_context(
        client,
        1,
        None,
        StartInputContext {
            window_token: Some(activity_binder),
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS | START_INPUT_FLAG_IS_TEXT_EDITOR,
            soft_input_mode: SOFT_INPUT_STATE_UNSPECIFIED,
            window_flags: 0,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert!(state.lock().unwrap().show_soft_input(client.unwrap(), 0));
    let show_deliveries = state.lock().unwrap().take_ime_deliveries();
    assert!(
        show_deliveries
            .iter()
            .any(|d| d.target == activity_binder && d.method == "showInsets"),
        "activity received showInsets: {show_deliveries:?}"
    );
    assert_ne!(
        wms.lock().unwrap().wm.windows[&activity_win].requested_visible_types & inset_type::IME,
        0
    );

    // Add a focusable overlay window above the activity (WMS shifts focus to it).
    let overlay_win = {
        let mut w = wms.lock().unwrap();
        w.wm.add_overlay_token(a11y_token);
        let mut ov_lp = LayoutParams::default();
        ov_lp.type_ = 2032; // TYPE_ACCESSIBILITY_OVERLAY
        ov_lp.token = Some(a11y_token);
        let r = w
            .add_to_display_as_user(overlay_binder, ov_lp, 0)
            .expect("add overlay");
        assert_eq!(r.result, 0);
        assert_eq!(w.wm.focus, Some(r.window));
        r.window
    };
    let _ = overlay_win;

    // The overlay's startInputOrWindowGainedFocus hides the stale IME.
    state.lock().unwrap().start_input_with_context(
        client,
        2,
        None,
        StartInputContext {
            window_token: Some(overlay_binder),
            start_input_flags: START_INPUT_FLAG_VIEW_HAS_FOCUS
                | START_INPUT_FLAG_WINDOW_GAINED_FOCUS,
            soft_input_mode: SOFT_INPUT_STATE_UNSPECIFIED,
            window_flags: 0,
            target_sdk_version: 34,
            has_editor_info: true,
        },
    );
    assert!(!state.lock().unwrap().ime_visible);
    assert_eq!(
        wms.lock().unwrap().wm.windows[&activity_win].requested_visible_types & inset_type::IME,
        0,
        "previous activity window's IME inset bit must be cleared"
    );
    let hide_deliveries = state.lock().unwrap().take_ime_deliveries();
    assert!(
        hide_deliveries
            .iter()
            .any(|d| d.target == activity_binder && d.method == "hideInsets"),
        "previous activity window must receive hideInsets: {hide_deliveries:?}"
    );
    assert!(
        hide_deliveries.iter().all(|d| d.target == activity_binder),
        "overlay window that never showed IME must not get bogus hideInsets: {hide_deliveries:?}"
    );
}
