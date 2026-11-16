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
        "android.app.servertransaction.ClientTransaction" => Parcelable::ordered(
            class,
            vec![
                Value::Bool(false),
                Value::Binder(None),
                Value::NamedParcelable(Parcelable::Null),
                Value::Bool(false),
                Value::List(None),
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

/// REL3P regression: a `getPackageInfo(pkg, GET_SIGNATURES|GET_META_DATA|
/// GET_PERMISSIONS = 0x100024c0)` reply must stay in sync with the framework's
/// own reader.
///
/// `PackageInfo.writeToParcel` writes `splitNames` and `requestedPermissions`
/// with `writeString8Array` (image `classes.dex` `Landroid/content/pm/
/// PackageInfo;->writeToParcel` at `0x000b` and `0x0072`; tag
/// `PackageInfo.java:585,608`), i.e. each element is `int32 len` in UTF-8
/// **bytes** + bytes + NUL. Before REL3P the codec wrote the String16 form, so
/// every field after a non-empty `requestedPermissions` was read out of the
/// middle of the string payload: the framework read a `Signature[]` length of
/// 7 209 071 and threw `OutOfMemoryError: Failed to allocate a 7676737936 byte
/// allocation at Parcel.createString8Array`.
///
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
    for field in ["configPreferences", "reqFeatures", "featureGroups", "attributions"] {
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
    assert_eq!(parcel.read_string8_array().unwrap(), None, "splitSourceDirs");
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
