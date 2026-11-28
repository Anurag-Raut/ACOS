//! End-to-end tests for Tier 1 Slice B: Deep Links (`open_url`) and
//! File & Photo Picker (`content://com.acos.picker/...`).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;

use acos_parcel::{BinderId, Parcel};
use acos_services::ams::{
    AmsState, DataAuthority, DataPattern, DeclaredComponent, Extra, Filter, IntentFacts,
    LaunchOrigin, OpenUrlDelivery, PackageRecord,
};
use acos_services::capability_prompts::{CapabilityPromptState, PromptOptionKind, PromptTrigger};
use acos_services::cursor::read_query_reply;
use acos_services::error::{codes, ServiceError};
use acos_services::host::ServiceHost;
use acos_services::intents::{
    capture_eligible, IntentState, ACTION_CHOOSER, ACTION_CREATE_DOCUMENT, ACTION_GET_CONTENT,
    ACTION_OPEN_DOCUMENT, ACTION_PICK, EXTRA_INTENT, EXTRA_TITLE, FLAG_GRANT_READ_URI_PERMISSION,
};
use acos_services::picker_provider::{CONTENT_PROVIDER_DESCRIPTOR, PICKER_AUTHORITY};
use acos_services::registry::install_with_handles;
use acos_services::value::{Parcelable, Value};

fn deep_link_ams() -> AmsState {
    let mut ams = AmsState::new();
    ams.manifests.packages.push(PackageRecord {
        package: "dev.acos.shop".into(),
        uid: 10042,
        activities: vec![
            DeclaredComponent {
                package: "dev.acos.shop".into(),
                class: "dev.acos.shop.MainActivity".into(),
                exported: true,
                launch_mode: 1, // singleTop
                filters: vec![
                    Filter {
                        actions: vec!["android.intent.action.VIEW".into()],
                        categories: vec![
                            "android.intent.category.DEFAULT".into(),
                            "android.intent.category.BROWSABLE".into(),
                        ],
                        schemes: vec!["myapp".into()],
                        authorities: vec![DataAuthority {
                            host: Some("product".into()),
                            port: -1,
                            wild: false,
                        }],
                        paths: vec![DataPattern {
                            pattern: Some("/item/".into()),
                            kind: 1, // PATTERN_PREFIX
                        }],
                        mime_types: Vec::new(),
                    },
                    Filter {
                        actions: vec!["android.intent.action.VIEW".into()],
                        categories: vec![
                            "android.intent.category.DEFAULT".into(),
                            "android.intent.category.BROWSABLE".into(),
                        ],
                        schemes: vec!["https".into()],
                        authorities: vec![DataAuthority {
                            host: Some("*.shop.example".into()),
                            port: 8443,
                            wild: true,
                        }],
                        paths: vec![DataPattern {
                            pattern: Some("/p/.*".into()),
                            kind: 2, // PATTERN_SIMPLE_GLOB
                        }],
                        mime_types: Vec::new(),
                    },
                ],
            },
            DeclaredComponent {
                package: "dev.acos.shop".into(),
                class: "dev.acos.shop.DetailActivity".into(),
                exported: true,
                launch_mode: 0, // standard
                filters: vec![Filter {
                    actions: vec!["android.intent.action.VIEW".into()],
                    categories: vec![
                        "android.intent.category.DEFAULT".into(),
                        "android.intent.category.BROWSABLE".into(),
                    ],
                    schemes: vec!["https".into()],
                    authorities: vec![DataAuthority {
                        host: Some("shop.example".into()),
                        port: -1,
                        wild: false,
                    }],
                    paths: vec![DataPattern {
                        pattern: Some("/promo".into()),
                        kind: 0, // PATTERN_LITERAL
                    }],
                    mime_types: Vec::new(),
                }],
            },
        ],
        ..Default::default()
    });
    ams
}

#[test]
fn open_url_matches_custom_scheme_and_delivers_on_new_intent_when_single_top() {
    let mut ams = deep_link_ams();
    let token = 0x1001u64;
    ams.register_adopted_activity(token, "dev.acos.shop", "dev.acos.shop.MainActivity");

    // Matching custom scheme + host + pathPrefix with query string stripped for path matching.
    let outcome = ams
        .open_url("myapp://product/item/42?ref=banner#top", None, None, 0)
        .expect("custom scheme matches singleTop MainActivity");
    assert_eq!(outcome.delivery, OpenUrlDelivery::NewIntent);
    assert_eq!(outcome.package, "dev.acos.shop");
    assert_eq!(outcome.class, "dev.acos.shop.MainActivity");
    assert_eq!(outcome.token, token);

    let (ni_token, ni_intent) = ams
        .take_pending_new_intent(0)
        .expect("onNewIntent must be queued for primary process");
    assert_eq!(ni_token, token);
    assert_eq!(ni_intent.action.as_deref(), Some("android.intent.action.VIEW"));
    assert_eq!(
        ni_intent.data.as_deref(),
        Some("myapp://product/item/42?ref=banner#top")
    );
    assert!(ni_intent
        .categories
        .iter()
        .any(|c| c == "android.intent.category.BROWSABLE"));
    assert!(ams.pending_launches.is_empty());
}

#[test]
fn open_url_matches_https_wildcard_port_glob_and_launches_standard_activity() {
    let mut ams = deep_link_ams();

    // Wildcard subdomain + explicit port + pathPattern glob launches MainActivity when not running.
    let first = ams
        .open_url("https://EU.SHOP.EXAMPLE:8443/p/sku-9?utm=1", None, None, 0)
        .expect("wildcard https host+port+glob matches");
    assert_eq!(first.delivery, OpenUrlDelivery::Launch);
    assert_eq!(first.class, "dev.acos.shop.MainActivity");
    assert_eq!(ams.pending_launches.len(), 1);
    assert_eq!(ams.pending_launches[0].origin, LaunchOrigin::Host);

    // Literal path on standard activity launches a fresh instance.
    let second = ams
        .open_url("https://shop.example/promo?code=SAVE", None, None, 0)
        .expect("literal path matches DetailActivity");
    assert_eq!(second.delivery, OpenUrlDelivery::Launch);
    assert_eq!(second.class, "dev.acos.shop.DetailActivity");

    // Calling open_url again with FLAG_ACTIVITY_SINGLE_TOP reuses the top standard activity.
    const FLAG_ACTIVITY_SINGLE_TOP: i32 = 0x2000_0000;
    let third = ams
        .open_url(
            "https://shop.example/promo?code=AGAIN",
            None,
            None,
            FLAG_ACTIVITY_SINGLE_TOP,
        )
        .expect("singleTop flag reuses top DetailActivity");
    assert_eq!(third.delivery, OpenUrlDelivery::NewIntent);
    assert_eq!(third.token, second.token);
}

#[test]
fn open_url_fails_honestly_with_start_not_found_and_never_captures() {
    let mut ams = deep_link_ams();
    let intents = std::sync::Arc::new(std::sync::Mutex::new(IntentState::new()));
    ams.intents = Some(std::sync::Arc::clone(&intents));

    for unmatched in [
        "myapp://wronghost/item/42",
        "myapp://product/other/42",
        "https://eu.shop.example:9999/p/sku-9",
        "https://unmatched.example/foo",
        "unknownscheme://hello",
    ] {
        let err = ams
            .open_url(unmatched, None, None, 0)
            .expect_err(&format!("{unmatched} must fail with START_NOT_FOUND"));
        assert_eq!(err.code(), codes::START_NOT_FOUND);
        assert!(matches!(err, ServiceError::StartNotFound { .. }));
    }
    assert!(
        intents.lock().unwrap().list().is_empty(),
        "unmatched open_url must never silently fall back to external capture"
    );
}

#[test]
fn picker_actions_and_chooser_wrapper_are_captured_and_emit_prompts() {
    let mut intents = IntentState::new();
    let mut prompts = CapabilityPromptState::new();

    // 1. OPEN_DOCUMENT
    let open_doc = IntentFacts {
        action: Some(ACTION_OPEN_DOCUMENT.into()),
        mime_type: Some("image/*".into()),
        categories: vec!["android.intent.category.OPENABLE".into()],
        ..Default::default()
    };
    assert!(capture_eligible(&open_doc));
    let cap1 = intents.capture(&open_doc, 0, 101, true, 1_000).expect("captured");
    assert!(cap1.is_picker());
    assert!(!cap1.is_create_document());
    let prompt1 = prompts.record_captured_intent(&cap1);
    assert_eq!(prompt1.trigger, PromptTrigger::IntentCaptured);
    assert_eq!(prompt1.options[0].kind, PromptOptionKind::FilePicker);
    assert_eq!(prompt1.options[0].capture_seq, Some(cap1.seq));
    assert_eq!(prompt1.options[0].accept.as_deref(), Some("image/*"));
    assert_eq!(prompt1.options[1].kind, PromptOptionKind::CancelPicker);

    // 2. CREATE_DOCUMENT with EXTRA_TITLE
    let mut extras = BTreeMap::new();
    extras.insert(EXTRA_TITLE.into(), Extra::String("report.csv".into()));
    let create_doc = IntentFacts {
        action: Some(ACTION_CREATE_DOCUMENT.into()),
        mime_type: Some("text/csv".into()),
        extras,
        ..Default::default()
    };
    let cap2 = intents.capture(&create_doc, 0, 102, true, 2_000).expect("captured");
    assert!(cap2.is_picker());
    assert!(cap2.is_create_document());
    assert_eq!(cap2.suggested_filename(), Some("report.csv"));

    // 3. ACTION_CHOOSER wrapping ACTION_GET_CONTENT
    let inner = IntentFacts {
        action: Some(ACTION_GET_CONTENT.into()),
        mime_type: Some("application/pdf".into()),
        ..Default::default()
    };
    let mut chooser_extras = BTreeMap::new();
    chooser_extras.insert(EXTRA_INTENT.into(), Extra::Parcelable(inner.to_parcelable()));
    let chooser = IntentFacts {
        action: Some(ACTION_CHOOSER.into()),
        extras: chooser_extras,
        ..Default::default()
    };
    assert!(capture_eligible(&chooser));
    let cap3 = intents.capture(&chooser, 0, 103, true, 3_000).expect("captured");
    assert!(cap3.is_picker());
    assert_eq!(cap3.wrapped_action.as_deref(), Some(ACTION_GET_CONTENT));
    assert_eq!(cap3.effective_mime_type(), Some("application/pdf"));

    // A chooser's own EXTRA_TITLE is the dialog title; the wrapped picker's
    // EXTRA_TITLE is the suggested file name and wins.
    let mut inner_extras = BTreeMap::new();
    inner_extras.insert(EXTRA_TITLE.into(), Extra::String("report.csv".into()));
    let titled_inner = IntentFacts {
        action: Some(ACTION_CREATE_DOCUMENT.into()),
        mime_type: Some("text/csv".into()),
        extras: inner_extras,
        ..Default::default()
    };
    let mut outer_extras = BTreeMap::new();
    outer_extras.insert(
        EXTRA_INTENT.into(),
        Extra::Parcelable(titled_inner.to_parcelable()),
    );
    outer_extras.insert(EXTRA_TITLE.into(), Extra::String("Save as".into()));
    let titled_chooser = IntentFacts {
        action: Some(ACTION_CHOOSER.into()),
        extras: outer_extras,
        ..Default::default()
    };
    let titled = intents
        .capture(&titled_chooser, 0, 105, true, 3_500)
        .expect("captured");
    assert_eq!(titled.suggested_filename(), Some("report.csv"));

    // 4. ACTION_PICK with data URI (e.g. MediaStore Images URI) and no MIME type
    let pick_uri = IntentFacts {
        action: Some(ACTION_PICK.into()),
        data: Some("content://media/external/images/media".into()),
        ..Default::default()
    };
    assert!(capture_eligible(&pick_uri));
    let cap4 = intents.capture(&pick_uri, 0, 104, true, 4_000).expect("captured");
    assert!(cap4.is_picker());

    // Scripted result preserves URI permission grant flags.
    let scripted = intents
        .script_result_with_flags(
            Some(cap1.seq),
            Some(-1),
            Some("content://com.acos.picker/1/photo.png".into()),
            Some("image/png".into()),
            None,
            FLAG_GRANT_READ_URI_PERMISSION,
            BTreeMap::new(),
        )
        .expect("script result");
    assert_eq!(scripted.flags, FLAG_GRANT_READ_URI_PERMISSION);
}

fn write_attribution_source(parcel: &mut Parcel) {
    let attr = Parcelable::ordered(
        "android.content.AttributionSource",
        vec![
            Value::Int(0),
            Value::Int(1000),
            Value::Int(10000),
            Value::Str(Some("dev.acos.test".into())),
            Value::Str(None),
            Value::Binder(None),
            Value::StringArray(None),
            Value::List(None),
        ],
    );
    acos_services::parcelable::write_payload(parcel, &attr).expect("write AttributionSource");
}

fn write_string_uri(parcel: &mut Parcel, uri: &str) {
    let p = Parcelable::ordered(
        "android.net.Uri",
        vec![Value::Int(1), Value::Str8(Some(uri.to_string()))],
    );
    acos_services::parcelable::write_payload(parcel, &p).expect("write Uri");
}

#[test]
fn picker_content_provider_serves_fd_cursor_and_mime_over_binder() {
    let dir = std::env::temp_dir().join(format!(
        "acos-picker-binder-test-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let sample = dir.join("receipt.pdf");
    let expected_bytes = b"%PDF-1.7\nACOS picker payload\n%%EOF";
    std::fs::write(&sample, expected_bytes).expect("write sample");

    let mut host = ServiceHost::new();
    let live = install_with_handles(&mut host);

    let entry = live
        .picker
        .lock()
        .unwrap()
        .register_file(&sample, None, None, false)
        .expect("register file");
    assert!(entry.uri.starts_with("content://com.acos.picker/1/"));
    assert_eq!(entry.mime_type, "application/pdf");

    // Look up the com.acos.picker binder through ATMS's ProviderRegistry (the
    // same table IActivityManager.getContentProvider consults).
    let picker_binder = {
        let mut atms = live.atms.lock().unwrap();
        let provider_entry = atms
            .provider_registry
            .acquire("getContentProvider", PICKER_AUTHORITY, "dev.acos.test")
            .expect("com.acos.picker must be registered in ProviderRegistry");
        BinderId(provider_entry.binder)
    };

    // 1. openFile (code 14) -> ParcelFileDescriptor carrying real OS fd.
    let mut req = Parcel::new();
    req.write_interface_token(CONTENT_PROVIDER_DESCRIPTOR).unwrap();
    write_attribution_source(&mut req);
    write_string_uri(&mut req, &entry.uri);
    req.write_string16("r").unwrap();
    req.write_strong_binder_opt(None);
    req.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(picker_binder, 14, &mut req, &mut reply, 0)
        .expect("openFile transact");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "PFD present");
    assert_eq!(reply.read_int32().unwrap(), 0, "no commFd");
    let raw_fd = reply
        .read_file_descriptor()
        .expect("read fd")
        .expect("non-null fd");
    let fd_borrowed = reply
        .borrowed_file_descriptor(raw_fd)
        .expect("parcel owns fd");
    let mut file = File::from(fd_borrowed.try_clone_to_owned().expect("clone fd"));
    let mut got = Vec::new();
    file.read_to_end(&mut got).expect("read from binder fd");
    assert_eq!(got, expected_bytes);

    // 2. openAssetFile (code 15) -> ParcelFileDescriptor + offset=0 +
    //    UNKNOWN_LENGTH (a whole file; a declared length breaks
    //    openFileDescriptor and bounds openOutputStream).
    let mut req = Parcel::new();
    req.write_interface_token(CONTENT_PROVIDER_DESCRIPTOR).unwrap();
    write_attribution_source(&mut req);
    write_string_uri(&mut req, &entry.uri);
    req.write_string16("r").unwrap();
    req.write_strong_binder_opt(None);
    req.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(picker_binder, 15, &mut req, &mut reply, 0)
        .expect("openAssetFile transact");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_int32().unwrap(), 1, "AFD present");
    assert_eq!(reply.read_int32().unwrap(), 0, "no commFd");
    let _afd_fd = reply.read_file_descriptor().expect("read afd fd");
    assert_eq!(reply.read_int64().unwrap(), 0, "mStartOffset");
    assert_eq!(reply.read_int64().unwrap(), -1, "mLength is UNKNOWN_LENGTH");
    assert_eq!(reply.read_int32().unwrap(), 0, "mExtras null");

    // 2b. openTypedAssetFile (code 23): a matching filter and */* open the
    //     file; any other type is a FileNotFoundException (DatabaseUtils code 1).
    for (filter, matches) in [
        ("application/pdf", true),
        ("application/*", true),
        ("*/*", true),
        ("image/png", false),
    ] {
        let mut req = Parcel::new();
        req.write_interface_token(CONTENT_PROVIDER_DESCRIPTOR).unwrap();
        write_attribution_source(&mut req);
        write_string_uri(&mut req, &entry.uri);
        req.write_string16(filter).unwrap();
        acos_services::parcelable::write_bundle(&mut req, None).unwrap();
        req.write_strong_binder_opt(None);
        req.set_data_position(0);
        let mut reply = Parcel::new();
        host.transact(picker_binder, 23, &mut req, &mut reply, 0)
            .expect("openTypedAssetFile transact");
        reply.set_data_position(0);
        if matches {
            assert_eq!(reply.read_int32().unwrap(), 0, "no exception for {filter}");
            assert_eq!(reply.read_int32().unwrap(), 1, "AFD present");
            assert_eq!(reply.read_int32().unwrap(), 0, "no commFd");
            let _fd = reply.read_file_descriptor().expect("typed fd");
            assert_eq!(reply.read_int64().unwrap(), 0, "mStartOffset");
            assert_eq!(reply.read_int64().unwrap(), -1, "mLength");
        } else {
            assert_eq!(reply.read_int32().unwrap(), 1, "FileNotFoundException");
            let message = reply.read_string16().unwrap().expect("message");
            assert!(message.contains("image/png"), "{message}");
        }
    }

    // 2c. createCancellationSignal (code 24) -> a null ICancellationSignal.
    let mut req = Parcel::new();
    req.write_interface_token(CONTENT_PROVIDER_DESCRIPTOR).unwrap();
    req.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(picker_binder, 24, &mut req, &mut reply, 0)
        .expect("createCancellationSignal transact");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(reply.read_strong_binder().expect("binder"), None);

    // 3. query (code 1) -> BulkCursorDescriptor + inline CursorWindow.
    let mut req = Parcel::new();
    req.write_interface_token(CONTENT_PROVIDER_DESCRIPTOR).unwrap();
    write_attribution_source(&mut req);
    write_string_uri(&mut req, &entry.uri);
    req.write_int32(3); // projection count
    req.write_string16("_display_name").unwrap();
    req.write_string16("_size").unwrap();
    req.write_string16("mime_type").unwrap();
    acos_services::parcelable::write_bundle(&mut req, None).unwrap();
    req.write_strong_binder_opt(None); // observer
    req.write_strong_binder_opt(None); // cancellation signal
    req.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(picker_binder, 1, &mut req, &mut reply, 0)
        .expect("query transact");
    reply.set_data_position(0);
    let desc = read_query_reply(&mut reply)
        .expect("decode BulkCursorDescriptor")
        .expect("cursor present");
    assert_eq!(desc.count, 1);
    assert_eq!(desc.column_names, vec!["_display_name", "_size", "mime_type"]);
    let win = desc.window.expect("inline CursorWindow");
    assert_eq!(win.string(0, 0).as_deref(), Some("receipt.pdf"));
    assert_eq!(win.long(0, 1), Some(expected_bytes.len() as i64));
    assert_eq!(win.string(0, 2).as_deref(), Some("application/pdf"));

    // 4. getType (code 2) -> "application/pdf".
    let mut req = Parcel::new();
    req.write_interface_token(CONTENT_PROVIDER_DESCRIPTOR).unwrap();
    write_attribution_source(&mut req);
    write_string_uri(&mut req, &entry.uri);
    req.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(picker_binder, 2, &mut req, &mut reply, 0)
        .expect("getType transact");
    reply.set_data_position(0);
    assert_eq!(reply.read_int32().unwrap(), 0, "no exception");
    assert_eq!(
        reply.read_string16().unwrap().as_deref(),
        Some("application/pdf")
    );

    // 5. Path traversal attempt over Binder is refused.
    let mut req = Parcel::new();
    req.write_interface_token(CONTENT_PROVIDER_DESCRIPTOR).unwrap();
    write_attribution_source(&mut req);
    write_string_uri(&mut req, "content://com.acos.picker/1/../../etc/passwd");
    req.write_string16("r").unwrap();
    req.write_strong_binder_opt(None);
    req.set_data_position(0);
    let mut reply = Parcel::new();
    assert!(
        host.transact(picker_binder, 14, &mut req, &mut reply, 0)
            .is_err(),
        "path traversal URI must be refused over Binder"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
