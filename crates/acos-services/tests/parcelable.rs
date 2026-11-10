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
        let (matched, real_len) = parcelable::verify_prefix(spec, &scanned);
        match spec.verification {
            Verification::Full => {
                if matched != spec.fields.len() || matched != real_len {
                    failures.push(format!(
                        "{}: matched {matched}/{real_len}, spec {}",
                        spec.class,
                        spec.fields.len()
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
        if spec.class == "android.os.ParcelFileDescriptor" {
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
        "android.content.pm.ApplicationInfo" => {
            acos_services::pms::AppInfo::hello_views_fallback(std::path::Path::new("hello-views.apk"))
                .to_parcelable()
        }
        "android.view.autofill.AutofillId" => {
            Parcelable::ordered(class, vec![Value::Int(1), Value::Int(2)])
        }
        "android.window.SizeConfigurationBuckets" => {
            Parcelable::ordered(class, vec![Value::IntArray(Some(vec![1, 2, 3]))])
        }
        "android.app.servertransaction.ClientTransaction" => {
            Parcelable::ordered(class, vec![Value::IntArray(Some(vec![1, 2]))])
        }
        "android.content.AttributionSource" => Parcelable::ordered(
            class,
            vec![
                Value::Int(10192),
                Value::Str(Some("dev.acos.fixtures.helloviews".into())),
                Value::Str(None),
            ],
        ),
        "android.net.Uri" => Parcelable::ordered(class, vec![Value::Str(Some("content://settings/system".into()))]),
        _ => return None,
    })
}
