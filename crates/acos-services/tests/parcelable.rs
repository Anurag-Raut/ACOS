use acos_services::exception::{
    EX_BAD_PARCELABLE, EX_ILLEGAL_ARGUMENT, EX_ILLEGAL_STATE, EX_NETWORK_MAIN_THREAD,
    EX_NULL_POINTER, EX_PARCELABLE, EX_SECURITY, EX_SERVICE_SPECIFIC, EX_UNSUPPORTED_OPERATION,
    NO_EXCEPTION,
};
use acos_services::parcelable::{self, Verification};
use acos_services::value::{Parcelable, RawFdValue, Value};
use acos_services::Bundle;

fn dex_dir() -> std::path::PathBuf {
    acos_services::workspace_path(".cache/boot/android-34/dex/framework")
        .expect("boot DEX cache (run from the repo root)")
}

#[test]
fn codec_field_orders_match_the_framework_dex() {
    let dir = dex_dir();
    let mut checked = 0usize;
    let mut full = 0usize;
    let mut failures = Vec::new();
    for spec in parcelable::SPECS {
        let Ok(scanned) = parcelable::Scanner::write_to_parcel(&dir, spec.class) else {
            // Some curated ACOS-owned values have no Java `writeToParcel` on the
            // boot classpath; that is expected.
            continue;
        };
        let result = parcelable::verify(spec, &scanned);
        let matched = result.matched_calls;
        let real_len = result.real_calls;
        match spec.verification {
            Verification::Full => {
                if !result.is_full() {
                    failures.push(format!(
                        "{}: consumed {matched}/{real_len} calls, spec {} fields, all_fields={}",
                        spec.class,
                        spec.fields.len(),
                        result.all_fields
                    ));
                } else {
                    full += 1;
                }
            }
            Verification::Prefix => {
                // At least the leading field must be in the real order.
                if matched == 0 && !spec.fields.is_empty() {
                    failures.push(format!(
                        "{}: no prefix match ({real_len} real ops)",
                        spec.class
                    ));
                }
            }
            Verification::Curated => {}
        }
        checked += 1;
        println!(
            "{:<60} {:8} {:>3}/{:<3} real={:<3}",
            spec.class,
            format!("{:?}", spec.verification),
            matched,
            spec.fields.len(),
            real_len
        );
    }
    println!("verified {checked} specs against the boot DEX ({full} full)");
    assert!(checked >= 10, "expected to verify at least 10 codecs");
    assert!(failures.is_empty(), "codec drift:\n{}", failures.join("\n"));
}

#[test]
fn parcel_exception_constants_match_the_framework() {
    let dir = dex_dir();
    let bytes = {
        // `android.os.Parcel` lives in classes3.dex on the reference image; scan
        // every file rather than hard-code the split.
        let mut found = None;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().map(|e| e == "dex").unwrap_or(false) {
                let data = std::fs::read(&path).unwrap();
                if let Ok(dex) = acos_dex::DexFile::parse(data) {
                    if dex.find_class("Landroid/os/Parcel;").is_some() {
                        found = Some(dex);
                        break;
                    }
                }
            }
        }
        found.expect("android.os.Parcel in the boot DEX")
    };
    let def = bytes.find_class("Landroid/os/Parcel;").unwrap();
    let values = framework_static_ints(&bytes, def);
    let expected = [
        ("EX_SECURITY", EX_SECURITY),
        ("EX_BAD_PARCELABLE", EX_BAD_PARCELABLE),
        ("EX_ILLEGAL_ARGUMENT", EX_ILLEGAL_ARGUMENT),
        ("EX_NULL_POINTER", EX_NULL_POINTER),
        ("EX_ILLEGAL_STATE", EX_ILLEGAL_STATE),
        ("EX_NETWORK_MAIN_THREAD", EX_NETWORK_MAIN_THREAD),
        ("EX_UNSUPPORTED_OPERATION", EX_UNSUPPORTED_OPERATION),
        ("EX_SERVICE_SPECIFIC", EX_SERVICE_SPECIFIC),
        ("EX_PARCELABLE", EX_PARCELABLE),
    ];
    for (name, value) in expected {
        let framework = values
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("{name} not in the boot DEX"));
        assert_eq!(framework, i64::from(value), "{name} drifted");
    }
    assert_eq!(NO_EXCEPTION, 0);
}

fn framework_static_ints(dex: &acos_dex::DexFile, def: &acos_dex::ClassDef) -> Vec<(String, i64)> {
    let mut out = Vec::new();
    let Ok(Some(cd)) = dex.class_data(def) else {
        return out;
    };
    let Ok(Some(values)) = dex.static_values(def) else {
        return out;
    };
    for (i, field) in cd.static_fields.iter().enumerate() {
        let Ok(fr) = dex.field_ref(field.field_idx) else {
            continue;
        };
        if let Some(acos_dex::EncodedValue::Int(v)) = values.get(i) {
            out.push((fr.name.to_string(), i64::from(*v)));
        }
    }
    out
}

#[test]
fn task_description_reads_the_api34_conditional_layout() {
    // Regression (W60-A/W65-A): API-34 `TaskDescription.writeToParcel` writes
    // `int labelPresent; [writeString label]`, `int iconPresent; [Icon]`, four
    // colours, two contrast booleans, resizeMode/minWidth/minHeight, then
    // `int iconFilenamePresent; [writeString iconFilename]`, then
    // colorBackgroundFloating. The old linear six-field spec read the icon
    // presence marker as a string length and failed with "String16 is not NUL
    // terminated". This is the exact launch-path byte layout (Activity's default
    // `new TaskDescription()` with only the status-bar colour set).
    let mut parcel = acos_parcel::Parcel::new();
    parcel.write_int32(0); // labelPresent
    parcel.write_int32(0); // iconPresent
    parcel.write_int32(0); // colorPrimary
    parcel.write_int32(0); // colorBackground
    parcel.write_int32(0xff11_2233u32 as i32); // statusBarColor
    parcel.write_int32(0); // navigationBarColor
    parcel.write_bool(false); // ensureStatusBarContrastWhenTransparent
    parcel.write_bool(false); // ensureNavigationBarContrastWhenTransparent
    parcel.write_int32(2); // resizeMode
    parcel.write_int32(-1); // minWidth
    parcel.write_int32(-1); // minHeight
    parcel.write_int32(0); // iconFilenamePresent
    parcel.write_int32(0); // colorBackgroundFloating
    parcel.set_data_position(0);
    let decoded =
        parcelable::read_payload("android.app.ActivityManager$TaskDescription", &mut parcel)
            .expect("read the real API-34 TaskDescription payload");
    parcel
        .enforce_no_data_avail()
        .expect("the conditional reader consumed the whole payload");
    assert_eq!(
        decoded.field(0),
        Some(&Value::Cond {
            present: false,
            values: vec![]
        })
    );
    assert_eq!(decoded.field(4), Some(&Value::Int(0xff11_2233u32 as i32)));
    assert_eq!(decoded.field(8), Some(&Value::Int(2)));
    assert_eq!(decoded.field(9), Some(&Value::Int(-1)));
    assert_eq!(decoded.field(12), Some(&Value::Int(0)));

    // Present branches: a label and an icon filename, no icon.
    let mut parcel = acos_parcel::Parcel::new();
    parcel.write_int32(1);
    parcel.write_string16_opt(Some("Counter")).unwrap();
    parcel.write_int32(0); // iconPresent
    parcel.write_int32(0);
    parcel.write_int32(0);
    parcel.write_int32(0);
    parcel.write_int32(0);
    parcel.write_bool(true);
    parcel.write_bool(false);
    parcel.write_int32(2);
    parcel.write_int32(-1);
    parcel.write_int32(-1);
    parcel.write_int32(1);
    parcel.write_string16_opt(Some("icon.png")).unwrap();
    parcel.write_int32(0);
    parcel.set_data_position(0);
    let decoded =
        parcelable::read_payload("android.app.ActivityManager$TaskDescription", &mut parcel)
            .expect("read a TaskDescription with label + iconFilename");
    parcel
        .enforce_no_data_avail()
        .expect("consumed present branches");
    assert_eq!(
        decoded.field(0),
        Some(&Value::Cond {
            present: true,
            values: vec![Value::Str(Some("Counter".into()))],
        })
    );
    assert_eq!(
        decoded.field(11),
        Some(&Value::Cond {
            present: true,
            values: vec![Value::Str(Some("icon.png".into()))],
        })
    );
}

#[test]
fn every_spec_round_trips_through_a_parcel() {
    for spec in parcelable::SPECS {
        let value = sample_for(spec.class);
        let Some(value) = value else {
            continue;
        };
        let mut parcel = acos_parcel::Parcel::new();
        parcelable::write_payload(&mut parcel, &value).expect("write payload");
        parcel.set_data_position(0);
        let decoded = parcelable::read_payload(spec.class, &mut parcel).expect("read payload");
        if spec.class == "android.view.DisplayInfo" {
            // `hdrSdrRatio` is `Float.NaN` on the wire (the framework's "no
            // SDR/HDR ratio known" value) and NaN is not equal to itself, so
            // compare the decoded struct (which is what consumers see) rather
            // than the raw field list.
            assert_eq!(
                acos_services::display::DisplayInfo::from_parcelable(&decoded)
                    .expect("decode DisplayInfo"),
                acos_services::display::DisplayInfo::from_parcelable(&value)
                    .expect("decode the original DisplayInfo"),
                "{} did not round-trip",
                spec.class
            );
        } else if spec.class == "android.os.ParcelFileDescriptor" {
            // Descriptors are duplicated by the parcel, so compare presence.
            // Field 0 is the `mCommFd` presence marker, field 1 the fd.
            assert!(matches!(value.field(1), Some(Value::RawFd(Some(_)))));
            assert!(matches!(decoded.field(1), Some(Value::RawFd(Some(_)))));
        } else {
            assert_eq!(decoded, value, "{} did not round-trip", spec.class);
        }
        parcel.enforce_no_data_avail().expect("no trailing bytes");
    }
}

fn sample_for(class: &str) -> Option<Parcelable> {
    Some(match class {
        "android.graphics.Rect" => Parcelable::ordered(
            class,
            vec![
                Value::Int(0),
                Value::Int(128),
                Value::Int(1080),
                Value::Int(2337),
            ],
        ),
        "android.graphics.Point" => {
            Parcelable::ordered(class, vec![Value::Int(1080), Value::Int(2400)])
        }
        "android.content.ComponentName" => Parcelable::ordered(
            class,
            vec![
                Value::Str(Some("dev.acos.fixtures.helloviews".into())),
                Value::Str(Some("dev.acos.fixtures.helloviews.MainActivity".into())),
            ],
        ),
        "android.hardware.OverlayProperties" => {
            Parcelable::ordered(class, vec![Value::Bool(false), Value::Bool(false)])
        }
        "android.os.ParcelFileDescriptor" => {
            let fd = acos_parcel::ParcelFd::memfd("test-pfd").ok()?;
            Parcelable::ordered(
                class,
                vec![Value::Int(0), Value::RawFd(Some(RawFdValue(fd)))],
            )
        }
        "android.os.Bundle" => {
            let mut b = Bundle::new();
            b.put("value", Value::Str(Some("hello".into())));
            Parcelable::ordered(class, vec![Value::Bundle(Some(b))])
        }
        "android.view.DisplayInfo" => {
            acos_services::display::DisplayInfo::hello_views().to_parcelable()
        }
        "android.view.Display$Mode" => Parcelable::ordered(
            class,
            vec![
                Value::Int(1),
                Value::Int(1080),
                Value::Int(2400),
                Value::Float(60.000004),
                Value::FloatArray(Some(vec![50.0, 120.0])),
                Value::IntArray(Some(vec![1, 3])),
            ],
        ),
        "android.view.SurfaceControl$RefreshRateRange" => {
            Parcelable::ordered(class, vec![Value::Float(60.0), Value::Float(120.0)])
        }
        "android.content.pm.ApplicationInfo" => acos_services::pms::AppInfo::hello_views_fallback(
            std::path::Path::new("hello-views.apk"),
        )
        .to_parcelable(),
        "android.view.autofill.AutofillId" => {
            Parcelable::ordered(class, vec![Value::Int(1), Value::Int(2)])
        }
        "android.window.SizeConfigurationBuckets" => Parcelable::ordered(
            class,
            vec![
                Value::Byte(0),
                Value::IntArray(Some(vec![1080, 2400])),
                Value::IntArray(Some(vec![2400, 1080])),
                Value::IntArray(Some(vec![411])),
                Value::IntArray(Some(vec![914])),
            ],
        ),
        // The image's shape: two `writeBoolean` presence flags guarding the
        // binder and the callback list (MPROC6).
        "android.app.servertransaction.ClientTransaction" => Parcelable::ordered(
            class,
            vec![
                Value::Cond {
                    present: true,
                    values: vec![Value::Binder(None)],
                },
                Value::NamedParcelable(Parcelable::Null),
                Value::Cond {
                    present: true,
                    values: vec![Value::List(Some(vec![Value::Parcelable(
                        Parcelable::ordered(
                            "android.app.servertransaction.ResumeActivityItem",
                            vec![
                                Value::Int(2),
                                Value::Bool(false),
                                Value::Bool(false),
                                Value::Bool(false),
                            ],
                        ),
                    )]))],
                },
            ],
        ),
        "android.os.LocaleList" => {
            Parcelable::ordered(class, vec![Value::Str8(Some("en-US".into()))])
        }
        "android.app.WindowConfiguration" => Parcelable::ordered(
            class,
            vec![
                Value::Parcelable(Parcelable::ordered(
                    "android.graphics.Rect",
                    vec![
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(1080),
                        Value::Int(2400),
                    ],
                )),
                Value::TypedObject(Parcelable::ordered(
                    "android.graphics.Rect",
                    vec![
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(1080),
                        Value::Int(2400),
                    ],
                )),
                Value::Parcelable(Parcelable::ordered(
                    "android.graphics.Rect",
                    vec![
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(1080),
                        Value::Int(2400),
                    ],
                )),
                Value::Int(1),
                Value::Int(1),
                Value::Int(0),
                Value::Int(0),
                Value::Int(1),
                Value::Int(0),
            ],
        ),
        "android.content.res.Configuration" => Parcelable::ordered(
            class,
            vec![
                Value::Float(1.0),
                Value::Int(0), // mcc
                Value::Int(0), // mnc
                Value::TypedObject(Parcelable::ordered(
                    "android.os.LocaleList",
                    vec![Value::Str8(Some("en-US".into()))],
                )),
                Value::Int(0),   // userSetLocale
                Value::Int(0),   // touchscreen
                Value::Int(0),   // keyboard
                Value::Int(0),   // keyboardHidden
                Value::Int(0),   // hardKeyboardHidden
                Value::Int(0),   // navigation
                Value::Int(0),   // navigationHidden
                Value::Int(0),   // orientation
                Value::Int(0),   // screenLayout
                Value::Int(0),   // colorMode
                Value::Int(0),   // uiMode
                Value::Int(411), // screenWidthDp
                Value::Int(914), // screenHeightDp
                Value::Int(411), // smallestScreenWidthDp
                Value::Int(420), // densityDpi
                Value::Int(0),   // compatScreenWidthDp
                Value::Int(0),   // compatScreenHeightDp
                Value::Int(0),   // compatSmallestScreenWidthDp
                Value::Parcelable(Parcelable::ordered(
                    "android.app.WindowConfiguration",
                    vec![
                        Value::Parcelable(Parcelable::ordered(
                            "android.graphics.Rect",
                            vec![
                                Value::Int(0),
                                Value::Int(0),
                                Value::Int(1080),
                                Value::Int(2400),
                            ],
                        )),
                        Value::TypedObject(Parcelable::ordered(
                            "android.graphics.Rect",
                            vec![
                                Value::Int(0),
                                Value::Int(0),
                                Value::Int(1080),
                                Value::Int(2400),
                            ],
                        )),
                        Value::Parcelable(Parcelable::ordered(
                            "android.graphics.Rect",
                            vec![
                                Value::Int(0),
                                Value::Int(0),
                                Value::Int(1080),
                                Value::Int(2400),
                            ],
                        )),
                        Value::Int(1),
                        Value::Int(1),
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(1),
                        Value::Int(0),
                    ],
                )),
                Value::Int(0), // assetsSeq
                Value::Int(0), // seq
                Value::Int(0), // fontWeightAdjustment
                Value::Int(0), // mGrammaticalGender
            ],
        ),
        "android.content.AttributionSource" => Parcelable::ordered(
            class,
            vec![
                Value::Int(0),
                Value::Int(4242),
                Value::Int(10192),
                Value::Str(Some("dev.acos.fixtures.helloviews".into())),
                Value::Str(None),
                Value::Binder(None),
                Value::StringArray(None),
                Value::List(None),
            ],
        ),
        // `Uri.writeToParcel`: type tag then String8.
        "android.net.Uri" => Parcelable::ordered(
            class,
            vec![
                Value::Int(3), // HierarchicalUri
                Value::Str8(Some("content://settings/system".into())),
            ],
        ),
        "android.app.ResultInfo" => Parcelable::ordered(
            class,
            vec![
                Value::Str(None),
                Value::Int(1),
                Value::Int(-1),
                Value::Cond {
                    present: false,
                    values: vec![],
                },
            ],
        ),
        "android.os.PersistableBundle" => {
            let mut b = Bundle::new();
            b.put("value", Value::Str(Some("hello".into())));
            Parcelable::ordered(class, vec![Value::Bundle(Some(b))])
        }
        "android.app.servertransaction.ResumeActivityItem" => Parcelable::ordered(
            class,
            vec![
                Value::Int(10),
                Value::Bool(true),
                Value::Bool(false),
                Value::Bool(true),
            ],
        ),
        "android.app.servertransaction.ActivityRelaunchItem" => Parcelable::ordered(
            class,
            vec![
                Value::List(None),
                Value::List(None),
                Value::Int(0),
                Value::TypedObject(Parcelable::Null),
                Value::Bool(false),
            ],
        ),
        "android.window.ClientWindowFrames" => {
            let rect = || {
                Value::Parcelable(Parcelable::ordered(
                    "android.graphics.Rect",
                    vec![
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(1080),
                        Value::Int(2400),
                    ],
                ))
            };
            Parcelable::ordered(
                class,
                vec![
                    rect(),
                    rect(),
                    rect(),
                    Value::TypedObject(Parcelable::ordered(
                        "android.graphics.Rect",
                        vec![
                            Value::Int(0),
                            Value::Int(0),
                            Value::Int(1080),
                            Value::Int(2400),
                        ],
                    )),
                    Value::Bool(false),
                    Value::Float(1.0),
                ],
            )
        }
        "android.graphics.Insets" => Parcelable::ordered(
            class,
            vec![
                Value::Int(0),
                Value::Int(128),
                Value::Int(0),
                Value::Int(63),
            ],
        ),
        "android.view.InsetsSource" => Parcelable::ordered(
            class,
            vec![
                Value::Int(1),
                Value::Int(0),
                Value::Parcelable(Parcelable::ordered(
                    "android.graphics.Rect",
                    vec![
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(1080),
                        Value::Int(128),
                    ],
                )),
                Value::Cond {
                    present: false,
                    values: vec![],
                },
                Value::Bool(true),
                Value::Int(0),
                Value::Bool(false),
            ],
        ),
        "android.view.InsetsSourceControl" => Parcelable::ordered(
            class,
            vec![
                Value::Int(1),
                Value::Int(0),
                Value::TypedObject(Parcelable::Null),
                Value::Bool(true),
                Value::TypedObject(Parcelable::Null),
                Value::TypedObject(Parcelable::Null),
                Value::Bool(false),
            ],
        ),
        "android.view.InsetsSourceControl$Array" => {
            Parcelable::ordered(class, vec![Value::List(None)])
        }
        "android.app.Notification" => Parcelable::ordered(
            class,
            vec![
                Value::Int(1),                  // version
                Value::Binder(None),            // mAllowlistToken
                Value::Long(1_700_000_000_000), // when
                Value::Long(1_700_000_000_000), // creationTime
                Value::Cond {
                    present: true,
                    values: vec![Value::Parcelable(Parcelable::ordered(
                        "android.graphics.drawable.Icon",
                        vec![
                            Value::Int(2), // TYPE_RESOURCE
                            Value::Str(Some("dev.acos.fixtures.notif".into())),
                            Value::Int(0x7f_01_0001),
                            Value::Cond {
                                present: false,
                                values: vec![],
                            }, // no tint
                            Value::Int(5), // BlendMode.SRC_IN (the image's default)
                        ],
                    ))],
                },
                Value::Int(1), // number
                Value::Cond {
                    present: true,
                    values: vec![Value::Parcelable(Parcelable::ordered(
                        "android.app.PendingIntent",
                        vec![Value::Binder(Some(acos_parcel::binder::BinderId(7)))],
                    ))],
                },
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // deleteIntent
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // tickerText
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // tickerView
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // contentView
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // mLargeIcon
                Value::Int(0),    // defaults
                Value::Int(0x10), // flags (FLAG_ONGOING_EVENT)
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // sound
                Value::Int(0),    // audioStreamType
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // audioAttributes
                Value::LongArray(None), // vibrate
                Value::Int(0),    // ledARGB
                Value::Int(0),    // ledOnMS
                Value::Int(0),    // ledOffMS
                Value::Int(0),    // iconLevel
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // fullScreenIntent
                Value::Int(0),    // priority
                Value::Str8(None), // category
                Value::Str8(Some("fixture-group".into())), // mGroupKey
                Value::Str8(None), // mSortKey
                Value::Bundle(Some({
                    let mut b = Bundle::new();
                    b.put("android.title", Value::Str(Some("hello".into())));
                    b.put("android.text", Value::Str(Some("world".into())));
                    b
                })),
                Value::List(Some(vec![Value::Parcelable(Parcelable::ordered(
                    "android.app.Notification$Action",
                    vec![
                        Value::Cond {
                            present: false,
                            values: vec![],
                        }, // icon
                        Value::Str(Some("Reply".into())), // title
                        Value::Cond {
                            present: false,
                            values: vec![],
                        }, // actionIntent
                        Value::Bundle(None),              // mExtras
                        Value::List(None),                // mRemoteInputs
                        Value::Int(1),                    // mAllowGeneratedReplies
                        Value::Int(0),                    // mSemanticAction
                        Value::Int(0),                    // mIsContextual
                        Value::Int(0),                    // mAuthenticationRequired
                    ],
                ))])),
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // bigContentView
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // headsUpContentView
                Value::Int(1), // visibility
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // publicVersion
                Value::Int(0), // color
                Value::Cond {
                    present: true,
                    values: vec![Value::Str8(Some("fixture-channel".into()))],
                },
                Value::Long(0), // mTimeout
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // mShortcutId
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // mLocusId
                Value::Int(0),  // mBadgeIcon
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // mSettingsText
                Value::Int(0),  // mGroupAlertBehavior
                Value::Cond {
                    present: false,
                    values: vec![],
                }, // mBubbleMetadata
                Value::Bool(false), // mAllowSystemGeneratedContextualActions
                Value::Int(0),  // mFgsDeferBehavior
                Value::List(None), // allPendingIntents (null ArraySet)
            ],
        ),
        "android.app.Notification$Action" => Parcelable::ordered(
            class,
            vec![
                Value::Cond {
                    present: false,
                    values: vec![],
                },
                Value::Str(Some("Reply".into())),
                Value::Cond {
                    present: false,
                    values: vec![],
                },
                Value::Bundle(None),
                Value::List(None),
                Value::Int(1),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
            ],
        ),
        "android.app.PendingIntent" => Parcelable::ordered(
            class,
            vec![Value::Binder(Some(acos_parcel::binder::BinderId(7)))],
        ),
        "android.graphics.drawable.Icon" => Parcelable::ordered(
            class,
            vec![
                Value::Int(2), // TYPE_RESOURCE
                Value::Str(Some("dev.acos.fixtures.notif".into())),
                Value::Int(0x7f_01_0001),
                Value::Cond {
                    present: false,
                    values: vec![],
                },
                Value::Int(5), // BlendMode.SRC_IN
            ],
        ),
        "android.view.RoundedCorners" => Parcelable::ordered(
            class,
            vec![Value::Cond {
                present: false,
                values: vec![],
            }],
        ),
        "android.view.PrivacyIndicatorBounds" => {
            Parcelable::ordered(class, vec![Value::List(None), Value::Int(0)])
        }
        "android.view.DisplayShape" => Parcelable::ordered(
            class,
            vec![
                Value::Str8(Some("M0,0".into())),
                Value::Int(1080),
                Value::Int(2400),
                Value::Float(1.0),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0),
                Value::Float(1.0),
            ],
        ),
        _ => return None,
    })
}

/// The `Icon` branch logic: a resource icon round-trips, and a bitmap icon is a
/// structured `UnsupportedParcelable` rather than a mis-read payload.
#[test]
fn icon_payload_round_trips() {
    let icon = Parcelable::ordered(
        "android.graphics.drawable.Icon",
        vec![
            Value::Int(2),
            Value::Str(Some("dev.acos.fixtures.notif".into())),
            Value::Int(0x7f_01_0001),
            Value::Cond {
                present: false,
                values: vec![],
            },
            Value::Int(5), // BlendMode.SRC_IN
        ],
    );
    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_payload(&mut parcel, &icon).expect("write icon");
    parcel.set_data_position(0);
    let decoded =
        parcelable::read_payload("android.graphics.drawable.Icon", &mut parcel).expect("read icon");
    assert_eq!(decoded, icon);
}

/// `Notification.writeToParcel` ends with `writeArraySet(allPendingIntents)`
/// (`Notification.java:3184-3190`): an `int` count (`-1` for null) and one
/// `writeValue` per `PendingIntent` the marshaling listener collected. The
/// MODNOTIF3 codec wrote the null encoding only; a notification that carries a
/// `contentIntent` really produces a one-element set, so the field is modelled
/// (`FieldOp::ArraySet`) and both shapes round-trip.
#[test]
fn notification_all_pending_intents_array_set_round_trips() {
    let base = sample_for("android.app.Notification").expect("notification sample");
    // Absent: the framework's `writeInt(-1)`.
    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_payload(&mut parcel, &base).expect("write notification");
    let null_tail = parcel.marshall();
    assert_eq!(
        null_tail[null_tail.len() - 4..],
        [0xff, 0xff, 0xff, 0xff],
        "a null allPendingIntents is the -1 count"
    );

    // Present: the contentIntent is collected into the set, and the set's
    // element is that same `PendingIntent` written as a `VAL_PARCELABLE`.
    let intent = Parcelable::ordered(
        "android.app.PendingIntent",
        vec![Value::Binder(Some(acos_parcel::binder::BinderId(7)))],
    );
    let mut values = base.fields().expect("ordered").to_vec();
    let last = values.len() - 1;
    values[last] = Value::List(Some(vec![Value::Parcelable(intent)]));
    let with_set = Parcelable::ordered("android.app.Notification", values);

    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_payload(&mut parcel, &with_set).expect("write notification");
    let bytes = parcel.marshall();
    // The tail is the count (1), the `VAL_PARCELABLE` tag of the collected
    // intent and, at the very end, the strong binder the `PendingIntent` holds.
    let mut binder_only = acos_parcel::Parcel::new();
    binder_only.write_strong_binder(acos_parcel::binder::BinderId(7));
    let binder_bytes = binder_only.marshall();
    assert!(
        bytes.ends_with(&binder_bytes),
        "the collected PendingIntent is written as its strong binder"
    );
    // Everything before the trailing set is byte-identical to the null case
    // (there the `-1` count sits at the end), and the set is
    // `count=1`, `VAL_PARCELABLE`, the body length, the class name
    // (`writeString`: u32 length + UTF-16 + NUL) and the strong binder.
    let count_at = null_tail.len() - 4;
    assert_eq!(
        &bytes[..count_at],
        &null_tail[..count_at],
        "the fields before allPendingIntents are unchanged"
    );
    let tail = &bytes[count_at..];
    assert_eq!(&tail[..4], &[1, 0, 0, 0], "one collected PendingIntent");
    assert_eq!(&tail[4..8], &[4, 0, 0, 0], "VAL_PARCELABLE");
    let name = "android.app.PendingIntent";
    assert_eq!(
        &tail[12..16],
        &(name.len() as u32).to_le_bytes(),
        "class name length"
    );
    let mut utf16 = Vec::new();
    for unit in name.encode_utf16() {
        utf16.extend_from_slice(&unit.to_le_bytes());
    }
    assert_eq!(&tail[16..16 + utf16.len()], &utf16[..], "class name");
    assert!(
        tail.ends_with(&binder_bytes),
        "the collected PendingIntent is written as its strong binder"
    );

    parcel.set_data_position(0);
    let decoded =
        parcelable::read_payload("android.app.Notification", &mut parcel).expect("read payload");
    assert_eq!(decoded, with_set, "the ArraySet did not round-trip");
    parcel.enforce_no_data_avail().expect("no trailing bytes");
}

// ---------------------------------------------------------------------------
// MODNOTIF5: two codec bugs a real `Notification.Builder` notification exposed
// (the Java differential built its own parcel, so writer and reader shared the
// transcription and both were wrong in the same way).
// ---------------------------------------------------------------------------

/// `android.media.AudioAttributes.writeToParcel` branches on `FLATTEN_TAGS`
/// (`base/media/java/android/media/AudioAttributes.java:1349-1368`) and writes
/// the bundle only after a `VALID_BUNDLE` sentinel.
///
/// The bytes below are the exact wire form a real `Notification.Builder`
/// notification carried (captured from the ART run with `ACOS_PARCEL_DUMP`):
/// usage 5 (USAGE_NOTIFICATION), contentType 4, source -1, flags 0x800,
/// parcel flags 0 (tags as `String[]`), no tags, and a **null** bundle
/// (`ATTR_PARCEL_IS_NULL_BUNDLE = -1977`, no body). The old fixed field list
/// read a `String` and a `Bundle` here, consuming 8 bytes too many and
/// desynchronising every later notification field.
#[test]
fn audio_attributes_reads_the_image_wire_form() {
    let mut parcel = acos_parcel::Parcel::new();
    parcel.write_int32(5); // usage
    parcel.write_int32(4); // contentType
    parcel.write_int32(-1); // source
    parcel.write_int32(0x800); // flags
    parcel.write_int32(0); // parcel flags: FLATTEN_TAGS clear
    parcel.write_string16_array(Some(&[])); // tags
    parcel.write_int32(-1977); // ATTR_PARCEL_IS_NULL_BUNDLE: no body
    let wire_len = parcel.data_size();
    parcel.set_data_position(0);

    let payload =
        acos_services::parcelable::read_payload("android.media.AudioAttributes", &mut parcel)
            .expect("the image's AudioAttributes wire form decodes");
    assert_eq!(
        parcel.data_position(),
        wire_len,
        "the codec consumes exactly the fields the writer wrote"
    );
    let fields = payload.fields().expect("ordered parcelable");
    assert_eq!(fields[0], Value::Int(5), "usage first (USAGE_NOTIFICATION)");
    assert_eq!(fields[1], Value::Int(4), "contentType");
    assert_eq!(fields[2], Value::Int(-1), "source");
    assert_eq!(fields[3], Value::Int(0x800), "flags");
    assert_eq!(fields[4], Value::Int(0), "parcel flags");
    assert_eq!(fields[5], Value::StringArray(Some(Vec::new())), "tags");
    assert_eq!(
        fields[6],
        Value::Cond {
            present: false,
            values: Vec::new()
        },
        "a null bundle is the sentinel, not a field"
    );

    // Round trip: the writer emits the same bytes.
    let mut again = acos_parcel::Parcel::new();
    acos_services::parcelable::write_payload(&mut again, &payload).expect("re-encode");
    let mut original = acos_parcel::Parcel::new();
    original.write_int32(5);
    original.write_int32(4);
    original.write_int32(-1);
    original.write_int32(0x800);
    original.write_int32(0);
    original.write_string16_array(Some(&[]));
    original.write_int32(-1977);
    assert_eq!(
        again.marshall(),
        original.marshall(),
        "read -> write is byte-identical for the image's form"
    );
}

/// A **flattened** tag list (the other branch) still round-trips.
#[test]
fn audio_attributes_reads_the_flattened_tag_form() {
    let mut parcel = acos_parcel::Parcel::new();
    parcel.write_int32(5);
    parcel.write_int32(0);
    parcel.write_int32(-1);
    parcel.write_int32(0);
    parcel.write_int32(1); // FLATTEN_TAGS
    parcel.write_string16("a;b");
    parcel.write_int32(1980); // ATTR_PARCEL_IS_VALID_BUNDLE
    parcel.write_int32(0); // empty bundle: the "special case" length
    let wire_len = parcel.data_size();
    parcel.set_data_position(0);

    let payload =
        acos_services::parcelable::read_payload("android.media.AudioAttributes", &mut parcel)
            .expect("flattened form decodes");
    assert_eq!(parcel.data_position(), wire_len);
    let fields = payload.fields().expect("ordered");
    assert_eq!(fields[5], Value::Str(Some("a;b".into())));
    assert!(matches!(fields[6], Value::Cond { present: true, .. }));
}

/// `Parcel.writeValue`'s length prefix counts the libbinder stability record the
/// guest writes after every flat object, but `peer::import_parcel` strips those
/// records. A body with one binder therefore arrives 4 bytes shorter than its
/// declared length; the reader accepts exactly that difference (per recorded
/// object) and nothing else.
///
/// This is the failure a real notification hit on its trailing
/// `allPendingIntents` ArraySet: "body consumed 80 bytes but declared 84".
#[test]
fn a_value_length_prefix_counts_the_stripped_stability_records() {
    use acos_parcel::Parcel;

    // The body the framework writes: class name `writeValue` + the payload
    // (`PendingIntent.writeToParcel` = one strong binder).
    let class = "android.app.PendingIntent";
    let mut body = Parcel::new();
    body.write_string16(class);
    let object_offset = body.data_size();
    body.write_strong_binder(acos_parcel::BinderId(31));
    body.write_int32(0); // the stability record libbinder appends
    let body = body.marshall();

    // The guest's wire form: VAL_PARCELABLE + its length prefix + body.
    let mut wire = Parcel::new();
    wire.write_int32(4); // VAL_PARCELABLE
    wire.write_int32(body.len() as i32);
    wire.write_raw_bytes(&body);
    let wire = wire.marshall();
    let declared_at = 4usize;
    let body_start = 8usize;

    // The import: drop the 4 bytes after the flat object, keep its new offset.
    let mut imported_bytes = Vec::with_capacity(wire.len() - 4);
    imported_bytes.extend_from_slice(&wire[..body_start + object_offset + 24]);
    imported_bytes.extend_from_slice(&wire[body_start + object_offset + 28..]);
    let mut imported = Parcel::new();
    imported.unmarshall(&imported_bytes);
    imported.register_object(body_start + object_offset);
    imported.set_data_position(0);

    let value = acos_services::parcelable::read_dynamic(&mut imported)
        .expect("the declared length counts the stripped stability record");
    assert!(
        matches!(value, acos_services::value::DynamicValue::Parcelable(_)),
        "substantive: {value:?}"
    );
    assert_eq!(
        imported.data_position(),
        imported_bytes.len(),
        "the stability record is gone from the imported parcel"
    );

    // Anything else is still refused: a declared length two records too long is
    // a desync, not a wire-form difference.
    let mut forged = imported_bytes.clone();
    forged[declared_at..declared_at + 4].copy_from_slice(&((body.len() + 4) as i32).to_le_bytes());
    let mut forged_parcel = Parcel::new();
    forged_parcel.unmarshall(&forged);
    forged_parcel.register_object(body_start + object_offset);
    forged_parcel.set_data_position(0);
    assert!(
        acos_services::parcelable::read_dynamic(&mut forged_parcel).is_err(),
        "a length that is not explained by a stripped record is refused"
    );
}

/// MPROC7: `PackageItemInfo.writeToParcel`'s field order is
/// `name, packageName, labelRes, label, icon, logo, metaData, banner,
/// showUser` — labelRes **before** the inline label, and the logo slot after
/// the icon.
///
/// The PMS encoder used to write `banner, label, icon, labelRes`, i.e. it put
/// the application's `labelRes` into the guest's `logo` slot. Nothing noticed
/// while every fixture's `android:label` was a literal string (`labelRes = 0`);
/// `proc-b` declares `android:label="@string/app_name"`, and the guest's
/// `ComponentInfo.getLogoResource()` (`logo != 0 ? logo :
/// applicationInfo.logo`) then asked for a Drawable with a *string* resource id
/// (`Resources$NotFoundException: Drawable … string/app_name`). The isolated
/// ApplicationInfo differential could not catch it: it compares a
/// *name-built* Java object with all-zero label/logo fields.
#[test]
fn application_info_writes_label_res_before_label_and_logo_after_icon() {
    use acos_services::pms::AppInfo;
    let mut app = AppInfo::hello_views_fallback(std::path::Path::new("/data/app/x.apk"));
    app.label_res = 0x7f03_0001; // @string/app_name
    app.icon = 0x7f02_0003;
    let encoded = app.to_parcelable();
    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_payload(&mut parcel, &encoded).expect("encode ApplicationInfo");
    let bytes = parcel.marshall();
    let mut parcel = acos_parcel::Parcel::new();
    parcel.unmarshall(&bytes);
    let decoded = parcelable::read_payload("android.content.pm.ApplicationInfo", &mut parcel)
        .expect("decode ApplicationInfo");
    let values = decoded.fields().expect("ordered ApplicationInfo");
    // Field 0 is the `maybeWriteSquashed` marker; PackageItemInfo starts at 1.
    assert_eq!(
        values[3],
        Value::Int(0x7f03_0001),
        "field 3 is `PackageItemInfo.labelRes`"
    );
    assert_eq!(values[4], Value::Str(None), "field 4 is the inline label");
    assert_eq!(values[5], Value::Int(0x7f02_0003), "field 5 is `icon`");
    assert_eq!(
        values[6],
        Value::Int(0),
        "field 6 is `logo` (never the label resource)"
    );
    assert_eq!(values[8], Value::Int(0), "field 8 is `banner`");
    assert_eq!(values[9], Value::Int(0), "field 9 is `showUser`");
}

/// MPROC7: `activityStopped`'s saved-state `Bundle` carries the window
/// hierarchy's `SparseArray` (the image's `VAL_SPARSEARRAY`, tag 12). The reader
/// had no model for it, so the whole `activityStopped` request failed
/// (`unmodelled Parcel.VAL_* tag 12`) and the stop never reached the activity
/// manager. The body is a size, then `(int key, writeValue value)` pairs; the
/// decoder verifies the length word, so a differently shaped body fails there
/// instead of desynchronising the rest of the parcel.
#[test]
fn bundle_reader_consumes_a_sparse_array_and_checks_its_length() {
    use acos_services::value::DynamicValue;
    let sparse = DynamicValue::SparseArray(vec![
        (0x0102_0012, DynamicValue::Int(7)),
        (3, DynamicValue::String(Some("state".into()))),
        (4, DynamicValue::Bool(true)),
    ]);
    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_dynamic(&mut parcel, &sparse).expect("write VAL_SPARSEARRAY");
    let bytes = parcel.marshall();
    let mut parcel = acos_parcel::Parcel::new();
    parcel.unmarshall(&bytes);
    assert_eq!(
        parcelable::read_dynamic(&mut parcel).expect("read VAL_SPARSEARRAY"),
        sparse
    );
    assert_eq!(
        parcel.data_position(),
        bytes.len(),
        "the array consumed its bytes"
    );

    // The first word of the body is the size, not a value tag: that is what
    // separates the sparse-array shape from a list (the guest's real key was
    // `android:id/content`, 0x01020012, which is not a `writeValue` tag).
    assert_eq!(
        i32::from_le_bytes(bytes[8..12].try_into().expect("size")),
        3
    );

    // A declared length that does not match the body is refused (the check is
    // what makes a wrong tag shape a structured error, not silent corruption).
    let mut bad = bytes.clone();
    let declared = i32::from_le_bytes(bad[4..8].try_into().expect("len word"));
    bad[4..8].copy_from_slice(&(declared + 4).to_le_bytes());
    let mut parcel = acos_parcel::Parcel::new();
    parcel.unmarshall(&bad);
    assert!(
        parcelable::read_dynamic(&mut parcel).is_err(),
        "a VAL_SPARSEARRAY whose body does not match its length word must be refused"
    );
}
/// This walks the bytes in `PackageInfo.readFromParcel` order using the same
/// primitives the framework uses (`readString8`, `createString8Array`,
/// `readIntArray`, `readTypedArray`), so a wrong array encoding shows up as a
/// desynchronised field instead of a symmetric ACOS-internal round trip.
#[test]
fn package_info_reply_reads_back_in_the_framework_order() {
    use acos_services::pms::{AppInfo, PackageInfo};

    let package = "dev.acos.fixtures.pmsprobe";
    let info = PackageInfo {
        package_name: package.into(),
        version_code: 7,
        version_name: "1.0".into(),
        application: AppInfo::hello_views_fallback(std::path::Path::new("probe.apk")),
        activities: Vec::new(),
        requested_permissions: vec![
            "android.permission.INTERNET".into(),
            "android.permission.VIBRATE".into(),
        ],
        first_install_time: 1,
        last_update_time: 2,
        signatures: vec![vec![0x30, 0x82, 0x01, 0x0a, 0xde, 0xad]],
    };
    let value = info.to_parcelable_with_flags(0x1000_24c0);

    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_payload(&mut parcel, &value).expect("write PackageInfo");
    parcel.set_data_position(0);

    // Parcel.readString8(packageName)
    assert_eq!(parcel.read_string8().unwrap().as_deref(), Some(package));
    // Parcel.createString8Array(splitNames)
    assert_eq!(parcel.read_string8_array().unwrap(), None, "splitNames");
    // readInt(versionCode), readInt(versionCodeMajor)
    assert_eq!(parcel.read_int32().unwrap(), 7);
    assert_eq!(parcel.read_int32().unwrap(), 0);
    // readString8(versionName), readInt(baseRevisionCode), readIntArray(splitRevisionCodes)
    assert_eq!(parcel.read_string8().unwrap().as_deref(), Some("1.0"));
    assert_eq!(parcel.read_int32().unwrap(), 0);
    assert_eq!(parcel.read_int32_array().unwrap(), None);
    // readString8(sharedUserId), readInt(sharedUserLabel)
    assert_eq!(parcel.read_string8().unwrap(), None);
    assert_eq!(parcel.read_int32().unwrap(), 0);
    // applicationInfo presence marker + the nested ApplicationInfo payload
    assert_eq!(parcel.read_int32().unwrap(), 1);
    parcelable::read_payload("android.content.pm.ApplicationInfo", &mut parcel)
        .expect("nested ApplicationInfo");
    // readLong(firstInstallTime), readLong(lastUpdateTime), readIntArray(gids)
    assert_eq!(parcel.read_int64().unwrap(), 1);
    assert_eq!(parcel.read_int64().unwrap(), 2);
    assert_eq!(parcel.read_int32_array().unwrap(), None);
    // 6 absent typed arrays: activities, receivers, services, providers,
    // instrumentation, permissions (a null typed array is a single -1 count).
    for field in [
        "activities",
        "receivers",
        "services",
        "providers",
        "instrumentation",
        "permissions",
    ] {
        assert_eq!(parcel.read_int32().unwrap(), -1, "{field} count");
    }
    // createString8Array(requestedPermissions) — the REL3P defect: a String16
    // encoding desynchronises here and the next int is string payload bytes.
    assert_eq!(
        parcel.read_string8_array().unwrap(),
        Some(vec![
            Some("android.permission.INTERNET".to_owned()),
            Some("android.permission.VIBRATE".to_owned()),
        ]),
        "requestedPermissions must be a String8 array"
    );
    // readIntArray(requestedPermissionsFlags) — a desynchronised reader sees the
    // UTF-16 payload here instead of the `-1` null marker.
    assert_eq!(
        parcel.read_int32_array().unwrap(),
        None,
        "requestedPermissionsFlags must follow requestedPermissions exactly"
    );
    // readTypedArray(signatures): count 1, presence 1, then `Signature`'s
    // single `writeByteArray`.
    assert_eq!(parcel.read_int32().unwrap(), 1, "signatures count");
    assert_eq!(parcel.read_int32().unwrap(), 1, "signature present");
    assert_eq!(
        parcel.read_byte_array().unwrap(),
        Some(vec![0x30, 0x82, 0x01, 0x0a, 0xde, 0xad])
    );
    // The four remaining typed arrays (configPreferences, reqFeatures,
    // featureGroups, attributions) are absent.
    for field in [
        "configPreferences",
        "reqFeatures",
        "featureGroups",
        "attributions",
    ] {
        assert_eq!(parcel.read_int32().unwrap(), -1, "{field} count");
    }
    // ...and the scalar tail is still aligned: installLocation is 0.
    assert_eq!(parcel.read_int32().unwrap(), 0, "installLocation");
}

/// REL3P: `ApplicationInfo`'s split/resource arrays are `String8` arrays too
/// (image `classes.dex` `Landroid/content/pm/ApplicationInfo;->writeToParcel`
/// `0x0076`-`0x00bc`). The `sharedLibraryFiles` array precedes the
/// `SharedLibraryInfo` typed list, so a wrong encoding there desynchronises the
/// `dataDir` that follows it.
#[test]
fn application_info_split_arrays_are_string8() {
    let mut app = acos_services::pms::AppInfo::hello_views_fallback(std::path::Path::new(
        "/data/app/dev.acos.fixtures.helloviews/base.apk",
    ));
    app.split_names = vec!["config.xxhdpi".into()];
    app.resource_dirs = vec!["/data/app/split_config.xxhdpi.apk".into()];
    app.shared_library_files = vec!["/system/framework/android.test.runner.jar".into()];
    let value = app.to_parcelable_with_flags(0);
    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_payload(&mut parcel, &value).expect("write ApplicationInfo");
    parcel.set_data_position(0);

    // maybeWriteSquashed marker, then the PackageItemInfo prefix.
    assert_eq!(parcel.read_int32().unwrap(), 0);
    let _name = parcel.read_string8().unwrap();
    let _pkg = parcel.read_string8().unwrap();
    let _banner = parcel.read_int32().unwrap();
    // TextUtils.writeToParcel(label): kind marker + String8.
    assert_eq!(parcel.read_int32().unwrap(), 1);
    let _label = parcel.read_string8().unwrap();
    let _icon = parcel.read_int32().unwrap();
    let _label_res = parcel.read_int32().unwrap();
    let _meta = parcel.read_int32_array().unwrap_or(None); // Bundle null marker
    let _show_user = parcel.read_int32().unwrap();
    let _show_badge = parcel.read_int32().unwrap();
    for _ in 0..4 {
        let _ = parcel.read_string8().unwrap(); // taskAffinity, permission, processName, className
    }
    for _ in 0..7 {
        let _ = parcel.read_int32().unwrap(); // theme, flags, privateFlags{,Ext}, 3 width dp
    }
    // storageUuid: an int presence marker, then 2 longs when present.
    if parcel.read_int32().unwrap() != 0 {
        let _ = parcel.read_int64().unwrap();
        let _ = parcel.read_int64().unwrap();
    }
    for _ in 0..4 {
        let _ = parcel.read_string8().unwrap(); // scanSourceDir, scanPublicSourceDir, sourceDir, publicSourceDir
    }
    assert_eq!(
        parcel.read_string8_array().unwrap(),
        Some(vec![Some("config.xxhdpi".to_owned())]),
        "splitNames must be a String8 array"
    );
    assert_eq!(
        parcel.read_string8_array().unwrap(),
        None,
        "splitSourceDirs"
    );
    assert_eq!(
        parcel.read_string8_array().unwrap(),
        None,
        "splitPublicSourceDirs"
    );
    // splitDependencies: null SparseArray.
    assert_eq!(parcel.read_int32().unwrap(), -1);
    for _ in 0..3 {
        let _ = parcel.read_string8().unwrap(); // nativeLibraryDir, secondaryNativeLibraryDir, nativeLibraryRootDir
    }
    let _ = parcel.read_int32().unwrap(); // nativeLibraryRootRequiresIsa
    let _ = parcel.read_string8().unwrap(); // primaryCpuAbi
    let _ = parcel.read_string8().unwrap(); // secondaryCpuAbi
    assert_eq!(
        parcel.read_string8_array().unwrap(),
        Some(vec![Some("/data/app/split_config.xxhdpi.apk".to_owned())]),
        "resourceDirs must be a String8 array"
    );
    assert_eq!(parcel.read_string8_array().unwrap(), None, "overlayPaths");
    let _ = parcel.read_string8().unwrap(); // seInfo
    let _ = parcel.read_string8().unwrap(); // seInfoUser
    assert_eq!(
        parcel.read_string8_array().unwrap(),
        Some(vec![Some(
            "/system/framework/android.test.runner.jar".to_owned()
        )]),
        "sharedLibraryFiles must be a String8 array"
    );
    // sharedLibraryInfos is a null typed list, then dataDir is a String8.
    assert_eq!(parcel.read_int32().unwrap(), -1);
    assert_eq!(
        parcel.read_string8().unwrap().as_deref(),
        Some("/data/user/0/dev.acos.fixtures.helloviews"),
        "dataDir must follow sharedLibraryInfos exactly"
    );
}

/// REL3P: a `writeTypedArray` payload round-trips through the codec's own
/// reader, including the per-element `writeTypedObject` presence marker.
///
/// `Parcel.writeTypedArray` writes `int count` then, per element,
/// `writeTypedObject`: `int 1` + payload. The writer emitted that marker but
/// the reader skipped it, so the first element's marker was read as the next
/// element's payload and everything after it desynchronised. Nothing caught it
/// because every typed array in the PMS replies was empty (`-1`) until
/// `PackageInfo.signatures` started carrying the APK's certificate.
#[test]
fn typed_array_elements_round_trip_with_their_presence_marker() {
    use acos_services::pms::{AppInfo, PackageInfo};

    let info = PackageInfo {
        package_name: "dev.acos.fixtures.pmsprobe".into(),
        version_code: 1,
        version_name: "1.0".into(),
        application: AppInfo::hello_views_fallback(std::path::Path::new("probe.apk")),
        activities: Vec::new(),
        requested_permissions: vec!["android.permission.INTERNET".into()],
        first_install_time: 0,
        last_update_time: 0,
        signatures: vec![vec![0x30, 0x82, 0x01, 0x0a]],
    };
    let value = info.to_parcelable_with_flags(0x1000_24c0);
    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_payload(&mut parcel, &value).expect("write PackageInfo");
    parcel.set_data_position(0);
    let decoded = parcelable::read_payload("android.content.pm.PackageInfo", &mut parcel)
        .expect("read PackageInfo");
    parcel
        .enforce_no_data_avail()
        .expect("the reader must consume the whole payload");
    // The `Signature[]` field (index 21 in `PackageInfo.writeToParcel` order).
    let signatures = decoded.field(21).expect("signatures field");
    assert_eq!(
        signatures,
        &Value::List(Some(vec![Value::Parcelable(Parcelable::ordered(
            "android.content.pm.Signature",
            vec![Value::ByteArray(Some(vec![0x30, 0x82, 0x01, 0x0a]))],
        ))])),
        "typed array element lost its payload"
    );
    assert_eq!(
        decoded.field(19),
        Some(&Value::StringArray(Some(vec![Some(
            "android.permission.INTERNET".to_owned()
        )]))),
        "requestedPermissions did not survive the round trip"
    );
}
