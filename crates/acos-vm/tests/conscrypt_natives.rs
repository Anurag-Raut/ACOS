//! Real-Android-14-corelib acceptance for the `com.android.org.conscrypt`
//! natives (`fwk_conscrypt.rs`).
//!
//! This runs the built `25_security_digest` DEX on the real boot classpath and
//! compares stdout byte-for-byte with the ART reference, in both the normal
//! and the collection-before-every-allocation (`ACOS_GC_STRESS`) modes. It
//! covers the whole chain the fixture exercises:
//!
//! * `java.security.Security` provider bootstrap (needs `conscrypt.jar` on the
//!   boot classpath and the reflective no-arg `Constructor` path);
//! * `MessageDigest` MD5/SHA-1/SHA-224/256/384/512;
//! * `SecureRandom` SHA1PRNG (`RAND_bytes`);
//! * `Mac` HmacSHA1/HmacSHA256;
//! * AES-128/256 ECB, AES-128 CBC and AES-128 CTR known-answer vectors.
//!
//! The test skips loudly when `fixtures/build` or the boot-classpath cache is
//! absent, exactly like the other real-corelib suites.

use std::path::{Path, PathBuf};

use acos_dex::DexFile;
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run_fixture(gc_stress: bool) -> Option<String> {
    let dex_path = repo_root().join("fixtures/build/dexprogs/25_security_digest/classes.dex");
    if !dex_path.is_file() {
        acos_test_support::require_prereq(false, "run_fixture", "conscrypt_natives: fixtures/build/dexprogs/25_security_digest is absent");
        return None;
    }
    let bytes = std::fs::read(&dex_path).expect("read fixture dex");
    let options = VmOptions {
        corelib: CoreLib::Real,
        gc_stress,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => {
            acos_test_support::require_prereq(false, "run_fixture", &format!("conscrypt_natives: {m}"));
            return None;
        }
        Err(e) => panic!("Vm::new: {e}"),
    };
    vm.add_dex(DexFile::parse(bytes).expect("parse fixture dex"));
    let outcome = vm.run("Main", &[]);
    assert_eq!(outcome.exit_code, 0, "fixture exit code");
    Some(outcome.stdout)
}

fn expected() -> String {
    std::fs::read_to_string(
        repo_root().join("fixtures/expected-art/dexprogs/25_security_digest.stdout"),
    )
    .expect("expected-art stdout")
}

#[test]
fn security_digest_matches_art_reference() {
    let Some(stdout) = run_fixture(false) else {
        return;
    };
    assert_eq!(stdout, expected());
}

#[test]
fn security_digest_matches_art_reference_under_gc_stress() {
    let Some(stdout) = run_fixture(true) else {
        return;
    };
    assert_eq!(stdout, expected());
}

/// The NIST-verified extension lines must be present and non-fabricated: each
/// is a published vector. This guards the fixture's own integrity even if the
/// expected file is regenerated.
#[test]
fn extension_lines_are_published_vectors() {
    let want = expected();
    for line in [
        "SHA-224=730e109bd7a8a32b1cb9d9a09aa2325d2430587ddbc0c38bad911525",
        "SHA-384=ca737f1014a48f4c0b6dd43cb177b0afd9e5169367544c494011e3317dbf9a509cb1e5dc1e85a941bbee3d7f2afbc9b1",
        "SHA-512=07e547d9586f6a73f73fbac0435ed76951218fb7d0c8d788a309d785436bbb642e93a252a954f23912547d1e8a3b5ed6e1bfd7097821233fa0538f3db854fee6",
        "HmacSHA1=2626f9b848d53f6c136bd14366fee076082bf7aa",
        "AES-128-ECB=69c4e0d86a7b0430d8cdb78070b4c55a",
        "AES-256-ECB=8ea2b7ca516745bfeafc49904b496089",
        "AES-128-CBC=7649abac8119b246cee98e9b12e9197d",
        "AES-128-CTR=874d6191b620e3261bef6864990db6ce",
    ] {
        assert!(want.contains(line), "expected-art is missing {line}");
    }
}
