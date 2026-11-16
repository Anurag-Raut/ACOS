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
                    failures.push(format!("{}: no prefix match ({real_len} real ops)", spec.class));
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

fn framework_static_ints(
    dex: &acos_dex::DexFile,
    def: &acos_dex::ClassDef,
) -> Vec<(String, i64)> {
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
    let decoded = parcelable::read_payload("android.app.ActivityManager$TaskDescription", &mut parcel)
        .expect("read the real API-34 TaskDescription payload");
    parcel
        .enforce_no_data_avail()
        .expect("the conditional reader consumed the whole payload");
    assert_eq!(decoded.field(0), Some(&Value::Cond { present: false, values: vec![] }));
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
    let decoded = parcelable::read_payload("android.app.ActivityManager$TaskDescription", &mut parcel)
        .expect("read a TaskDescription with label + iconFilename");
    parcel.enforce_no_data_avail().expect("consumed present branches");
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
            assert_eq!(
                decoded, value,
                "{} did not round-trip",
                spec.class
            );
        }
        parcel.enforce_no_data_avail().expect("no trailing bytes");
    }
}

fn sample_for(class: &str) -> Option<Parcelable> {
    Some(match class {
        "android.graphics.Rect" => {
            Parcelable::ordered(class, vec![Value::Int(0), Value::Int(128), Value::Int(1080), Value::Int(2337)])
        }
        "android.graphics.Point" => Parcelable::ordered(class, vec![Value::Int(1080), Value::Int(2400)]),
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
        "android.view.DisplayInfo" => acos_services::display::DisplayInfo::hello_views().to_parcelable(),
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
        "android.content.pm.ApplicationInfo" => {
            acos_services::pms::AppInfo::hello_views_fallback(std::path::Path::new("hello-views.apk"))
                .to_parcelable()
        }
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
                Value::Cond { present: true, values: vec![Value::Binder(None)] },
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
        "android.os.LocaleList" => Parcelable::ordered(
            class,
            vec![Value::Str8(Some("en-US".into()))],
        ),
        "android.app.WindowConfiguration" => Parcelable::ordered(
            class,
            vec![
                Value::Parcelable(Parcelable::ordered(
                    "android.graphics.Rect",
                    vec![Value::Int(0), Value::Int(0), Value::Int(1080), Value::Int(2400)],
                )),
                Value::TypedObject(Parcelable::ordered(
                    "android.graphics.Rect",
                    vec![Value::Int(0), Value::Int(0), Value::Int(1080), Value::Int(2400)],
                )),
                Value::Parcelable(Parcelable::ordered(
                    "android.graphics.Rect",
                    vec![Value::Int(0), Value::Int(0), Value::Int(1080), Value::Int(2400)],
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
                Value::Int(0),  // mcc
                Value::Int(0),  // mnc
                Value::TypedObject(Parcelable::ordered(
                    "android.os.LocaleList",
                    vec![Value::Str8(Some("en-US".into()))],
                )),
                Value::Int(0),  // userSetLocale
                Value::Int(0),  // touchscreen
                Value::Int(0),  // keyboard
                Value::Int(0),  // keyboardHidden
                Value::Int(0),  // hardKeyboardHidden
                Value::Int(0),  // navigation
                Value::Int(0),  // navigationHidden
                Value::Int(0),  // orientation
                Value::Int(0),  // screenLayout
                Value::Int(0),  // colorMode
                Value::Int(0),  // uiMode
                Value::Int(411), // screenWidthDp
                Value::Int(914), // screenHeightDp
                Value::Int(411), // smallestScreenWidthDp
                Value::Int(420), // densityDpi
                Value::Int(0),  // compatScreenWidthDp
                Value::Int(0),  // compatScreenHeightDp
                Value::Int(0),  // compatSmallestScreenWidthDp
                Value::Parcelable(Parcelable::ordered(
                    "android.app.WindowConfiguration",
                    vec![
                        Value::Parcelable(Parcelable::ordered(
                            "android.graphics.Rect",
                            vec![Value::Int(0), Value::Int(0), Value::Int(1080), Value::Int(2400)],
                        )),
                        Value::TypedObject(Parcelable::ordered(
                            "android.graphics.Rect",
                            vec![Value::Int(0), Value::Int(0), Value::Int(1080), Value::Int(2400)],
                        )),
                        Value::Parcelable(Parcelable::ordered(
                            "android.graphics.Rect",
                            vec![Value::Int(0), Value::Int(0), Value::Int(1080), Value::Int(2400)],
                        )),
                        Value::Int(1),
                        Value::Int(1),
                        Value::Int(0),
                        Value::Int(0),
                        Value::Int(1),
                        Value::Int(0),
                    ],
                )),
                Value::Int(0),  // assetsSeq
                Value::Int(0),  // seq
                Value::Int(0),  // fontWeightAdjustment
                Value::Int(0),  // mGrammaticalGender
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
                Value::Cond { present: false, values: vec![] },
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
                    vec![Value::Int(0), Value::Int(0), Value::Int(1080), Value::Int(2400)],
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
                        vec![Value::Int(0), Value::Int(0), Value::Int(1080), Value::Int(2400)],
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
                    vec![Value::Int(0), Value::Int(0), Value::Int(1080), Value::Int(128)],
                )),
                Value::Cond { present: false, values: vec![] },
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
        "android.view.InsetsSourceControl$Array" => Parcelable::ordered(
            class,
            vec![Value::List(None)],
        ),
        "android.view.RoundedCorners" => {
            Parcelable::ordered(class, vec![Value::Cond { present: false, values: vec![] }])
        }
        "android.view.PrivacyIndicatorBounds" => Parcelable::ordered(
            class,
            vec![Value::List(None), Value::Int(0)],
        ),
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
