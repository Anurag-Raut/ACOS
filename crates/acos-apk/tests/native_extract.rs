//! Native-library ABI gating and zip-slip-safe extraction.
//!
//! These exercise the "never trust names" half of the W59-A native enable path
//! without running any guest code: an APK whose only libs are for another ABI
//! is [`UNSUPPORTED_ABI`], and a traversal-shaped entry name is [`ZIP_SLIP`].

use std::io::Write;
use std::path::{Path, PathBuf};

use acos_apk::error::{NATIVE_LIB_NOT_FOUND, UNSUPPORTED_ABI, ZIP_SLIP};
use acos_apk::{Apk, HOST_ABI};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn hello_jni_apk() -> Option<PathBuf> {
    // Hard-fail under ACOS_REQUIRE_FIXTURES; otherwise a loud skip.
    if !acos_test_support::have_fixture("fixtures/build/apps/hello-jni.apk") {
        return None;
    }
    Some(repo_root().join("fixtures/build/apps/hello-jni.apk"))
}

/// Build a minimal in-memory APK containing exactly the given `lib/...` entries.
fn apk_with_libs(entries: &[(&str, &[u8])]) -> Apk {
    let mut buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            zip.start_file(*name, options).expect("start entry");
            zip.write_all(bytes).expect("write entry");
        }
        zip.finish().expect("finish zip");
    }
    Apk::from_bytes(buf).expect("parse synthetic apk")
}

#[test]
fn wrong_abi_is_structured_unsupported_abi() {
    let apk = apk_with_libs(&[
        ("lib/arm64-v8a/libfoo.so", b"\x7fELFarm"),
        ("lib/armeabi-v7a/libfoo.so", b"\x7fELFarm32"),
    ]);
    assert_eq!(apk.native_abis(), vec!["arm64-v8a", "armeabi-v7a"]);
    assert!(apk.has_native_libs());

    let err = apk.check_native_abi(HOST_ABI).expect_err("host ABI absent");
    assert_eq!(err.code(), UNSUPPORTED_ABI);
    assert!(err.message().contains("arm64-v8a"), "{}", err.message());
    assert!(err.message().contains("armeabi-v7a"), "{}", err.message());

    // The ABI that exists is accepted.
    apk.check_native_abi("arm64-v8a").expect("present ABI");
}

#[test]
fn apk_without_native_code_passes_abi_check() {
    let apk = apk_with_libs(&[("classes.dex", b"dex\n035\0")]);
    assert!(!apk.has_native_libs());
    apk.check_native_abi(HOST_ABI).expect("no native code");
}

#[test]
fn entry_names_with_traversal_are_rejected_before_extraction() {
    let apk = apk_with_libs(&[
        ("lib/x86_64/../evil.so", b"\x7fELFevil"),
        ("lib/x86_64/sub/evil.so", b"\x7fELFsub"),
        ("lib/x86_64/ok.so", b"\x7fELFok"),
    ]);
    // Traversal-shaped entries never become catalogue entries.
    let libs = apk.native_libs.get("x86_64").expect("x86_64 group");
    assert_eq!(
        libs.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        vec!["ok.so"]
    );

    let dest = std::env::temp_dir().join(format!("acos-zipslip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    for bad in ["../evil.so", "sub/evil.so", "a/../../b.so", ".", "..", ""] {
        let err = apk
            .extract_native_lib("x86_64", bad, &dest)
            .expect_err("must reject");
        assert_eq!(err.code(), ZIP_SLIP, "name {bad:?}: {err}");
    }
    // The benign name is not in the catalogue either way; it is missing only
    // under a different code.
    let err = apk
        .extract_native_lib("x86_64", "missing.so", &dest)
        .expect_err("missing");
    assert_eq!(err.code(), NATIVE_LIB_NOT_FOUND);
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn extract_writes_only_into_the_destination() {
    let apk = apk_with_libs(&[
        ("lib/x86_64/libfoo.so", b"\x7fELF x86_64 payload"),
        ("lib/arm64-v8a/libfoo.so", b"\x7fELF arm64 payload"),
    ]);
    let dest = std::env::temp_dir().join(format!("acos-extract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);

    let path = apk
        .extract_native_lib("x86_64", "libfoo.so", &dest)
        .expect("extract");
    assert_eq!(path.parent(), Some(dest.as_path()));
    assert_eq!(
        std::fs::read(&path).expect("read back"),
        b"\x7fELF x86_64 payload"
    );

    let all = apk.extract_native_libs("x86_64", &dest).expect("extract all");
    assert_eq!(all.len(), 1);
    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn hello_jni_fixture_apk_gates_and_extracts() {
    let Some(apk_path) = hello_jni_apk() else {
        eprintln!("SKIP: fixtures/build/apps/hello-jni.apk absent (run tools/build-fixtures.sh)");
        return;
    };
    let apk = Apk::open(&apk_path).expect("open fixture apk");
    assert!(apk.native_abis().iter().any(|a| a == HOST_ABI));
    apk.check_native_abi(HOST_ABI).expect("host abi present");

    let lib = apk
        .find_native_lib(HOST_ABI, "libhellojni.so")
        .expect("libhellojni in fixture");
    assert!(lib.size > 0);
    assert!(!lib.uncompressed_in_apk, "fixture is deflated");
    assert_eq!(lib.method, "deflate");
    assert!(lib.compressed_size > 0);
    assert!(lib.data_offset > 0);

    let dest = std::env::temp_dir().join(format!("acos-hello-jni-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest);
    let path = apk
        .extract_native_lib(HOST_ABI, "libhellojni.so", &dest)
        .expect("extract libhellojni");
    let bytes = std::fs::read(&path).expect("read lib");
    assert_eq!(&bytes[..4], b"\x7fELF");
    assert_eq!(bytes.len() as u64, lib.size);
    let _ = std::fs::remove_dir_all(&dest);
}
