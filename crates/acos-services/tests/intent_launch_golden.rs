//! LAUNCHEXTRAS: the launch `Intent` bytes are pinned.
//!
//! The wire-format rule for this path is the same one MPROC6/MODNOTIF4 use: the
//! bytes must be what real Java's `Intent.writeToParcel`/`Bundle` writes. That
//! is proved **differentially** by
//! `crates/acos-runtime/tests/client_transaction_java.rs`
//! (`launch_intent_extras`: the real framework builds the same Intent, writes
//! it, and reads the host's bytes back, asserting the action, data URI and one
//! extra of every supported Java type).
//!
//! That harness needs the boot classpath and the javac/d8 toolchain, so the
//! acceptance suite also carries this **golden**: the exact bytes the Java case
//! produced, committed as `tests/fixtures/intent-launch-extras.bin`. A codec
//! change that moves one byte fails here without needing the toolchain.
//!
//! Refresh deliberately with `UPDATE_INTENT_GOLDEN=1 cargo test -p acos-services`
//! **after** the real-Java differential passes; the fixture is never allowed to
//! move just because the encoder did.

use std::collections::BTreeMap;

use acos_services::client_transaction as ctx;
use acos_services::value::Value;
use acos_protocol::{ExtraValue, LaunchIntent};

fn launch_intent() -> LaunchIntent {
    LaunchIntent {
        action: Some("dev.acos.fixtures.launchextras.ACTION".to_string()),
        data: Some("acos://launch/extras?source=scenario".to_string()),
        extras: BTreeMap::from([
            ("probe_string".to_string(), ExtraValue::String("hello".to_string())),
            ("probe_int".to_string(), ExtraValue::Int(42)),
            ("probe_long".to_string(), ExtraValue::Long(5_000_000_000)),
            // A `long` that fits an `int`: the explicit form is the only one
            // whose type survives the JSON hop to the launcher, and the bytes
            // must be the 8-byte `VAL_LONG` Java writes, not `VAL_INTEGER`.
            ("probe_long_small".to_string(), ExtraValue::Long(7)),
            ("probe_boolean".to_string(), ExtraValue::Boolean(true)),
            ("probe_float".to_string(), ExtraValue::Float(1.5)),
            ("probe_double".to_string(), ExtraValue::Double(0.25)),
            (
                "probe_string_array".to_string(),
                ExtraValue::StringArray(vec!["alpha".to_string(), "beta".to_string()]),
            ),
        ]),
    }
}

const PACKAGE: &str = "dev.acos.fixtures.launchextras";
const ACTIVITY: &str = "dev.acos.fixtures.launchextras.MainActivity";

fn golden_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/intent-launch-extras.bin")
}

#[test]
fn launch_intent_bytes_match_the_real_java_golden() {
    let bytes = ctx::encode_launch_intent(PACKAGE, ACTIVITY, &launch_intent())
        .expect("encode the launch Intent");
    if std::env::var_os("UPDATE_INTENT_GOLDEN").is_some() {
        std::fs::write(golden_path(), &bytes).expect("write the golden");
        eprintln!(
            "UPDATE_INTENT_GOLDEN: wrote {} bytes to {}",
            bytes.len(),
            golden_path().display()
        );
        return;
    }
    let golden = std::fs::read(golden_path()).unwrap_or_else(|error| {
        panic!(
            "missing golden {}: {error} (refresh with UPDATE_INTENT_GOLDEN=1 after the \
             real-Java differential passes)",
            golden_path().display()
        )
    });
    assert_eq!(
        golden,
        bytes,
        "the launch Intent bytes moved; the real-Java differential must be re-run before \
         the golden is refreshed"
    );
}

/// The golden is not an opaque blob: the codec reads it back into the same
/// action, data URI and typed extras. This catches a codec whose writer and
/// reader drift together.
#[test]
fn the_golden_decodes_to_the_declared_intent() {
    let bytes = std::fs::read(golden_path()).expect("golden");
    let decoded = ctx::decode("android.content.Intent", &bytes).expect("decode the Intent");
    let acos_services::value::Parcelable::Ordered { values, .. } = decoded else {
        panic!("the golden is not an Intent payload");
    };
    assert_eq!(
        values[acos_services::parcelable::INTENT_ACTION],
        Value::Str(Some("dev.acos.fixtures.launchextras.ACTION".to_string()))
    );
    assert_eq!(
        values[acos_services::parcelable::INTENT_DATA],
        Value::Str(Some("acos://launch/extras?source=scenario".to_string())),
        "the data URI is written as the image's StringUri"
    );
    assert_eq!(
        values[acos_services::parcelable::INTENT_FLAGS],
        Value::Int(0x1000_0000)
    );
    let Some(Value::Bundle(Some(bundle))) = values
        .get(acos_services::parcelable::INTENT_EXTRAS)
        .cloned()
    else {
        panic!("the Intent carries no extras Bundle");
    };
    for (key, expected) in [
        ("probe_string", Value::Str(Some("hello".to_string()))),
        ("probe_int", Value::Int(42)),
        ("probe_long", Value::Long(5_000_000_000)),
        ("probe_long_small", Value::Long(7)),
        ("probe_boolean", Value::Bool(true)),
        ("probe_float", Value::Float(1.5)),
        ("probe_double", Value::Double(0.25)),
        (
            "probe_string_array",
            Value::StringArray(Some(vec![
                Some("alpha".to_string()),
                Some("beta".to_string()),
            ])),
        ),
    ] {
        assert_eq!(bundle.get(key), Some(&expected), "extra `{key}`");
    }
}

/// A `Parcelable` extra is the one type this path refuses: it needs the app's
/// class loader, and silently dropping it is the WVFAIR bug.
#[test]
fn a_parcelable_extra_is_a_structured_refusal() {
    let intent = LaunchIntent {
        action: None,
        data: None,
        extras: BTreeMap::from([(
            "device".to_string(),
            ExtraValue::Parcelable("android.os.Bundle".to_string()),
        )]),
    };
    let error = ctx::encode_launch_intent(PACKAGE, ACTIVITY, &intent)
        .expect_err("a Parcelable extra is refused");
    let text = error.to_string();
    assert!(text.contains("device"), "{text}");
    assert!(text.contains("android.os.Bundle"), "{text}");
}

/// With no declared action the Intent is the launcher's stock one: MAIN + the
/// LAUNCHER category + the explicit component + NEW_TASK.
#[test]
fn the_stock_launch_intent_keeps_main_and_the_launcher_category() {
    let decoded = ctx::launch_intent(PACKAGE, ACTIVITY, &LaunchIntent::default())
        .expect("stock Intent");
    let acos_services::value::Parcelable::Ordered { values, .. } = decoded else {
        panic!("not an Intent");
    };
    assert_eq!(
        values[acos_services::parcelable::INTENT_ACTION],
        Value::Str8(Some("android.intent.action.MAIN".to_string())),
        "the writer value (Str8 is an Intent-level string on the wire)"
    );
    assert_eq!(
        values[acos_services::parcelable::INTENT_CATEGORIES],
        Value::StringList(Some(vec![Some(
            "android.intent.category.LAUNCHER".to_string()
        )]))
    );
    assert_eq!(
        values[acos_services::parcelable::INTENT_COMPONENT],
        Value::Parcelable(acos_services::value::Parcelable::ordered(
            "android.content.ComponentName",
            vec![Value::Str(Some(PACKAGE.to_string())), Value::Str(Some(ACTIVITY.to_string()))]
        ))
    );
}
