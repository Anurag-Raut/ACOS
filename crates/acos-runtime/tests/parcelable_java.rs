//! W112-A real-Java parcel differential harness.
//!
//! `tests/java/ParcelableCompareMain.java` runs on the real Android 14
//! framework: it constructs `Configuration` (with a `LocaleList` and a
//! non-default `WindowConfiguration`), `PersistableBundle`, calls the real
//! `writeToParcel` into an ACOS-native `Parcel`, and prints the marshalled
//! bytes. This test encodes the *same* logical value with
//! `acos-services::parcelable`, asserts the two byte streams are identical, and
//! then feeds the Rust bytes back to the real `CREATOR.createFromParcel`,
//! checking the reconstructed fields.
//!
//! The Java case skips loudly when the `javac`/`d8`/`android.jar` toolchain is
//! absent; it never silently passes.

mod common;

use std::collections::HashMap;

use common::{compile_case, run_case_args};
use acos_services::parcelable;
use acos_services::value::{Parcelable, Value};
use acos_services::Bundle;

fn rect(left: i32, top: i32, right: i32, bottom: i32) -> Value {
    Value::Parcelable(Parcelable::ordered(
        "android.graphics.Rect",
        vec![
            Value::Int(left),
            Value::Int(top),
            Value::Int(right),
            Value::Int(bottom),
        ],
    ))
}

/// The logical value the Java `configuration()` case builds, in
/// `writeToParcel` field order.
fn configuration_value() -> Parcelable {
    let window = Parcelable::ordered(
        "android.app.WindowConfiguration",
        vec![
            rect(0, 0, 1080, 2400),                       // mBounds (Nested)
            Value::TypedObject(Parcelable::ordered(
                "android.graphics.Rect",
                vec![
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(1080),
                    Value::Int(2337),
                ],
            )), // mAppBounds
            rect(0, 0, 1080, 2400),                       // mMaxBounds (Nested)
            Value::Int(1), // mWindowingMode
            Value::Int(1), // mActivityType
            Value::Int(0), // mAlwaysOnTop
            Value::Int(0), // mRotation
            Value::Int(1), // mDisplayWindowingMode
            Value::Int(0), // mDisplayRotation
        ],
    );
    let locale = Value::TypedObject(Parcelable::ordered(
        "android.os.LocaleList",
        vec![Value::Str8(Some("en-US".into()))],
    ));
    Parcelable::ordered(
        "android.content.res.Configuration",
        vec![
            Value::Float(1.25),
            Value::Int(310), // mcc
            Value::Int(260), // mnc
            locale,
            Value::Int(1), // userSetLocale (setLocales sets it true)
            Value::Int(3), // touchscreen
            Value::Int(2), // keyboard
            Value::Int(1), // keyboardHidden
            Value::Int(1), // hardKeyboardHidden
            Value::Int(1), // navigation
            Value::Int(2), // navigationHidden
            Value::Int(2), // orientation
            Value::Int(0x20), // screenLayout
            Value::Int(0), // colorMode
            Value::Int(0x11), // uiMode
            Value::Int(411), // screenWidthDp
            Value::Int(914), // screenHeightDp
            Value::Int(411), // smallestScreenWidthDp
            Value::Int(420), // densityDpi
            Value::Int(411), // compatScreenWidthDp
            Value::Int(914), // compatScreenHeightDp
            Value::Int(411), // compatSmallestScreenWidthDp
            Value::Parcelable(window),
            Value::Int(7),   // assetsSeq
            Value::Int(3),   // seq
            Value::Int(100), // fontWeightAdjustment
            Value::Int(3),   // mGrammaticalGender
        ],
    )
}

fn persistable_bundle_value() -> Parcelable {
    let mut b = Bundle::new();
    b.put("name", Value::Str(Some("helloviews".into())));
    Parcelable::ordered(
        "android.os.PersistableBundle",
        vec![Value::Bundle(Some(b))],
    )
}

fn encoded(value: &Parcelable) -> Vec<u8> {
    let mut parcel = acos_parcel::Parcel::new();
    parcelable::write_payload(&mut parcel, value).expect("encode payload");
    parcel.marshall()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(out, "{b:02x}").unwrap();
    }
    out
}

fn parse_writes(stdout: &str) -> HashMap<String, String> {
    stdout
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("write.")?;
            let (name, hex) = rest.split_once('=')?;
            Some((name.to_string(), hex.to_string()))
        })
        .collect()
}

#[test]
fn real_java_write_to_parcel_matches_the_rust_codec() {
    let Some(dex) = compile_case(&["ParcelableCompareMain"]) else {
        eprintln!("SKIP real_java_write_to_parcel_matches_the_rust_codec: toolchain unavailable");
        return;
    };

    // Forward: the real framework's bytes vs. the Rust encoder's bytes.
    let (_rt, outcome) = run_case_args(&dex, "ParcelableCompareMain", &[]);
    assert!(
        outcome.vm_error.is_none() && outcome.exit_code == 0,
        "ParcelableCompareMain failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    let writes = parse_writes(&outcome.stdout);
    let config = configuration_value();
    let config_bytes = encoded(&config);
    assert_eq!(
        writes.get("configuration").map(String::as_str),
        Some(hex(&config_bytes).as_str()),
        "Configuration real-Java bytes differ from the Rust codec\njava={:?}\nrust={}",
        writes.get("configuration"),
        hex(&config_bytes)
    );
    let bundle = persistable_bundle_value();
    let bundle_bytes = encoded(&bundle);
    assert_eq!(
        writes.get("persistable_bundle").map(String::as_str),
        Some(hex(&bundle_bytes).as_str()),
        "PersistableBundle real-Java bytes differ from the Rust codec\njava={:?}\nrust={}",
        writes.get("persistable_bundle"),
        hex(&bundle_bytes)
    );

    // Reverse: feed the Rust bytes to the real createFromParcel.
    let args = vec![
        format!("configuration={}", hex(&config_bytes)),
        format!("persistable_bundle={}", hex(&bundle_bytes)),
    ];
    let (_rt, outcome) = run_case_args(&dex, "ParcelableCompareMain", &args);
    assert!(
        outcome.vm_error.is_none() && outcome.exit_code == 0,
        "reverse createFromParcel failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.vm_error,
        outcome.stdout,
        outcome.stderr
    );
    assert!(
        outcome.stdout.contains("read.configuration=ok"),
        "real Configuration.createFromParcel rejected the Rust bytes:\n{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("read.persistable_bundle=ok"),
        "real PersistableBundle.createFromParcel rejected the Rust bytes:\n{}",
        outcome.stdout
    );
}
